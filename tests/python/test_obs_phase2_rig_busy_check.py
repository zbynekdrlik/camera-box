"""#406/#312 item5: unit tests for the obs_phase2 `rig-busy-check` subcommand.

This is the pre-flight signal the automatic `pull_request`-triggered full-path-e2e CI gate
(scripts/rig-busy-gate.sh) uses before it reroutes strih/stream's production OBS program scenes
to run the real E2E — driving the recording harness over a LIVE broadcast would be a genuine
production incident. These tests pin the DECISION logic (busy true/false/unreachable) against a
fake WebSocket, with no live OBS involved.
"""
import argparse
import importlib.util
import json
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_rbc", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_rbc"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


class _FakeWS:
    def close(self):
        pass


def _args(strih_host="10.77.9.202", stream_host="10.77.9.204", password=""):
    return argparse.Namespace(strih_host=strih_host, stream_host=stream_host, password=password)


def test_rig_busy_check_function_exists():
    assert callable(obs_phase2.rig_busy_check)


def test_rig_busy_check_subcommand_parses_and_dispatches(monkeypatch):
    captured = {}

    def fake_rig_busy_check(a):
        captured["strih_host"] = a.strih_host
        captured["stream_host"] = a.stream_host
        captured["password"] = a.password

    monkeypatch.setattr(obs_phase2, "rig_busy_check", fake_rig_busy_check)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "rig-busy-check",
         "--strih-host", "10.77.9.202", "--stream-host", "10.77.9.204", "--password", "secret"],
    )
    obs_phase2.main()
    assert captured == {
        "strih_host": "10.77.9.202", "stream_host": "10.77.9.204", "password": "secret",
    }


def test_rig_busy_check_subcommand_defaults_host_and_password(monkeypatch):
    # Omitting the flags must still parse — defaults come from STRIH_HOST/STREAM_HOST/OBS_PASSWORD
    # env vars (or the hardcoded rig IPs), never a required-argument failure.
    captured = {}

    def fake_rig_busy_check(a):
        captured["strih_host"] = a.strih_host
        captured["stream_host"] = a.stream_host
        captured["password"] = a.password

    monkeypatch.setattr(obs_phase2, "rig_busy_check", fake_rig_busy_check)
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "rig-busy-check"])
    obs_phase2.main()
    assert captured["strih_host"] == "10.77.9.202"
    assert captured["stream_host"] == "10.77.9.204"
    assert captured["password"] == ""


def _fake_rpc_factory(strih_stream_active, strih_record_active, stream_stream_active,
                       stream_record_active):
    """Build a fake `_rpc` that answers GetStreamStatus/GetRecordStatus per-host, keyed by which
    host each `_conn` call was made for (captured via a mutable "current host" cell)."""
    current = {"host": None}

    def fake_conn(host, password=""):
        current["host"] = host
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        is_strih = current["host"] == "10.77.9.202"
        if rtype == "GetStreamStatus":
            active = strih_stream_active if is_strih else stream_stream_active
        elif rtype == "GetRecordStatus":
            active = strih_record_active if is_strih else stream_record_active
        else:
            raise AssertionError(f"unexpected request type {rtype!r}")
        return {"outputActive": active}

    return fake_conn, fake_rpc


def test_rig_busy_check_both_idle_reports_not_busy(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory(False, False, False, False)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())  # must NOT raise/exit

    out = json.loads(capsys.readouterr().out)
    assert out == {"busy": False, "reasons": []}


def test_rig_busy_check_strih_streaming_reports_busy_with_reason(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory(True, False, False, False)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert len(out["reasons"]) == 1
    assert "strih" in out["reasons"][0]
    assert "streaming" in out["reasons"][0]


def test_rig_busy_check_stream_recording_reports_busy_with_reason(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory(False, False, False, True)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert len(out["reasons"]) == 1
    assert "stream" in out["reasons"][0]
    assert "recording" in out["reasons"][0]


def test_rig_busy_check_both_boxes_fully_busy_reports_all_four_reasons(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory(True, True, True, True)
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert len(out["reasons"]) == 4


def test_rig_busy_check_strih_unreachable_fails_closed_exit_3(monkeypatch, capsys):
    def fake_conn(host, password=""):
        if host == "10.77.9.202":
            raise ConnectionRefusedError("no route to host")
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        return {"outputActive": False}

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.rig_busy_check(_args())

    assert exc_info.value.code == 3
    out = json.loads(capsys.readouterr().out)
    # Never silently reported as busy=false when a box could not be observed.
    assert out["busy"] is None
    assert any("10.77.9.202" in r for r in out["reasons"])


def test_rig_busy_check_stream_unreachable_fails_closed_exit_3(monkeypatch, capsys):
    def fake_conn(host, password=""):
        if host == "10.77.9.204":
            raise TimeoutError("connect timed out")
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        return {"outputActive": False}

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.rig_busy_check(_args())

    assert exc_info.value.code == 3
    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is None
    assert any("10.77.9.204" in r for r in out["reasons"])


def test_rig_busy_check_both_unreachable_reports_both_and_exits_3(monkeypatch, capsys):
    def fake_conn(host, password=""):
        raise OSError(f"unreachable: {host}")

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", lambda *a, **k: {"outputActive": False})

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.rig_busy_check(_args())

    assert exc_info.value.code == 3
    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is None
    assert len(out["reasons"]) == 2


def test_rig_busy_check_rpc_error_on_one_host_fails_closed_not_busy_false(monkeypatch, capsys):
    # An RPC-level error (e.g. OBS returns a request-status failure) on strih must ALSO fail
    # closed — never silently treated as "strih is idle".
    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        raise RuntimeError("GetStreamStatus failed: {'result': False}")

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    with pytest.raises(SystemExit) as exc_info:
        obs_phase2.rig_busy_check(_args())

    assert exc_info.value.code == 3
    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is None
