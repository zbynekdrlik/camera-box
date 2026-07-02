"""#427 (#188 Task 6) — unit tests for scripts/av_sync_calibrate.py, the A/V-sync auto-set
controller: measured offset -> genlock video-delay over OBS WS + persisted av-sync-last.json.

Covers, with NO live OBS:
  a. required_delay_ms() -- MUST mirror camera_box::qpsk_marker::required_delay_ms
     (src/qpsk_marker.rs) sign + clamp EXACTLY (same 5 cases pinned on the Rust side).
  b. offset_from_verdict_json() -- reads the REAL landed `recording-verdict --av-sync`
     top-level `av_offset_ms` field (src/bin/recording-verdict.rs::run_av_sync); fails loud
     on a missing/unresolved measurement, never guesses 0.
  c. read_current_latency() -- reads genlock_latency_ms_src via GetInputSettings.
  d. apply_latency() happy path -- builds the correct SetInputSettings payload, verifies via
     read-back.
  e. apply_latency() verify-failure -- on a read-back mismatch, ROLLS BACK to the pre-change
     value and FAILS LOUD (SystemExit) -- the source is never left half-set (#358 pattern).
  f. write_last_json() -- persists {source, offset_ms, applied_latency_ms, ts} at the given path.
  g. default_last_json_path() -- resolves under %PROGRAMDATA%/camera-box when PROGRAMDATA is
     set (the real Windows OBS-box case), falls back to a local path otherwise (testable off-rig).
  h. CLI wiring -- --offset-ms/--verdict-json mutually exclusive; dry-run by default (no
     SetInputSettings without --apply); --apply drives the full apply + persist flow.
"""
import json
import pathlib
import sys

import pytest

# av_sync_calibrate.py does `from obs_phase2 import _conn, _rpc`, so scripts/ must be importable
# (same convention as tests/python/test_obs_burn_filter.py).
_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_calibrate  # noqa: E402


# ---------------------------------------------------------------------------
# (a) required_delay_ms -- MUST mirror src/qpsk_marker.rs::required_delay_ms exactly
# ---------------------------------------------------------------------------

class TestRequiredDelayMs:
    def test_video_lags_audio_reduces_delay(self):
        # video lags audio (+120 ms) -> reduce delay. Rust: required_delay_ms(1000, 120.0) == 880
        assert av_sync_calibrate.required_delay_ms(1000, 120.0) == 880

    def test_video_leads_audio_increases_delay(self):
        # video leads audio (-120 ms) -> increase delay. Rust: required_delay_ms(1000, -120.0) == 1120
        assert av_sync_calibrate.required_delay_ms(1000, -120.0) == 1120

    def test_clamps_low(self):
        assert av_sync_calibrate.required_delay_ms(1000, 5000.0) == 3

    def test_clamps_high(self):
        assert av_sync_calibrate.required_delay_ms(1000, -5000.0) == 2000

    def test_already_at_floor_stays_at_floor(self):
        assert av_sync_calibrate.required_delay_ms(3, 0.0) == 3

    def test_rounds_to_nearest_int(self):
        assert av_sync_calibrate.required_delay_ms(1000, 0.6) == 999  # round(1000 - 0.6) = 999


# ---------------------------------------------------------------------------
# (b) offset_from_verdict_json -- reads the REAL landed top-level `av_offset_ms` field
# ---------------------------------------------------------------------------

class TestOffsetFromVerdictJson:
    def test_reads_top_level_av_offset_ms(self, tmp_path):
        # This is the ACTUAL shape src/bin/recording-verdict.rs::run_av_sync prints -- flat,
        # NOT nested under an "av_sync" key.
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps({
            "av_offset_ms": 123.4,
            "mad_ms": 5.0,
            "matched": 6,
            "latency_adjust_ms": -123.4,
        }))
        assert av_sync_calibrate.offset_from_verdict_json(str(p)) == 123.4

    def test_missing_field_fails_loud(self, tmp_path):
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps({"matched": 0}))  # no av_offset_ms -- UNRESOLVED
        with pytest.raises(SystemExit):
            av_sync_calibrate.offset_from_verdict_json(str(p))

    def test_null_field_fails_loud(self, tmp_path):
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps({"av_offset_ms": None}))
        with pytest.raises(SystemExit):
            av_sync_calibrate.offset_from_verdict_json(str(p))


# ---------------------------------------------------------------------------
# fake OBS-websocket RPC layer (mirrors tests/python/test_obs_burn_filter.py's FakeObs)
# ---------------------------------------------------------------------------

class FakeObs:
    """Minimal in-memory OBS-WebSocket stand-in for genlock_latency_ms_src on one source."""

    def __init__(self, *, latency_ms=450, readback_override=None):
        self.latency_ms = latency_ms
        # When set, GetInputSettings AFTER a SetInputSettings returns this value instead of the
        # real one -- simulates a genuine read-back mismatch (e.g. the #292 force-drain class).
        self._readback_override = readback_override
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False, timeout_s=None):
        self.calls.append((method, dict(params or {})))
        if method == "GetInputSettings":
            reported = (
                self._readback_override
                if self._readback_override is not None
                else self.latency_ms
            )
            return {"inputSettings": {av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY: reported}}
        if method == "SetInputSettings":
            self.latency_ms = params["inputSettings"][av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY]
            return {}
        return {}

    def set_calls(self):
        return [(m, p) for (m, p) in self.calls if m == "SetInputSettings"]


# ---------------------------------------------------------------------------
# (c) read_current_latency
# ---------------------------------------------------------------------------

class TestReadCurrentLatency:
    def test_reads_genlock_latency_ms_src(self, monkeypatch):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        assert av_sync_calibrate.read_current_latency(None, "NDI 2ME PGM") == 450
        gets = [c for c in fake.calls if c[0] == "GetInputSettings"]
        assert any(p.get("inputName") == "NDI 2ME PGM" for _, p in gets)

    def test_defaults_to_floor_when_absent(self, monkeypatch):
        def rpc(ws, method, params=None, ignore_err=False, timeout_s=None):
            return {"inputSettings": {}}
        monkeypatch.setattr(av_sync_calibrate, "_rpc", rpc)
        assert av_sync_calibrate.read_current_latency(None, "NDI 2ME PGM") == 3


# ---------------------------------------------------------------------------
# (d) apply_latency happy path
# ---------------------------------------------------------------------------

class TestApplyLatencyHappyPath:
    def test_sets_and_verifies(self, monkeypatch):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        actual = av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 450, 880)
        assert actual == 880

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI 2ME PGM"
            and av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        assert len(latency_sets) == 1, f"expected exactly one apply (no rollback), got {sets}"
        assert latency_sets[0]["inputSettings"][av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY] == 880
        assert latency_sets[0].get("overlay") is True


# ---------------------------------------------------------------------------
# (e) apply_latency verify-failure -- rollback + fail loud, never half-set
# ---------------------------------------------------------------------------

class TestApplyLatencyRollback:
    def test_readback_mismatch_rolls_back_and_raises(self, monkeypatch):
        # GetInputSettings always reports 3ms no matter what we SET -- simulates the #292
        # force-drain class where the configured value never actually takes.
        fake = FakeObs(latency_ms=450, readback_override=3)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 450, 880)

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI 2ME PGM"
            and av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        # First SetInputSettings applies the NEW value; second ROLLS BACK to the pre-change value.
        assert len(latency_sets) == 2, f"expected apply + rollback, got {sets}"
        assert latency_sets[0]["inputSettings"][av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY] == 880
        assert latency_sets[1]["inputSettings"][av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY] == 450, (
            "verify-failure MUST roll back to the pre-change value -- never leave the source "
            "half-set"
        )

    def test_rollback_failure_still_raises_with_warning(self, monkeypatch, capsys):
        # Even the ROLLBACK read-back mismatches (450 requested, still reads 3) -- must still
        # fail loud (not silently accept a broken rollback) and print a LOUD warning.
        fake = FakeObs(latency_ms=450, readback_override=3)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 450, 880)

        captured = capsys.readouterr()
        combined = (captured.out + captured.err).lower()
        assert "warn" in combined or "manual check" in combined, (
            f"rollback failure must print a LOUD warning; got stdout={captured.out!r} "
            f"stderr={captured.err!r}"
        )


# ---------------------------------------------------------------------------
# (f) write_last_json shape
# ---------------------------------------------------------------------------

class TestWriteLastJson:
    def test_writes_expected_shape(self, tmp_path):
        json_path = tmp_path / "camera-box" / "av-sync-last.json"
        av_sync_calibrate.write_last_json(json_path, "NDI 2ME PGM", 123.4, 880)

        assert json_path.exists()
        data = json.loads(json_path.read_text())
        assert data["source"] == "NDI 2ME PGM"
        assert data["offset_ms"] == 123.4
        assert data["applied_latency_ms"] == 880
        assert "ts" in data and isinstance(data["ts"], (int, float))

    def test_creates_parent_dirs(self, tmp_path):
        json_path = tmp_path / "does" / "not" / "exist" / "av-sync-last.json"
        av_sync_calibrate.write_last_json(json_path, "NDI 2ME PGM", 0.0, 3)
        assert json_path.exists()


# ---------------------------------------------------------------------------
# (g) default_last_json_path
# ---------------------------------------------------------------------------

class TestDefaultLastJsonPath:
    def test_resolves_under_programdata_when_set(self, monkeypatch):
        monkeypatch.setenv("PROGRAMDATA", r"C:\ProgramData")
        p = av_sync_calibrate.default_last_json_path()
        assert str(p) in (
            r"C:\ProgramData\camera-box\av-sync-last.json",
            "C:/ProgramData/camera-box/av-sync-last.json",
        ) or (p.parts[-2:] == ("camera-box", "av-sync-last.json") and "ProgramData" in str(p))

    def test_falls_back_when_programdata_unset(self, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        p = av_sync_calibrate.default_last_json_path()
        assert p.name == "av-sync-last.json"
        assert p.parent.name in ("camera-box", ".camera-box")


# ---------------------------------------------------------------------------
# (h) CLI wiring
# ---------------------------------------------------------------------------

class TestCLI:
    def test_offset_ms_and_verdict_json_are_mutually_exclusive(self, monkeypatch, capsys):
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "h", "--offset-ms", "10", "--verdict-json", "x.json"],
        )
        with pytest.raises(SystemExit):
            av_sync_calibrate.main()

    def test_dry_run_never_calls_set_input_settings(self, monkeypatch, capsys):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0"],
        )
        av_sync_calibrate.main()
        assert fake.set_calls() == [], "dry-run (no --apply) must never call SetInputSettings"
        out = capsys.readouterr().out
        assert "dry-run" in out

    def test_apply_flag_drives_full_flow_and_persists(self, monkeypatch, tmp_path):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        json_path = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0",
             "--apply", "--json-path", str(json_path)],
        )
        av_sync_calibrate.main()

        assert fake.latency_ms == 330  # required_delay_ms(450, 120.0) == round(450-120) == 330
        assert json_path.exists()
        data = json.loads(json_path.read_text())
        assert data["applied_latency_ms"] == fake.latency_ms
        assert data["offset_ms"] == 120.0
