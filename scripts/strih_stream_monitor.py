#!/usr/bin/env python3
"""strih-stream-monitor: the stream box's headphones play the REAL outgoing stream, in VLC (issues 1436, 1456).

WHY (owner 15.10.2026, issue 1436): "a spomen si ako to mame riesene v snv ze sa pocuva cez vlc ktore ma
nacitany stream rtmp z obs alebo z restreamera" + "pozadujem aby sa budovalo jednotne riesenie pp/snv".
stream-snv's operators hear the ENCODED stream that really leaves the box (scripts/avsync-vlc-monitor.ps1,
VLC on the restreamer's local RTMP). And (owner 21.10.2026, issue 1456): "tak chcem to mat rovnako cize to
tiez z implementuj aby na stream-pp tiez bolo vlc, a neviem si v stream-pp nastavovat hlasitost vystuput
sluchatkoveho a je to potichu" -- the issue-1436 ffmpeg -> aplay relay had no volume control at all, and
the card's ALSA Master sat at the -23.25 dB PipeWire had left. So the player is VLC; its volume slider is
the operator's control (VLC saves it), and this supervisor runs it. Issue 1437: never through PipeWire --
VLC writes the headphone card directly through ALSA.

WHAT, one supervised --user unit (strih-stream-monitor.service, Restart=always):
  * resolves the headphone card by its PLAYBACK PCM NAME in /proc/asound (default ` Analog` at the end:
    stream-pp's `ALC257 Analog`), never a card index -- both stream-pp cards are called `HD-Audio
    Generic` and the id `Generic_1` is enumeration order. Addressed as plughw:CARD=<id>,DEV=<n>, re-read
    before every VLC start;
  * when another process holds that PCM (PipeWire, a second player) VLC is not started; it says who;
  * before each VLC start: the card's Master / Headphone / PCM to 0 dB unmuted (a unity baseline, the
    level is VLC's slider) and Speaker muted (PipeWire's ACP leaves the laptop speaker route on, Auto-Mute
    disabled: the stream must sound in the headphones only). A control the card does not have is named
    once;
  * starts VLC on the restreamer's RTMP (default rtmp://127.0.0.1:1234/live/<short hostname>, the key
    setup-strih writes into the stream OBS profile): ALSA straight to the jack, video in a small window
    (--zoom 0.25), 1 s network cache (SNV's), no error dialogs, its http status interface on 127.0.0.1
    with a per-run random password;
  * reads that status every POLL_S: `stopped` (nothing published, a stream end) -> `pl_play` every
    RETRY_S; `paused` for more than PAUSE_MAX_S -> `pl_stop` (restreamer's per-subscriber channel is
    unbounded: a paused player must never hold a subscriber); a VLC that exits (the operator closed it)
    is started again after RETRY_S, one that exits within STARTUP_S of its start is `vlc-failing`; an
    http interface that never answers within STARTUP_S is `vlc-failing` and VLC is restarted.

Logs: `watching ...` at start, `playing <url> -> <device> (start N)`, one line per idle-state change
(`waiting` / `busy` / `no-card` / `vlc-failing`), `paused for N s: stopped ...`, `vlc exited ...`.
State file (atomic, %t/strih-stream-monitor.json): verify-strih's `--check-state` reads it and the card
owner: a PipeWire (or any non-VLC) stream on the headphone card FAILs. Std-only.
"""
import argparse
import base64
import json
import os
import re
import secrets
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

DEFAULT_SERVER = "rtmp://127.0.0.1:1234/live"
# The headphone jack's playback PCM: its name ENDS in ` Analog` (stream-pp: `ALC257 Analog`), but a Realtek
# codec's second analog PCM (`ALC... Alt Analog`) is never the jack.
DEFAULT_PCM_MATCH = r"(?<!\bAlt) Analog$"
# The mixer: these to 0 dB (unmuted where they have a switch), Speaker muted (issue 1456).
UNITY_CONTROLS = (("Master", True), ("Headphone", True), ("PCM", False))
DEFAULT_SPEAKER_CONTROL = "Speaker"
DEFAULT_HTTP_PORT = 8090  # stream-snv's VLC http port (avsync-vlc-monitor.ps1)
NETWORK_CACHING_MS = 1000
VIDEO_ZOOM = "0.25"
RETRY_S = 3.0
POLL_S = 1.0
PAUSE_MAX_S = 10.0
STARTUP_S = 15.0
HEARTBEAT_S = 5.0
STATE_MAX_AGE_S = 30.0
STATE_VERSION = 2

# FAULT states verify FAILs; the rest (starting / waiting / playing / paused) are healthy
FAULT_STATES = ("no-card", "busy", "vlc-failing")
# the only process allowed to play to the headphone PCM
OWNER_OK = ("vlc",)
ASOUND_ROOT = os.environ.get("STRIH_STREAM_MONITOR_ASOUND", "/proc/asound")
PROC_ROOT = os.environ.get("STRIH_STREAM_MONITOR_PROC", "/proc")


def _read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def list_playback_pcms(asound_root=ASOUND_ROOT):
    """Every playback PCM in /proc/asound: [{card, card_id, device, name}], sorted by card + device."""
    out = []
    try:
        cards = os.listdir(asound_root)
    except OSError:
        return out  # no /proc/asound = no sound card: resolve_pcm names it
    for c in cards:
        m = re.fullmatch(r"card(\d+)", c)
        if not m:
            continue
        cdir = os.path.join(asound_root, c)
        try:
            card_id = _read(os.path.join(cdir, "id")).strip()
            entries = os.listdir(cdir)
        except OSError:
            continue  # a card that vanished while it was read is no candidate
        for e in entries:
            pm = re.fullmatch(r"pcm(\d+)p", e)
            if not pm:
                continue
            try:
                info = _read(os.path.join(cdir, e, "info"))
            except OSError:
                continue
            nm = re.search(r"^name: (.*)$", info, re.M)
            out.append({"card": int(m.group(1)), "card_id": card_id, "device": int(pm.group(1)),
                        "name": nm.group(1).strip() if nm else ""})
    return sorted(out, key=lambda p: (p["card"], p["device"]))


def resolve_pcm(pcms, pattern):
    """(pcm, None) when exactly one playback PCM name matches PATTERN, else (None, the reason)."""
    hits = [p for p in pcms if re.search(pattern, p["name"])]
    if len(hits) == 1:
        return hits[0], None
    names = ", ".join("%s:%d %s" % (p["card_id"], p["device"], p["name"]) for p in pcms) or "none"
    if not hits:
        return None, "no playback PCM name matches %r (playback PCMs: %s)" % (pattern, names)
    return None, "%d playback PCMs match %r (%s) -- ambiguous" % (len(hits), pattern, names)


def alsa_device(pcm):
    """The ALSA device string of a resolved PCM: by card ID, never an index."""
    return "plughw:CARD=%s,DEV=%d" % (pcm["card_id"], pcm["device"])


def pcm_owner(asound_root, proc_root, card, device):
    """Who holds playback PCM card/device: {state: closed | open | unreadable, pid, comm}."""
    try:
        status = _read(os.path.join(asound_root, "card%d" % card, "pcm%dp" % device, "sub0", "status"))
    except OSError:
        return {"state": "unreadable", "pid": None, "comm": None}
    if status.strip() == "closed":
        return {"state": "closed", "pid": None, "comm": None}
    m = re.search(r"^owner_pid\s*:\s*(\d+)", status, re.M)
    pid = int(m.group(1)) if m else None
    comm = None
    if pid is not None:
        try:
            comm = _read(os.path.join(proc_root, str(pid), "comm")).strip()
        except OSError:
            comm = None  # the owner exited between the two reads: named as "?"
    return {"state": "open", "pid": pid, "comm": comm}



def default_url():
    """The local restreamer + this box's short hostname = the stream key setup-strih writes (issue 1436)."""
    return "%s/%s" % (DEFAULT_SERVER, socket.gethostname().split(".")[0])



def vlc_argv(vlc, device, url, http_port, password):
    """VLC on URL, ALSA straight to DEVICE, its http status interface on loopback (issue 1456). No
    --repeat: the supervisor retries with pl_play, never a tight VLC reconnect loop."""
    return [vlc, "--no-qt-error-dialogs", "--no-qt-privacy-ask", "--zoom=%s" % VIDEO_ZOOM,
            "--network-caching=%d" % NETWORK_CACHING_MS, "--aout=alsa", "--alsa-audio-device=%s" % device,
            "--extraintf=http", "--http-host=127.0.0.1", "--http-port=%d" % http_port,
            "--http-password=%s" % password, url]


def mixer_unity_cmds(amixer, card_id, speaker=DEFAULT_SPEAKER_CONTROL):
    """The card's level controls to 0 dB (unmuted where they switch), then the speaker muted."""
    cmds = []
    for name, has_switch in UNITY_CONTROLS:
        c = [amixer, "-q", "-c", card_id, "sset", name, "0dB"]
        cmds.append(c + ["unmute"] if has_switch else c)
    if speaker:
        cmds.append([amixer, "-q", "-c", card_id, "sset", speaker, "mute"])
    return cmds


class VlcHttp:
    """VLC's http status interface on 127.0.0.1 (Basic auth, an empty user)."""

    def __init__(self, port, password, timeout=2.0):
        self.base = "http://127.0.0.1:%d/requests/status.json" % port
        self.auth = "Basic " + base64.b64encode((":" + password).encode()).decode()
        self.timeout = timeout

    def call(self, command=None):
        """The player state (playing / paused / stopped / ...), None when the interface does not answer."""
        url = self.base + ("?command=%s" % command if command else "")
        req = urllib.request.Request(url, headers={"Authorization": self.auth})
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                return str(json.loads(r.read().decode("utf-8", "replace")).get("state") or "")
        except (urllib.error.URLError, OSError, ValueError):
            return None


class Monitor:
    def __init__(self, url, state_file, pcm_match=DEFAULT_PCM_MATCH, vlc="vlc", amixer="amixer",
                 asound_root=ASOUND_ROOT, proc_root=PROC_ROOT, http_port=DEFAULT_HTTP_PORT, retry_s=RETRY_S,
                 poll_s=POLL_S, pause_max_s=PAUSE_MAX_S, startup_s=STARTUP_S,
                 speaker_control=DEFAULT_SPEAKER_CONTROL, log=None):
        self.url = url
        self.state_file = state_file
        self.pcm_match = pcm_match
        self.vlc = vlc
        self.amixer = amixer
        self.asound_root = asound_root
        self.proc_root = proc_root
        self.http_port = http_port
        self.retry_s = retry_s
        self.poll_s = poll_s
        self.pause_max_s = pause_max_s
        self.startup_s = startup_s
        self.speaker_control = speaker_control
        self.log = log or (lambda s: print(s, flush=True))
        self.stopping = False
        self.proc = None
        self.http = None
        self.started_at = 0.0
        self.answered = False
        self.last_play_cmd = 0.0
        self.paused_since = None
        self.next_start = 0.0
        self._mixer_logged = set()
        self._logged = None
        self._last_write = 0.0
        self.st = {"version": STATE_VERSION, "state": "starting", "url": url, "device": None, "card": None,
                   "pcm_device": None, "starts": 0, "reason": "", "since_epoch_s": time.time(),
                   "updated_epoch_s": time.time()}

    # state ------------------------------------------------------------------------------------------------
    def _write_state(self):
        self.st["updated_epoch_s"] = time.time()
        self._last_write = time.monotonic()
        if not self.state_file:
            return
        d = os.path.dirname(self.state_file)
        tmp = "%s.tmp-%d" % (self.state_file, os.getpid())
        try:
            if d:
                os.makedirs(d, exist_ok=True)
            with open(tmp, "w") as fh:
                json.dump(self.st, fh)
            os.replace(tmp, self.state_file)
        except OSError as e:
            self.log("cannot write the state file %s: %s" % (self.state_file, e))

    def set_state(self, state, reason, line=None, **info):
        """A state change: logged once per (state, reason) -- LINE overrides the logged text."""
        if self.st["state"] != state:
            self.st["since_epoch_s"] = time.time()
        self.st.update(state=state, reason=reason, **info)
        key = (state, reason)
        if key != self._logged:
            self._logged = key
            self.log(line or "%s: %s" % (state, reason))
        self._write_state()

    # VLC -------------------------------------------------------------------------------------------------
    def _mixer(self, card_id):
        env = dict(os.environ, LC_ALL="C")
        for cmd in mixer_unity_cmds(self.amixer, card_id, self.speaker_control):
            try:
                r = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=10)
                err = (r.stderr or r.stdout).strip() if r.returncode else ""
            except (OSError, subprocess.TimeoutExpired) as e:
                err = str(e)
            name = cmd[5]
            if err and name not in self._mixer_logged:
                self._mixer_logged.add(name)
                self.log("mixer: %s of %s not set (%s) -- the stream plays anyway" % (name, card_id, err))

    def _start_vlc(self):
        pcm, why = resolve_pcm(list_playback_pcms(self.asound_root), self.pcm_match)
        if pcm is None:
            self.set_state("no-card", why, device=None, card=None, pcm_device=None)
            return False
        device = alsa_device(pcm)
        self.st.update(device=device, card=pcm["card"], pcm_device=pcm["device"])
        owner = pcm_owner(self.asound_root, self.proc_root, pcm["card"], pcm["device"])
        if owner["state"] == "open":
            self.set_state("busy", "the headphone PCM %s is held by %s (pid %s) -- VLC not started, retrying every %g s"
                           % (device, owner["comm"] or "?", owner["pid"], self.retry_s))
            return False
        self._mixer(pcm["card_id"])
        password = secrets.token_hex(16)
        try:
            self.proc = subprocess.Popen(vlc_argv(self.vlc, device, self.url, self.http_port, password),
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL)
        except OSError as e:
            self.set_state("vlc-failing", "cannot start %s: %s" % (self.vlc, e))
            return False
        self.http = VlcHttp(self.http_port, password)
        self.started_at = time.monotonic()
        self.answered = False
        self.paused_since = None
        self.last_play_cmd = time.monotonic()
        self.st["starts"] += 1
        self._write_state()
        return True

    def _stop_vlc(self, grace=3.0):
        p, self.proc = self.proc, None
        if p is None or p.poll() is not None:
            return
        p.terminate()
        try:
            p.wait(grace)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait(5)

    def _vlc_exited(self):
        rc = self.proc.returncode
        ran = time.monotonic() - self.started_at
        self.proc = None
        self.next_start = time.monotonic() + self.retry_s
        if ran < self.startup_s and not self.answered:
            self.set_state("vlc-failing", "vlc exited after %.1f s (rc %s) before it answered -- started again in %g s"
                           % (ran, rc, self.retry_s))
        else:
            self._logged = None
            self.log("vlc exited after %.0f s (rc %s) -- started again in %g s" % (ran, rc, self.retry_s))

    def tick(self):
        now = time.monotonic()
        if self.proc is not None and self.proc.poll() is not None:
            self._vlc_exited()
        if self.proc is None:
            if now >= self.next_start and not self._start_vlc():
                self.next_start = now + self.retry_s
            return
        st = self.http.call()
        if st is None:
            if not self.answered and now - self.started_at > self.startup_s:
                self.set_state("vlc-failing", "the VLC http interface did not answer within %g s -- restarting VLC"
                               % self.startup_s)
                self._stop_vlc()
                self.next_start = now + self.retry_s
            return
        self.answered = True
        if st == "playing":
            self.paused_since = None
            self.set_state("playing", "", line="playing %s -> %s (start %d)" % (self.url, self.st["device"],
                                                                                 self.st["starts"]))
        elif st == "paused":
            if self.paused_since is None:
                self.paused_since = now
            if now - self.paused_since > self.pause_max_s:
                self.http.call("pl_stop")
                self.log("paused for %.0f s: stopped (a paused player must not hold a restreamer subscriber)"
                         % (now - self.paused_since))
                self.paused_since = None
                self.last_play_cmd = now
            else:
                self.set_state("paused", "paused in VLC")
        else:
            self.paused_since = None
            self.set_state("waiting", "nothing playing on %s -- VLC retries every %g s" % (self.url, self.retry_s))
            if now - self.last_play_cmd >= self.retry_s:
                self.http.call("pl_play")
                self.last_play_cmd = now

    def run(self, run_s=None):
        t_end = time.monotonic() + run_s if run_s is not None else None
        while not self.stopping and (t_end is None or time.monotonic() < t_end):
            self.tick()
            if time.monotonic() - self._last_write >= HEARTBEAT_S:
                self._write_state()
            time.sleep(self.poll_s)
        self._stop_vlc()
        self._write_state()


# --- verify: --check-state -------------------------------------------------------------------------------

def check_state(path, now, asound_root=ASOUND_ROOT, proc_root=PROC_ROOT, max_age=STATE_MAX_AGE_S):
    """verify-strih's (stream-monitor-pass) row: (ok, one line). OK = a pass within MAX_AGE seconds, the
    player starting, waiting, playing or paused, and the headphone PCM free or held by VLC -- never
    PipeWire."""
    try:
        st = json.loads(_read(path))
    except (OSError, ValueError) as e:
        return False, "no state file %s (%s) -- the monitor never ran a pass" % (path, e)
    age = now - float(st.get("updated_epoch_s") or 0)
    if age > max_age:
        return False, "last pass %.0f s ago (> %.0f s) -- the monitor is not running" % (age, max_age)
    state = st.get("state")
    device = st.get("device") or "?"
    if state in FAULT_STATES:
        return False, "%s: %s" % (state, st.get("reason", ""))
    if state not in ("starting", "waiting", "playing", "paused"):
        return False, "unknown state %r" % state
    card, dev = st.get("card"), st.get("pcm_device")
    if not isinstance(card, int) or not isinstance(dev, int):
        return False, "state %s names no headphone PCM -- the monitor resolved no card" % state
    owner = pcm_owner(asound_root, proc_root, card, dev)
    if owner["state"] == "unreadable":
        return False, "the headphone PCM card%d/pcm%dp status is unreadable" % (card, dev)
    if owner["state"] == "open" and owner["comm"] not in OWNER_OK:
        return False, ("%s (pid %s) holds the headphone PCM %s -- nothing but the monitor's VLC may play to it "
                       "(PipeWire must not)" % (owner["comm"] or "?", owner["pid"], device))
    holder = "free" if owner["state"] == "closed" else "held by %s pid %s" % (owner["comm"], owner["pid"])
    return True, "%s %s -> %s (start %s), headphone PCM %s" % (state, st.get("url"), device, st.get("starts"),
                                                               holder)


# --- main -------------------------------------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--url", default=None, help="the RTMP stream to play (default %s/<short hostname>)" % DEFAULT_SERVER)
    ap.add_argument("--state-file", default=None)
    ap.add_argument("--pcm-match", default=DEFAULT_PCM_MATCH, help="regex on the playback PCM name")
    ap.add_argument("--vlc", default="vlc")
    ap.add_argument("--amixer", default="amixer")
    ap.add_argument("--speaker-control", default=DEFAULT_SPEAKER_CONTROL,
                    help="the mixer switch muted before VLC starts ('' = leave it alone)")
    ap.add_argument("--asound-root", default=ASOUND_ROOT)
    ap.add_argument("--proc-root", default=PROC_ROOT)
    ap.add_argument("--http-port", type=int, default=DEFAULT_HTTP_PORT)
    ap.add_argument("--retry-s", type=float, default=RETRY_S)
    ap.add_argument("--poll-s", type=float, default=POLL_S)
    ap.add_argument("--pause-max-s", type=float, default=PAUSE_MAX_S)
    ap.add_argument("--startup-s", type=float, default=STARTUP_S)
    ap.add_argument("--run-s", type=float, default=None, help="stop after N seconds (tests)")
    ap.add_argument("--check-state", metavar="FILE", default=None,
                    help="read-only: grade the state file + the headphone PCM owner (verify-strih), rc 0/1")
    a = ap.parse_args(argv)
    if a.check_state:
        ok, line = check_state(a.check_state, time.time(), a.asound_root, a.proc_root)
        print(line)
        return 0 if ok else 1
    mon = Monitor(a.url or default_url(), a.state_file, a.pcm_match, a.vlc, a.amixer, a.asound_root, a.proc_root,
                  a.http_port, a.retry_s, a.poll_s, a.pause_max_s, a.startup_s, a.speaker_control)

    def _stop_signal(_sig, _frm):
        mon.stopping = True

    signal.signal(signal.SIGTERM, _stop_signal)
    signal.signal(signal.SIGINT, _stop_signal)
    mon.log("watching %s -> the headphone PCM matching %r in VLC (http status on 127.0.0.1:%d)"
            % (mon.url, a.pcm_match, a.http_port))
    mon.run(a.run_s)
    return 0


if __name__ == "__main__":
    sys.exit(main())
