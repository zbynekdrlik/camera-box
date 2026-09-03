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
        # video lags audio (+30 ms, within the default 50ms/run step) -> reduce delay by exactly
        # the measured offset. Rust: required_delay_ms(1000, 30.0, 50) == 970
        assert av_sync_calibrate.required_delay_ms(1000, 30.0) == 970

    def test_video_leads_audio_increases_delay(self):
        # video leads audio (-30 ms, within step) -> increase delay by exactly the offset.
        # Rust: required_delay_ms(1000, -30.0, 50) == 1030
        assert av_sync_calibrate.required_delay_ms(1000, -30.0) == 1030

    def test_clamps_low(self):
        # current is already near the floor -- even a step-clamped move overshoots it, so the
        # hardware floor still applies AFTER the #871 step clamp (mirrors Rust's
        # required_delay_ms(10, 5000.0, 50) == 3 -- current=1000 would NOT reach the floor via a
        # single clamped 50ms step, so this uses a current near the edge on purpose).
        assert av_sync_calibrate.required_delay_ms(10, 5000.0) == 3

    def test_clamps_high(self):
        assert av_sync_calibrate.required_delay_ms(1990, -5000.0) == 2000

    def test_already_at_floor_stays_at_floor(self):
        assert av_sync_calibrate.required_delay_ms(3, 0.0) == 3

    def test_rounds_to_nearest_int(self):
        assert av_sync_calibrate.required_delay_ms(1000, 0.6) == 999  # round(1000 - 0.6) = 999


# ---------------------------------------------------------------------------
# (a2) #871 -- required_delay_ms() per-run STEP clamp
# ---------------------------------------------------------------------------

class TestRequiredDelayMsStepClamp:
    def test_large_offset_clamped_to_default_step(self):
        # #871: a single run may only move the applied latency by +/- AV_SYNC_MAX_STEP_MS
        # (default 50ms), never jump the whole raw distance in one step -- the incident this
        # fixes moved genlock_latency_ms_src 920 -> 1845ms (a ~925ms single-run correction) off a
        # measurement whose video timebase was corrupted by a delivery defect (#707).
        assert av_sync_calibrate.required_delay_ms(1000, 925.0) == 950  # not 75

    def test_large_negative_offset_clamped_to_default_step(self):
        assert av_sync_calibrate.required_delay_ms(1000, -925.0) == 1050  # not 1925

    def test_step_clamp_respects_env_override(self, monkeypatch):
        monkeypatch.setattr(av_sync_calibrate, "AV_SYNC_MAX_STEP_MS", 10)
        assert av_sync_calibrate.required_delay_ms(1000, 925.0) == 990

    def test_step_clamp_prints_loud_line_with_raw_and_residual(self, monkeypatch, capsys):
        # raw = round(1000 - 925) = 75; clamped result = 950; residual = raw - result = -875.
        av_sync_calibrate.required_delay_ms(1000, 925.0)
        err = capsys.readouterr().err
        assert "STEP CLAMPED" in err, f"expected a LOUD stderr line, got: {err!r}"
        assert "75" in err, f"raw target must be visible: {err!r}"
        assert "950" in err, f"applied (clamped) value must be visible: {err!r}"
        assert "-875" in err, f"remaining residual must be visible: {err!r}"

    def test_no_log_when_within_step(self, capsys):
        av_sync_calibrate.required_delay_ms(1000, 30.0)
        err = capsys.readouterr().err
        assert err == "", f"no clamp bit -- must stay silent, got: {err!r}"

    def test_default_max_step_is_50ms(self):
        if "AV_SYNC_MAX_STEP_MS" not in __import__("os").environ:
            assert av_sync_calibrate.AV_SYNC_MAX_STEP_MS == 50


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

        # #871: raw = round(450-120) = 330, but the per-run step clamp (default 50ms) limits the
        # move to current-50 = 400 -- not the full 120ms in one step.
        assert fake.latency_ms == 400
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


# ---------------------------------------------------------------------------
# #805 -- baseline calibration: aggregate N confident SyncNet windows -> average +
# sub-frame (parabolic) precision + 95% CI, offline (no OBS connection needed).
# ---------------------------------------------------------------------------

class TestParabolicSubframeOffset:
    def test_symmetric_curve_returns_zero(self):
        # true minimum exactly centered -- no sub-bin correction needed.
        assert av_sync_calibrate.parabolic_subframe_offset(5.0, 0.0, 5.0) == 0.0

    def test_peak_skewed_right_gives_positive_delta(self):
        # right neighbor is closer to the minimum than the left -- true minimum sits slightly
        # to the RIGHT of the reported bin.
        delta = av_sync_calibrate.parabolic_subframe_offset(5.0, 0.0, 1.0)
        assert 0.0 < delta <= 0.5

    def test_peak_skewed_left_gives_negative_delta(self):
        delta = av_sync_calibrate.parabolic_subframe_offset(1.0, 0.0, 5.0)
        assert -0.5 <= delta < 0.0

    def test_flat_curve_no_curvature_returns_zero(self):
        # collinear points (denominator ~0) -- nothing to interpolate, don't blow up.
        assert av_sync_calibrate.parabolic_subframe_offset(1.0, 2.0, 3.0) == 0.0

    def test_degenerate_curve_clamps_to_half_bin(self):
        # y-1=1, y0=0, y+1=-100: center is NOT the true local minimum (violates the normal
        # precondition) -- raw vertex formula would overshoot past +/-0.5; must clamp.
        delta = av_sync_calibrate.parabolic_subframe_offset(1.0, 0.0, -100.0)
        assert delta == -0.5


class TestWindowOffsetMs:
    def test_no_curve_falls_back_to_frame_quantized(self):
        ms = av_sync_calibrate.window_offset_ms({"offset_frames": 2, "confidence": 9.0})
        assert ms == 80.0  # 2 * FRAME_MS(40)

    def test_with_curve_applies_subframe_refinement(self):
        # right neighbor closer to the min -> true offset sits slightly above bin 2.
        rec = {"offset_frames": 2, "confidence": 9.0, "dist_curve": [5.0, 0.0, 1.0]}
        ms = av_sync_calibrate.window_offset_ms(rec)
        assert 80.0 < ms < 80.0 + 40.0 * 0.5

    def test_negative_offset_frames(self):
        ms = av_sync_calibrate.window_offset_ms({"offset_frames": -3, "confidence": 9.0})
        assert ms == -120.0


class TestAggregateSyncnetWindows:
    def test_filters_by_confidence_and_averages(self):
        records = [
            {"offset_frames": 2, "confidence": 9.0},   # 80ms, confident
            {"offset_frames": 3, "confidence": 8.0},   # 120ms, confident
            {"offset_frames": 20, "confidence": 1.0},  # low confidence -- excluded
        ]
        agg = av_sync_calibrate.aggregate_syncnet_windows(records)
        assert agg["n"] == 2
        assert agg["n_total"] == 3
        assert agg["mean_offset_ms"] == pytest.approx(100.0)

    def test_no_confident_windows_fails_loud(self):
        records = [{"offset_frames": 20, "confidence": 1.0}]
        with pytest.raises(SystemExit):
            av_sync_calibrate.aggregate_syncnet_windows(records)

    def test_single_window_has_no_ci(self):
        agg = av_sync_calibrate.aggregate_syncnet_windows([{"offset_frames": 2, "confidence": 9.0}])
        assert agg["n"] == 1
        assert agg["ci95_ms"] is None

    def test_multi_window_ci_shrinks_quantization_noise(self):
        # #805's own claim: averaging N +/-40ms-quantized windows should give a CI narrower than
        # a single frame's raw quantization step.
        records = [
            {"offset_frames": 2, "confidence": 9.0},
            {"offset_frames": 3, "confidence": 9.0},
            {"offset_frames": 2, "confidence": 9.0},
            {"offset_frames": 3, "confidence": 9.0},
            {"offset_frames": 2, "confidence": 9.0},
        ]
        agg = av_sync_calibrate.aggregate_syncnet_windows(records)
        assert agg["n"] == 5
        assert agg["ci95_ms"] is not None
        assert agg["ci95_ms"] < 40.0

    def test_respects_custom_conf_min(self):
        records = [{"offset_frames": 2, "confidence": 5.0}]
        with pytest.raises(SystemExit):
            av_sync_calibrate.aggregate_syncnet_windows(records, conf_min=6.0)
        agg = av_sync_calibrate.aggregate_syncnet_windows(records, conf_min=4.0)
        assert agg["n"] == 1


class TestBaselineLatencyMs:
    def test_moves_the_full_distance_no_step_clamp(self):
        # unlike required_delay_ms(), a baseline recalibration is the trusted averaged result --
        # allowed to jump the FULL distance in one shot (only the hardware range clamps it).
        assert av_sync_calibrate.baseline_latency_ms(1000, 925.0) == 75

    def test_clamps_to_hardware_floor(self):
        assert av_sync_calibrate.baseline_latency_ms(10, 5000.0) == av_sync_calibrate.LATENCY_MIN

    def test_clamps_to_hardware_ceiling(self):
        assert av_sync_calibrate.baseline_latency_ms(1990, -5000.0) == av_sync_calibrate.LATENCY_MAX


class TestFormatCalibrationReport:
    def test_reports_recommendation_without_current_latency(self):
        agg = {"n": 3, "n_total": 3, "mean_offset_ms": 50.0, "stdev_ms": 5.0, "ci95_ms": 6.0}
        report = av_sync_calibrate.format_calibration_report(agg)
        assert "n=3/3" in report
        assert "CI" in report
        assert "-50.0" in report  # adjust by -offset when no absolute target is computable

    def test_reports_absolute_target_with_current_latency(self):
        agg = {"n": 3, "n_total": 3, "mean_offset_ms": 50.0, "stdev_ms": 5.0, "ci95_ms": 6.0}
        report = av_sync_calibrate.format_calibration_report(agg, current_latency_ms=1000)
        assert "950" in report  # baseline_latency_ms(1000, 50.0) == 950
        assert "1000" in report

    def test_reports_na_ci_for_single_window(self):
        agg = {"n": 1, "n_total": 1, "mean_offset_ms": 50.0, "stdev_ms": 0.0, "ci95_ms": None}
        report = av_sync_calibrate.format_calibration_report(agg)
        assert "n/a" in report


class TestLoadCalibrationWindows:
    def test_reads_jsonl_skipping_blank_lines(self, tmp_path):
        p = tmp_path / "calib.jsonl"
        p.write_text(
            json.dumps({"offset_frames": 2, "confidence": 9.0}) + "\n"
            "\n"
            + json.dumps({"offset_frames": 3, "confidence": 8.0}) + "\n"
        )
        records = av_sync_calibrate.load_calibration_windows(str(p))
        assert len(records) == 2
        assert records[0]["offset_frames"] == 2
        assert records[1]["offset_frames"] == 3


class TestCLICalibrateMode:
    def test_calibrate_mode_needs_no_host(self, tmp_path, capsys):
        log_path = tmp_path / "calib.jsonl"
        log_path.write_text(
            "\n".join(
                json.dumps({"offset_frames": f, "confidence": 9.0}) for f in (2, 3, 2)
            )
            + "\n"
        )
        with pytest.MonkeyPatch.context() as m:
            m.setattr(
                sys, "argv",
                ["av_sync_calibrate.py", "--calibrate", str(log_path)],
            )
            av_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "CALIBRATION" in out
        assert "RECOMMENDATION" in out

    def test_calibrate_mode_with_current_latency_and_report_json(self, tmp_path, capsys):
        log_path = tmp_path / "calib.jsonl"
        log_path.write_text(json.dumps({"offset_frames": 2, "confidence": 9.0}) + "\n")
        report_path = tmp_path / "report.json"
        with pytest.MonkeyPatch.context() as m:
            m.setattr(
                sys, "argv",
                ["av_sync_calibrate.py", "--calibrate", str(log_path),
                 "--current-latency-ms", "1000", "--report-json", str(report_path)],
            )
            av_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "920" in out  # baseline_latency_ms(1000, 80.0) == round(1000-80) == 920

        assert report_path.exists()
        data = json.loads(report_path.read_text())
        assert data["n"] == 1

    def test_no_mode_selected_fails_loud(self, monkeypatch):
        monkeypatch.setattr(sys, "argv", ["av_sync_calibrate.py"])
        with pytest.raises(SystemExit):
            av_sync_calibrate.main()

    def test_offset_mode_without_calibrate_still_requires_host(self, monkeypatch):
        monkeypatch.setattr(sys, "argv", ["av_sync_calibrate.py", "--offset-ms", "10"])
        with pytest.raises(SystemExit):
            av_sync_calibrate.main()


# ---------------------------------------------------------------------------
# #1265 -- the #856 controller loop-gain damping context: write_last_json ADDS loop_gain +
# combined_offset_ms_raw when the #856 controller passes them (never renames/removes the existing
# source/offset_ms/applied_latency_ms/ts keys -- av-sync-last.json is a live data contract), and
# main() emits ONE grep-able gain log line at apply time showing combined/gain/damped/clamped/pin.
# The offset_ms this script APPLIES is already the damped value (the gain is applied upstream at
# [8/8g]); --loop-gain/--combined-offset-ms are passed for logging + persistence only.
# ---------------------------------------------------------------------------

class TestWriteLastJsonLoopGainKeys:
    def test_adds_loop_gain_and_raw_when_given(self, tmp_path):
        json_path = tmp_path / "av-sync-last.json"
        av_sync_calibrate.write_last_json(
            json_path, "NDI 2ME PGM", -24.54, 938,
            loop_gain=0.4, combined_offset_ms_raw=-61.35,
        )
        data = json.loads(json_path.read_text())
        # existing contract keys still present + unchanged shape
        assert data["source"] == "NDI 2ME PGM"
        assert data["offset_ms"] == -24.54
        assert data["applied_latency_ms"] == 938
        assert "ts" in data
        # new #1265 keys
        assert data["loop_gain"] == pytest.approx(0.4)
        assert data["combined_offset_ms_raw"] == pytest.approx(-61.35)

    def test_omits_new_keys_when_not_given(self, tmp_path):
        # the operator/aligner path (no gain context) keeps the old schema byte-for-byte -- no
        # loop_gain / combined_offset_ms_raw keys appear at all.
        json_path = tmp_path / "av-sync-last.json"
        av_sync_calibrate.write_last_json(json_path, "NDI 2ME PGM", 12.0, 900)
        data = json.loads(json_path.read_text())
        assert "loop_gain" not in data
        assert "combined_offset_ms_raw" not in data
        assert set(data) == {"source", "offset_ms", "applied_latency_ms", "ts"}


class TestGainLogLineAndApply:
    def _run_apply(self, monkeypatch, tmp_path, current, offset, extra_args):
        fake = FakeObs(latency_ms=current)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        json_path = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", str(offset),
             "--apply", "--json-path", str(json_path)] + extra_args,
        )
        av_sync_calibrate.main()
        return fake, json_path

    def test_616_scenario_pin_913_damped_lands_at_938_with_gain_line(self, monkeypatch, tmp_path, capsys):
        # damped -24.54 (0.4 * combined -61.35) applied at pin 913 -> 938 ~ predicted null 940.
        fake, json_path = self._run_apply(
            monkeypatch, tmp_path, current=913, offset=-24.54,
            extra_args=["--loop-gain", "0.4", "--combined-offset-ms", "-61.35"],
        )
        assert fake.latency_ms == 938
        out = capsys.readouterr().out
        assert "[av-sync] gain:" in out, f"expected the grep-able gain line, got: {out!r}"
        assert "combined=-61.35" in out
        assert "gain=0.40" in out
        assert "damped=-24.54" in out
        assert "clamped=-24.54" in out
        assert "pin 913 -> 938" in out
        # persists the #1265 keys
        data = json.loads(json_path.read_text())
        assert data["loop_gain"] == pytest.approx(0.4)
        assert data["combined_offset_ms_raw"] == pytest.approx(-61.35)
        assert data["applied_latency_ms"] == 938

    def test_set_line_stays_byte_identical(self, monkeypatch, tmp_path, capsys):
        # other consumers grep the exact `[av-sync] SET '...' genlock_latency_ms_src: A -> B` line;
        # the gain line is ADDITIONAL, never a replacement.
        self._run_apply(
            monkeypatch, tmp_path, current=913, offset=-24.54,
            extra_args=["--loop-gain", "0.4", "--combined-offset-ms", "-61.35"],
        )
        out = capsys.readouterr().out
        assert "[av-sync] SET 'NDI 2ME PGM' genlock_latency_ms_src: 913 -> 938" in out

    def test_gain_line_shows_the_step_clamp_when_it_bites(self, monkeypatch, tmp_path, capsys):
        # a damped offset larger than the +/-50/run step: clamped shows the +/-50-limited offset,
        # pin shows the real clamped result.
        self._run_apply(
            monkeypatch, tmp_path, current=1000, offset=-80.0,
            extra_args=["--loop-gain", "0.4", "--combined-offset-ms", "-200.0"],
        )
        out = capsys.readouterr().out
        assert "[av-sync] gain:" in out
        assert "damped=-80.00" in out
        assert "clamped=-50.00" in out  # +/-50/run step clamp on the offset
        assert "pin 1000 -> 1050" in out

    def test_no_gain_line_without_loop_gain_arg(self, monkeypatch, tmp_path, capsys):
        # the operator/aligner path (no --loop-gain) must NOT emit a gain line or persist the keys.
        _, json_path = self._run_apply(
            monkeypatch, tmp_path, current=1000, offset=20.0, extra_args=[],
        )
        out = capsys.readouterr().out
        assert "[av-sync] gain:" not in out
        data = json.loads(json_path.read_text())
        assert "loop_gain" not in data
        assert "combined_offset_ms_raw" not in data
