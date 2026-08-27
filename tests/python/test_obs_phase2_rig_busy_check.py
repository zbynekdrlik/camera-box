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
    #
    # The env vars are CLEARED first so this pins the hardcoded fallbacks rather than whatever the
    # runner happens to export. Supervisor boxes load OBS_PASSWORD from
    # ~/.config/environment.d/, so without this the assertion below failed there — and pytest
    # printed the real WebSocket password into the failure diff. CI has no such env, so the leak
    # only ever fired on a developer box, which is exactly where it is least likely to be noticed.
    for _var in ("STRIH_HOST", "STREAM_HOST", "OBS_PASSWORD"):
        monkeypatch.delenv(_var, raising=False)
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
    assert out["busy"] is False
    assert out["reasons"] == []
    # #649 item 3: diagnostics are ALWAYS present (busy or not) — one entry per box.
    assert out["diagnostics"] == [
        {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert "hint" not in out  # no hint when nothing is busy


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


def _fake_rpc_factory_with_timecode(strih_stream, strih_record, strih_tc,
                                     stream_stream, stream_record, stream_tc):
    """Same shape as _fake_rpc_factory, but GetRecordStatus also returns outputTimecode (#649
    item 3) — needed to test the timecode-in-reason and the record-only-vs-real-broadcast hint."""
    current = {"host": None}

    def fake_conn(host, password=""):
        current["host"] = host
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        is_strih = current["host"] == "10.77.9.202"
        if rtype == "GetStreamStatus":
            return {"outputActive": strih_stream if is_strih else stream_stream}
        elif rtype == "GetRecordStatus":
            active = strih_record if is_strih else stream_record
            tc = strih_tc if is_strih else stream_tc
            return {"outputActive": active, "outputTimecode": tc}
        raise AssertionError(f"unexpected request type {rtype!r}")

    return fake_conn, fake_rpc


def test_rig_busy_check_reason_includes_output_timecode_when_recording(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory_with_timecode(
        False, True, "00:12:34.567", False, False, "00:00:00.000")
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert any("00:12:34.567" in r for r in out["reasons"])
    strih_diag = next(d for d in out["diagnostics"] if d["host"] == "strih")
    assert strih_diag == {
        "host": "strih", "streaming": False, "recording": True,
        "recordTimecode": "00:12:34.567",
    }


def test_rig_busy_check_hint_flags_record_only_as_a_likely_stray_recording(monkeypatch, capsys):
    # #649 live incident shape: BOTH boxes recording, NEITHER streaming.
    fake_conn, fake_rpc = _fake_rpc_factory_with_timecode(
        False, True, "03:22:11.000", False, True, "00:41:56.000")
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert "hint" in out
    assert "strih" in out["hint"] and "stream" in out["hint"]
    assert "#649" in out["hint"]
    assert "REAL" not in out["hint"]  # must NOT claim this looks like a real broadcast


def test_rig_busy_check_hint_flags_record_and_stream_as_a_real_broadcast(monkeypatch, capsys):
    # A box streaming AND recording together must be called out as a likely REAL broadcast, and
    # the hint must say not to stop it automatically.
    fake_conn, fake_rpc = _fake_rpc_factory_with_timecode(
        True, True, "00:05:00.000", False, False, "00:00:00.000")
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert "hint" in out
    assert "REAL" in out["hint"]
    assert "do NOT stop" in out["hint"]


def test_rig_busy_check_strih_unreachable_fails_closed_exit_3(monkeypatch, capsys):
    # #651: keep this fail-closed test fast — no real retry backoff sleep.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
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
    # #651: keep this fail-closed test fast — no real retry backoff sleep.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
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
    # #651: keep this fail-closed test fast — no real retry backoff sleep.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
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


def test_rig_busy_check_retries_on_transient_connection_refused_then_succeeds(monkeypatch, capsys):
    # #651 live incident (2026-07-10, PR #647 run 29065733523): a SINGLE connection-refused
    # blip during a legitimate mid-broadcast OBS restart on stream must NOT immediately fail
    # the whole rig-busy-gate wait closed — a retry within the SAME check must recover it.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
    attempts = {"stream": 0}

    def fake_conn(host, password=""):
        if host == "10.77.9.204":
            attempts["stream"] += 1
            if attempts["stream"] == 1:
                raise ConnectionRefusedError("[Errno 111] Connection refused")
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        return {"outputActive": False}

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())  # must NOT raise/exit — the retry recovers it

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is False
    assert attempts["stream"] == 2, "expected exactly one retry after the first connection-refused"


def test_rig_busy_check_exhausts_retries_then_still_fails_closed(monkeypatch, capsys):
    # A GENUINE persistent outage (every attempt fails) must still fail closed exactly as
    # before — the retry only absorbs a transient blip, never a real outage.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
    attempts = {"strih": 0}

    def fake_conn(host, password=""):
        if host == "10.77.9.202":
            attempts["strih"] += 1
            raise ConnectionRefusedError("[Errno 111] Connection refused")
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
    assert any("10.77.9.202" in r for r in out["reasons"])
    # 1 initial attempt + RIG_BUSY_QUERY_RETRIES retries, every one exhausted before giving up.
    assert attempts["strih"] == 1 + obs_phase2.RIG_BUSY_QUERY_RETRIES


def test_rig_busy_check_retry_sleeps_the_configured_backoff_between_attempts(monkeypatch, capsys):
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 7)
    sleeps = []
    monkeypatch.setattr(obs_phase2.time, "sleep", lambda s: sleeps.append(s))
    attempts = {"strih": 0}

    def fake_conn(host, password=""):
        if host == "10.77.9.202":
            attempts["strih"] += 1
            if attempts["strih"] == 1:
                raise ConnectionRefusedError("no route to host")
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        return {"outputActive": False}

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    assert sleeps == [7], f"expected exactly one 7s backoff sleep between the 2 attempts, got {sleeps}"


def test_rig_busy_check_rpc_error_on_one_host_fails_closed_not_busy_false(monkeypatch, capsys):
    # #651: keep this fail-closed test fast — no real retry backoff sleep.
    monkeypatch.setattr(obs_phase2, "RIG_BUSY_QUERY_RETRY_SLEEP_S", 0)
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


# ── #649 item 3: _rig_busy_hint — pure, no I/O, tested directly (no fake WebSocket needed) ──────

def test_rig_busy_hint_empty_when_nothing_busy():
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert obs_phase2._rig_busy_hint(diagnostics) == ""


def test_rig_busy_hint_record_only_matches_stray_recording_pattern():
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": True, "recordTimecode": "03:07:49.000"},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    hint = obs_phase2._rig_busy_hint(diagnostics)
    assert "strih" in hint
    assert "#649" in hint
    assert "stray" in hint
    assert "REAL" not in hint


def test_rig_busy_hint_record_and_stream_matches_real_broadcast_pattern():
    diagnostics = [
        {"host": "strih", "streaming": True, "recording": True, "recordTimecode": "00:05:00.000"},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    hint = obs_phase2._rig_busy_hint(diagnostics)
    assert "strih" in hint
    assert "REAL" in hint
    assert "do NOT stop" in hint


def test_rig_busy_hint_streaming_only_flags_for_manual_check():
    diagnostics = [
        {"host": "strih", "streaming": True, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    hint = obs_phase2._rig_busy_hint(diagnostics)
    assert "strih" in hint
    assert "check manually" in hint


def test_rig_busy_hint_combines_multiple_boxes_in_one_hint():
    # strih looks like a stray recording, stream looks like a real broadcast — both parts must
    # appear (the hint must never collapse to only the first match).
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": True, "recordTimecode": "01:00:00.000"},
        {"host": "stream", "streaming": True, "recording": True, "recordTimecode": "00:10:00.000"},
    ]
    hint = obs_phase2._rig_busy_hint(diagnostics)
    assert "strih" in hint and "stray" in hint
    assert "stream" in hint and "REAL" in hint


# ── #657: _stray_recording_hosts — pure, no I/O — the self-heal DECISION list ────────────────────

def test_stray_recording_hosts_empty_when_nothing_busy():
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == []


def test_stray_recording_hosts_includes_recording_only_box():
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": True, "recordTimecode": "03:07:49.000"},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == ["strih"]


def test_stray_recording_hosts_never_includes_a_real_broadcast_box():
    # #657 safety invariant: a box that is BOTH recording and streaming must NEVER appear in the
    # self-heal list, no matter what — that's a real broadcast, never touch it automatically.
    diagnostics = [
        {"host": "strih", "streaming": True, "recording": True, "recordTimecode": "00:05:00.000"},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == []


def test_stray_recording_hosts_never_includes_streaming_only_box():
    diagnostics = [
        {"host": "strih", "streaming": True, "recording": False, "recordTimecode": None},
        {"host": "stream", "streaming": False, "recording": False, "recordTimecode": None},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == []


def test_stray_recording_hosts_can_include_both_boxes_at_once():
    # The exact #657 live-incident shape: both boxes left recording (neither streaming).
    diagnostics = [
        {"host": "strih", "streaming": False, "recording": True, "recordTimecode": "03:22:11.000"},
        {"host": "stream", "streaming": False, "recording": True, "recordTimecode": "00:41:56.000"},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == ["strih", "stream"]


def test_stray_recording_hosts_mixed_picks_only_the_stray_one():
    # strih is a real broadcast (never touch); stream is our own stray leftover.
    diagnostics = [
        {"host": "strih", "streaming": True, "recording": True, "recordTimecode": "00:05:00.000"},
        {"host": "stream", "streaming": False, "recording": True, "recordTimecode": "00:41:56.000"},
    ]
    assert obs_phase2._stray_recording_hosts(diagnostics) == ["stream"]


def test_rig_busy_check_json_includes_stray_hosts_when_present(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory_with_timecode(
        False, True, "03:22:11.000", False, False, "00:00:00.000")
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert out["stray_hosts"] == ["strih"]


def test_rig_busy_check_json_omits_stray_hosts_when_it_is_a_real_broadcast(monkeypatch, capsys):
    fake_conn, fake_rpc = _fake_rpc_factory_with_timecode(
        True, True, "00:05:00.000", False, False, "00:00:00.000")
    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)

    obs_phase2.rig_busy_check(_args())

    out = json.loads(capsys.readouterr().out)
    assert out["busy"] is True
    assert "stray_hosts" not in out
