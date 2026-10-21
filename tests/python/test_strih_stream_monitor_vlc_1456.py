"""issue 1456 (owner 21.10.2026): the stream box's headphone monitor plays in VLC, the same as stream-snv.

"tak chcem to mat rovnako cize to tiez z implementuj aby na stream-pp tiez bolo vlc, a neviem si v stream-pp
nastavovat hlasitost vystuput sluchatkoveho a je to potichu". Design comment 6321122421:
  * scripts/strih_stream_monitor.py stays the unit's supervisor and keeps the verify contract (the
    headphone PCM by name, busy / no-card, the state file, --check-state); the player is VLC, straight to
    ALSA (never PipeWire), its volume slider the operator's control;
  * before each VLC start the card's Master / Headphone / PCM go to 0 dB unmuted (stream-pp read -23.25 dB
    on 21.10.2026) and Speaker is muted;
  * the supervisor reads VLC's http status: stopped -> pl_play every retry, paused too long -> pl_stop (a
    paused player never holds a restreamer subscriber), a VLC that exits is started again.

The fake `vlc` below serves VLC's http status interface (Basic auth, empty user) on the --http-port it is
given, logs every command and its argv, and plays when a "published" flag file exists.
"""
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import LEAKY, PROG, REPO, _mod, asound  # noqa: E402,F401

FAKE_VLC = r'''
import base64, json, os, signal, sys, time
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse, parse_qs
args = sys.argv[1:]
opt = dict(a[2:].split("=", 1) for a in args if a.startswith("--") and "=" in a)
d = os.environ["FAKE_VLC_DIR"]
def log(s):
    with open(os.path.join(d, "log"), "a") as f:
        f.write(s + "\n")
log("argv " + json.dumps(args))
log("pid %d" % os.getpid())
state = {"s": "stopped"}
def published():
    return os.path.exists(os.path.join(d, "published"))
def own_pcm(on):
    st = os.environ.get("FAKE_PCM_STATUS")
    if not st:
        return
    if on:
        open(st, "w").write("state: RUNNING\nowner_pid   : %d\ntrigger_time: 1.0\n" % os.getpid())
    else:
        open(st, "w").write("closed\n")
def play():
    state["s"] = "playing" if published() else "stopped"
    own_pcm(state["s"] == "playing")
if os.path.exists(os.path.join(d, "exit-at-start")):
    os.remove(os.path.join(d, "exit-at-start"))
    log("exit")
    sys.exit(3)
play()
want = "Basic " + base64.b64encode((":" + opt["http-password"]).encode()).decode()
class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def do_GET(self):
        if self.headers.get("Authorization") != want:
            self.send_response(401); self.end_headers(); return
        q = parse_qs(urlparse(self.path).query)
        cmd = (q.get("command") or [""])[0]
        if cmd:
            log("cmd " + cmd)
            if cmd == "pl_play":
                play()
            elif cmd == "pl_stop":
                state["s"] = "stopped"; own_pcm(False)
        forced = os.path.join(d, "force-state")
        if os.path.exists(forced):
            state["s"] = open(forced).read().strip()
        body = json.dumps({"state": state["s"], "version": "3.0.23"}).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json"); self.end_headers()
        self.wfile.write(body)
def bye(*_):
    log("sigterm")
    own_pcm(False)
    os._exit(0)
signal.signal(signal.SIGTERM, bye)
HTTPServer((opt["http-host"], int(opt["http-port"])), H).serve_forever()
'''

FAKE_AMIXER = r'''
import os, sys
d = os.environ["FAKE_VLC_DIR"]
with open(os.path.join(d, "amixer"), "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\n")
missing = os.environ.get("FAKE_AMIXER_MISSING", "").split(",")
if len(sys.argv) > 4 and sys.argv[4] in missing:
    sys.stderr.write("amixer: Unable to find simple control '%s',0\n" % sys.argv[4])
    sys.exit(1)
'''


def _free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class Rig:
    def __init__(self, tmp_path, owner=None, analog=True):
        self.tmp = tmp_path
        self.root, self.proc = asound(tmp_path, analog=analog, owner=owner)
        self.dir = tmp_path / "vlc"
        self.dir.mkdir()
        self.bin = tmp_path / "fakebin"
        self.bin.mkdir()
        for name, body in (("vlc", FAKE_VLC), ("amixer", FAKE_AMIXER)):
            (self.bin / name).write_text("#!%s\n%s" % (sys.executable, body))
            (self.bin / name).chmod(0o755)
        self.state = tmp_path / "run" / "strih-stream-monitor.json"
        self.port = _free_port()
        self.pcm_status = self.root / "card1" / "pcm0p" / "sub0" / "status"

    def env(self, **extra):
        e = {k: v for k, v in os.environ.items() if k not in LEAKY}
        e.update({"FAKE_VLC_DIR": str(self.dir), "FAKE_PCM_STATUS": str(self.pcm_status)})
        e.update(extra)
        return e

    def argv(self, run_s, *extra):
        return [sys.executable, str(PROG), "--url", "rtmp://127.0.0.1:1234/live/stream-pp",
                "--state-file", str(self.state), "--vlc", str(self.bin / "vlc"), "--amixer", str(self.bin / "amixer"),
                "--asound-root", str(self.root), "--proc-root", str(self.proc), "--http-port", str(self.port),
                "--retry-s", "0.3", "--poll-s", "0.1", "--pause-max-s", "0.6", "--startup-s", "5",
                "--run-s", str(run_s), *extra]

    def run(self, run_s=3.0, *extra, **env):
        return subprocess.run(self.argv(run_s, *extra), capture_output=True, text=True, env=self.env(**env),
                              timeout=60)

    def log(self):
        p = self.dir / "log"
        return p.read_text().splitlines() if p.exists() else []

    def cmds(self):
        return [ln[4:] for ln in self.log() if ln.startswith("cmd ")]

    def vlc_argv(self):
        return [json.loads(ln[5:]) for ln in self.log() if ln.startswith("argv ")]

    def amixer(self):
        p = self.dir / "amixer"
        return p.read_text().splitlines() if p.exists() else []

    def st(self):
        return json.loads(self.state.read_text())


# --- the VLC command line ------------------------------------------------------------------------------------

def test_the_vlc_argv_plays_straight_to_the_alsa_jack_with_its_http_status_on_loopback():
    m = _mod()
    argv = m.vlc_argv("vlc", "plughw:CARD=Generic_1,DEV=0", "rtmp://127.0.0.1:1234/live/stream-pp", 8090, "pw")
    assert argv[0] == "vlc" and argv[-1] == "rtmp://127.0.0.1:1234/live/stream-pp"
    for a in ("--aout=alsa", "--alsa-audio-device=plughw:CARD=Generic_1,DEV=0", "--extraintf=http",
              "--http-host=127.0.0.1", "--http-port=8090", "--http-password=pw", "--network-caching=1000",
              "--no-qt-error-dialogs", "--no-qt-privacy-ask"):
        assert a in argv, (a, argv)
    assert not [a for a in argv if "pulse" in a or "pipewire" in a]
    assert "--repeat" not in argv, "the supervisor retries with pl_play, never a tight VLC repeat loop"


def test_the_mixer_goes_to_unity_and_the_speaker_is_muted():
    m = _mod()
    cmds = m.mixer_unity_cmds("amixer", "Generic_1")
    assert cmds == [["amixer", "-q", "-c", "Generic_1", "sset", "Master", "0dB", "unmute"],
                    ["amixer", "-q", "-c", "Generic_1", "sset", "Headphone", "0dB", "unmute"],
                    ["amixer", "-q", "-c", "Generic_1", "sset", "PCM", "0dB"],
                    ["amixer", "-q", "-c", "Generic_1", "sset", "Speaker", "mute"]]


# --- the supervisor loop ---------------------------------------------------------------------------------

def test_a_published_stream_plays_in_vlc_and_reads_playing(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "published").write_text("")
    r = rig.run(2.0)
    assert r.returncode == 0, r.stdout + r.stderr
    argv = rig.vlc_argv()
    assert len(argv) == 1 and "--alsa-audio-device=plughw:CARD=Generic_1,DEV=0" in argv[0], argv
    assert any(ln.startswith("playing rtmp://127.0.0.1:1234/live/stream-pp -> plughw:CARD=Generic_1,DEV=0")
               for ln in r.stdout.splitlines()), r.stdout
    assert ["-q", "-c", "Generic_1", "sset", "Master", "0dB", "unmute"] in [a.split() for a in rig.amixer()]


def test_nothing_published_waits_and_retries_with_pl_play(tmp_path):
    rig = Rig(tmp_path)
    r = rig.run(2.0)
    assert r.returncode == 0, r.stderr
    assert rig.cmds().count("pl_play") >= 3, rig.log()
    assert len([ln for ln in r.stdout.splitlines() if ln.startswith("waiting:")]) == 1, r.stdout


def test_a_stream_that_starts_later_is_picked_up(tmp_path):
    rig = Rig(tmp_path)
    p = subprocess.Popen(rig.argv(4.0), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=rig.env())
    time.sleep(1.2)
    (rig.dir / "published").write_text("")
    out, err = p.communicate(timeout=30)
    assert p.returncode == 0, err
    assert any(ln.startswith("playing ") for ln in out.splitlines()), out


def test_a_paused_player_is_stopped_so_it_never_holds_a_subscriber(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "published").write_text("")
    (rig.dir / "force-state").write_text("paused\n")
    r = rig.run(2.5)
    assert r.returncode == 0, r.stderr
    assert "pl_stop" in rig.cmds(), rig.log()
    assert any("paused" in ln and "stopped" in ln for ln in r.stdout.splitlines()), r.stdout


def test_a_vlc_that_exits_is_started_again(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "exit-at-start").write_text("")
    (rig.dir / "published").write_text("")
    r = rig.run(3.0)
    assert r.returncode == 0, r.stderr
    assert len(rig.vlc_argv()) == 2, rig.log()
    assert any("vlc exited" in ln for ln in r.stdout.splitlines()), r.stdout


def test_a_card_held_by_pipewire_never_starts_vlc(tmp_path):
    rig = Rig(tmp_path, owner=(4242, "pipewire"))
    r = rig.run(1.5)
    assert r.returncode == 0, r.stderr
    assert rig.vlc_argv() == [], rig.log()
    assert rig.st()["state"] == "busy" and "pipewire" in rig.st()["reason"], rig.st()


def test_no_headphone_pcm_never_starts_vlc(tmp_path):
    rig = Rig(tmp_path, analog=False)
    r = rig.run(1.5)
    assert r.returncode == 0, r.stderr
    assert rig.vlc_argv() == []
    assert rig.st()["state"] == "no-card"


def test_a_missing_mixer_control_is_named_once_and_vlc_still_plays(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "exit-at-start").write_text("")
    (rig.dir / "published").write_text("")
    r = rig.run(3.0, FAKE_AMIXER_MISSING="Headphone")
    assert r.returncode == 0, r.stderr
    named = [ln for ln in r.stdout.splitlines() if "Headphone" in ln]
    assert len(named) == 1, r.stdout
    assert any(ln.startswith("playing ") for ln in r.stdout.splitlines()), r.stdout


def test_sigterm_stops_vlc(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "published").write_text("")
    p = subprocess.Popen(rig.argv(60.0), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=rig.env())
    try:
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and not any(ln.startswith("pid ") for ln in rig.log()):
            time.sleep(0.05)
        time.sleep(0.5)
        p.send_signal(15)
        out, _err = p.communicate(timeout=15)
    finally:
        if p.poll() is None:
            p.kill()
            p.communicate(timeout=5)
    assert p.returncode == 0
    assert "sigterm" in rig.log(), rig.log()


def test_the_state_file_names_the_card_while_playing(tmp_path):
    rig = Rig(tmp_path)
    (rig.dir / "published").write_text("")
    rig.run(1.5)
    st = rig.st()
    assert st["device"] == "plughw:CARD=Generic_1,DEV=0" and st["card"] == 1 and st["pcm_device"] == 0, st


# --- verify: --check-state with VLC on the card ----------------------------------------------------------------

def _state(tmp_path, **kw):
    d = {"version": 1, "updated_epoch_s": time.time(), "state": "playing",
         "url": "rtmp://127.0.0.1:1234/live/stream-pp", "device": "plughw:CARD=Generic_1,DEV=0",
         "card": 1, "pcm_device": 0, "connects": 1, "reason": ""}
    d.update(kw)
    p = tmp_path / "state.json"
    p.write_text(json.dumps(d))
    return p


def test_check_state_ok_with_vlc_on_the_card(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path, owner=(555, "vlc"))
    ok, line = m.check_state(str(_state(tmp_path)), time.time(), str(root), str(proc))
    assert ok and "vlc" in line, line


def test_check_state_fails_pipewire_or_aplay_on_the_card(tmp_path):
    m = _mod()
    for comm in ("pipewire", "aplay"):
        sub = tmp_path / comm
        sub.mkdir()
        root, proc = asound(sub, owner=(556, comm))
        ok, line = m.check_state(str(_state(sub)), time.time(), str(root), str(proc))
        assert not ok and comm in line, line


def test_check_state_fails_a_failing_vlc(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path)
    ok, line = m.check_state(str(_state(tmp_path, state="vlc-failing", reason="exits at once")), time.time(),
                             str(root), str(proc))
    assert not ok and "vlc-failing" in line


def test_the_packages_are_vlc_and_alsa_utils():
    text = (REPO / "scripts" / "lib" / "strih-stream-monitor.sh").read_text()
    assert "STRIH_STREAM_MONITOR_PACKAGES=(vlc alsa-utils)" in text
