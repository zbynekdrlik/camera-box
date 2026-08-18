"""#1086 — unit tests for the obs_phase2 `idle-receiver` keepalive-bypass PRIMITIVE.

The deliberate keepalive-bypass cold cut (scripts/lib/cold-cut-step.sh) idles ONE strih NDI
receiver so a natural sweep cold cut goes GENUINELY cold under the #767 keep-alive build, then
restores it before the cut. These tests pin the pure settings-builder + the argument-parsing /
dispatch wiring (NOT the live WebSocket SetInputSettings, which needs a real OBS) so a future edit
that drops the subcommand, its required `--input`, or the idle/restore settings shape fails loudly
in CI.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_idle_receiver", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_idle_receiver"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_idle_settings_clear_ndi_source_name_and_genlock_fifo():
    # Idle (no restore) tears the receiver down cold: clear ndi_source_name + genlock_fifo off,
    # exactly the idle discipline _quiesce_probe_input / teardown already use.
    assert obs_phase2._idle_restore_settings("") == {
        "ndi_source_name": "",
        "genlock_fifo": False,
    }
    assert obs_phase2._idle_restore_settings(None) == {
        "ndi_source_name": "",
        "genlock_fifo": False,
    }


def test_restore_settings_repoint_and_reenable_genlock():
    # Restore re-points the receiver + re-enables the genlock FIFO. overlay:True (in idle_receiver)
    # keeps the per-source genlock latency pin intact, so ONLY these two keys ever change.
    assert obs_phase2._idle_restore_settings("CAM1 (usb)") == {
        "ndi_source_name": "CAM1 (usb)",
        "genlock_fifo": True,
    }


def test_idle_receiver_function_exists():
    # The cold-cut step depends on this entrypoint existing.
    assert callable(obs_phase2.idle_receiver)


def test_idle_receiver_subcommand_parses_and_dispatches(monkeypatch):
    # `obs_phase2.py idle-receiver --host H --input "NDI cam1"` must parse + dispatch to
    # idle_receiver() with restore defaulting to "" (idle mode), never to another subcommand.
    captured = {}

    def fake_idle(a):
        captured["host"] = a.host
        captured["input"] = a.input
        captured["restore"] = a.restore

    monkeypatch.setattr(obs_phase2, "idle_receiver", fake_idle)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "idle-receiver", "--host", "10.77.9.202", "--input", "NDI cam1"],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "input": "NDI cam1", "restore": ""}


def test_idle_receiver_restore_flag_parses(monkeypatch):
    captured = {}

    def fake_idle(a):
        captured["restore"] = a.restore

    monkeypatch.setattr(obs_phase2, "idle_receiver", fake_idle)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "idle-receiver", "--host", "h", "--input", "NDI cam1",
         "--restore", "CAM1 (usb)"],
    )
    obs_phase2.main()
    assert captured == {"restore": "CAM1 (usb)"}


def test_idle_receiver_requires_input(monkeypatch):
    # The receiver to idle is mandatory — a wrong/absent input would idle the wrong receiver on a
    # live box, so this must fail loudly, never silently no-op.
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "idle-receiver", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()
