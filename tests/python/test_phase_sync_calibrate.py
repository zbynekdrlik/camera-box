"""#286 -- unit tests for scripts/phase_sync_calibrate.py, the 4-camera MUTUAL phase-sync
auto-set controller: measured per-camera cam->strih latencies -> per-camera genlock video-delay
over OBS WS + persisted phase-sync-last.json.

Covers, with NO live OBS:
  a. compute_phase_sync_offsets() -- MUST mirror
     camera_box::phase_sync::compute_phase_sync_offsets (src/phase_sync.rs) EXACTLY: slowest
     maps to the floor, faster cameras get floor+deficit, clamp rails, degenerate cases (1
     camera, all-equal, tie at the max), same as the Rust kernel's own unit tests.
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
# (a) compute_phase_sync_offsets -- MUST mirror src/phase_sync.rs exactly
# ---------------------------------------------------------------------------

class TestComputePhaseSyncOffsets:
    def test_slowest_camera_maps_to_the_floor(self):
        # Rust: compute_phase_sync_offsets(&[(1, 50.0), (2, 80.0)]) -> cam1=33, cam2=3(floor).
        out = phase_sync_calibrate.compute_phase_sync_offsets({"cam1": 50.0, "cam2": 80.0})
        assert out == {"cam1": 33, "cam2": 3}

    def test_faster_camera_gets_floor_plus_the_deficit(self):
        out = phase_sync_calibrate.compute_phase_sync_offsets(
            {"cam10": 100.0, "cam20": 90.0, "cam30": 80.0}
        )
        assert out == {"cam10": 3, "cam20": 13, "cam30": 23}

    def test_tie_at_the_max_maps_every_tied_camera_to_the_floor(self):
        out = phase_sync_calibrate.compute_phase_sync_offsets(
            {"cam1": 80.0, "cam2": 80.0, "cam3": 50.0}
        )
        assert out == {"cam1": 3, "cam2": 3, "cam3": 33}

    def test_clamps_low(self):
        # A camera far faster than the slowest would need an offset above the 2000ms cap.
        out = phase_sync_calibrate.compute_phase_sync_offsets({"fast": 5000.0, "slow": 0.0})
        assert out["slow"] == phase_sync_calibrate.PHASE_SYNC_CAP_MS
        assert out["fast"] == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS

    def test_single_camera_is_its_own_slowest_and_gets_the_floor(self):
        out = phase_sync_calibrate.compute_phase_sync_offsets({"only": 123.4})
        assert out == {"only": phase_sync_calibrate.PHASE_SYNC_FLOOR_MS}

    def test_all_equal_latencies_all_map_to_the_floor(self):
        out = phase_sync_calibrate.compute_phase_sync_offsets(
            {"a": 42.0, "b": 42.0, "c": 42.0}
        )
        assert all(v == phase_sync_calibrate.PHASE_SYNC_FLOOR_MS for v in out.values())

    def test_empty_input_yields_empty_output(self):
        assert phase_sync_calibrate.compute_phase_sync_offsets({}) == {}


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
        assert phase_sync_calibrate.read_current_latency(None, "NDI cam5") == 3


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
# (h) CLI wiring
# ---------------------------------------------------------------------------

class TestCLI:
    def test_dry_run_never_calls_set_input_settings(self, monkeypatch, tmp_path):
        fake = FakeObs(latencies={"NDI cam5": 450, "NDI cam1": 450})
        monkeypatch.setattr(phase_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(phase_sync_calibrate, "_conn", lambda host, password="": None)
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
        measured_path = tmp_path / "measured.json"
        # slowest = cam5 (100ms) -> floor(3). cam1 (90ms) -> 3+10=13. cam3 (80ms) -> 3+20=23.
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
