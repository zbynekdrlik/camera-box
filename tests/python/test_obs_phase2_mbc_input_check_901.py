"""#901 original item 2 — unit tests for the obs_phase2 `mbc-input-check` subcommand.

"The Dante transport is actually up, not merely 'the app is running'... A muted/rerouted/renamed
input is exactly as fatal as a dead card and is a one-call read" (issue 901). This reads the mbc
input's `device_id` setting + mute state over the EXISTING OBS-WS RPC path (no event
subscription) and hard-fails loud on an unambiguously wrong state, BEFORE ever attempting a
measurement-audio probe recording.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_mic", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_mic"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


# --- pure helper -----------------------------------------------------------------------------

DANTE_DEVICE = "Dante Virtual Soundcard (x64)"


def test_mbc_transport_problem_none_when_healthy():
    assert obs_phase2._mbc_transport_problem(DANTE_DEVICE, False, DANTE_DEVICE) is None


def test_mbc_transport_problem_flags_muted_regardless_of_device():
    problem = obs_phase2._mbc_transport_problem(DANTE_DEVICE, True, DANTE_DEVICE)
    assert problem is not None
    assert "mute" in problem.lower()


def test_mbc_transport_problem_flags_empty_device_id():
    problem = obs_phase2._mbc_transport_problem("", False, DANTE_DEVICE)
    assert problem is not None
    assert "device_id" in problem.lower()


def test_mbc_transport_problem_flags_wrong_device():
    problem = obs_phase2._mbc_transport_problem("Some Other Device", False, DANTE_DEVICE)
    assert problem is not None
    assert "Some Other Device" in problem
    assert DANTE_DEVICE in problem


def test_mbc_transport_problem_mute_takes_priority_over_device_mismatch():
    # A muted input with ALSO a wrong device -- report the mute first, single clear message.
    problem = obs_phase2._mbc_transport_problem("wrong-device", True, DANTE_DEVICE)
    assert "mute" in problem.lower()


# --- CLI wiring --------------------------------------------------------------------------------

def test_mbc_input_check_function_exists():
    assert callable(obs_phase2.mbc_input_check)


def test_mbc_input_check_subcommand_parses_and_dispatches(monkeypatch):
    captured = {}

    def fake(a):
        captured["host"] = a.host
        captured["input"] = a.input
        captured["expected_device_id"] = a.expected_device_id

    monkeypatch.setattr(obs_phase2, "mbc_input_check", fake)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "mbc-input-check", "--host", "10.77.9.204", "--input", "mbc"],
    )
    obs_phase2.main()
    assert captured["host"] == "10.77.9.204"
    assert captured["input"] == "mbc"
    assert captured["expected_device_id"] == DANTE_DEVICE


def test_mbc_input_check_input_defaults_to_mbc(monkeypatch):
    captured = {}

    def fake(a):
        captured["input"] = a.input

    monkeypatch.setattr(obs_phase2, "mbc_input_check", fake)
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "mbc-input-check", "--host", "h"])
    obs_phase2.main()
    assert captured["input"] == "mbc"


# --- handler, live RPC path mocked --------------------------------------------------------------

class _FakeWS:
    def close(self):
        pass


def _fake_conn_rpc(device_id, muted):
    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        if rtype == "GetInputSettings":
            return {"inputSettings": {"device_id": device_id}}
        if rtype == "GetInputMute":
            return {"inputMuted": muted}
        raise AssertionError(f"unexpected rpc {rtype}")

    return fake_conn, fake_rpc


def test_mbc_input_check_passes_on_a_healthy_transport(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_conn_rpc(DANTE_DEVICE, False)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(
        host="10.77.9.204", password="", input="mbc", expected_device_id=DANTE_DEVICE
    )
    obs_phase2.mbc_input_check(a)
    out = capsys.readouterr().out
    assert "PASS" in out


def test_mbc_input_check_fails_loud_when_muted(monkeypatch):
    fake_conn, fake_rpc = _fake_conn_rpc(DANTE_DEVICE, True)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(
        host="10.77.9.204", password="", input="mbc", expected_device_id=DANTE_DEVICE
    )
    with pytest.raises(SystemExit, match="mute"):
        obs_phase2.mbc_input_check(a)


def test_mbc_input_check_fails_loud_on_wrong_device(monkeypatch):
    fake_conn, fake_rpc = _fake_conn_rpc("wrong-device", False)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    a = argparse.Namespace(
        host="10.77.9.204", password="", input="mbc", expected_device_id=DANTE_DEVICE
    )
    with pytest.raises(SystemExit):
        obs_phase2.mbc_input_check(a)
