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
# #707 -- the FINAL applied genlock_latency_ms_src must never go below the strih genlock FIFO's
# measured jitter-reserve floor, even though this controller's own clamp (LATENCY_MIN, the
# DistroAV hardware minimum) is much lower and unrelated. This controller writes the SAME OBS
# property phase_sync_calibrate.py computes per-camera offsets for -- without this floor, an
# A/V correction applied afterwards could silently undercut what phase-sync already respects.
# ---------------------------------------------------------------------------

class TestGenlockJitterFloor:
    def test_floor_matches_phase_sync_calibrate_in_lock_step(self):
        import phase_sync_calibrate
        assert (
            av_sync_calibrate.GENLOCK_JITTER_FLOOR_MS
            == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS
        ), "av_sync_calibrate's jitter floor must stay in lock-step with phase_sync_calibrate's"

    def test_clamping_below_floor_warns_loudly_never_silently(self, capsys):
        """#707 follow-up: the clamp raises an A/V ALIGNMENT hold, which is a different quantity
        from the per-camera FIFO jitter reserve sharing the same OBS property. If it ever bites,
        the applied hold is NOT the computed alignment -- audio can be out of sync by up to the
        floor. That must be visible in the run output, never silent."""
        out = av_sync_calibrate.enforce_jitter_floor_ms(10)
        assert out == av_sync_calibrate.GENLOCK_JITTER_FLOOR_MS
        err = capsys.readouterr().err
        assert "BELOW the genlock jitter floor" in err, (
            f"clamping an A/V hold up must warn loudly on stderr, got: {err!r}"
        )
        assert "out of sync by up to" in err, (
            f"the warning must state the alignment error it introduces, got: {err!r}"
        )

    def test_no_warning_when_the_clamp_does_not_bite(self, capsys):
        """The normal operating point (~973ms measured hold) must stay silent -- a warning that
        fires every run is noise nobody reads."""
        out = av_sync_calibrate.enforce_jitter_floor_ms(973)
        assert out == 973
        assert capsys.readouterr().err == ""

    def test_below_floor_target_is_clamped_up_before_writing(self, monkeypatch):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        actual = av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 450, 1)
        assert actual == av_sync_calibrate.GENLOCK_JITTER_FLOOR_MS

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI 2ME PGM"
            and av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        assert len(latency_sets) == 1, f"expected exactly one apply (no rollback), got {sets}"
        assert latency_sets[0]["inputSettings"][
            av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
        ] == av_sync_calibrate.GENLOCK_JITTER_FLOOR_MS

    def test_a_value_already_above_the_floor_is_unaffected(self, monkeypatch):
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        actual = av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 450, 880)
        assert actual == 880


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
# (i) #465/#529 -- remote push plan: when run OFF the stream box (the normal case -- this
# script connects to --host over the OBS WebSocket, it does not need to run ON that box),
# default_last_json_path() falls back to a LOCAL path nothing on the stream box can read.
# scp/ssh to Windows was historically believed denied on this rig; #701 proved plain scp/ssh
# actually reaches strih/stream with the targets.md creds, but for a short in-memory JSON blob
# like this one, the established channel this script still uses is the win-* MCP FileWrite tool
# (recording-fetch-windows.sh, obs-self-heal-install.sh use the same PLAN convention), driven by the
# operator/agent. remote_push_plan() prints an explicit, copy-pasteable plan -- same convention
# as obs-self-heal-install.sh's PLAN block -- instead of silently leaving a file nobody reads.
# ---------------------------------------------------------------------------

class TestMcpNameForHost:
    def test_stream_host_resolves_to_win_stream_snv(self):
        assert av_sync_calibrate.mcp_name_for_host("10.77.9.204") == "win-stream-snv"

    def test_strih_host_resolves_to_win_strih(self):
        assert av_sync_calibrate.mcp_name_for_host("10.77.9.202") == "win-strih"

    def test_unknown_host_returns_none(self):
        assert av_sync_calibrate.mcp_name_for_host("10.0.0.99") is None


class TestRemotePushPlan:
    def test_plan_names_the_canonical_windows_destination(self):
        payload = {
            "source": "NDI 2ME PGM", "offset_ms": 123.4, "applied_latency_ms": 926,
            "ts": 1720000000.0,
        }
        plan = av_sync_calibrate.remote_push_plan("10.77.9.204", payload)
        assert r"C:\ProgramData\camera-box\av-sync-last.json" in plan
        assert "win-stream-snv" in plan
        assert "10.77.9.204" in plan
        assert "FileWrite" in plan

    def test_plan_includes_the_exact_json_content(self):
        payload = {
            "source": "NDI 2ME PGM", "offset_ms": -82.4, "applied_latency_ms": 926,
            "ts": 1720000000.0,
        }
        plan = av_sync_calibrate.remote_push_plan("10.77.9.204", payload)
        embedded = json.loads(plan.split("content:\n", 1)[1])
        assert embedded == payload

    def test_plan_for_unknown_host_still_names_the_destination(self):
        payload = {"source": "x", "offset_ms": 0.0, "applied_latency_ms": 3, "ts": 1.0}
        plan = av_sync_calibrate.remote_push_plan("10.0.0.99", payload)
        assert r"C:\ProgramData\camera-box\av-sync-last.json" in plan
        assert "10.0.0.99" in plan


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

    def test_explicit_json_path_suppresses_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # An explicit --json-path means the caller is taking control of the destination
        # themselves (e.g. a future on-box runner) -- no auto plan needed.
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        json_path = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0",
             "--apply", "--json-path", str(json_path)],
        )
        av_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" not in out

    def test_default_path_off_box_prints_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # #465 finding: run from dev1 (PROGRAMDATA unset) with the DEFAULT path -- the write
        # lands under ~/.camera-box, which nothing on the stream box can read. main() must
        # surface an explicit push plan so the operator/agent pushes it via MCP FileWrite.
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        monkeypatch.setattr(
            av_sync_calibrate, "default_last_json_path",
            lambda: tmp_path / ".camera-box" / "av-sync-last.json",
        )
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0", "--apply"],
        )
        av_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" in out
        assert r"C:\ProgramData\camera-box\av-sync-last.json" in out
        assert "win-stream-snv" in out

    def test_printed_plan_json_matches_what_was_actually_persisted(
        self, monkeypatch, tmp_path, capsys,
    ):
        # Integration-level drift guard: the plan's embedded JSON block must be byte-identical
        # to what write_last_json() actually wrote to json_path -- never reconstructed
        # separately (that would let the pushed content silently diverge from the local record).
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        local_json_path = tmp_path / ".camera-box" / "av-sync-last.json"
        monkeypatch.setattr(
            av_sync_calibrate, "default_last_json_path", lambda: local_json_path,
        )
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0", "--apply"],
        )
        av_sync_calibrate.main()
        out = capsys.readouterr().out

        persisted = json.loads(local_json_path.read_text())
        printed = json.loads(out.split("content:\n", 1)[1])
        assert printed == persisted

    def test_default_path_on_box_does_not_print_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # If PROGRAMDATA IS set (running ON the Windows box), default_last_json_path() already
        # resolves to the canonical stream-box path -- no push needed.
        fake = FakeObs(latency_ms=450)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setenv("PROGRAMDATA", str(tmp_path / "ProgramData"))
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "120.0", "--apply"],
        )
        av_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" not in out
