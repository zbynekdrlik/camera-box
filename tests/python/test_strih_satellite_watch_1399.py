"""Issue 1399 -- the Companion Satellite liveness watch (scripts/strih_satellite_watch.py).

WHY (owner, 4.10.2026: "streamdeck tam nejako crashol teraz na strih"): the strih Stream Deck XL is
driven by Companion Satellite. A Satellite that runs but no longer works keeps the deck dead: the unit's
Restart=always only catches a process that exits.

What this pins (design comment 5979157737, Approach 1):
  * the pure `decide()` as tables: connected / not connected with the Companion port answering / the
    port down or unknown / surfaces empty with and without the Stream Deck on USB / the 60 s window;
    nothing ever restarts while Companion itself is down;
  * `observe()` against a real local HTTP server (the Satellite REST shapes of v3.4.0), a real TCP
    listener (the Companion port) and a fake sysfs USB tree;
  * `run_pass()` across several runs sharing one state file (one run = one timer firing): a fault is
    logged once when it starts, the restart fires once at 60 s and is logged, a quiet pass logs nothing;
  * `--check-state` (verify-strih's pass row) and `main()` end to end with a fake systemctl;
  * the oneshot service + the 30 s timer.

Tier-0: pytest only (no rig, no network beyond 127.0.0.1, no systemd).
"""
import http.server
import importlib.util
import json
import os
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
WATCH = REPO / "scripts" / "strih_satellite_watch.py"
SERVICE = REPO / "systemd" / "strih-satellite-watch.service"
TIMER = REPO / "systemd" / "strih-satellite-watch.timer"
LIB = REPO / "scripts" / "lib" / "strih-session-apps.sh"
PROVISION = REPO / "scripts" / "lib" / "strih-provision.sh"


def _load():
    spec = importlib.util.spec_from_file_location("strih_satellite_watch_1399", WATCH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


W = _load()
NC, NS = W.FAULT_NOT_CONNECTED, W.FAULT_NO_SURFACES


def obs(connected=True, surfaces=("Elgato Stream Deck XL",), companion_up=True, deck=True, rest_ok=True,
        companion="10.77.9.205:16622"):
    return W.Observation(rest_ok=rest_ok, rest_error=None if rest_ok else "connection refused",
                         connected=connected if rest_ok else None,
                         surfaces=tuple(surfaces) if surfaces is not None else None,
                         companion=companion, companion_up=companion_up, companion_error=None,
                         deck_on_usb=deck)


# --- decide(): the tables --------------------------------------------------------------------------

@pytest.mark.parametrize("o,want", [
    (obs(), ()),
    (obs(connected=False), (NC,)),
    (obs(connected=False, companion_up=False), ()),          # Companion down: never a fault
    (obs(connected=False, companion_up=None), ()),           # its port unknown: never a fault
    (obs(surfaces=()), (NS,)),
    (obs(surfaces=(), deck=False), ()),                      # the deck is not plugged in
    (obs(surfaces=(), deck=None), ()),                       # USB unreadable: no information
    (obs(surfaces=None), ()),                                # /api/surfaces unreadable
    (obs(surfaces=(), companion_up=False), ()),              # never while Companion is down
    (obs(connected=False, surfaces=()), (NC, NS)),
    (obs(rest_ok=False), ()),                                # REST silent: no information
    (obs(connected=None), ()),
])
def test_faults_table(o, want):
    assert W.faults(o) == want


@pytest.mark.parametrize("since,o,now,want_since,want_restart", [
    # healthy: nothing
    ({}, obs(), 1000.0, {}, ()),
    # a fault starts now
    ({}, obs(connected=False), 1000.0, {NC: 1000.0}, ()),
    # still under 60 s: keep the start, no restart
    ({NC: 941.0}, obs(connected=False), 1000.0, {NC: 941.0}, ()),
    # held 60 s: restart, the window starts over
    ({NC: 940.0}, obs(connected=False), 1000.0, {}, (NC,)),
    ({NC: 100.0}, obs(connected=False), 1000.0, {}, (NC,)),
    # the Companion port went down: the fault (and its window) is gone, no restart
    ({NC: 100.0}, obs(connected=False, companion_up=False), 1000.0, {}, ()),
    ({NC: 100.0}, obs(connected=False, companion_up=None), 1000.0, {}, ()),
    # the REST went silent: no information, the window starts over
    ({NC: 100.0}, obs(rest_ok=False), 1000.0, {}, ()),
    # surfaces empty with the deck on USB, 60 s
    ({NS: 940.0}, obs(surfaces=()), 1000.0, {}, (NS,)),
    # surfaces empty, the deck unplugged: nothing to fix
    ({NS: 100.0}, obs(surfaces=(), deck=False), 1000.0, {}, ()),
    # surfaces empty with the deck, but Companion is down: never a restart
    ({NS: 100.0}, obs(surfaces=(), companion_up=False), 1000.0, {}, ()),
    # the fault cleared
    ({NC: 990.0}, obs(), 1000.0, {}, ()),
    # two faults, one due: one restart naming the due one, both windows start over
    ({NC: 900.0}, obs(connected=False, surfaces=()), 1000.0, {}, (NC,)),
    ({NC: 900.0, NS: 930.0}, obs(connected=False, surfaces=()), 1000.0, {}, (NC, NS)),
    ({NC: 990.0, NS: 930.0}, obs(connected=False, surfaces=()), 1000.0, {}, (NS,)),
    # a start in the future (a bad state file) or not a number counts from now
    ({NC: 5000.0}, obs(connected=False), 1000.0, {NC: 1000.0}, ()),
    ({NC: "x"}, obs(connected=False), 1000.0, {NC: 1000.0}, ()),
    ({NC: True}, obs(connected=False), 1000.0, {NC: 1000.0}, ()),
    (None, obs(connected=False), 1000.0, {NC: 1000.0}, ()),
])
def test_decide_table(since, o, now, want_since, want_restart):
    d = W.decide(since, o, now)
    assert (d.since, d.restart) == (want_since, want_restart)


def test_decide_sustain_is_60_s_by_default_and_a_parameter():
    assert W.SUSTAIN_S == 60.0
    assert W.decide({NC: 970.0}, obs(connected=False), 1000.0, sustain=30.0).restart == (NC,)
    assert W.decide({NC: 970.0}, obs(connected=False), 1000.0).restart == ()


@pytest.mark.parametrize("unhealed,want", [(0, 60.0), (1, 60.0), (2, 60.0), (3, 120.0), (4, 240.0), (5, 480.0),
                                            (6, 900.0), (40, 900.0), (-1, 60.0), ("x", 60.0)])
def test_effective_sustain_backs_off_after_three_restarts_that_cured_nothing(unhealed, want):
    assert W.effective_sustain(60.0, unhealed) == want


def test_decide_is_pure():
    since = {NC: 900.0}
    W.decide(since, obs(connected=False), 1000.0)
    assert since == {NC: 900.0}


# --- observe(): the REST, the Companion port and USB -------------------------------------------------

class _Rest(http.server.BaseHTTPRequestHandler):
    routes = {}

    def do_GET(self):  # noqa: N802
        code, body = self.routes.get(self.path, (404, "Not Found"))
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


@pytest.fixture
def rest():
    class H(_Rest):
        routes = {}
    srv = http.server.HTTPServer(("127.0.0.1", 0), H)
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    yield H.routes, "http://127.0.0.1:%d" % srv.server_address[1]
    srv.shutdown()
    srv.server_close()


@pytest.fixture
def companion():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    s.listen(8)
    yield s.getsockname()[1]
    s.close()


def _closed_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def usb_tree(tmp_path, *ids):
    root = tmp_path / "usb"
    root.mkdir(exist_ok=True)
    for i, vp in enumerate(ids):
        vid, pid = vp.split(":")
        d = root / ("1-%d" % (i + 1))
        d.mkdir()
        (d / "idVendor").write_text(vid + "\n")
        (d / "idProduct").write_text(pid + "\n")
    (root / "usb1").mkdir()  # a root hub with no ids readable here: skipped
    return root


def _routes(routes, port, connected=True, surfaces=None, protocol="tcp"):
    routes["/api/status"] = (200, {"connected": connected, "companionVersion": "5.0.6",
                                   "companionApiVersion": "1.8.0", "companionUnsupportedApi": False})
    routes["/api/surfaces"] = (200, [{"surfaceId": "streamdeck:A00SA4232OZTF5", "productName": "Elgato Stream Deck XL",
                                      "pluginId": "elgato-stream-deck"}] if surfaces is None else surfaces)
    routes["/api/config"] = (200, {"protocol": protocol, "host": "127.0.0.1", "port": port, "httpEnabled": True,
                                   "httpPort": 9999, "mdnsEnabled": False})


def test_observe_a_healthy_satellite(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, companion)
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f", "8087:0026"), ("0fd9:008f",))
    assert o.rest_ok and o.connected is True
    assert o.surfaces == ("Elgato Stream Deck XL",)
    assert o.companion == "127.0.0.1:%d" % companion and o.companion_up is True
    assert o.deck_on_usb is True
    assert W.faults(o) == ()


def test_observe_reads_the_two_fault_shapes(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, companion, connected=False, surfaces=[])
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert o.connected is False and o.surfaces == ()
    assert W.faults(o) == (NC, NS)


def test_observe_a_closed_companion_port_is_down(tmp_path, rest):
    routes, url = rest
    _routes(routes, _closed_port(), connected=False)
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert o.companion_up is False and o.companion_error
    assert W.faults(o) == ()


def test_observe_a_non_tcp_protocol_leaves_the_companion_unknown(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, companion, connected=False, protocol="ws")
    o = W.observe(url, usb_tree(tmp_path), ("0fd9:008f",))
    assert o.companion is None and o.companion_up is None and "ws" in o.companion_error
    assert W.faults(o) == ()


def test_observe_a_port_given_as_text_is_read(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, str(companion), connected=False)  # not connected: the port is really probed
    assert W.observe(url, usb_tree(tmp_path), ("0fd9:008f",)).companion_up is True


def test_observe_never_probes_companion_while_the_satellite_is_connected(tmp_path, rest, monkeypatch):
    # a live session already proves the port answers; a bare TCP connect every 30 s would be a client
    # session on the venue Companion each pass
    routes, url = rest
    _routes(routes, _closed_port(), connected=True)
    probes = []
    monkeypatch.setattr(W, "tcp_probe", lambda *a, **k: probes.append(a) or (False, "probed"))
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert probes == [] and o.companion_up is True and o.companion_error is None
    _routes(routes, _closed_port(), connected=False)
    W.observe(url, tmp_path / "usb", ("0fd9:008f",))
    assert len(probes) == 1, "a fault candidate is probed"


def test_tcp_probe_never_raises_on_a_bad_host():
    ok, err = W.tcp_probe("a..b", 16622, timeout=1.0)  # the IDNA codec rejects it with a UnicodeError
    assert ok is False and err


@pytest.mark.parametrize("status", [(500, "Internal Server Error"), (200, "not json"), (200, [1, 2])])
def test_observe_an_unusable_status_is_a_silent_rest(tmp_path, rest, companion, status):
    routes, url = rest
    _routes(routes, companion)
    routes["/api/status"] = status
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert o.rest_ok is False and o.rest_error and o.connected is None
    assert W.faults(o) == ()


def test_observe_a_closed_rest_port_is_a_silent_rest(tmp_path):
    o = W.observe("http://127.0.0.1:%d" % _closed_port(), usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert o.rest_ok is False and o.surfaces is None and o.companion is None


def test_observe_unreadable_surfaces_are_no_information(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, companion)
    routes["/api/surfaces"] = (200, {"not": "a list"})
    o = W.observe(url, usb_tree(tmp_path, "0fd9:008f"), ("0fd9:008f",))
    assert o.surfaces is None and W.faults(o) == ()


def test_usb_only_the_stream_deck_ids_count(tmp_path):
    # 0fd9:0066 is an Elgato Cam Link: the same vendor, not a Stream Deck -- it must never read as the deck
    assert W.deck_on_usb(usb_tree(tmp_path, "0fd9:0066", "046d:c52b"), ("0fd9:008f",)) is False
    assert W.deck_on_usb(tmp_path / "usb", ("0fd9:008f", "0fd9:0066")) is True
    assert W.deck_on_usb(tmp_path / "missing", ("0fd9:008f",)) is None


def test_usb_ids_are_case_insensitive(tmp_path):
    assert W.deck_on_usb(usb_tree(tmp_path, "0FD9:008F"), ("0fd9:008f",)) is True


def test_default_usb_id_is_the_strih_deck_and_the_rest_the_satellite_port():
    assert W.STREAM_DECK_USB_IDS == ("0fd9:008f",)
    assert W.REST_URL == "http://127.0.0.1:9999"
    assert W.USB_ROOT == "/sys/bus/usb/devices"


# --- run_pass(): several timer firings over one state file -------------------------------------------

class Rig:
    """One state file + the injected observation, boot clock, wall clock and restart, as successive
    timer firings see them."""

    def __init__(self, tmp_path, restart_rc=0):
        self.state = tmp_path / "run" / "strih-satellite-watch.json"
        self.obs = obs()
        self.boot = 1000.0
        self.logs = []
        self.restarts = []
        self.restart_rc = restart_rc

    def restart(self, unit):
        self.restarts.append(unit)
        return (self.restart_rc, "" if self.restart_rc == 0 else "Failed to connect to bus")

    def fire(self, at=None, o=None):
        if at is not None:
            self.boot = at
        if o is not None:
            self.obs = o
        n = len(self.logs)
        st = W.run_pass(str(self.state), lambda: self.obs, self.restart, log=self.logs.append,
                        wall=lambda: 1.79e9 + self.boot, boot=lambda: self.boot)
        return st, self.logs[n:]


def test_run_pass_restarts_once_after_60_s_and_logs_it(tmp_path):
    rig = Rig(tmp_path)
    _st, lines = rig.fire(1000.0)
    assert len(lines) == 1 and "watching companion-satellite.service" in lines[0]
    _st, lines = rig.fire(1030.0)
    assert lines == [], "a quiet healthy pass logs nothing"
    _st, lines = rig.fire(1060.0, obs(connected=False))
    assert len(lines) == 1 and "FAULT" in lines[0] and "not connected to Companion 10.77.9.205:16622" in lines[0]
    assert "restarted if this holds for 60 s" in lines[0]
    _st, lines = rig.fire(1090.0)
    assert lines == [] and rig.restarts == [], "the fault is logged once, not every 30 s"
    st, lines = rig.fire(1120.0)
    assert rig.restarts == ["companion-satellite.service"]
    assert len(lines) == 1
    assert lines[0].startswith("restarting companion-satellite.service: not connected to Companion "
                               "10.77.9.205:16622 while its port answers, for 60 s")
    assert "(restart #1 since boot)" in lines[0]
    assert st["restarts"] == 1 and st["since"] == {} and st["last_restart"]["faults"] == [NC]
    # the next firing still sees the fault: a new window, logged, the restart waits another 60 s
    _st, lines = rig.fire(1150.0)
    assert len(lines) == 1 and "FAULT" in lines[0] and rig.restarts == ["companion-satellite.service"]
    rig.fire(1180.0)
    assert len(rig.restarts) == 1
    _st, lines = rig.fire(1210.0)
    assert len(rig.restarts) == 2 and "(restart #2 since boot)" in lines[0]
    # healthy again: one line
    _st, lines = rig.fire(1240.0, obs())
    assert len(lines) == 1 and "healthy again" in lines[0]


def test_run_pass_backs_off_when_restarts_cure_nothing_and_resets_on_health(tmp_path):
    rig = Rig(tmp_path)
    rig.fire(1000.0, obs(connected=False))
    times = []
    t = 1000.0
    while len(rig.restarts) < 6 and t < 5000.0:
        t += 30.0
        n = len(rig.restarts)
        _st, lines = rig.fire(t)
        if len(rig.restarts) > n:
            times.append(t)
    gaps = [b - a for a, b in zip([1000.0] + times, times)]
    # a restart, then the window restarts from the next firing: 60 s for three, then 120, 240, 480 s
    assert gaps[:3] == [60.0, 90.0, 90.0]
    assert gaps[3] == 150.0 and gaps[4] == 270.0 and gaps[5] == 510.0
    assert any("backing off" in line for line in rig.logs)
    assert sum("backing off" in line for line in rig.logs) == 1, "the back-off is logged once"
    st = json.loads(rig.state.read_text())
    assert st["unhealed_restarts"] == 6 and st["effective_sustain_s"] == 900.0
    rig.fire(t + 30.0, obs())
    st = json.loads(rig.state.read_text())
    assert st["unhealed_restarts"] == 0 and st["effective_sustain_s"] == 60.0


def test_run_pass_reads_the_clock_before_the_slow_observation(tmp_path):
    order = []
    rig = Rig(tmp_path)

    def boot():
        order.append("boot")
        return 1000.0

    def observe():
        order.append("observe")
        return obs()
    W.run_pass(str(rig.state), observe, rig.restart, log=rig.logs.append, wall=lambda: 1.79e9, boot=boot)
    assert order[:2] == ["boot", "observe"]


def test_run_pass_a_gap_between_passes_starts_the_windows_over(tmp_path):
    # a stopped timer or a suspended notebook: one faulty pass before the gap and one after are not 60 s of fault
    rig = Rig(tmp_path)
    rig.fire(1000.0, obs(connected=False))
    st, lines = rig.fire(1600.0)
    assert rig.restarts == [] and st["since"] == {NC: 1600.0}
    assert any("no watch pass for 600 s" in line for line in lines)
    assert W.MAX_PASS_GAP_S == 75.0


def test_run_pass_never_restarts_while_companion_is_down(tmp_path):
    rig = Rig(tmp_path)
    _st, lines = rig.fire(1000.0, obs(connected=False, companion_up=False))
    assert len(lines) == 1 and "Companion 10.77.9.205:16622 not answering" in lines[0]
    assert "never restarts" in lines[0]
    for t in range(1030, 1400, 30):
        _st, lines = rig.fire(float(t))
        assert lines == []
    assert rig.restarts == []


def test_run_pass_a_silent_rest_is_logged_once_and_never_restarts(tmp_path):
    rig = Rig(tmp_path)
    _st, lines = rig.fire(1000.0, obs(rest_ok=False))
    assert len(lines) == 1 and "REST http://127.0.0.1:9999 not answering" in lines[0]
    rig.fire(1030.0)
    rig.fire(1200.0)
    assert rig.restarts == [] and len(rig.logs) == 1


def test_run_pass_no_surfaces_with_the_deck_on_usb(tmp_path):
    rig = Rig(tmp_path)
    _st, lines = rig.fire(1000.0, obs(surfaces=()))
    assert "no surface open while the Stream Deck is on USB" in lines[0]
    rig.fire(1030.0)
    _st, lines = rig.fire(1060.0)
    assert rig.restarts == ["companion-satellite.service"]
    assert "no surface open while the Stream Deck is on USB, for 60 s" in lines[0]


def test_run_pass_a_failed_restart_is_logged_and_not_counted(tmp_path):
    rig = Rig(tmp_path, restart_rc=1)
    rig.fire(1000.0, obs(connected=False))
    st, lines = rig.fire(1060.0)
    assert rig.restarts == ["companion-satellite.service"]
    assert lines[0].startswith("could NOT restart companion-satellite.service")
    assert "Failed to connect to bus" in lines[0]
    assert st["restarts"] == 0 and st["last_restart"]["ok"] is False and st["last_error"]


def test_run_pass_an_unreadable_state_file_starts_fresh(tmp_path):
    rig = Rig(tmp_path)
    rig.state.parent.mkdir(parents=True)
    rig.state.write_text("{not json")
    st, lines = rig.fire(1000.0, obs(connected=False))
    assert any("unreadable" in line for line in lines)
    assert st["since"] == {NC: 1000.0}
    assert json.loads(rig.state.read_text())["since"] == {NC: 1000.0}


def test_run_pass_writes_the_state_atomically_and_completely(tmp_path):
    rig = Rig(tmp_path)
    rig.fire(1000.0)
    st = json.loads(rig.state.read_text())
    for key in ("version", "updated_epoch_s", "boot_s", "since", "condition", "observation", "restarts",
                "last_restart", "unit", "sustain_s", "last_error"):
        assert key in st, key
    assert st["observation"]["surfaces"] == ["Elgato Stream Deck XL"]
    assert [p.name for p in rig.state.parent.iterdir()] == ["strih-satellite-watch.json"]


# --- --check-state (verify-strih) ------------------------------------------------------------------------

def _state(tmp_path, age, **extra):
    p = tmp_path / "w.json"
    st = {"version": 1, "updated_epoch_s": time.time() - age, "boot_s": 1000.0, "since": {}, "condition": "ok",
          "restarts": 2, "last_restart": None, "unit": "companion-satellite.service", "sustain_s": 60.0,
          "last_error": None, "observation": W.observation_dict(obs())}
    st.update(extra)
    p.write_text(json.dumps(st))
    return p


def test_check_state_a_recent_pass_is_ok_and_named(tmp_path):
    rc, line = W.check_state_file(str(_state(tmp_path, 5)))
    assert rc == 0
    assert line.startswith("last pass 5 s ago: Satellite connected to Companion 10.77.9.205:16622")
    assert "1 surface(s) (Elgato Stream Deck XL)" in line and "2 restart(s)" in line


def test_check_state_names_a_fault_in_progress(tmp_path):
    p = _state(tmp_path, 5, since={NC: 970.0}, condition="fault:" + NC,
               observation=W.observation_dict(obs(connected=False)))
    rc, line = W.check_state_file(str(p))
    assert rc == 0 and "FAULT in progress: not connected" in line and "for 30 s" in line


def test_check_state_stale_and_unreadable_fail(tmp_path):
    rc, line = W.check_state_file(str(_state(tmp_path, 600)))
    assert rc == 1 and line.startswith("stale: last pass 600 s ago") and "strih-satellite-watch.timer" in line
    rc, line = W.check_state_file(str(tmp_path / "missing.json"))
    assert rc == 1 and "unreadable" in line
    (tmp_path / "bad.json").write_text("[]")
    assert W.check_state_file(str(tmp_path / "bad.json"))[0] == 1


def test_check_state_limit_covers_three_timer_periods():
    assert W.STATE_MAX_AGE_S == 90.0


def test_check_state_fails_a_watch_that_could_not_restart_while_the_fault_holds(tmp_path):
    lr = {"epoch_s": time.time() - 20, "faults": [NC], "ok": False, "error": "Failed to connect to bus"}
    p = _state(tmp_path, 5, condition="fault:" + NC, since={NC: 990.0}, last_restart=lr,
               observation=W.observation_dict(obs(connected=False)))
    rc, line = W.check_state_file(str(p))
    assert rc == 1 and "could NOT restart companion-satellite.service" in line and "Failed to connect to bus" in line
    # healthy again: an old failed restart is history, not a failure
    p = _state(tmp_path, 5, last_restart=lr)
    assert W.check_state_file(str(p))[0] == 0


def test_check_state_fails_a_backed_off_watch_while_the_fault_holds(tmp_path):
    p = _state(tmp_path, 5, condition="fault:" + NC, since={NC: 990.0}, unhealed_restarts=4,
               effective_sustain_s=240.0, observation=W.observation_dict(obs(connected=False)))
    rc, line = W.check_state_file(str(p))
    assert rc == 1 and "restarted companion-satellite.service 4 times with no healthy pass" in line
    assert "240 s" in line
    p = _state(tmp_path, 5, unhealed_restarts=0)
    assert W.check_state_file(str(p))[0] == 0


def test_a_pass_fits_its_oneshot_timeout():
    worst = 3 * W.HTTP_TIMEOUT_S + W.TCP_TIMEOUT_S + W.SYSTEMCTL_TIMEOUT_S
    assert worst + 2 <= int(_unit(SERVICE)["TimeoutStartSec"]), worst


# --- main(): the CLI end to end ------------------------------------------------------------------------

def _fake_systemctl(tmp_path):
    log = tmp_path / "systemctl.log"
    p = tmp_path / "systemctl"
    p.write_text("#!%s\nimport sys\nopen(%r, 'a').write(' '.join(sys.argv[1:]) + '\\n')\n" % (sys.executable, str(log)))
    p.chmod(0o755)
    return p, log


def test_main_restarts_through_systemctl_without_blocking(tmp_path, rest, companion):
    routes, url = rest
    _routes(routes, companion, connected=False)
    sc, log = _fake_systemctl(tmp_path)
    usb = usb_tree(tmp_path, "0fd9:008f")
    state = tmp_path / "w.json"
    args = ["--rest-url", url, "--usb-root", str(usb), "--state-file", str(state), "--systemctl", str(sc),
            "--sustain", "0"]
    r = subprocess.run([sys.executable, str(WATCH)] + args, capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert log.read_text() == "--user --no-block try-restart companion-satellite.service\n"
    assert "restarting companion-satellite.service" in r.stdout
    rc = subprocess.run([sys.executable, str(WATCH), "--check-state", str(state)], capture_output=True, text=True)
    assert rc.returncode == 0 and rc.stdout.startswith("last pass")


def test_main_refuses_a_malformed_usb_id(tmp_path):
    r = subprocess.run([sys.executable, str(WATCH), "--usb-id", "0fd9-008f", "--state-file", str(tmp_path / "s")],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 2 and "0fd9-008f" in r.stderr
    assert not (tmp_path / "s").exists()


def test_watch_is_std_only_and_executable():
    src = WATCH.read_text()
    assert "import websocket" not in src and "import requests" not in src
    assert os.access(WATCH, os.X_OK), "commit it executable (git mode 100755)"


# --- the unit pair ---------------------------------------------------------------------------------------

def _unit(path):
    return {line.split("=", 1)[0]: line.split("=", 1)[1]
            for line in path.read_text().splitlines() if "=" in line and not line.startswith("#")}


def _lib(var):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "%s"; printf "%%s" "%s"' % (LIB, var)],
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_watch_service_is_one_bounded_quiet_pass():
    u = _unit(SERVICE)
    assert u["Type"] == "oneshot"
    assert u["ExecStart"] == ("/usr/bin/python3 /usr/local/bin/strih_satellite_watch.py --state-file %t/"
                              + _lib("$STRIH_SATELLITE_WATCH_STATE_FILE"))
    assert int(u["TimeoutStartSec"]) <= 25
    # every 30 s the user manager logs Starting/Finished at info: only the watch's own lines (and any failure) stay
    assert u["LogLevelMax"] == "notice" and u["SyslogLevel"] == "notice"
    assert "[Install]" not in SERVICE.read_text(), "the timer starts it; the service itself is never enabled"


def test_watch_timer_fires_every_30_s_soon_after_its_start():
    u = _unit(TIMER)
    assert u["OnUnitActiveSec"] == "30s"
    assert u["OnActiveSec"] == "5s", "OnUnitActiveSec alone never fires before the service ran once"
    assert u["AccuracySec"] == "1s", "a user timer's default accuracy is 1 min"
    assert u["Unit"] == "strih-satellite-watch.service"
    assert u["WantedBy"] == "graphical-session.target"


def test_watch_defaults_match_the_lib_and_the_provisioning():
    assert W.SATELLITE_UNIT == _lib("$STRIH_COMPANION_SATELLITE_UNIT") == "companion-satellite.service"
    r = subprocess.run(["bash", "-c", 'set -euo pipefail; . "%s"; strih_companion_satellite_rest_url' % PROVISION],
                       capture_output=True, text=True, env={k: v for k, v in os.environ.items()
                                                            if k != "COMPANION_SATELLITE_REST_URL"})
    assert r.returncode == 0 and r.stdout == W.REST_URL
