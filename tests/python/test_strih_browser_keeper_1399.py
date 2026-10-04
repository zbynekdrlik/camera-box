"""Issue 1399 -- strih-lx's browser-source keeper (scripts/strih_browser_keeper.py).

WHY: obs-browser loads a browser source's page ONCE at OBS start and never retries a failed load. On
4.10.2026 strih-lx booted before presenter.lan / fohabl.lan answered and four production scenes stayed
empty until a manual `refreshnocache` (owner: "tie browser sceny maju byt vzdy nacitane").

What this pins (design comment 5977447339, Approach 1):
  * the pure decision `decide` as a table: (state, probe, OBS run epoch) -> refresh / none. A source is
    refreshed once per OBS run as soon as its page server is first reachable, again on every
    unreachable -> reachable transition, never while the server is down, never periodically;
  * the URL -> probe target rule (a scheme-less `fohabl.lan` is http :80), the browser_source settings
    read, the verify-strih state verdict;
  * the bounded prober (a hung probe never stalls a pass, never piles up threads);
  * the REAL keeper loop against a real obs-websocket 5 server on localhost (a stdlib RFC 6455 server
    below) + fake probes: the exact refresh sequence across a page server outage, an OBS restart
    (dropped connection -> reconnect -> every source once more) and a start before OBS is up;
  * the `--check-state` CLI verify-strih runs.

Tier-0: pytest + the websocket-client package (already a python-tests CI dependency). No OBS, no rig.
"""
import importlib.util
import json
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
KEEPER_PATH = REPO / "scripts" / "strih_browser_keeper.py"


def _load():
    spec = importlib.util.spec_from_file_location("strih_browser_keeper_1399", KEEPER_PATH)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["strih_browser_keeper_1399"] = mod
    spec.loader.exec_module(mod)
    return mod


k = _load()

sys.path.insert(0, str(Path(__file__).resolve().parent))
from strih_keeper_fakes_1399 import FOHABL, INPUTS, PRESENTER, FakeObsWebSocket  # noqa: E402
S = k.SourceState


# --- the pure decision -------------------------------------------------------------------------

# (prev state or None, probe reachable, OBS run epoch) -> (new state, action)
DECISION_TABLE = [
    # never seen, server down: remember it is down, no refresh
    (None, False, 1, S(None, False), None),
    # never seen, server up: the once-per-connect refresh
    (None, True, 1, S(1, True), k.ACTION_OBS_RUN),
    # down at connect, now up: still the connect refresh (OBS loaded before the server answered)
    (S(None, False), True, 1, S(1, True), k.ACTION_OBS_RUN),
    # refreshed this epoch, still up: a working page is never refreshed again
    (S(1, True), True, 1, S(1, True), None),
    # refreshed this epoch, server went down: no refresh while down
    (S(1, True), False, 1, S(1, False), None),
    # refreshed this epoch, still down: nothing
    (S(1, False), False, 1, S(1, False), None),
    # refreshed this epoch, back up after being down: the recovered refresh
    (S(1, False), True, 1, S(1, True), k.ACTION_RECOVERED),
    # a new OBS run epoch (OBS restarted), server up: refresh once more
    (S(1, True), True, 2, S(2, True), k.ACTION_OBS_RUN),
    # a new OBS run epoch while the server is down: wait for it, no refresh
    (S(1, True), False, 2, S(1, False), None),
    # ... and then it comes up: the connect refresh (not "recovered")
    (S(1, False), True, 2, S(2, True), k.ACTION_OBS_RUN),
    # a state never probed (reachable None) but refreshed this epoch: up stays quiet
    (S(1, None), True, 1, S(1, True), None),
    # no verdict yet (no finished probe, or not enough failures): nothing changes, nothing pressed
    (None, None, 1, S(None, None), None),
    (S(1, True), None, 1, S(1, True), None),
    (S(1, False), None, 2, S(1, False), None),
]


# (failed probes in a row, last verdict, this probe) -> (failed probes in a row, verdict), DOWN_AFTER = 2
DEBOUNCE_TABLE = [
    (0, None, True, 0, True),       # a good probe is up at once
    (0, None, False, 1, None),      # one failed first probe: no verdict yet
    (1, None, False, 2, False),     # the second in a row: down
    (0, True, False, 1, True),      # one lost connect on a working server: still up
    (1, True, False, 2, False),     # the second in a row: down
    (1, True, True, 0, True),       # a good probe resets the count
    (2, False, True, 0, True),      # coming back is immediate
    (2, False, False, 3, False),
    (1, True, None, 1, True),       # an unfinished probe changes nothing
    (0, None, None, 0, None),
    (2, False, None, 2, False),
]


@pytest.mark.parametrize("fails,up,result,want_fails,want_up", DEBOUNCE_TABLE)
def test_debounce_table(fails, up, result, want_fails, want_up):
    assert k.debounce(fails, up, result) == (want_fails, want_up)
    assert k.DOWN_AFTER == 2


@pytest.mark.parametrize("prev,reachable,epoch,want_state,want_action", DECISION_TABLE)
def test_decide_table(prev, reachable, epoch, want_state, want_action):
    assert k.decide(prev, reachable, epoch) == (want_state, want_action)


def test_decide_never_refreshes_a_steady_working_page_over_many_passes():
    state, actions = None, []
    for _ in range(200):
        state, action = k.decide(state, True, 7)
        actions.append(action)
    assert actions[0] == k.ACTION_OBS_RUN
    assert actions[1:] == [None] * 199


def test_decide_never_refreshes_while_down():
    state = S(3, True)
    for _ in range(50):
        state, action = k.decide(state, False, 3)
        assert action is None
    assert state == S(3, False)


@pytest.mark.parametrize("prev,now,want", [
    (None, True, "first-up"), (None, False, "first-down"),
    (True, True, None), (False, False, None),
    (False, True, "up"), (True, False, "down"),
])
def test_transition_words(prev, now, want):
    assert k.transition(prev, now) == want


# --- URL -> probe target, the browser source settings read -----------------------------------------

@pytest.mark.parametrize("url,want", [
    ("http://presenter.lan/ui/camera", ("presenter.lan", 80)),
    ("http://presenter.lan/stream/moderator", ("presenter.lan", 80)),
    ("fohabl.lan", ("fohabl.lan", 80)),
    ("fohabl.lan/set?x=1", ("fohabl.lan", 80)),
    ("fohabl.lan/?next=http://x", ("fohabl.lan", 80)),  # a scheme only counts at the start
    ("ftp://files.lan/x", None),
    ("  fohabl.lan  ", ("fohabl.lan", 80)),
    ("https://obsproject.com/browser-source", ("obsproject.com", 443)),
    ("HTTP://Presenter.lan:8080/x", ("presenter.lan", 8080)),
    ("http://10.77.9.205:3000/", ("10.77.9.205", 3000)),
    ("http://[fe80::1]:81/", ("fe80::1", 81)),
    ("file:///home/newlevel/x.html", None),
    ("about:blank", None),
    ("http://host:notaport/", None),
    ("http:///nohost", None),
    ("", None),
    ("   ", None),
    (None, None),
    (5, None),
])
def test_probe_target(url, want):
    assert k.probe_target(url) == want


@pytest.mark.parametrize("settings,want", [
    ({"url": "http://presenter.lan/ui/camera"}, "http://presenter.lan/ui/camera"),
    ({"url": "fohabl.lan", "width": 1920}, "fohabl.lan"),
    ({}, k.BROWSER_DEFAULT_URL),  # GetInputSettings returns non-defaults only
    ({"is_local_file": True, "local_file": "/x.html", "url": "http://a/"}, None),
    ({"is_local_file": False, "url": "http://a/"}, "http://a/"),
    ({"url": ""}, None),
    ({"url": 3}, None),
    (None, None),
    ([], None),
])
def test_browser_source_url(settings, want):
    assert k.browser_source_url(settings) == want


def test_browser_default_url_is_the_vendored_obs_browser_default():
    src = (REPO / "vendor/obs-studio/plugins/obs-browser/obs-browser-plugin.cpp").read_text()
    assert 'obs_data_set_default_string(settings, "url", "%s")' % k.BROWSER_DEFAULT_URL in src
    assert '"%s", obs_module_text("RefreshNoCache")' % k.REFRESH_BUTTON in src
    assert 'info.id = "%s"' % k.BROWSER_KIND in src


# --- the verify-strih state verdict ------------------------------------------------------------------

NOW = 1_790_000_000.0


def _state(**kw):
    base = {"version": 1, "updated_epoch_s": NOW - 4, "connected": True, "obs_epoch": 2,
            "refreshes": 5, "last_error": None,
            "sources": [{"name": "Odpocet", "reachable": True}, {"name": "Browser Ableset", "reachable": False}]}
    base.update(kw)
    return base


def test_state_verdict_fresh_and_connected_passes_with_counts():
    ok, text = k.state_verdict(_state(), NOW)
    assert ok
    assert "last pass 4 s ago" in text and "connected (OBS run epoch 2)" in text
    assert "2 browser source(s), 1 with a reachable page server" in text and "5 refresh(es)" in text


@pytest.mark.parametrize("kw,needle", [
    ({"updated_epoch_s": NOW - 61}, "stale"),
    ({"updated_epoch_s": NOW + 120}, "stale"),
    ({"connected": False, "last_error": "connect: [Errno 111] Connection refused"}, "NOT connected"),
    ({"updated_epoch_s": None}, "unreadable"),
    ({"updated_epoch_s": "now"}, "unreadable"),
])
def test_state_verdict_fails(kw, needle):
    ok, text = k.state_verdict(_state(**kw), NOW)
    assert not ok
    assert needle in text


def test_state_verdict_not_a_dict():
    assert k.state_verdict([], NOW) == (False, "state unreadable (no updated_epoch_s)")


def test_check_state_cli(tmp_path):
    good = tmp_path / "good.json"
    good.write_text(json.dumps(_state(updated_epoch_s=time.time() - 2)))
    stale = tmp_path / "stale.json"
    stale.write_text(json.dumps(_state(updated_epoch_s=time.time() - 3600)))
    run = lambda p: subprocess.run([sys.executable, str(KEEPER_PATH), "--check-state", str(p)],
                                   capture_output=True, text=True, timeout=30)
    r = run(good)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "connected (OBS run epoch 2)" in r.stdout
    r = run(stale)
    assert r.returncode == 1 and "stale" in r.stdout
    r = run(tmp_path / "absent.json")
    assert r.returncode == 1 and "unreadable" in r.stdout and "strih-browser-keeper.service" in r.stdout


# --- the bounded prober --------------------------------------------------------------------------------

def test_prober_bounds_a_hung_probe_and_never_stacks_threads():
    release = threading.Event()
    calls = []

    def probe(host, port, timeout):
        calls.append((host, port))
        if host == "hung.lan":
            release.wait(30)
            return True
        return host == "up.lan"

    p = k.Prober(probe_fn=probe, timeout=0.2)
    t0 = time.monotonic()
    r = p.probe_all([("hung.lan", 80), ("up.lan", 80), ("down.lan", 80), ("up.lan", 80)])
    assert time.monotonic() - t0 < 3.0
    # an unfinished probe is NO information (None), never "down"
    assert r == {("hung.lan", 80): None, ("up.lan", 80): True, ("down.lan", 80): False}
    # the hung probe is still running: the next pass does not start a second thread for it
    r2 = p.probe_all([("hung.lan", 80), ("up.lan", 80)])
    assert r2[("hung.lan", 80)] is None and r2[("up.lan", 80)] is True
    assert calls.count(("hung.lan", 80)) == 1
    assert calls.count(("up.lan", 80)) == 2  # deduplicated within a pass
    release.set()


def test_prober_uses_a_late_result_on_the_next_pass():
    # slow DNS + a black-holed connect: the probe finishes only after its pass's deadline. Its result
    # must still count on the next pass, or a server that is really down would read "unknown" forever
    # and never get its recovered refresh.
    release = threading.Event()
    calls = []

    def probe(host, port, timeout):
        calls.append(host)
        release.wait(30)
        return False

    p = k.Prober(probe_fn=probe, timeout=0.2)
    assert p.probe_all([("slow.lan", 80)]) == {("slow.lan", 80): None}
    release.set()
    for _ in range(100):
        if not any(t.is_alive() for t in threading.enumerate() if t.name == "probe-slow.lan:80"):
            break
        time.sleep(0.02)
    assert p.probe_all([("slow.lan", 80)]) == {("slow.lan", 80): False}, "the late result, used once"
    assert calls == ["slow.lan"], "no new probe while a late result is waiting"
    assert p.probe_all([("slow.lan", 80)]) == {("slow.lan", 80): False}
    assert calls == ["slow.lan", "slow.lan"], "then a fresh probe"


def test_an_unknown_probe_between_two_failures_still_marks_the_server_down():
    # keeper level: a None (no information) between two failed probes neither resets nor counts
    keeper_logs = []
    seq = iter([None, False, None, False, True])

    class P:
        def probe_all(self, targets):
            r = next(seq)
            return {t: r for t in targets}

    keeper = k.Keeper(P(), keeper_logs.append)
    obs = _ScriptedObs()
    for _ in range(5):
        keeper.run_pass(obs, 1)
    assert any("unreachable" in line for line in keeper_logs)
    assert obs.presses == 1  # the connect refresh once the server answered


def test_prober_reads_a_raising_probe_as_unreachable():
    def probe(host, port, timeout):
        raise RuntimeError("boom")

    assert k.Prober(probe_fn=probe, timeout=0.5).probe_all([("x.lan", 80)]) == {("x.lan", 80): False}


def test_tcp_probe_against_a_real_listener():
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(1)
    port = srv.getsockname()[1]
    try:
        assert k.tcp_probe("127.0.0.1", port, 2.0) is True
    finally:
        srv.close()
    assert k.tcp_probe("127.0.0.1", port, 2.0) is False




def test_the_real_keeper_loop_against_a_fake_obs_websocket(tmp_path):
    server = FakeObsWebSocket(INPUTS)
    probes = {PRESENTER: False, FOHABL: True}
    logs = []
    state_file = tmp_path / "run" / "strih-browser-keeper.json"
    connects = {"n": 0}

    def connect():
        connects["n"] += 1
        if connects["n"] == 1:
            raise ConnectionRefusedError(111, "Connection refused")  # OBS not up yet
        return k.ObsClient("127.0.0.1", server.port, timeout=5.0)

    # what happens after the keeper finished pass N (N = GetInputList requests answered so far)
    script = {
        2: lambda: probes.__setitem__(PRESENTER, True),   # presenter comes up
        4: lambda: probes.__setitem__(FOHABL, False),     # Ableton box goes down mid-day (passes 5+6)
        6: lambda: probes.__setitem__(FOHABL, True),      # ... and back
        7: server.restart,                                # OBS restarts (a new process)
    }
    done, sleeps, seen = 9, {"n": 0}, set()
    first_state = {}

    def sleep(_interval):
        sleeps["n"] += 1
        assert sleeps["n"] < 40, "the keeper loop did not progress"
        if connects["n"] == 1 and "after_refused" not in first_state:
            first_state["after_refused"] = json.loads(state_file.read_text())
        n = server.list_calls
        if n in script and n not in seen:
            seen.add(n)
            script[n]()

    k.run(connect, k.Prober(probe_fn=lambda h, p, t: probes[(h, p)], timeout=2.0), interval=0.0,
          state_file=str(state_file), log=logs.append, sleep=sleep, clock=time.time,
          should_stop=lambda: server.list_calls >= done, endpoint="test", obs_process=server.process_identity)
    server.close()

    assert server.presses() == [
        # connection 1 = OBS run epoch 1
        (1, 1, "Browser Ableset", "refreshnocache"),       # first pass: only the reachable server
        (1, 3, "Browser camera crew", "refreshnocache"),   # presenter came up after pass 2
        (1, 3, "Odpocet", "refreshnocache"),
        (1, 7, "Browser Ableset", "refreshnocache"),       # fohabl back after its outage (down 5+6)
        # connection 2 = OBS run epoch 2 (OBS restarted): every source once more
        (2, 8, "Browser camera crew", "refreshnocache"),
        (2, 8, "Odpocet", "refreshnocache"),
        (2, 8, "Browser Ableset", "refreshnocache"),
    ]
    # the local-file source is never refreshed, a non-browser input never read
    settings_reads = {d["inputName"] for _c, _p, t, d in server.requests if t == "GetInputSettings"}
    assert settings_reads == {"Browser camera crew", "Odpocet", "Browser Ableset", "Local page"}
    # every session subscribes to no events (the event-flood lesson)
    assert [n for n, _ in server.identifies] == [1, 2]
    for _n, ident in server.identifies:
        assert ident["op"] == 1 and ident["d"]["eventSubscriptions"] == 0 and ident["d"]["rpcVersion"] == 1

    # before OBS answered, the state said so
    assert first_state["after_refused"]["connected"] is False
    assert "Connection refused" in first_state["after_refused"]["last_error"]
    final = json.loads(state_file.read_text())
    assert final["connected"] is True and final["obs_epoch"] == 2 and final["refreshes"] == 7
    assert [(s["name"], s["target"], s["reachable"], s["refreshed_epoch"]) for s in final["sources"]] == [
        ("Browser camera crew", "presenter.lan:80", True, 2),
        ("Odpocet", "presenter.lan:80", True, 2),
        ("Browser Ableset", "fohabl.lan:80", True, 2),
    ]
    ok, _text = k.state_verdict(final, time.time())
    assert ok

    text = "\n".join(logs)
    assert text.count("obs-websocket test not reachable") == 1
    assert "connected to obs-websocket test: a new OBS run (process boot-1:1000," in text
    assert "-> OBS run epoch 1," in text
    assert "lost obs-websocket test in OBS run epoch 1" in text
    assert "connected to obs-websocket test: a new OBS run (process boot-1:1000+," in text
    assert "-> OBS run epoch 2," in text
    assert "browser source 'Local page' has no network page server" in text
    assert "browser source 'Odpocet' page server presenter.lan:80 unreachable" in text
    assert "browser source 'Odpocet' page server presenter.lan:80 unreachable -> reachable" in text
    assert "browser source 'Browser Ableset' page server fohabl.lan:80 reachable -> unreachable" in text
    assert ("refreshed browser source 'Browser Ableset' (fohabl.lan): page server back after being "
            "unreachable, OBS run epoch 1") in text
    assert text.count("refreshed browser source") == 7
    # no log line per quiet pass: only refreshes, transitions and connection changes
    assert len(logs) <= 25, logs


class _ScriptedObs:
    """One browser source; PressInputPropertiesButton fails on the press numbers in `fail_on`."""

    def __init__(self, fail_on=()):
        self.presses = 0
        self.fail_on = set(fail_on)

    def request(self, rtype, data=None):
        if rtype == "GetInputList":
            return {"inputs": [{"inputName": "Odpocet", "unversionedInputKind": "browser_source"}]}
        if rtype == "GetInputSettings":
            return {"inputSettings": {"url": "http://presenter.lan/x"}}
        assert rtype == "PressInputPropertiesButton" and data["propertyName"] == "refreshnocache"
        self.presses += 1
        if self.presses in self.fail_on:
            raise k.ObsRequestError("PressInputPropertiesButton failed: code 600 ")
        return {}


def _run_passes(probe_results, obs, epoch=1):
    """Run one keeper pass per probe result (True/False/None) of presenter.lan:80."""
    seq = iter(probe_results)
    current = {}

    class OnePerPass:
        def probe_all(self, targets):
            current["r"] = next(seq)
            return {t: current["r"] for t in targets}

    logs = []
    keeper = k.Keeper(OnePerPass(), logs.append)
    for _ in probe_results:
        keeper.run_pass(obs, epoch)
    return keeper, logs


def test_a_failed_connect_refresh_is_pressed_again_and_logged_once():
    obs = _ScriptedObs(fail_on={1, 2})
    keeper, logs = _run_passes([True] * 5, obs)
    assert obs.presses == 3  # failed, failed, succeeded -- then quiet
    assert keeper.refreshes == 1
    assert sum("failed" in line for line in logs) == 1


def test_a_failed_recovered_refresh_is_pressed_again():
    # connect refresh ok, then the server goes down for two probes, comes back: the recovered refresh
    # fails once and must be pressed again -- never forgotten (the review's reproduction).
    obs = _ScriptedObs(fail_on={2})
    keeper, logs = _run_passes([True, False, False, True, True, True, True], obs)
    assert obs.presses == 3
    assert keeper.refreshes == 2
    assert sum("unreachable -> reachable" in line for line in logs) == 1, logs
    assert sum("reachable -> unreachable" in line for line in logs) == 1, logs
    assert sum("pressed again next pass" in line for line in logs) == 1
    assert sum("page server back after being unreachable" in line for line in logs) == 1


def test_one_lost_probe_or_an_unfinished_one_never_reloads_a_working_page():
    obs = _ScriptedObs()
    keeper, logs = _run_passes([True, False, True, None, True, False, None, True, None, None, True], obs)
    assert obs.presses == 1, "only the connect refresh"
    assert not any("unreachable" in line for line in logs), logs


def test_the_first_failed_probe_gives_no_verdict_and_no_log():
    obs = _ScriptedObs()
    keeper, logs = _run_passes([False], obs)
    assert obs.presses == 0 and logs == []
    assert keeper.last_sources[0]["reachable"] is None and keeper.last_sources[0]["refreshed_epoch"] is None
    keeper2, logs2 = _run_passes([False, False, True], _ScriptedObs())
    assert [l.split(" page server ")[1] for l in logs2 if " page server " in l] == [
        "presenter.lan:80 unreachable", "presenter.lan:80 unreachable -> reachable"]


def test_obs_client_authenticates_when_obs_demands_it():
    server = FakeObsWebSocket(INPUTS, password="s3cret")
    try:
        obs = k.ObsClient("127.0.0.1", server.port, timeout=5.0, password="s3cret")
        assert [i["inputName"] for i in obs.request("GetInputList")["inputs"]][0] == "Browser camera crew"
        obs.close()
        with pytest.raises(k.ObsError, match="demands auth"):
            k.ObsClient("127.0.0.1", server.port, timeout=5.0)
    finally:
        server.close()
    assert server.auth_ok == [True]
