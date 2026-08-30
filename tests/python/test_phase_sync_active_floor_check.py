"""#893 -- unit tests for scripts/phase_sync_active_floor_check.py, the live preflight that
reads strih's CURRENTLY-CONFIGURED genlock_latency_ms_src for every CAMERA_ACTIVE_SET camera
and shells the result to the compiled phase-sync-active-floor-gate Rust binary.

Covers, with NO live OBS/network and NO real compiled binary:
  a. active_camera_names() -- derives from CAMERA_ACTIVE_SET, never a literal range.
  b. read_active_pins() -- honest {} on nothing readable, partial dict on partial reads, never
     a fabricated/half-filled table that looks complete.
  c. main() CLI wiring -- constructs the correct {active_set, pins} JSON payload, relays the
     gate binary's stdout/stderr/exit code verbatim; a connect failure or an empty active set
     fails closed (exit 2), never a silent pass.
"""
import json
import pathlib
import subprocess
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import phase_sync_active_floor_check as psafc  # noqa: E402


class FakeWS:
    def __init__(self):
        self.closed = False

    def close(self):
        self.closed = True


# ---------------------------------------------------------------------------
# active_camera_names
# ---------------------------------------------------------------------------

class TestActiveCameraNames:
    def test_default_is_cam1_cam2_cam3(self, monkeypatch):
        # issue 1198 (2026-08-27, owner ruling): cam1 + cam2 RESTORED -- both cards confirmed
        # healthy on a live journal check, owner refused the physical swap. issue 1216
        # (2026-08-28): bigger splitter fitted, cam5/cam6/cam7 back too; issue 1217 (same day):
        # cam5 dropped back out (DEAD_PORT splitter leg). issue 1216 completion (2026-08-30,
        # owner directive "kamery od 1-7 bezia" after a physical cable reseat): cam4 (#947) and
        # cam5 (DEAD_PORT) both rejoin -- default mirrors camera-set.sh's CAMERA_ACTIVE_SET =
        # "cam1 cam2 cam3 cam4 cam5 cam6 cam7", the full seven-camera fleet.
        monkeypatch.delenv("CAMERA_ACTIVE_SET", raising=False)
        assert psafc.active_camera_names() == [
            "cam1",
            "cam2",
            "cam3",
            "cam4",
            "cam5",
            "cam6",
            "cam7",
        ]

    def test_env_override(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam5")
        assert psafc.active_camera_names() == ["cam1", "cam5"]

    def test_empty_override_is_an_empty_list(self, monkeypatch):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "   ")
        assert psafc.active_camera_names() == []

    def test_explicit_arg_wins_over_env(self, monkeypatch):
        # #893: recording-e2e.sh passes --active-set "$CAMERA_ACTIVE_SET" explicitly on the
        # command line (mirrors set-ndi-mapping.py's --active) rather than relying on the
        # shell variable happening to be exported to this Python subprocess.
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam2 cam3 cam4")
        assert psafc.active_camera_names("cam5 cam6") == ["cam5", "cam6"]


# ---------------------------------------------------------------------------
# read_active_pins
# ---------------------------------------------------------------------------

class TestReadActivePins:
    def test_reads_ndi_cam_source_per_active_camera(self, monkeypatch):
        ws = FakeWS()
        monkeypatch.setattr(psafc, "_conn", lambda host, password: ws)

        def fake_read_pin(w, name):
            assert w is ws
            return {"NDI cam1": 21, "NDI cam3": 3}.get(name)

        monkeypatch.setattr(psafc, "read_pin", fake_read_pin)
        pins = psafc.read_active_pins("10.77.9.202", "", ["cam1", "cam2", "cam3"])
        assert pins == {"cam1": 21, "cam3": 3}
        assert ws.closed is True

    def test_closes_the_websocket_even_if_a_read_raises(self, monkeypatch):
        ws = FakeWS()
        monkeypatch.setattr(psafc, "_conn", lambda host, password: ws)

        def raising_read_pin(w, name):
            raise RuntimeError("boom")

        monkeypatch.setattr(psafc, "read_pin", raising_read_pin)
        with pytest.raises(RuntimeError, match="boom"):
            psafc.read_active_pins("10.77.9.202", "", ["cam1"])
        assert ws.closed is True


# ---------------------------------------------------------------------------
# main() -- CLI wiring
# ---------------------------------------------------------------------------

class FakeCompleted:
    def __init__(self, returncode, stdout=b"", stderr=b""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class TestMain:
    def test_builds_correct_payload_and_relays_the_gate_verdict(self, monkeypatch, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1 cam2")
        monkeypatch.setattr(psafc, "_conn", lambda host, password: FakeWS())
        monkeypatch.setattr(
            psafc, "read_pin",
            lambda ws, name: {"NDI cam1": 21, "NDI cam2": 3}.get(name),
        )

        seen = {}

        def fake_run(cmd, input=None, capture_output=None):  # noqa: A002
            seen["cmd"] = cmd
            seen["payload"] = json.loads(input.decode())
            return FakeCompleted(0, stdout=b"PASS cam2\n")

        monkeypatch.setattr(subprocess, "run", fake_run)
        monkeypatch.setattr(psafc, "_find_gate_bin", lambda explicit: "/fake/gate-bin")

        rc = psafc.main(["--host", "10.77.9.202"])
        assert rc == 0
        assert seen["payload"] == {"active_set": ["cam1", "cam2"], "pins": {"cam1": 21, "cam2": 3}}
        assert "PASS cam2" in capsys.readouterr().out

    def test_relays_a_fail_verdict_exit_code_and_message(self, monkeypatch, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1")
        monkeypatch.setattr(psafc, "_conn", lambda host, password: FakeWS())
        monkeypatch.setattr(psafc, "read_pin", lambda ws, name: 21)
        monkeypatch.setattr(
            subprocess, "run",
            lambda cmd, input=None, capture_output=None: FakeCompleted(  # noqa: A002
                1, stdout=b"FAIL: no active camera at the floor\n"
            ),
        )
        monkeypatch.setattr(psafc, "_find_gate_bin", lambda explicit: "/fake/gate-bin")

        rc = psafc.main(["--host", "10.77.9.202"])
        assert rc == 1
        assert "FAIL" in capsys.readouterr().out

    def test_connect_failure_fails_closed_exit_2(self, monkeypatch, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "cam1")

        def raising_conn(host, password):
            raise ConnectionRefusedError("no route to host")

        monkeypatch.setattr(psafc, "_conn", raising_conn)
        rc = psafc.main(["--host", "10.77.9.202"])
        assert rc == 2
        assert "ERROR" in capsys.readouterr().err

    def test_empty_active_set_fails_closed_exit_2_never_a_silent_pass(self, monkeypatch, capsys):
        monkeypatch.setenv("CAMERA_ACTIVE_SET", "   ")
        rc = psafc.main(["--host", "10.77.9.202"])
        assert rc == 2
        assert "ERROR" in capsys.readouterr().err
