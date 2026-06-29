"""#331 — unit test: obs_phase2 `_conn()` must subscribe to ZERO obs-websocket events.

## The bug (#331, observed in #312 runs 312006/312007)

`obs_phase2.py` is a pure request/response client — it issues op-6 requests and waits for the
matching op-7 response in `_rpc`; it consumes NO events. But `_conn()` sent its op-1 Identify with
NO `eventSubscriptions` field, so the obs-websocket session defaulted to `EventSubscription::All`.

When the STREAM box does `SetCurrentProgramScene` onto the COLD `NDI 2ME PGM` receiver (strih's
program output), DistroAV cold-reactivates that receiver and OBS floods op-5 events. `_rpc`'s read
loop then drains that flood forever and the matching op-7 never arrives within the wall-clock cap →
the #328 TimeoutError that hung prod-scene (and teardown) on stream and failed the whole #312 run.
strih's own warm cam switches never hang — only stream's cold 2ME-PGM re-subscribe is pathological.

## The fix this test locks

`_conn()` now sends `eventSubscriptions: 0` in the Identify, so the session receives ZERO events.
A client that gets no op-5 events can NEVER have `_rpc` drown in them — this is structurally immune
to the entire event-flood class, not just this one source. The #328 wall-clock cap stays as the
loud backstop.

This test pins the Identify payload using a fake WebSocket (no live OBS).
"""
import importlib.util
import json
import pathlib
import sys

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"


def _load(modname="obs_phase2_conn_es"):
    spec = importlib.util.spec_from_file_location(modname, _MOD_PATH)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[modname] = mod
    spec.loader.exec_module(mod)
    return mod


obs_phase2 = _load()


class _FakeWS:
    """Minimal obs-websocket server stand-in: Hello (op 0, no auth) -> capture Identify -> Identified
    (op 2). Records every JSON the client sends so the test can assert the Identify payload."""

    def __init__(self):
        self.sent = []
        # recv() returns these in order: the Hello, then the post-Identify Identified ack.
        self._to_recv = [
            json.dumps({"op": 0, "d": {"rpcVersion": 1}}),
            json.dumps({"op": 2, "d": {"negotiatedRpcVersion": 1}}),
        ]

    def recv(self):
        return self._to_recv.pop(0)

    def send(self, msg):
        self.sent.append(json.loads(msg))


def test_conn_identify_subscribes_to_zero_events(monkeypatch):
    fake = _FakeWS()
    monkeypatch.setattr(obs_phase2, "create_connection", lambda *a, **k: fake)

    obs_phase2._conn("10.77.9.204")

    identify = next((m for m in fake.sent if m.get("op") == 1), None)
    assert identify is not None, "_conn must send an op-1 Identify"
    # The #331 fix: subscribe to ZERO events so a DistroAV reactivation flood can never drown _rpc.
    assert identify["d"].get("eventSubscriptions") == 0, (
        "_conn Identify must set eventSubscriptions=0 (else the session defaults to "
        "EventSubscription::All and a cold NDI reactivation floods op-5 events → #328 hang)"
    )
