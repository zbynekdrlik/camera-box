"""Issue 1465 (owner 23.10.2026): the stream operator's headphone box plays the outgoing stream from VLC,
and the VLC window no longer blinks while nothing is streamed.

ROZHODNUTE (issue comment 6362030709): "operator si chce otovorit vlc aby videl obraz a chce si vediet
prepnut zvuk do sluchatiek aby isiel z vlc tak vidi a pocuje co je na vystupe z obs a teda vie
skontrolovat lipsync a audio vypadky!!!" -- the VLC window stays, its audio goes to the headphone box.
Design comment 6362045028 (Approach 1):
  1. the headphone card: with no explicit --pcm-match the player prefers the playback PCM of the ONE USB
     audio interface (a /proc/asound card with a `usbid` file: stream-snv's Scarlett Solo 4th Gen, card
     id `Gen`, PCM `USB Audio`); none = the ` Analog$` jack as before (stream-pp's `ALC257 Analog`);
     two or more USB playback PCMs = named once, the jack. An explicit --pcm-match keeps its old meaning;
  2. no blinking: pl_play only while OBS really publishes to the URL -- an `obs` process holding an
     ESTABLISHED TCP connection to the URL's host:port (/proc/net/tcp + tcp6, the obs pids' socket fds);
  3. the login ordering cycle (strih-stream-monitor after strih-obs after graphical-session.target after
     strih-stream-monitor) is gone, and `enable` still means graphical-session.target.

Tier-0: pytest only, the real supervisor against the fake VLC of test_strih_stream_monitor_vlc_1456.py and
a fake /proc.
"""
import json
import os
import re
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import REPO, _mod, asound  # noqa: E402
from test_strih_stream_monitor_vlc_1456 import (  # noqa: E402
    OBS_INODE, OBS_PID, OBS_TO_RESTREAMER, Rig, fake_proc_net, fake_process, tcp_line)

URL = "rtmp://127.0.0.1:1234/live/stream-pp"


# --- 1. the headphone card -----------------------------------------------------------------------------------

def add_usb_card(root, card, card_id, name="USB Audio", devices=(0,)):
    """A USB audio interface: its card dir carries `usbid` (snd-usb-audio writes it), one playback PCM per
    DEVICE (stream-snv's Scarlett Solo 4th Gen: card id `Gen`, PCM `USB Audio`, device 0)."""
    d = root / ("card%d" % card)
    d.mkdir(parents=True, exist_ok=True)
    (d / "id").write_text(card_id + "\n")
    (d / "usbid").write_text("1235:8211\n")
    for dev in devices:
        p = d / ("pcm%dp" % dev)
        (p / "sub0").mkdir(parents=True, exist_ok=True)
        (p / "info").write_text("card: %d\ndevice: %d\nsubdevice: 0\nstream: PLAYBACK\nid: %s\nname: %s\n"
                                % (card, dev, name, name))
        (p / "sub0" / "status").write_text("closed\n")


def _pick(m, root, pcm_match=None):
    return m.resolve_headphone(m.list_playback_pcms(str(root)), pcm_match)


def test_the_pcm_walk_marks_a_usb_card(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen")
    usb = {(p["card_id"], p["device"]): p["usb"] for p in m.list_playback_pcms(str(root))}
    assert usb == {("Generic", 3): False, ("Generic", 7): False, ("Generic_1", 0): False, ("Gen", 0): True}


def test_the_one_usb_interface_wins_over_the_jack(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen")
    pcm, why, note = _pick(m, root)
    assert why is None and note is None and m.alsa_device(pcm) == "plughw:CARD=Gen,DEV=0", (why, note)


def test_no_usb_interface_keeps_the_jack(tmp_path):
    # stream-pp: no USB card, `ALC257 Analog` on Generic_1, exactly as before
    m = _mod()
    root, _proc = asound(tmp_path)
    pcm, why, note = _pick(m, root)
    assert why is None and note is None and m.alsa_device(pcm) == "plughw:CARD=Generic_1,DEV=0"


def test_two_usb_playback_pcms_are_named_and_the_jack_plays(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen")
    add_usb_card(root, 3, "Device", name="USB PnP Sound Device")
    pcm, why, note = _pick(m, root)
    assert why is None and m.alsa_device(pcm) == "plughw:CARD=Generic_1,DEV=0"
    assert "2 USB audio playback PCMs" in note and "Gen:0 USB Audio" in note and "Device:0" in note, note


def test_one_usb_card_with_two_playback_pcms_is_ambiguous_too(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen", devices=(0, 1))
    pcm, _why, note = _pick(m, root)
    assert m.alsa_device(pcm) == "plughw:CARD=Generic_1,DEV=0" and note, note


def test_an_explicit_pcm_match_wins_over_the_usb_interface(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen")
    pcm, why, note = _pick(m, root, m.DEFAULT_PCM_MATCH)
    assert why is None and note is None and m.alsa_device(pcm) == "plughw:CARD=Generic_1,DEV=0"
    pcm, why, _note = _pick(m, root, r"^USB Audio$")
    assert m.alsa_device(pcm) == "plughw:CARD=Gen,DEV=0"
    pcm, why, _note = _pick(m, root, r"Nothing")
    assert pcm is None and "no playback PCM name matches" in why


def test_no_usb_and_no_jack_is_no_card_with_the_reason(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path, analog=False)
    pcm, why, _note = _pick(m, root)
    assert pcm is None and "no playback PCM name matches" in why


def test_the_rule_line_names_how_the_card_is_picked():
    m = _mod()
    assert "ONE USB audio interface" in m.headphone_rule(None) and "Analog" in m.headphone_rule(None)
    assert m.headphone_rule("^USB Audio$") == "the playback PCM matching '^USB Audio$' (--pcm-match)"


# --- 1. the card through the real loop ------------------------------------------------------------------------

def test_the_loop_plays_to_the_usb_interface_and_says_why(tmp_path):
    rig = Rig(tmp_path)
    add_usb_card(rig.root, 2, "Gen")
    rig.publish()
    r = rig.run(2.0)
    assert r.returncode == 0, r.stderr
    argv = rig.vlc_argv()
    assert len(argv) == 1 and "--alsa-audio-device=plughw:CARD=Gen,DEV=0" in argv[0], argv
    watching = [ln for ln in r.stdout.splitlines() if ln.startswith("watching ")]
    assert len(watching) == 1 and "ONE USB audio interface" in watching[0], r.stdout
    assert rig.st()["device"] == "plughw:CARD=Gen,DEV=0" and rig.st()["card"] == 2


def test_the_loop_keeps_an_explicit_pcm_match(tmp_path):
    rig = Rig(tmp_path)
    add_usb_card(rig.root, 2, "Gen")
    rig.publish()
    r = rig.run(2.0, "--pcm-match", r"(?<!\bAlt) Analog$")
    assert r.returncode == 0, r.stderr
    assert "--alsa-audio-device=plughw:CARD=Generic_1,DEV=0" in rig.vlc_argv()[0]
    watching = [ln for ln in r.stdout.splitlines() if ln.startswith("watching ")]
    assert "(--pcm-match)" in watching[0] and "USB" not in watching[0], watching


def test_two_usb_interfaces_are_named_once_over_every_vlc_start(tmp_path):
    rig = Rig(tmp_path)
    add_usb_card(rig.root, 2, "Gen")
    add_usb_card(rig.root, 3, "Device", name="USB PnP Sound Device")
    (rig.dir / "exit-at-start").write_text("")  # the first VLC exits: a second start re-resolves the card
    rig.publish()
    r = rig.run(3.0)
    assert r.returncode == 0, r.stderr
    assert len(rig.vlc_argv()) == 2, rig.log()
    assert all("--alsa-audio-device=plughw:CARD=Generic_1,DEV=0" in a for a in rig.vlc_argv())
    named = [ln for ln in r.stdout.splitlines() if "2 USB audio playback PCMs" in ln]
    assert len(named) == 1, r.stdout


# --- 2. pl_play only while OBS publishes ------------------------------------------------------------------------

def _publisher(m, proc, url=URL):
    return m.obs_publisher(str(proc), url)


def test_obs_holding_the_established_connection_publishes(tmp_path):
    m = _mod()
    proc = tmp_path / "proc"
    fake_proc_net(proc, tcp_rows=[tcp_line(OBS_TO_RESTREAMER, OBS_INODE)])
    fake_process(proc, OBS_PID, "obs", OBS_INODE)
    ok, detail = _publisher(m, proc)
    assert ok, detail


def test_nothing_connected_is_not_publishing(tmp_path):
    m = _mod()
    proc = tmp_path / "proc"
    fake_proc_net(proc)  # only restreamer's own :1234 listener
    fake_process(proc, OBS_PID, "obs")
    ok, detail = _publisher(m, proc)
    assert not ok and "127.0.0.1:1234" in detail, detail


def test_a_player_holding_the_connection_is_not_a_publisher(tmp_path):
    # the monitor's own VLC holds an ESTABLISHED connection to the same host:port while it plays
    m = _mod()
    proc = tmp_path / "proc"
    fake_proc_net(proc, tcp_rows=[tcp_line(OBS_TO_RESTREAMER, OBS_INODE)])
    fake_process(proc, 8888, "vlc", OBS_INODE)
    fake_process(proc, OBS_PID, "obs")
    ok, detail = _publisher(m, proc)
    assert not ok and "obs" in detail, detail


def test_obs_connected_elsewhere_or_not_established_is_not_publishing(tmp_path):
    m = _mod()
    for head in ("1: 0100007F:B5A2 0100007F:04D3 01",   # another port (1235)
                 "1: 0100007F:B5A2 0200007F:04D2 01",   # another host (127.0.0.2)
                 "1: 0100007F:B5A2 0100007F:04D2 02",   # SYN_SENT: OBS still connecting
                 "1: 0100007F:B5A2 0100007F:04D2 06"):  # TIME_WAIT: a stream that just ended
        proc = tmp_path / head.replace(" ", "_").replace(":", "-")
        fake_proc_net(proc, tcp_rows=[tcp_line(head, OBS_INODE)])
        fake_process(proc, OBS_PID, "obs", OBS_INODE)
        ok, detail = _publisher(m, proc)
        assert not ok, (head, detail)


def test_an_ipv4_mapped_tcp6_connection_publishes(tmp_path):
    # ::ffff:127.0.0.1 -> the four 32-bit words in host order, as /proc/net/tcp6 prints them
    m = _mod()
    proc = tmp_path / "proc"
    head = "0: 0000000000000000FFFF00000100007F:B5A2 0000000000000000FFFF00000100007F:04D2 01"
    fake_proc_net(proc, tcp6_rows=[tcp_line(head, OBS_INODE)])
    fake_process(proc, OBS_PID, "obs", OBS_INODE)
    ok, detail = _publisher(m, proc)
    assert ok, detail


def test_the_rtmp_default_port_and_an_unreadable_proc(tmp_path):
    m = _mod()
    proc = tmp_path / "proc"
    fake_proc_net(proc, tcp_rows=[tcp_line("1: 0100007F:B5A2 0100007F:078F 01", OBS_INODE)])  # :1935
    fake_process(proc, OBS_PID, "obs", OBS_INODE)
    assert _publisher(m, proc, "rtmp://127.0.0.1/live/x")[0]
    ok, detail = _publisher(m, tmp_path / "no-proc")
    assert not ok and detail, detail


def test_a_vanished_or_unreadable_pid_is_skipped(tmp_path):
    m = _mod()
    proc = tmp_path / "proc"
    fake_proc_net(proc, tcp_rows=[tcp_line(OBS_TO_RESTREAMER, OBS_INODE)])
    (proc / "4000").mkdir(parents=True)
    (proc / "4000" / "comm").write_text("obs\n")  # no fd dir: exited / not ours
    (proc / "4001").mkdir(parents=True)  # no comm at all
    (proc / "4002" / "comm").parent.mkdir(parents=True)
    (proc / "4002" / "comm").write_bytes(b"\xff\xc4\n")  # a comm that is not UTF-8
    fake_process(proc, OBS_PID, "obs", OBS_INODE)
    ok, detail = _publisher(m, proc)
    assert ok, detail


def test_the_real_proc_reads_without_raising():
    m = _mod()
    ok, detail = m.obs_publisher("/proc", "rtmp://127.0.0.1:1/live/none")
    assert ok is False and detail


def test_obs_publishing_later_starts_playing_and_nothing_before(tmp_path):
    rig = Rig(tmp_path)
    p = subprocess.Popen(rig.argv(4.0), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=rig.env())
    time.sleep(1.5)
    before = rig.cmds().count("pl_play")
    rig.publish()
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, err
    assert before == 0, rig.log()
    assert any(ln.startswith("playing ") for ln in out.splitlines()), out
    waits = [ln for ln in out.splitlines() if ln.startswith("waiting:")]
    assert waits and "OBS is not publishing" in waits[0], out


def test_only_a_non_obs_connection_never_sends_pl_play(tmp_path):
    rig = Rig(tmp_path)
    fake_proc_net(rig.proc, tcp_rows=[tcp_line(OBS_TO_RESTREAMER, OBS_INODE)])
    fake_process(rig.proc, 8888, "ffmpeg", OBS_INODE)
    r = rig.run(2.0)
    assert r.returncode == 0, r.stderr
    assert rig.cmds().count("pl_play") == 0, rig.log()
    assert rig.st()["state"] == "waiting" and "OBS is not publishing" in rig.st()["reason"], rig.st()


def test_check_state_passes_a_monitor_waiting_for_obs(tmp_path):
    rig = Rig(tmp_path)
    r = rig.run(1.5)
    assert r.returncode == 0, r.stderr
    m = _mod()
    st = rig.st()
    st["updated_epoch_s"] = time.time()
    rig.state.write_text(json.dumps(st))
    ok, line = m.check_state(str(rig.state), time.time(), str(rig.root), str(rig.proc))
    assert ok and line.startswith("waiting "), line


# --- review round 1 -------------------------------------------------------------------------------------------

def test_a_usb_headphone_box_keeps_its_own_mixer(tmp_path):
    # snd-usb-audio names many headsets' / dongles' main output `Speaker`: the jack's speaker mute would
    # silence the very box the operator listens on, and a 0 dB write on its own volume is full scale
    rig = Rig(tmp_path)
    add_usb_card(rig.root, 2, "Gen")
    rig.publish()
    r = rig.run(2.0)
    assert r.returncode == 0, r.stderr
    assert "--alsa-audio-device=plughw:CARD=Gen,DEV=0" in rig.vlc_argv()[0]
    assert not [a for a in rig.amixer() if " Gen " in " %s " % a], rig.amixer()
    named = [ln for ln in r.stdout.splitlines() if "mixer:" in ln and "Gen" in ln]
    assert len(named) == 1 and "USB" in named[0], r.stdout


def test_the_jack_still_gets_its_unity_mixer_and_speaker_mute(tmp_path):
    rig = Rig(tmp_path)
    rig.publish()
    r = rig.run(1.5)
    assert r.returncode == 0, r.stderr
    assert ["-q", "-c", "Generic_1", "sset", "Speaker", "mute"] in [a.split() for a in rig.amixer()]


def test_the_waiting_line_stays_one_line_while_other_clients_come_and_go(tmp_path):
    # the not-published reason is constant; what holds the port (nothing / a non-obs client) is a state field
    rig = Rig(tmp_path)
    p = subprocess.Popen(rig.argv(3.0), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=rig.env())
    time.sleep(1.2)
    fake_proc_net(rig.proc, tcp_rows=[tcp_line(OBS_TO_RESTREAMER, OBS_INODE)])
    fake_process(rig.proc, 8888, "ffmpeg", OBS_INODE)
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, err
    waiting = [ln for ln in out.splitlines() if ln.startswith("waiting:")]
    assert len(waiting) == 1, out
    st = rig.st()
    assert st["state"] == "waiting" and "no obs process holds" in st["publisher"], st


def test_a_hostname_url_is_resolved_at_most_once_a_minute(tmp_path, monkeypatch):
    m = _mod()
    calls = []
    real = m.socket.getaddrinfo

    def counting(*a, **kw):
        calls.append(a[0])
        return real("127.0.0.1", *a[1:], **kw)

    monkeypatch.setattr(m.socket, "getaddrinfo", counting)
    mon = m.Monitor("rtmp://restreamer.example:1234/live/x", None, proc_root=str(tmp_path), log=lambda s: None)
    for _ in range(5):
        mon.publisher()
    assert calls == ["restreamer.example"], calls


def test_an_obs_process_name_the_kernel_cannot_report_is_refused(tmp_path):
    rig = Rig(tmp_path)
    r = rig.run(0.5, "--obs-process-name", "obs-with-a-long-name")
    assert r.returncode == 2 and "15" in r.stderr, (r.returncode, r.stderr)


def test_the_unit_says_where_the_audio_goes():
    text = (REPO / "systemd" / "strih-stream-monitor.service").read_text()
    desc = [ln for ln in text.splitlines() if ln.startswith("Description=")][0]
    assert "headphone box" in desc and "1465" in desc, desc


# --- 3. no login ordering cycle -------------------------------------------------------------------------------

def _unit_name(path):
    return path.name[:-3] if path.name.endswith(".in") else path.name


def _sections(text):
    out, sec = {}, None
    for raw in text.splitlines():
        ln = raw.strip()
        if not ln or ln.startswith(("#", ";")):
            continue
        if ln.startswith("[") and ln.endswith("]"):
            sec = ln[1:-1]
            continue
        k, _, v = ln.partition("=")
        out.setdefault((sec, k.strip()), []).extend(v.split())
    return out


def ordering_graph(units):
    """{unit: set(units it starts AFTER)} for UNITS = {name: unit text}, with the ordering a target adds by
    itself: a target is After= every unit it Wants (an [Install] WantedBy= link), unless that unit has
    DefaultDependencies=no or is already After= the target (systemd's own loop guard,
    unit_add_default_target_dependency: "Don't create loops")."""
    after = {}
    for name, text in units.items():
        s = _sections(text)
        after.setdefault(name, set()).update(s.get(("Unit", "After"), []))
        for other in s.get(("Unit", "Before"), []):
            after.setdefault(other, set()).add(name)
    for name, text in units.items():
        s = _sections(text)
        if s.get(("Unit", "DefaultDependencies"), ["yes"])[-1] == "no":
            continue
        for target in s.get(("Install", "WantedBy"), []):
            if target.endswith(".target") and target not in after.get(name, set()):
                after.setdefault(target, set()).add(name)
    return after


def find_cycle(after):
    state, stack = {}, []

    def visit(u):
        state[u] = 1
        stack.append(u)
        for v in sorted(after.get(u, ())):
            if state.get(v) == 1:
                return stack[stack.index(v):] + [v]
            if not state.get(v):
                c = visit(v)
                if c:
                    return c
        stack.pop()
        state[u] = 2
        return None

    for u in sorted(after):
        if not state.get(u):
            c = visit(u)
            if c:
                return c
    return None


def _repo_units():
    d = REPO / "systemd"
    return {_unit_name(p): p.read_text() for p in sorted(d.iterdir())
            if re.search(r"\.(service|timer|path|socket|target)(\.in)?$", p.name)}


def test_the_checker_sees_the_live_login_cycle():
    # the journal line of 23.10.2026: strih-stream-monitor after strih-obs after graphical-session.target
    # after strih-stream-monitor -- proves the model finds exactly that shape
    units = {"strih-obs.service": "[Unit]\nAfter=graphical-session.target\n[Install]\n"
                                  "WantedBy=graphical-session.target\n",
             "strih-stream-monitor.service": "[Unit]\nAfter=strih-obs.service\n[Install]\n"
                                             "WantedBy=graphical-session.target\n"}
    cycle = find_cycle(ordering_graph(units))
    assert cycle and set(cycle) == {"strih-stream-monitor.service", "strih-obs.service",
                                    "graphical-session.target"}, cycle
    assert find_cycle(ordering_graph({"strih-obs.service": units["strih-obs.service"]})) is None


def test_no_unit_in_the_checkout_has_an_ordering_cycle():
    cycle = find_cycle(ordering_graph(_repo_units()))
    assert cycle is None, " after ".join(cycle or [])


def test_the_monitor_and_the_keeper_still_follow_obs_and_stay_enableable():
    units = _repo_units()
    for name in ("strih-stream-monitor.service", "strih-browser-keeper.service"):
        s = _sections(units[name])
        assert "strih-obs.service" in s[("Unit", "After")], name
        assert s[("Install", "WantedBy")] == ["graphical-session.target"], name
