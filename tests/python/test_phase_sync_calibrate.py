"""#286 -- unit tests for scripts/phase_sync_calibrate.py, the 4-camera MUTUAL phase-sync
auto-set controller: measured per-camera cam->strih latencies -> per-camera genlock video-delay
over OBS WS + persisted phase-sync-last.json.

Covers, with NO live OBS:
  a. compute_phase_sync_offsets() / _find_gate_bin() / _run_gate_bin() -- #438: the offset
     MATH is no longer duplicated here. compute_phase_sync_offsets() DELEGATES to the compiled
     `phase-sync-gate` Rust binary (the SAME camera_box::phase_sync::compute_phase_sync_offsets
     kernel src/phase_sync.rs's 9 unit tests lock, proven identical at the CLI boundary by
     tests/harness_phase_sync_gate.rs) -- these tests cover the I/O WIRING (binary located
     correctly, correct JSON piped in, correct JSON parsed out, failures surfaced loudly),
     never re-derive the formula in Python.
  b. load_measured_json() -- reads the {source: latency_ms} measurement input; fails loud on
     an empty/malformed/missing file, never guesses a camera's latency.
  c. read_current_latency() -- reads genlock_latency_ms_src via GetInputSettings.
  d. apply_latency() happy path -- builds the correct SetInputSettings payload, verifies via
     read-back (same shape as av_sync_calibrate.apply_latency).
  e. apply_latency() verify-failure -- on a read-back mismatch, ROLLS BACK to the pre-change
     value and FAILS LOUD (SystemExit) -- the source is never left half-set (#358 pattern).
  f. write_last_json() -- persists {"cameras": [...], "ts": ...} at the given path.
  g. default_last_json_path() -- resolves under %PROGRAMDATA%/camera-box when PROGRAMDATA is
     set, falls back to a local path otherwise (testable off-rig).
  h. CLI wiring -- dry-run by default (no SetInputSettings without --apply); --apply drives the
     full multi-camera apply + persist flow, one genlock latency per strih source.
  i. active_ndi_sources() / main()'s active-set filter (#893) -- --measured-json entries for a
     source NOT in CAMERA_ACTIVE_SET are ignored (WARNED, never silently applied) before offsets
     are computed/applied, so a stale/foreign source can never corrupt the "slowest" formula or
     get a pin written to it.
"""
import json
import pathlib
import sys

import pytest

# phase_sync_calibrate.py does `from obs_phase2 import _conn, _rpc`, so scripts/ must be
# importable (same convention as tests/python/test_av_sync_calibrate.py).
_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import phase_sync_calibrate  # noqa: E402


# ---------------------------------------------------------------------------
# (a) compute_phase_sync_offsets / _find_gate_bin / _run_gate_bin -- #438: I/O wiring to the
# compiled phase-sync-gate Rust binary. The FORMULA itself is proven only in Rust (9 kernel
# unit tests in src/phase_sync.rs + tests/harness_phase_sync_gate.rs's CLI-boundary parity
# checks against those same kernel test vectors) -- these tests never re-derive it.
# ---------------------------------------------------------------------------

class TestComputePhaseSyncOffsets:
    def test_empty_input_yields_empty_output_without_invoking_the_binary(self, monkeypatch):
        calls = []
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin", lambda *a, **k: calls.append((a, k))
        )
        assert phase_sync_calibrate.compute_phase_sync_offsets({}) == {}
        assert calls == [], "empty input must never invoke the gate binary"

    def test_delegates_to_run_gate_bin_and_returns_its_result(self, monkeypatch):
        seen = {}

        def fake_run_gate_bin(measured, gate_bin=None):
            seen["measured"] = measured
            seen["gate_bin"] = gate_bin
            return {"cam1": 33, "cam2": 3}

        monkeypatch.setattr(phase_sync_calibrate, "_run_gate_bin", fake_run_gate_bin)
        out = phase_sync_calibrate.compute_phase_sync_offsets(
            {"cam1": 50.0, "cam2": 80.0}, gate_bin="/custom/phase-sync-gate"
        )
        assert out == {"cam1": 33, "cam2": 3}
        assert seen["measured"] == {"cam1": 50.0, "cam2": 80.0}
        assert seen["gate_bin"] == "/custom/phase-sync-gate"


class TestFindGateBin:
    def test_prefers_explicit_arg(self, tmp_path):
        fake_bin = tmp_path / "phase-sync-gate"
        fake_bin.write_text("#!/bin/sh\n")
        assert phase_sync_calibrate._find_gate_bin(str(fake_bin)) == str(fake_bin)

    def test_uses_env_var_when_no_explicit_arg(self, tmp_path, monkeypatch):
        fake_bin = tmp_path / "phase-sync-gate"
        fake_bin.write_text("#!/bin/sh\n")
        monkeypatch.setenv("PHASE_SYNC_GATE_BIN", str(fake_bin))
        assert phase_sync_calibrate._find_gate_bin(None) == str(fake_bin)

    def test_uses_probe_bin_dir_when_no_env_var(self, tmp_path, monkeypatch):
        (tmp_path / "phase-sync-gate").write_text("#!/bin/sh\n")
        monkeypatch.delenv("PHASE_SYNC_GATE_BIN", raising=False)
        monkeypatch.setenv("PROBE_BIN_DIR", str(tmp_path))
        found = phase_sync_calibrate._find_gate_bin(None)
        assert found == str(tmp_path / "phase-sync-gate")

    def test_exits_when_not_found_anywhere(self, monkeypatch, tmp_path):
        monkeypatch.delenv("PHASE_SYNC_GATE_BIN", raising=False)
        monkeypatch.setenv("PROBE_BIN_DIR", str(tmp_path))  # empty dir, binary absent
        with pytest.raises(SystemExit):
            phase_sync_calibrate._find_gate_bin(None)


class TestRunGateBin:
    def test_pipes_measured_json_on_stdin_and_parses_stdout(self, monkeypatch, tmp_path):
        fake_bin = str(tmp_path / "phase-sync-gate")
        seen = {}

        def fake_subprocess_run(cmd, input=None, capture_output=None):
            seen["cmd"] = cmd
            seen["stdin"] = json.loads(input.decode())

            class FakeResult:
                returncode = 0
                stdout = b'{"cam1": 33, "cam2": 3}'
                stderr = b""

            return FakeResult()

        monkeypatch.setattr(phase_sync_calibrate, "_find_gate_bin", lambda explicit: fake_bin)
        monkeypatch.setattr(phase_sync_calibrate.subprocess, "run", fake_subprocess_run)

        out = phase_sync_calibrate._run_gate_bin({"cam1": 50.0, "cam2": 80.0})
        assert out == {"cam1": 33, "cam2": 3}
        assert seen["cmd"] == [fake_bin]
        assert seen["stdin"] == {"cam1": 50.0, "cam2": 80.0}

    def test_nonzero_exit_raises_systemexit_with_stderr(self, monkeypatch):
        def fake_subprocess_run(cmd, input=None, capture_output=None):
            class FakeResult:
                returncode = 2
                stdout = b""
                stderr = b"ERROR: source 'cam1': latency value is missing or not a number"

            return FakeResult()

        monkeypatch.setattr(phase_sync_calibrate, "_find_gate_bin", lambda explicit: "/x")
        monkeypatch.setattr(phase_sync_calibrate.subprocess, "run", fake_subprocess_run)

        with pytest.raises(SystemExit, match="latency value is missing"):
            phase_sync_calibrate._run_gate_bin({"cam1": None})

    def test_malformed_stdout_json_raises_systemexit(self, monkeypatch):
        def fake_subprocess_run(cmd, input=None, capture_output=None):
            class FakeResult:
                returncode = 0
                stdout = b"not json"
                stderr = b""

            return FakeResult()

        monkeypatch.setattr(phase_sync_calibrate, "_find_gate_bin", lambda explicit: "/x")
        monkeypatch.setattr(phase_sync_calibrate.subprocess, "run", fake_subprocess_run)

        with pytest.raises(SystemExit, match="invalid JSON"):
            phase_sync_calibrate._run_gate_bin({"cam1": 50.0})


# ---------------------------------------------------------------------------
# (b) load_measured_json
# ---------------------------------------------------------------------------

class TestLoadMeasuredJson:
    def test_loads_source_to_latency_mapping(self, tmp_path):
        p = tmp_path / "measured.json"
        p.write_text(json.dumps({"NDI cam5": 100.0, "NDI cam1": 90.0}))
        assert phase_sync_calibrate.load_measured_json(str(p)) == {
            "NDI cam5": 100.0,
            "NDI cam1": 90.0,
        }

    def test_empty_object_fails_loud(self, tmp_path):
        p = tmp_path / "measured.json"
        p.write_text(json.dumps({}))
        with pytest.raises(SystemExit):
            phase_sync_calibrate.load_measured_json(str(p))

    def test_non_object_fails_loud(self, tmp_path):
        p = tmp_path / "measured.json"
        p.write_text(json.dumps([1, 2, 3]))
        with pytest.raises(SystemExit):
            phase_sync_calibrate.load_measured_json(str(p))


class TestApplyMargin:
    """#757 (2026-07-15 live regression): zero-headroom pins produced a uniform copies≈gaps
    pattern on EVERY camera (ordinary jitter flipping frames across the ts-align deadline).
    apply_margin() raises the floor by a uniform constant without disturbing the relative
    "slowest lowest pin, fastest highest" ordering the offset kernel already establishes."""

    def test_shifts_every_offset_by_the_same_constant(self):
        offsets = {"NDI cam1": 3, "NDI cam4": 47, "NDI cam5": 8}
        out = phase_sync_calibrate.apply_margin(offsets, 10)
        assert out == {"NDI cam1": 13, "NDI cam4": 57, "NDI cam5": 18}

    def test_preserves_relative_ordering_and_differences(self):
        offsets = {"NDI cam1": 3, "NDI cam4": 47}
        out = phase_sync_calibrate.apply_margin(offsets, 15)
        assert out["NDI cam4"] - out["NDI cam1"] == offsets["NDI cam4"] - offsets["NDI cam1"]

    def test_rounds_to_the_nearest_whole_ms(self):
        offsets = {"NDI cam1": 3.0}
        out = phase_sync_calibrate.apply_margin(offsets, 10.6)
        assert out == {"NDI cam1": 14}  # round(13.6) == 14

    def test_zero_or_negative_margin_is_a_noop(self):
        offsets = {"NDI cam1": 3, "NDI cam4": 47}
        assert phase_sync_calibrate.apply_margin(offsets, 0) == offsets
        assert phase_sync_calibrate.apply_margin(offsets, -5) == offsets

    def test_never_mutates_the_input_dict(self):
        offsets = {"NDI cam1": 3}
        phase_sync_calibrate.apply_margin(offsets, 10)
        assert offsets == {"NDI cam1": 3}, "apply_margin must not mutate its input"

    def test_empty_offsets_returns_empty(self):
        assert phase_sync_calibrate.apply_margin({}, 10) == {}


# ---------------------------------------------------------------------------
# fake OBS-websocket RPC layer (mirrors tests/python/test_av_sync_calibrate.py's FakeObs,
# extended to track PER-SOURCE state since #286 applies N sources, not one)
# ---------------------------------------------------------------------------

class FakeObs:
    """Minimal in-memory OBS-WebSocket stand-in for genlock_latency_ms_src across MULTIPLE
    strih NDI sources (#286 sets one latency per camera's source, unlike #427's single
    source)."""

    def __init__(self, *, latencies=None, readback_override_source=None,
                 readback_override_value=None):
        self.latencies = dict(latencies or {})
        # When set, a GetInputSettings for THIS source after a Set returns this value instead
        # of the real one -- simulates a genuine read-back mismatch (e.g. #292 force-drain).
        self._readback_override_source = readback_override_source
        self._readback_override_value = readback_override_value
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False, timeout_s=None):
        self.calls.append((method, dict(params or {})))
        name = (params or {}).get("inputName")
        if method == "GetInputSettings":
            if name == self._readback_override_source:
                reported = self._readback_override_value
            else:
                reported = self.latencies.get(
                    name, phase_sync_calibrate.PHASE_SYNC_FLOOR_MS
                )
            return {"inputSettings": {phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY: reported}}
        if method == "SetInputSettings":
            self.latencies[name] = params["inputSettings"][
                phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
            ]
            return {}
        return {}

    def set_calls(self):
        return [(m, p) for (m, p) in self.calls if m == "SetInputSettings"]


# ---------------------------------------------------------------------------
# (c) read_current_latency
# ---------------------------------------------------------------------------

class TestReadCurrentLatency:
    def test_reads_genlock_latency_ms_src(self, monkeypatch):
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        assert phase_sync_calibrate.read_current_latency(None, "NDI cam5") == 450

    def test_defaults_to_floor_when_absent(self, monkeypatch):
        def rpc(ws, method, params=None, ignore_err=False, timeout_s=None):
            return {"inputSettings": {}}
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", rpc)
        assert (
            phase_sync_calibrate.read_current_latency(None, "NDI cam5")
            == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS
        )


# ---------------------------------------------------------------------------
# (d) apply_latency happy path
# ---------------------------------------------------------------------------

class TestApplyLatencyHappyPath:
    def test_sets_and_verifies(self, monkeypatch):
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        actual = phase_sync_calibrate.apply_latency(None, "NDI cam5", 450, 33)
        assert actual == 33

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI cam5"
            and phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        assert len(latency_sets) == 1, f"expected exactly one apply (no rollback), got {sets}"
        assert latency_sets[0]["inputSettings"][
            phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
        ] == 33
        assert latency_sets[0].get("overlay") is True


# ---------------------------------------------------------------------------
# (e) apply_latency verify-failure -- rollback + fail loud, never half-set
# ---------------------------------------------------------------------------

class TestApplyLatencyRollback:
    def test_readback_mismatch_rolls_back_and_raises(self, monkeypatch):
        fake = FakeObs(
            latencies={"NDI cam5": 450},
            readback_override_source="NDI cam5",
            readback_override_value=3,
        )
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            phase_sync_calibrate.apply_latency(None, "NDI cam5", 450, 33)

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI cam5"
            and phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        assert len(latency_sets) == 2, f"expected apply + rollback, got {sets}"
        assert latency_sets[0]["inputSettings"][
            phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
        ] == 33
        assert latency_sets[1]["inputSettings"][
            phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
        ] == 450, (
            "verify-failure MUST roll back to the pre-change value -- never leave the source "
            "half-set"
        )

    def test_rollback_failure_still_raises_with_warning(self, monkeypatch, capsys):
        fake = FakeObs(
            latencies={"NDI cam5": 450},
            readback_override_source="NDI cam5",
            readback_override_value=3,
        )
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            phase_sync_calibrate.apply_latency(None, "NDI cam5", 450, 33)

        captured = capsys.readouterr()
        combined = (captured.out + captured.err).lower()
        assert "warn" in combined or "manual check" in combined, (
            f"rollback failure must print a LOUD warning; got stdout={captured.out!r} "
            f"stderr={captured.err!r}"
        )


# ---------------------------------------------------------------------------
# #707 -- the FINAL applied genlock_latency_ms_src must never go below PHASE_SYNC_FLOOR_MS,
# enforced at the point of application (apply_latency), not just inside the offset kernel --
# so a future caller (or av_sync_calibrate.py writing the SAME property afterwards, see that
# script's own mirrored test) can never silently undercut it.
# ---------------------------------------------------------------------------

class TestApplyLatencyEnforcesJitterFloor:
    def test_below_floor_target_is_clamped_up_before_writing(self, monkeypatch):
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        actual = phase_sync_calibrate.apply_latency(None, "NDI cam5", 450, 1)
        assert actual == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS

        sets = fake.set_calls()
        latency_sets = [
            p for _, p in sets
            if p.get("inputName") == "NDI cam5"
            and phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY in p.get("inputSettings", {})
        ]
        assert len(latency_sets) == 1, f"expected exactly one apply (no rollback), got {sets}"
        assert latency_sets[0]["inputSettings"][
            phase_sync_calibrate.GENLOCK_SRC_LATENCY_KEY
        ] == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS

    def test_a_value_already_above_the_floor_is_unaffected(self, monkeypatch):
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        actual = phase_sync_calibrate.apply_latency(None, "NDI cam5", 450, 200)
        assert actual == 200


# ---------------------------------------------------------------------------
# (f) write_last_json shape
# ---------------------------------------------------------------------------

class TestWriteLastJson:
    def test_writes_expected_shape(self, tmp_path):
        json_path = tmp_path / "camera-box" / "phase-sync-last.json"
        cameras = [
            {"source": "NDI cam5", "latency_ms": 100.0, "offset_ms": 3, "applied_latency_ms": 3},
            {"source": "NDI cam1", "latency_ms": 90.0, "offset_ms": 13, "applied_latency_ms": 13},
        ]
        phase_sync_calibrate.write_last_json(json_path, cameras)

        assert json_path.exists()
        data = json.loads(json_path.read_text())
        assert data["cameras"] == cameras
        assert "ts" in data and isinstance(data["ts"], (int, float))

    def test_creates_parent_dirs(self, tmp_path):
        json_path = tmp_path / "does" / "not" / "exist" / "phase-sync-last.json"
        phase_sync_calibrate.write_last_json(json_path, [])
        assert json_path.exists()


# ---------------------------------------------------------------------------
# (g) default_last_json_path
# ---------------------------------------------------------------------------

class TestDefaultLastJsonPath:
    def test_resolves_under_programdata_when_set(self, monkeypatch):
        monkeypatch.setenv("PROGRAMDATA", r"C:\ProgramData")
        p = phase_sync_calibrate.default_last_json_path()
        assert p.parts[-2:] == ("camera-box", "phase-sync-last.json")
        assert "ProgramData" in str(p)

    def test_falls_back_when_programdata_unset(self, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        p = phase_sync_calibrate.default_last_json_path()
        assert p.name == "phase-sync-last.json"
        assert p.parent.name in ("camera-box", ".camera-box")


# ---------------------------------------------------------------------------
# (i) #636 -- remote push plan: the SAME persist-location gap #465 fixed in
# av_sync_calibrate.py. This script also connects to --host over the OBS WebSocket and does
# not need to run ON the stream box, so default_last_json_path() falls back to a LOCAL path
# nothing on the stream box can read. scp/ssh to Windows was historically believed denied; #701
# proved plain scp/ssh actually reaches strih/stream, but for a short in-memory JSON blob like
# this one, the established channel this script still uses is the win-* MCP FileWrite tool,
# driven by the operator/agent.
# remote_push_plan() prints an explicit, copy-pasteable plan -- same convention as
# av_sync_calibrate.remote_push_plan() / obs-self-heal-install.sh's PLAN block.
# ---------------------------------------------------------------------------

class TestMcpNameForHost:
    def test_stream_host_resolves_to_win_stream_snv(self):
        assert phase_sync_calibrate.mcp_name_for_host("10.77.9.204") == "win-stream-snv"

    def test_strih_host_resolves_to_win_strih(self):
        assert phase_sync_calibrate.mcp_name_for_host("10.77.9.202") == "win-strih"

    def test_unknown_host_returns_none(self):
        assert phase_sync_calibrate.mcp_name_for_host("10.0.0.99") is None


class TestRemotePushPlan:
    def test_plan_names_the_canonical_windows_destination(self):
        payload = {
            "cameras": [{"source": "NDI cam5", "latency_ms": 100.0, "offset_ms": 3,
                         "applied_latency_ms": 3}],
            "ts": 1720000000.0,
        }
        plan = phase_sync_calibrate.remote_push_plan("10.77.9.202", payload)
        assert r"C:\ProgramData\camera-box\phase-sync-last.json" in plan
        assert "win-strih" in plan
        assert "10.77.9.202" in plan
        assert "FileWrite" in plan

    def test_plan_includes_the_exact_json_content(self):
        payload = {
            "cameras": [{"source": "NDI cam1", "latency_ms": 90.0, "offset_ms": 13,
                         "applied_latency_ms": 13}],
            "ts": 1720000000.0,
        }
        plan = phase_sync_calibrate.remote_push_plan("10.77.9.202", payload)
        embedded = json.loads(plan.split("content:\n", 1)[1])
        assert embedded == payload

    def test_plan_for_unknown_host_still_names_the_destination(self):
        payload = {"cameras": [], "ts": 1.0}
        plan = phase_sync_calibrate.remote_push_plan("10.0.0.99", payload)
        assert r"C:\ProgramData\camera-box\phase-sync-last.json" in plan
        assert "10.0.0.99" in plan


# ---------------------------------------------------------------------------
# (h) CLI wiring
# ---------------------------------------------------------------------------

class TestCLI:
    @pytest.fixture(autouse=True)
    def _default_active_set(self, monkeypatch):
        # #893: main() now restricts --measured-json to CAMERA_ACTIVE_SET. Default it to cover
        # every camera name this file's fixtures use (cam1/cam3/cam4/cam5) so pre-#893 scenarios
        # keep exercising exactly what they always tested, unaffected by the new filter. A test
        # that specifically wants to exercise the filter overrides this with its own
        # monkeypatch.setenv() call.
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam3 cam4 cam5")

    def test_dry_run_never_calls_set_input_settings(self, monkeypatch, tmp_path):
        fake = FakeObs(latencies={"NDI cam5": 450, "NDI cam1": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        # #438: the offset math is delegated to the compiled phase-sync-gate binary, not
        # reimplemented here -- mock the I/O boundary with the SAME values the Rust kernel's
        # own unit tests prove for this input (src/phase_sync.rs's
        # slowest_camera_maps_to_the_floor).
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3, "NDI cam1": 13},
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 100.0, "NDI cam1": 90.0}))
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path)],
        )
        phase_sync_calibrate.main()
        assert fake.set_calls() == [], "dry-run (no --apply) must never call SetInputSettings"

    def test_apply_flag_drives_full_multi_camera_flow_and_persists(self, monkeypatch, tmp_path):
        fake = FakeObs(latencies={"NDI cam5": 450, "NDI cam1": 450, "NDI cam3": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        # slowest = cam5 (100ms) -> floor(3). cam1 (90ms) -> 3+10=13. cam3 (80ms) -> 3+20=23
        # (same vectors as src/phase_sync.rs's faster_camera_gets_floor_plus_the_deficit).
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3, "NDI cam1": 13, "NDI cam3": 23},
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps(
            {"NDI cam5": 100.0, "NDI cam1": 90.0, "NDI cam3": 80.0}
        ))
        json_path = tmp_path / "phase-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply",
             "--json-path", str(json_path)],
        )
        phase_sync_calibrate.main()

        assert fake.latencies["NDI cam5"] == 3
        assert fake.latencies["NDI cam1"] == 13
        assert fake.latencies["NDI cam3"] == 23

        assert json_path.exists()
        data = json.loads(json_path.read_text())
        by_source = {c["source"]: c for c in data["cameras"]}
        assert by_source["NDI cam5"]["applied_latency_ms"] == 3
        assert by_source["NDI cam1"]["applied_latency_ms"] == 13
        assert by_source["NDI cam3"]["applied_latency_ms"] == 23
        assert by_source["NDI cam5"]["latency_ms"] == 100.0

    def test_explicit_json_path_suppresses_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # An explicit --json-path means the caller is taking control of the destination
        # themselves -- no auto plan needed (mirrors av_sync_calibrate's same test).
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3},
        )
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 100.0}))
        json_path = tmp_path / "phase-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply",
             "--json-path", str(json_path)],
        )
        phase_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" not in out

    def test_default_path_off_box_prints_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # #636 finding: run from dev1 (PROGRAMDATA unset) with the DEFAULT path -- the write
        # lands under ~/.camera-box, which nothing on the stream box can read. main() must
        # surface an explicit push plan so the operator/agent pushes it via MCP FileWrite --
        # the SAME gap #465 fixed in av_sync_calibrate.py.
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3},
        )
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        monkeypatch.setattr(
            phase_sync_calibrate, "default_last_json_path",
            lambda: tmp_path / ".camera-box" / "phase-sync-last.json",
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 100.0}))
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply"],
        )
        phase_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" in out
        assert r"C:\ProgramData\camera-box\phase-sync-last.json" in out
        assert "win-strih" in out

    def test_printed_plan_json_matches_what_was_actually_persisted(
        self, monkeypatch, tmp_path, capsys,
    ):
        # Integration-level drift guard (mirrors av_sync_calibrate's own such test): the
        # plan's embedded JSON must be byte-identical to what write_last_json() actually wrote
        # -- never reconstructed separately (that would let the pushed content silently
        # diverge from the local record).
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3},
        )
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        local_json_path = tmp_path / ".camera-box" / "phase-sync-last.json"
        monkeypatch.setattr(
            phase_sync_calibrate, "default_last_json_path", lambda: local_json_path,
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 100.0}))
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply"],
        )
        phase_sync_calibrate.main()
        out = capsys.readouterr().out

        persisted = json.loads(local_json_path.read_text())
        printed = json.loads(out.split("content:\n", 1)[1])
        assert printed == persisted

    def test_default_path_on_box_does_not_print_remote_push_plan(self, monkeypatch, tmp_path, capsys):
        # If PROGRAMDATA IS set (running ON the Windows box), default_last_json_path() already
        # resolves to the canonical stream-box path -- no push needed.
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {"NDI cam5": 3},
        )
        monkeypatch.setenv("PROGRAMDATA", str(tmp_path / "ProgramData"))
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 100.0}))
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply"],
        )
        phase_sync_calibrate.main()
        out = capsys.readouterr().out
        assert "REMOTE PUSH REQUIRED" not in out


# ---------------------------------------------------------------------------
# (i) active_ndi_sources() / main()'s active-set filter -- #893
# ---------------------------------------------------------------------------

class TestActiveNdiSources:
    def test_default_is_cam1_cam2_cam3(self, monkeypatch):
        # issue 1198 (2026-08-27, owner ruling): cam1 + cam2 RESTORED -- both cards confirmed
        # healthy on a live journal check, owner refused the physical swap. issue 1216
        # (2026-08-28): bigger splitter fitted, cam5/cam6/cam7 back too; issue 1217 (same day):
        # cam5 dropped back out (DEAD_PORT splitter leg). issue 1216 completion (2026-08-30,
        # owner directive "kamery od 1-7 bezia" after a physical cable reseat): cam4 (#947) and
        # cam5 (DEAD_PORT) both rejoin -- default mirrors camera-set.sh's CAMERA_ACTIVE_SET =
        # "cam1 cam2 cam3 cam4 cam5 cam6 cam7", the full seven-camera fleet.
        monkeypatch.delenv("CAMERA_ACTIVE_SET", raising=False)
        assert phase_sync_calibrate.active_ndi_sources() == {
            "NDI cam1",
            "NDI cam2",
            "NDI cam3",
            "NDI cam4",
            "NDI cam5",
            "NDI cam6",
            "NDI cam7",
        }

    def test_env_override_narrows_and_widens(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam5")
        assert phase_sync_calibrate.active_ndi_sources() == {"NDI cam1", "NDI cam5"}


class TestCLIActiveSetFilter:
    def test_a_non_active_source_is_ignored_never_applied(self, monkeypatch, tmp_path):
        # The exact live #893 shape: --measured-json carries a RETIRED camera's source
        # alongside real active ones. It must be dropped before computing offsets AND never
        # get a SetInputSettings call -- never silently corrupt the "slowest" determination or
        # get a pin written.
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam3")
        fake = FakeObs(latencies={"NDI cam1": 450, "NDI cam3": 450, "NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)

        seen_measured = {}

        def fake_run_gate_bin(measured, gate_bin=None):
            seen_measured.update(measured)
            return {s: 3 for s in measured}

        monkeypatch.setattr(phase_sync_calibrate, "_run_gate_bin", fake_run_gate_bin)
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps(
            {"NDI cam1": 90.0, "NDI cam3": 80.0, "NDI cam5": 999.0}
        ))
        json_path = tmp_path / "phase-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply", "--json-path", str(json_path)],
        )
        phase_sync_calibrate.main()

        assert "NDI cam5" not in seen_measured, (
            "a source outside CAMERA_ACTIVE_SET must never reach compute_phase_sync_offsets"
        )
        assert set(seen_measured) == {"NDI cam1", "NDI cam3"}
        assert not any(p.get("inputName") == "NDI cam5" for _, p in fake.set_calls()), (
            "a non-active source must never receive a SetInputSettings call"
        )

    def test_non_active_source_warns_on_stderr(self, monkeypatch, tmp_path, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1")
        fake = FakeObs(latencies={"NDI cam1": 450, "NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {s: 3 for s in measured},
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam1": 90.0, "NDI cam5": 999.0}))
        json_path = tmp_path / "phase-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply", "--json-path", str(json_path)],
        )
        phase_sync_calibrate.main()
        err = capsys.readouterr().err
        assert "NDI cam5" in err, f"dropping a non-active source must be WARNED, got stderr={err!r}"

    def test_no_active_source_present_warns_and_applies_nothing(self, monkeypatch, tmp_path, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1")
        fake = FakeObs(latencies={"NDI cam5": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
        monkeypatch.setattr(
            phase_sync_calibrate, "_run_gate_bin",
            lambda measured, gate_bin=None: {s: 3 for s in measured},
        )
        measured_path = tmp_path / "measured.json"
        measured_path.write_text(json.dumps({"NDI cam5": 999.0}))
        json_path = tmp_path / "phase-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["phase_sync_calibrate.py", "--host", "10.77.9.202",
             "--measured-json", str(measured_path), "--apply", "--json-path", str(json_path)],
        )
        phase_sync_calibrate.main()
        assert fake.set_calls() == [], "no active camera present -- nothing must be applied"
        err = capsys.readouterr().err
        assert "no ACTIVE camera" in err.lower() or "cam5" in err
