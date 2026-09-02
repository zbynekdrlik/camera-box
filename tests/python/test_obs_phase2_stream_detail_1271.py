"""issue 1271 -- unit tests for the NEW obs_phase2.py `stream-detail` action + its pure
`redact_stream_server` helper. `stream-detail` is a READ-ONLY refusal-detail read used by the
reordered `[0/8]` stray-session check (scripts/lib/stray-session-check.sh): when a stray/production
stream is found on strih/stream, print WHAT is streaming -- the ingest SERVER url (with any stream
KEY defensively redacted, NEVER printed even partially) + GetStreamStatus.outputDuration -- so a
LIVE production broadcast is obvious in the log.

It is a SEPARATE, additive action -- it deliberately does NOT touch the existing `stream_status`,
whose exact `active=<bool> path=` output is pinned by the EVENT-contract tests
(test_obs_phase2_event_assert_actions.py) and consumed by rig-mode.sh / event_assert.py /
avsync-lineup-alert-watchdog.sh.

Same mocking pattern as tests/python/test_obs_phase2_event_assert_actions.py: patch `_rpc`/`_conn`
to avoid a live OBS connection, capture every call, and assert on the DECISION, not the transport.
"""
import argparse
import importlib.util
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_stream_detail", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_stream_detail"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def _patch(monkeypatch, rpc_by_op):
    """rpc_by_op: {op_name: return_value | Exception}. Patches _rpc to answer per op (raising when
    the value is an Exception instance/class) and _conn to avoid a real websocket. Returns the
    captured (op, payload) calls list."""
    calls = []

    def fake_rpc(ws, op, payload=None, ignore_err=False):
        calls.append((op, payload or {}))
        v = rpc_by_op.get(op, {})
        if isinstance(v, BaseException) or (isinstance(v, type) and issubclass(v, BaseException)):
            raise v if isinstance(v, BaseException) else v("boom")
        return v

    class FakeWS:
        def close(self):
            pass

    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password="": FakeWS())
    return calls


def _args(**kw):
    return argparse.Namespace(**kw)


# ---------------------------------------------------------------------------
# redact_stream_server (pure)
# ---------------------------------------------------------------------------


def test_redact_strips_key_embedded_in_server_url():
    assert (
        obs_phase2.redact_stream_server("rtmp://127.0.0.1:1234/live/SUPERSECRETKEY", "SUPERSECRETKEY")
        == "rtmp://127.0.0.1:1234/live/<redacted-key>"
    )


def test_redact_leaves_a_key_free_server_untouched():
    # OBS's rtmp_custom service keeps server + key as SEPARATE fields, so the common case is a
    # server with NO key in it -- it must pass through verbatim.
    assert (
        obs_phase2.redact_stream_server("rtmp://127.0.0.1:1234/live", "SUPERSECRETKEY")
        == "rtmp://127.0.0.1:1234/live"
    )


def test_redact_tolerates_empty_key_and_empty_server():
    assert obs_phase2.redact_stream_server("rtmp://h/live", "") == "rtmp://h/live"
    assert obs_phase2.redact_stream_server("", "K") == ""
    assert obs_phase2.redact_stream_server(None, None) == ""


# ---------------------------------------------------------------------------
# stream_detail (read-only)
# ---------------------------------------------------------------------------


def test_stream_detail_prints_server_and_duration_never_the_key(monkeypatch, capsys):
    _patch(
        monkeypatch,
        {
            "GetStreamStatus": {"outputActive": True, "outputDuration": 754123, "outputTimecode": "00:12:34.123"},
            "GetStreamServiceSettings": {
                "streamServiceSettings": {"server": "rtmp://127.0.0.1:1234/live", "key": "SUPERSECRETKEY"}
            },
        },
    )
    obs_phase2.stream_detail(_args(host="10.77.9.204", password=""))
    out = capsys.readouterr().out
    assert "server=rtmp://127.0.0.1:1234/live" in out
    assert "duration_ms=754123" in out
    assert "SUPERSECRETKEY" not in out  # the stream key must NEVER appear, even partially


def test_stream_detail_redacts_a_key_embedded_in_the_server_field(monkeypatch, capsys):
    _patch(
        monkeypatch,
        {
            "GetStreamStatus": {"outputActive": True, "outputDuration": 1000},
            "GetStreamServiceSettings": {
                "streamServiceSettings": {"server": "rtmp://h/live/EMBEDDEDKEY", "key": "EMBEDDEDKEY"}
            },
        },
    )
    obs_phase2.stream_detail(_args(host="10.77.9.204", password=""))
    out = capsys.readouterr().out
    assert "EMBEDDEDKEY" not in out
    assert "<redacted-key>" in out


def test_stream_detail_is_read_only_never_starts_or_stops(monkeypatch):
    calls = _patch(
        monkeypatch,
        {
            "GetStreamStatus": {"outputActive": True, "outputDuration": 5},
            "GetStreamServiceSettings": {"streamServiceSettings": {"server": "rtmp://h/live", "key": ""}},
        },
    )
    obs_phase2.stream_detail(_args(host="10.77.9.204", password=""))
    ops = [c[0] for c in calls]
    assert ops == ["GetStreamStatus", "GetStreamServiceSettings"]
    assert not any(op.startswith("Start") or op.startswith("Stop") for op in ops)


def test_stream_detail_tolerates_unreadable_service_settings(monkeypatch, capsys):
    # A failing GetStreamServiceSettings must not crash the refusal-detail read -- still print the
    # duration (the refusal fires regardless; the detail is best-effort).
    _patch(
        monkeypatch,
        {
            "GetStreamStatus": {"outputActive": True, "outputDuration": 42},
            "GetStreamServiceSettings": RuntimeError,
        },
    )
    obs_phase2.stream_detail(_args(host="10.77.9.204", password=""))
    out = capsys.readouterr().out
    assert "duration_ms=42" in out
    assert "server=" in out
