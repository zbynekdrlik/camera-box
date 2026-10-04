"""Issue 1399 -- the keeper's refresh round follows a real OBS RESTART only (main ROZHODNUTE 5978724027).

WHY: a refresh blanks a browser source for a moment. A keeper crash or redeploy during an event, or a
WS reconnect after a slow request, must not blink every graphic on air. So the OBS run epoch goes up
only when OBS itself (re)started: the keeper reads the OBS identity on every connect (the local `obs`
process start time from /proc, and GetStats `renderTotalFrames`, which a restart sends backwards) and
keeps it, the epoch and the per-source states in its /run state file across its own restarts.

What this pins, with the REAL keeper loop against a real obs-websocket server on localhost:
  * a keeper restart under the same OBS -> no refresh (also a pending refresh survives the restart);
  * an OBS restart -> each reachable source refreshed once (while the keeper ran, or while it was down);
  * a WS reconnect without an OBS restart -> no refresh;
  * the frame-count fallback when no local process is visible;
  * the pure pieces: obs_restarted, proc_start_ticks, local_obs_process_identity, restored_sources,
    load_state.

Tier-0: pytest + websocket-client, localhost only.
"""
import importlib.util
import json
import os
import sys
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
from strih_keeper_fakes_1399 import FOHABL, INPUTS, PRESENTER, FakeObsWebSocket  # noqa: E402

spec = importlib.util.spec_from_file_location("strih_browser_keeper_obs_run_1399", REPO / "scripts" / "strih_browser_keeper.py")
k = importlib.util.module_from_spec(spec)
sys.modules["strih_browser_keeper_obs_run_1399"] = k
spec.loader.exec_module(k)
S = k.SourceState


# --- pure pieces ------------------------------------------------------------------------------------

@pytest.mark.parametrize("stored,current,want", [
    (None, {"process": "b:1", "frames": 10}, True),                       # first run after a boot
    ("garbage", {"process": "b:1", "frames": 10}, True),
    ({"process": "b:1", "frames": 900}, {"process": "b:1", "frames": 990}, False),
    ({"process": "b:1", "frames": 900}, {"process": "b:1", "frames": 30}, False),   # same process wins
    ({"process": "b:1", "frames": 900}, {"process": "b:2", "frames": 9000}, True),  # a new process
    ({"process": None, "frames": 900}, {"process": None, "frames": 990}, False),    # frame fallback
    ({"process": None, "frames": 900}, {"process": None, "frames": 30}, True),
    ({"process": "b:1", "frames": 900}, {"process": None, "frames": 30}, True),     # one side unknown
    ({"process": "b:1", "frames": 900}, {"process": None, "frames": 950}, False),
    ({"process": None, "frames": None}, {"process": None, "frames": 30}, True),     # nothing comparable
    ({"process": None, "frames": 900}, {"process": None, "frames": None}, True),
    ({"process": None, "frames": True}, {"process": None, "frames": 30}, True),     # a bool is no count
])
def test_obs_restarted_table(stored, current, want):
    assert k.obs_restarted(stored, current) is want


@pytest.mark.parametrize("text,want", [
    ("4242 (obs) S 1 4242 4242 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 987654 123 456", 987654),
    ("4242 (my (odd) obs) R 1 4242 4242 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 55 1 2", 55),
    ("4242 (obs) S 1 2 3", None),
    ("4242 (obs) S 1 4242 4242 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 x 1 2", None),
    ("no parens at all", None),
    (None, None),
])
def test_proc_start_ticks(text, want):
    assert k.proc_start_ticks(text) == want


def _fake_proc(root, procs, boot="boot-a"):
    """PROCS: (pid, comm as str or raw bytes, start ticks)."""
    (root / "sys" / "kernel" / "random").mkdir(parents=True)
    (root / "sys" / "kernel" / "random" / "boot_id").write_text(boot + "\n")
    for pid, comm, ticks in procs:
        d = root / str(pid)
        d.mkdir()
        raw = comm if isinstance(comm, bytes) else comm.encode()
        (d / "comm").write_bytes(raw + b"\n")
        (d / "stat").write_bytes(b"%d (%s) S 1 %d %d 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 %d 1 2\n"
                                 % (pid, raw, pid, pid, ticks))
    return root


def test_local_obs_process_identity_reads_the_oldest_obs(tmp_path):
    # the OLDEST: OBS is single-instance, so a second `obs` is a short-lived stray (an "already
    # running" dialog of a desktop launch) -- the newest would read it as a new OBS run and blink
    root = _fake_proc(tmp_path / "proc", [(100, "bash", 5), (200, "obs", 700), (300, "obs-ffmpeg-mux", 900),
                                          (400, "obs", 800)])
    assert k.local_obs_process_identity(str(root), "obs", os.getuid()) == "boot-a:700"
    assert k.local_obs_process_identity(str(root), "obs", os.getuid() + 1) is None  # another user's
    assert k.local_obs_process_identity(str(root), "absent", os.getuid()) is None
    assert k.local_obs_process_identity(str(tmp_path / "no-proc"), "obs", os.getuid()) is None
    # the real /proc: this python is a process of ours, read without crashing
    me = k.local_obs_process_identity("/proc", Path("/proc/self/comm").read_text().strip(), os.getuid())
    assert me is not None and ":" in me


def test_a_comm_that_is_not_utf8_never_crashes_the_identity_read(tmp_path):
    # a 15-byte comm can cut a multi-byte character (an executable with a Slovak diacritic): read as
    # text it raised UnicodeDecodeError, which is no OSError, and crash-looped the keeper on every connect
    root = _fake_proc(tmp_path / "proc", [(100, b"zm\xc4\x9bna-\xc4", 5), (200, b"\xff\xfe", 6), (300, "obs", 42),
                                          (400, b"obs\xc4", 1)])
    assert k.local_obs_process_identity(str(root), "obs", os.getuid()) == "boot-a:42"
    (root / "300" / "stat").write_bytes(b"300 (obs\xc4\x9b) S 1 300 300 0 -1 4194560 1 2 3 4 5 6 7 8 20 0 40 0 77 1 2\n")
    assert k.local_obs_process_identity(str(root), "obs", os.getuid()) == "boot-a:77"


def test_restored_sources_validates_every_entry():
    state = {"obs_epoch": 3, "sources": [
        # the memory, not this pass's verdict: Odpocet's verdict is still unknown, its memory says up
        {"name": "Odpocet", "url": "http://presenter.lan/x", "reachable": None, "refreshed_epoch": 3,
         "remembered_reachable": True},
        {"name": "Ableset", "url": "fohabl.lan", "reachable": True, "refreshed_epoch": None,
         "remembered_reachable": False},
        {"name": "bad epoch", "url": "u", "refreshed_epoch": "3", "remembered_reachable": True},
        {"name": "bad bool", "url": "u", "refreshed_epoch": 3, "remembered_reachable": 1},
        {"name": "flag epoch", "url": "u", "refreshed_epoch": True, "remembered_reachable": True},
        {"url": "no name"},
        "junk",
    ]}
    assert k.restored_sources(state) == {
        ("Odpocet", "http://presenter.lan/x"): S(3, True),
        ("Ableset", "fohabl.lan"): S(None, False),
    }
    assert k.restored_sources({"connect_epoch": 3, "sources": state["sources"]}) == {}  # no obs_epoch
    assert k.restored_sources(None) == {}


def test_source_entry_keeps_the_verdict_and_the_memory_apart():
    e = k.source_entry("Odpocet", "http://presenter.lan/x", ("presenter.lan", 80), None, S(2, False))
    assert e == {"name": "Odpocet", "url": "http://presenter.lan/x", "target": "presenter.lan:80",
                 "reachable": None, "refreshed_epoch": 2, "remembered_reachable": False}
    assert k.source_entry("x", "about:blank", None, None, None)["target"] is None
    assert k.restored_sources({"obs_epoch": 2, "sources": [e]}) == {("Odpocet", "http://presenter.lan/x"): S(2, False)}


def test_check_state_names_what_identifies_the_obs_run():
    base = {"updated_epoch_s": 100.0, "connected": True, "obs_epoch": 2, "sources": [], "refreshes": 0}
    ok, text = k.state_verdict(dict(base, obs_identity={"process": "b:7", "frames": 9}), 101.0)
    assert ok and text.endswith("; OBS run identified by the obs process start time")
    ok, text = k.state_verdict(dict(base, obs_identity={"process": None, "frames": 9}), 101.0)
    assert ok and text.endswith("; OBS run identified by the frame count only (no local obs process seen)")
    ok, text = k.state_verdict(dict(base, obs_identity=None), 101.0)
    assert ok and text.endswith("; OBS run identified by the frame count only (no local obs process seen)")


def test_main_reads_the_local_obs_process_only_for_a_local_obs(tmp_path, monkeypatch):
    seen = []
    monkeypatch.setattr(k, "run", lambda connect, prober, **kw: seen.append(kw))
    monkeypatch.setattr(k, "local_obs_process_identity", lambda name="obs": "id-of-" + name)
    assert k.main(["--state-file", str(tmp_path / "s.json")]) == 0
    assert seen[-1]["obs_process"]() == "id-of-obs"
    assert k.main(["--state-file", str(tmp_path / "s.json"), "--obs-process-name", "obs-dev"]) == 0
    assert seen[-1]["obs_process"]() == "id-of-obs-dev"
    assert k.main(["--host", "10.77.9.202", "--state-file", str(tmp_path / "s.json")]) == 0
    assert seen[-1]["obs_process"] is None  # a remote OBS: the frame count decides


def test_load_state(tmp_path):
    assert k.load_state(str(tmp_path / "absent.json")) == (None, None)  # the first run after a boot
    (tmp_path / "bad.json").write_text("{not json")
    st, err = k.load_state(str(tmp_path / "bad.json"))
    assert st is None and err
    (tmp_path / "list.json").write_text("[]")
    assert k.load_state(str(tmp_path / "list.json")) == (None, "not a JSON object")
    (tmp_path / "ok.json").write_text('{"obs_epoch": 2}')
    assert k.load_state(str(tmp_path / "ok.json")) == ({"obs_epoch": 2}, None)


# --- the real keeper loop ------------------------------------------------------------------------------

def _keeper_run(server, state_file, probes, passes, events=None, obs_process="server"):
    """Run the real keeper loop for PASSES passes (GetInputList requests) of THIS run; EVENTS maps a
    pass number (of this run) to a callable run after it. Returns (presses in this run, logs)."""
    events = dict(events or {})
    base_list, base_presses = server.list_calls, len(server.presses())
    logs, sleeps = [], {"n": 0}

    def sleep(_interval):
        sleeps["n"] += 1
        assert sleeps["n"] < 60, "the keeper loop did not progress"
        n = server.list_calls - base_list
        if n in events:
            events.pop(n)()

    proc = server.process_identity if obs_process == "server" else obs_process
    k.run(lambda: k.ObsClient("127.0.0.1", server.port, timeout=5.0),
          k.Prober(probe_fn=lambda h, p, t: probes[(h, p)], timeout=2.0), interval=0.0,
          state_file=str(state_file), log=logs.append, sleep=sleep, clock=time.time,
          should_stop=lambda: server.list_calls - base_list >= passes, endpoint="test", obs_process=proc)
    return [p[2] for p in server.presses()[base_presses:]], logs


BROWSERS = ["Browser camera crew", "Odpocet", "Browser Ableset"]


class DropOnPress(FakeObsWebSocket):
    """Loses the connection (same OBS process) on the `drop_at_press`-th refresh press once armed."""

    def __init__(self, *a, **kw):
        super().__init__(*a, **kw)
        self.drop_at_press = None
        self._armed_presses = 0

    def _handle(self, rtype, data):
        if rtype == "PressInputPropertiesButton" and self.drop_at_press is not None:
            self._armed_presses += 1
            if self._armed_presses == self.drop_at_press:
                self.drop_at_press = None
                with self.lock:
                    self.requests.pop()  # this press never reached the button: not a press
                self.drop()
                return False, None
        return super()._handle(rtype, data)


@pytest.fixture
def obs_server():
    server = FakeObsWebSocket(INPUTS)
    yield server
    server.close()


def test_a_keeper_restart_under_the_same_obs_refreshes_nothing(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: True, FOHABL: True}
    first, _ = _keeper_run(obs_server, state, probes, 3)
    assert sorted(first) == sorted(BROWSERS), "the first run after a boot refreshes each source once"
    second, logs = _keeper_run(obs_server, state, probes, 4)  # a NEW keeper process, the same OBS
    assert second == [], "a keeper restart must not blink the graphics"
    text = "\n".join(logs)
    assert "resumed from %s: OBS run epoch 1, 3 browser source(s) remembered" % state in text
    assert "the same OBS run as before (OBS run epoch 1) -- no refresh round" in text
    assert json.loads(state.read_text())["obs_epoch"] == 1


def test_an_obs_restart_while_the_keeper_was_down_refreshes_each_source_once(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: True, FOHABL: True}
    _keeper_run(obs_server, state, probes, 2)
    obs_server.restart()  # OBS restarts while no keeper runs
    again, logs = _keeper_run(obs_server, state, probes, 4)
    assert sorted(again) == sorted(BROWSERS), "each source once, never twice"
    assert "a new OBS run (process boot-1:1000+," in "\n".join(logs)
    assert json.loads(state.read_text())["obs_epoch"] == 2


def test_an_obs_restart_while_the_keeper_runs_refreshes_each_source_once(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    presses, logs = _keeper_run(obs_server, state, {PRESENTER: True, FOHABL: True}, 5, {2: obs_server.restart})
    assert sorted(presses[:3]) == sorted(BROWSERS) and sorted(presses[3:]) == sorted(BROWSERS)
    assert len(presses) == 6


def test_a_ws_reconnect_without_an_obs_restart_refreshes_nothing(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    presses, logs = _keeper_run(obs_server, state, {PRESENTER: True, FOHABL: True}, 6, {2: obs_server.drop})
    assert sorted(presses) == sorted(BROWSERS), "only the first round: the reconnect is the same OBS run"
    text = "\n".join(logs)
    assert "lost obs-websocket test in OBS run epoch 1" in text
    assert "the same OBS run as before (OBS run epoch 1) -- no refresh round" in text
    assert obs_server.connections == 2


def test_after_an_obs_restart_a_ws_reconnect_refreshes_nothing(tmp_path, obs_server):
    # the identity must follow the NEW OBS: compared with the pre-restart one, every later reconnect
    # would read as a new run and blink every graphic again
    state = tmp_path / "strih-browser-keeper.json"
    presses, _ = _keeper_run(obs_server, state, {PRESENTER: True, FOHABL: True}, 6,
                             {2: obs_server.restart, 4: obs_server.drop})
    assert len(presses) == 6 and sorted(presses[3:]) == sorted(BROWSERS)


def test_after_an_obs_restart_a_keeper_restart_refreshes_nothing(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: True, FOHABL: True}
    _keeper_run(obs_server, state, probes, 4, {2: obs_server.restart})
    again, logs = _keeper_run(obs_server, state, probes, 3)
    assert again == []
    assert "the same OBS run as before (OBS run epoch 2) -- no refresh round" in "\n".join(logs)


def test_a_pass_cut_short_after_a_refresh_never_refreshes_that_source_twice(tmp_path):
    # OBS restarts; the new round's SECOND press loses the socket; the keeper is restarted before it
    # finished another pass. The refresh that went through is in the state file, so the next keeper
    # presses only the sources still owed in this OBS run.
    server = DropOnPress(INPUTS)
    try:
        state = tmp_path / "strih-browser-keeper.json"
        probes = {PRESENTER: True, FOHABL: True}
        _keeper_run(server, state, probes, 2)

        def restart_and_arm():
            server.restart()
            server.drop_at_press = 2

        cut, _ = _keeper_run(server, state, probes, 3, {2: restart_and_arm})
        assert cut == ["Browser camera crew"]
        rest, _ = _keeper_run(server, state, probes, 2)
        assert sorted(rest) == ["Browser Ableset", "Odpocet"]
    finally:
        server.close()


def test_a_pending_refresh_survives_a_keeper_restart(tmp_path, obs_server):
    # presenter was down for the whole first keeper run (its pages never got their refresh in this OBS
    # run); after a keeper restart under the same OBS, presenter comes up: refreshed once, fohabl not.
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: False, FOHABL: True}
    first, _ = _keeper_run(obs_server, state, probes, 3)
    assert first == ["Browser Ableset"]
    probes[PRESENTER] = True
    second, _ = _keeper_run(obs_server, state, probes, 3)
    assert sorted(second) == ["Browser camera crew", "Odpocet"]


def test_a_down_server_remembered_across_a_keeper_restart_gets_its_recovered_refresh(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: True, FOHABL: True}
    _keeper_run(obs_server, state, probes, 2)
    probes[FOHABL] = False
    _keeper_run(obs_server, state, probes, 3)  # fohabl goes down (2 failed probes) under keeper run 2
    # a third keeper process ends after ONE pass: its own probe history is fresh, so its verdict on
    # fohabl is still unknown (down only after 2 failed probes) -- the memory must still say down
    third, _ = _keeper_run(obs_server, state, probes, 1)
    assert third == []
    row = [s for s in json.loads(state.read_text())["sources"] if s["name"] == "Browser Ableset"][0]
    assert row["reachable"] is None and row["remembered_reachable"] is False
    probes[FOHABL] = True
    fourth, logs = _keeper_run(obs_server, state, probes, 2)  # a fourth keeper process: fohabl is back
    assert fourth == ["Browser Ableset"], "the remembered outage still earns its one recovered refresh"
    assert "page server fohabl.lan:80 unreachable -> reachable" in "\n".join(logs)


def test_the_frame_count_tells_a_restart_when_no_local_process_is_visible(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    probes = {PRESENTER: True, FOHABL: True}
    no_proc = lambda: None  # noqa: E731 -- a remote OBS: no process identity
    presses, _ = _keeper_run(obs_server, state, probes, 6, {2: obs_server.drop, 4: obs_server.restart},
                             obs_process=no_proc)
    # round 1, nothing after the drop (frames kept growing), round 2 after the restart (frames from 0)
    assert len(presses) == 6 and sorted(presses[:3]) == sorted(BROWSERS)
    assert json.loads(state.read_text())["obs_identity"]["process"] is None


def test_the_frame_count_is_updated_every_pass_so_a_long_run_still_tells_a_restart(tmp_path, obs_server):
    # the new OBS answers 90 at the reconnect: above the old run's count at ITS connect (30), below its
    # count at the last pass (150). Only the per-pass update tells this restart apart.
    state = tmp_path / "strih-browser-keeper.json"
    presses, _ = _keeper_run(obs_server, state, {PRESENTER: True, FOHABL: True}, 6,
                             {4: lambda: obs_server.restart(frames=60)}, obs_process=lambda: None)
    assert len(presses) == 6 and sorted(presses[3:]) == sorted(BROWSERS)


def test_the_state_file_carries_the_obs_identity_while_obs_is_down(tmp_path, obs_server):
    state = tmp_path / "strih-browser-keeper.json"
    _keeper_run(obs_server, state, {PRESENTER: True, FOHABL: True}, 2)
    st = json.loads(state.read_text())
    assert st["version"] == 2 and st["obs_epoch"] == 1
    assert st["obs_identity"]["process"] == "boot-1:1000" and st["obs_identity"]["frames"] > 0
    calls = {"n": 0}

    def refused():
        calls["n"] += 1
        raise ConnectionRefusedError(111, "Connection refused")

    k.run(refused, k.Prober(probe_fn=lambda h, p, t: True, timeout=1.0), interval=0.0, state_file=str(state),
          log=lambda m: None, sleep=lambda s: None, should_stop=lambda: calls["n"] >= 2)
    down = json.loads(state.read_text())
    assert down["connected"] is False and down["obs_identity"] == st["obs_identity"] and down["obs_epoch"] == 1
    assert [s["name"] for s in down["sources"]] == [s["name"] for s in st["sources"]]
