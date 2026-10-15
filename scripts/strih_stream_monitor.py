#!/usr/bin/env python3
"""strih-stream-monitor (issue 1436): the stream box's headphones play the REAL outgoing stream.

WHY (owner 15.10.2026): "a spomen si ako to mame riesene v snv ze sa pocuva cez vlc ktore ma nacitany
stream rtmp z obs alebo z restreamera" + "pozadujem aby sa budovalo jednotne riesenie pp/snv". stream-pp's
jack played ONE OBS source (`Zvuk z mixu` monitored to the default device) -- also with no stream
running, also when something else went out. stream-snv's operators hear the ENCODED stream that really
leaves the box: scripts/avsync-vlc-monitor.ps1 plays the restreamer's local RTMP. This is that design
on every Linux stream-role box (main design: issue 1436 comment 6189806622).

And (owner 15.10.2026, issue 1437): "pozadujem hlbkove preverenie nutnosti vyuzivat pipewire" -- the
player writes the headphone card DIRECTLY through ALSA, never through PipeWire.

WHAT, one supervised --user unit (strih-stream-monitor.service, Restart=always):
  * resolves the headphone card by its PLAYBACK PCM NAME in /proc/asound (default ` Analog` at the end:
    stream-pp's `ALC257 Analog`), never a card index -- both stream-pp cards are called `HD-Audio
    Generic` and the id `Generic_1` is enumeration order. Addressed as plughw:CARD=<id>,DEV=<n>, re-read
    before every connect;
  * when another process holds that PCM (PipeWire, a second player) it does not connect at all and
    says who holds it;
  * runs `ffmpeg` on the restreamer's RTMP (default rtmp://127.0.0.1:1234/live/<short hostname>, the
    key setup-strih writes into the stream OBS profile): the audio only, s16le 48 kHz stereo on stdout.
    Nothing published = ffmpeg fails, the player stays silent and retries every RETRY_S;
  * on the first audio it mutes the card's `Speaker` switch by control name (PipeWire leaves the laptop
    speaker route to itself, Auto-Mute disabled: the stream must sound in the headphones only), then
    starts `aplay -N` on the card (the intercom hub's AplaySink pattern: a busy PCM fails the open at once
    instead of hanging) and relays the audio through a pipe, in whole frames;
  * NEVER holds a paused subscriber (restreamer's per-subscriber channel is unbounded): the relay always
    reads ffmpeg, writes the aplay pipe NON-blocking, and drops (and counts) a chunk that would push the
    pipe past the latency ceiling (FIONREAD, the hub's pipe-fill measure, default 200 ms) or that the
    full pipe refuses. That also bounds the latency when the card's clock runs slower than the sender's;
    a faster card shows as an aplay underrun.
  * the stream ends (ffmpeg EOF) or stalls (no audio for STALL_S) = ffmpeg is stopped first (the
    subscriber goes at once), aplay plays out its buffer, and it reconnects after RETRY_S. An aplay that
    fails (not busy) is the fault state `aplay-failing`, retried with a doubling pause (up to 20 x);
    a stream with no audio track is `no-audio`. Children run in the C locale (the busy / underrun text
    the player reads is English).

Logs: one line per connect (`playing <url> -> <device>`) and per disconnect (`stream ended after ...`),
and one line when the idle state changes (`waiting` / `busy` / `no-card` / `no-audio` / `aplay-failing`); a
quiet retry logs nothing.
State file (atomic, %t/strih-stream-monitor.json): verify-strih's `--check-state` reads it and the card
owner: a PipeWire stream on the headphone card FAILs. Std-only.
"""
import argparse
import fcntl
import json
import os
import re
import select
import signal
import socket
import struct
import subprocess
import sys
import termios
import threading
import time

DEFAULT_SERVER = "rtmp://127.0.0.1:1234/live"
# A playback PCM name ending in " Analog": the notebook codec's analog output (stream-pp: `ALC257
# Analog`, read live 15.10.2026). HDMI PCMs are named `HDMI <n>`, a USB interface `USB Audio`; a Realtek
# codec's second analog PCM (`ALC257 Alt Analog`) is never the jack.
DEFAULT_PCM_MATCH = r"(?<!\bAlt) Analog$"
# The mixer switch muted before the stream sounds (PipeWire's ACP keeps Auto-Mute disabled and routes the
# speaker itself; stream-pp 15.10.2026: Auto-Mute Mode Disabled, Speaker off, Headphone on).
DEFAULT_SPEAKER_CONTROL = "Speaker"
RATE = 48000
CHANNELS = 2
FRAME_BYTES = 4  # s16le stereo
CHUNK_BYTES = 3840  # 20 ms
DEFAULT_CEILING_MS = 200
APLAY_BUFFER_US = 100000
RETRY_S = 3.0
BACKOFF_MAX = 20  # an aplay that keeps failing is retried after up to 20 x RETRY_S
CONNECT_TIMEOUT_S = 15.0
STALL_S = 5.0
HEARTBEAT_S = 5.0
DRAIN_S = 3.0
STATE_MAX_AGE_S = 30.0
STATE_VERSION = 1
# The states verify FAILs (anything else it does not know FAILs too).
FAULT_STATES = ("no-card", "busy", "aplay-failing", "no-audio")
# The process that may hold the headphone PCM: our own aplay. Anything else (pipewire, wireplumber, a
# second player) is a fault verify names.
OWNER_OK = ("aplay",)
ASOUND_ROOT = os.environ.get("STRIH_STREAM_MONITOR_ASOUND", "/proc/asound")
PROC_ROOT = os.environ.get("STRIH_STREAM_MONITOR_PROC", "/proc")


# --- pure parts -------------------------------------------------------------------------------------------

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


def relay_action(pending, n, ceiling):
    """`write` when the chunk of N bytes keeps the aplay pipe at or under CEILING bytes, else `drop`. An
    unreadable pipe level (None) writes."""
    if pending is None:
        return "write"
    return "write" if pending + n <= ceiling else "drop"


def ceiling_bytes(ms):
    """The pipe ceiling in bytes for MS of 48 kHz stereo s16le, a whole frame."""
    return int(RATE * ms / 1000) * FRAME_BYTES


def ffmpeg_argv(ffmpeg, url):
    """Read the RTMP stream, decode only its first audio track, write s16le 48 kHz stereo to stdout."""
    return [ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error", "-rw_timeout", "5000000",
            "-i", url, "-map", "0:a:0", "-vn", "-sn", "-dn", "-ac", str(CHANNELS), "-ar", str(RATE),
            "-f", "s16le", "pipe:1"]


def aplay_argv(aplay, device, buffer_us=APLAY_BUFFER_US):
    """Play raw s16le 48 kHz stereo from stdin straight to the ALSA DEVICE (-N: a busy PCM fails the open)."""
    return [aplay, "-N", "-q", "-D", device, "-t", "raw", "-f", "S16_LE", "-r", str(RATE), "-c", str(CHANNELS),
            "--buffer-time=%d" % buffer_us, "-"]


def default_url():
    """The local restreamer + this box's short hostname = the stream key setup-strih writes (issue 1436)."""
    return "%s/%s" % (DEFAULT_SERVER, socket.gethostname().split(".")[0])


def fionread(fd):
    """Bytes waiting in the pipe behind FD (Linux FIONREAD works on either end), None when unreadable."""
    try:
        return struct.unpack("i", fcntl.ioctl(fd, termios.FIONREAD, b"\0\0\0\0"))[0]
    except OSError:
        return None  # relay_action writes on an unknown level: never a silent drop


# --- the loop -------------------------------------------------------------------------------------------------

class StderrTail:
    """Reads a child's stderr on a daemon thread (a full stderr pipe must never block the child): keeps the
    last non-empty line and counts lines containing COUNT."""

    def __init__(self, stream, count=None):
        self.last = ""
        self.counted = 0
        self._count = count
        self._t = threading.Thread(target=self._run, args=(stream,), daemon=True)
        self._t.start()

    def _run(self, stream):
        for raw in iter(stream.readline, b""):
            line = raw.decode("utf-8", "replace").strip()
            if line:
                self.last = line
                if self._count and self._count in line:
                    self.counted += 1
        stream.close()

    def join(self, timeout=1.0):
        self._t.join(timeout)


def _stop(proc, grace=2.0):
    """Terminate PROC (if still running) and reap it; kill it after GRACE seconds."""
    if proc is None or proc.poll() is not None:
        return
    try:
        proc.terminate()
        proc.wait(grace)
    except subprocess.TimeoutExpired:
        print("pid %d ignored SIGTERM for %g s -- killing it" % (proc.pid, grace), file=sys.stderr, flush=True)
        proc.kill()
        try:
            proc.wait(grace)
        except subprocess.TimeoutExpired:
            print("pid %d did not exit %g s after SIGKILL (D state?)" % (proc.pid, grace), file=sys.stderr, flush=True)
    except OSError as e:
        print("cannot stop pid %d: %s" % (proc.pid, e), file=sys.stderr, flush=True)


class Monitor:
    def __init__(self, url, state_file, pcm_match=DEFAULT_PCM_MATCH, ffmpeg="ffmpeg", aplay="aplay",
                 asound_root=ASOUND_ROOT, proc_root=PROC_ROOT, retry_s=RETRY_S, connect_timeout_s=CONNECT_TIMEOUT_S,
                 stall_s=STALL_S, ceiling=ceiling_bytes(DEFAULT_CEILING_MS), log=None, amixer="amixer",
                 speaker_control=DEFAULT_SPEAKER_CONTROL):
        self.url = url
        self.state_file = state_file
        self.pcm_match = pcm_match
        self.ffmpeg = ffmpeg
        self.aplay = aplay
        self.amixer = amixer
        self.speaker_control = speaker_control
        # the C locale for every child: the busy / underrun / no-stream text read below is English
        self.child_env = dict(os.environ, LC_ALL="C")
        self.fail_streak = 0
        self._speaker_logged = None
        self.asound_root = asound_root
        self.proc_root = proc_root
        self.retry_s = retry_s
        self.connect_timeout_s = connect_timeout_s
        self.stall_s = stall_s
        self.ceiling = ceiling
        self.log = log or (lambda s: print(s, flush=True))
        self.stopping = False
        self.st = {"version": STATE_VERSION, "state": "starting", "url": url, "device": None, "card": None,
                   "pcm_device": None, "connects": 0, "dropped_bytes": 0, "reason": "", "owner": None,
                   "since_epoch_s": time.time(), "updated_epoch_s": time.time()}
        self._last_write = 0.0
        self._logged = None

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

    def heartbeat(self):
        if time.monotonic() - self._last_write >= HEARTBEAT_S:
            self._write_state()

    def set_idle(self, state, reason, **info):
        """An idle state (waiting / busy / no-card): logged once per (state, reason) change."""
        if self.st["state"] != state:
            self.st["since_epoch_s"] = time.time()
        self.st.update(state=state, reason=reason, **info)
        key = (state, reason)
        if key != self._logged:
            self._logged = key
            self.log("%s: %s" % (state, reason))
        self._write_state()

    # one connect ----------------------------------------------------------------------------------------
    def cycle(self):
        pcm, why = resolve_pcm(list_playback_pcms(self.asound_root), self.pcm_match)
        if pcm is None:
            self.set_idle("no-card", why, device=None, card=None, pcm_device=None, owner=None)
            return
        device = alsa_device(pcm)
        self.st.update(device=device, card=pcm["card"], pcm_device=pcm["device"])
        owner = pcm_owner(self.asound_root, self.proc_root, pcm["card"], pcm["device"])
        if owner["state"] == "open":
            self.set_idle("busy", "the headphone PCM %s is held by %s (pid %s) -- not connecting, retrying every %g s"
                          % (device, owner["comm"] or "?", owner["pid"], self.retry_s), owner=owner["comm"])
            return
        ff = subprocess.Popen(ffmpeg_argv(self.ffmpeg, self.url), stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, env=self.child_env)
        ff_err = StderrTail(ff.stderr)
        ap = None
        try:
            first = self._read(ff, self.connect_timeout_s)
            if not first:
                _stop(ff)
                ff_err.join()
                if self.stopping:
                    return
                if "matches no streams" in ff_err.last:
                    self.set_idle("no-audio", "the stream on %s carries no audio track (ffmpeg: %s)"
                                  % (self.url, ff_err.last), owner=None)
                else:
                    self.set_idle("waiting", "nothing published on %s (ffmpeg: %s) -- silent, retrying every %g s"
                                  % (self.url, ff_err.last or "no audio within %g s" % self.connect_timeout_s,
                                     self.retry_s), owner=None)
                return
            self._mute_speaker(pcm["card_id"])
            ap = subprocess.Popen(aplay_argv(self.aplay, device), stdin=subprocess.PIPE,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=self.child_env)
            ap_err = StderrTail(ap.stderr, count="underrun")
            self._play(ff, ap, ap_err, ff_err, device, first)
        finally:
            _stop(ff)
            _stop(ap)

    def _mute_speaker(self, card_id):
        """Mute the card's speaker switch by control name before the stream sounds; logged once per outcome."""
        if not self.speaker_control:
            return
        argv = [self.amixer, "-q", "-c", card_id, "sset", self.speaker_control, "mute"]
        try:
            r = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=5,
                               env=self.child_env)
            line = (r.stderr or r.stdout).strip().splitlines()
            out = ("muted the %s switch of %s (the stream plays in the headphones only)" % (self.speaker_control, card_id)
                   if r.returncode == 0 else
                   "could not mute the %s switch of %s (amixer rc %d: %s) -- playing anyway"
                   % (self.speaker_control, card_id, r.returncode, line[-1] if line else "no message"))
        except (OSError, subprocess.TimeoutExpired) as e:
            out = "could not mute the %s switch of %s (%s) -- playing anyway" % (self.speaker_control, card_id, e)
        if out != self._speaker_logged:
            self._speaker_logged = out
            self.log(out)

    def _read(self, ff, timeout):
        """Up to CHUNK_BYTES from ffmpeg within TIMEOUT (in 1 s slices, so a stop is seen): the bytes, b"" on
        EOF, None on a timeout or a stop."""
        fd = ff.stdout.fileno()
        deadline = time.monotonic() + timeout
        while not self.stopping:
            left = deadline - time.monotonic()
            if left <= 0:
                return None
            r, _w, _x = select.select([fd], [], [], min(1.0, left))
            if r:
                return os.read(fd, CHUNK_BYTES)
            self.heartbeat()
        return None

    def _drop(self, n):
        self.st["dropped_bytes"] += n
        return n

    def _relay(self, ff, ap, ap_err, ff_err, data):
        """Relay ffmpeg -> aplay until the stream ends; returns (why it ended, dropped bytes). The aplay pipe
        is written NON-blocking in whole frames: a chunk past the ceiling, or one the full pipe refuses,
        is dropped -- the ffmpeg side (the RTMP subscriber) is never held."""
        out = ap.stdin.fileno()
        os.set_blocking(out, False)
        dropped = 0
        carry = b""
        while True:
            if ap.poll() is not None:
                ap_err.join()
                return "aplay exited %s: %s" % (ap.returncode, ap_err.last or "no message"), dropped
            data = carry + data
            cut = len(data) - len(data) % FRAME_BYTES
            data, carry = data[:cut], data[cut:]
            if data and relay_action(fionread(out), len(data), self.ceiling) == "write":
                try:
                    n = os.write(out, data)  # <= PIPE_BUF: atomic, all or EAGAIN
                    if n < len(data):
                        dropped += self._drop(len(data) - n)
                except BlockingIOError:
                    dropped += self._drop(len(data))
                except BrokenPipeError:
                    try:
                        ap.wait(DRAIN_S)
                    except subprocess.TimeoutExpired:
                        self.log("aplay closed its input but did not exit within %g s" % DRAIN_S)
                    ap_err.join()
                    return "aplay exited %s: %s" % (ap.returncode, ap_err.last or "broken pipe"), dropped
            elif data:
                dropped += self._drop(len(data))
            if self.stopping:
                return "stopping", dropped
            data = self._read(ff, self.stall_s)
            if data is None:
                if self.stopping:
                    return "stopping", dropped
                return "stalled (no audio for %g s)" % self.stall_s, dropped
            if not data:
                # EOF: ffmpeg closes its output a moment before it exits at a stream end
                try:
                    ff.wait(2.0)
                except subprocess.TimeoutExpired:
                    self.log("ffmpeg closed its output but did not exit within 2 s")
                ff_err.join()
                return "ended (ffmpeg rc %s%s)" % (ff.returncode, ": " + ff_err.last if ff_err.last else ""), dropped
            self.heartbeat()

    def _play(self, ff, ap, ap_err, ff_err, device, data):
        self.st["connects"] += 1
        self.st.update(state="playing", reason="", owner="aplay", since_epoch_s=time.time())
        start = time.monotonic()
        self.log("playing %s -> %s (connect %d)" % (self.url, device, self.st["connects"]))
        self._write_state()
        end, dropped = self._relay(ff, ap, ap_err, ff_err, data)
        _stop(ff)  # the subscriber goes at once, never held through the aplay drain
        try:
            os.set_blocking(ap.stdin.fileno(), True)
            ap.stdin.close()  # EOF: aplay plays out its buffer and exits
        except OSError as e:
            self.log("closing the aplay input failed: %s" % e)
        try:
            ap.wait(DRAIN_S)
        except subprocess.TimeoutExpired:
            self.log("aplay did not finish within %g s of its EOF -- stopping it" % DRAIN_S)
        ap_err.join()
        if ap.returncode not in (0, None) and end != "stopping" and not end.startswith("aplay exited"):
            # a short stream can end before the relay saw aplay fail its open: its exit code says so
            end = "aplay exited %s: %s" % (ap.returncode, ap_err.last or "no message")
        self.log("stream ended after %.1f s: %s; %d ms dropped over the %d ms ceiling; %d aplay underrun(s)"
                 % (time.monotonic() - start, end, dropped * 1000 // (RATE * FRAME_BYTES),
                    self.ceiling * 1000 // (RATE * FRAME_BYTES), ap_err.counted))
        if end.startswith("aplay exited") and "busy" in end.lower():
            self.set_idle("busy", "the headphone PCM %s: %s" % (device, end), owner=None)
        elif end.startswith("aplay exited"):
            self.fail_streak += 1
            self.set_idle("aplay-failing", "aplay on the headphone PCM %s: %s -- retrying with a doubling pause"
                          % (device, end.split(": ", 1)[-1]), owner=None)
        else:
            self.fail_streak = 0
            self._logged = None  # after a real stream the next idle state is news again
            self.st.update(state="ended", reason=end)
            self._write_state()

    def run(self, cycles=None):
        n = 0
        while not self.stopping and (cycles is None or n < cycles):
            n += 1
            self.cycle()
            if self.stopping or (cycles is not None and n >= cycles):
                break
            wait = self.retry_s * (min(2 ** self.fail_streak, BACKOFF_MAX) if self.fail_streak else 1)
            deadline = time.monotonic() + wait
            while not self.stopping and time.monotonic() < deadline:
                time.sleep(min(0.5, max(0.0, deadline - time.monotonic())))
                self.heartbeat()
        self._write_state()


# --- verify: --check-state -------------------------------------------------------------------------------

def check_state(path, now, asound_root=ASOUND_ROOT, proc_root=PROC_ROOT, max_age=STATE_MAX_AGE_S):
    """verify-strih's (stream-monitor-pass) row: (ok, one line). OK = a pass within MAX_AGE seconds, the
    player waiting or playing, and the headphone PCM free or held by our aplay -- never PipeWire."""
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
    card, dev = st.get("card"), st.get("pcm_device")
    if not isinstance(card, int) or not isinstance(dev, int):
        return False, "state %s names no headphone PCM -- the monitor resolved no card" % state
    owner = pcm_owner(asound_root, proc_root, card, dev)
    if owner["state"] == "unreadable":
        return False, "the headphone PCM card%d/pcm%dp status is unreadable" % (card, dev)
    if owner["state"] == "open" and owner["comm"] not in OWNER_OK:
        return False, ("%s (pid %s) holds the headphone PCM %s -- nothing but the monitor's aplay may play to it "
                       "(PipeWire must not)" % (owner["comm"] or "?", owner["pid"], device))
    holder = "free" if owner["state"] == "closed" else "held by %s pid %s" % (owner["comm"], owner["pid"])
    if state == "playing":
        return True, "playing %s -> %s (connect %s), headphone PCM %s, %d ms dropped in total" % (
            st.get("url"), device, st.get("connects"), holder,
            int(st.get("dropped_bytes") or 0) * 1000 // (RATE * FRAME_BYTES))
    if state in ("waiting", "ended", "starting"):
        return True, "%s on %s (%s), headphone PCM %s %s" % (state, st.get("url"), st.get("reason") or "",
                                                              device, holder)
    return False, "unknown state %r" % state


# --- main -------------------------------------------------------------------------------------------------

def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--url", default=None, help="the RTMP stream to play (default %s/<short hostname>)" % DEFAULT_SERVER)
    ap.add_argument("--state-file", default=None)
    ap.add_argument("--pcm-match", default=DEFAULT_PCM_MATCH, help="regex on the playback PCM name")
    ap.add_argument("--ffmpeg", default="ffmpeg")
    ap.add_argument("--aplay", default="aplay")
    ap.add_argument("--amixer", default="amixer")
    ap.add_argument("--speaker-control", default=DEFAULT_SPEAKER_CONTROL,
                    help="the mixer switch muted before the stream sounds ('' = leave the mixer alone)")
    ap.add_argument("--asound-root", default=ASOUND_ROOT)
    ap.add_argument("--proc-root", default=PROC_ROOT)
    ap.add_argument("--retry-s", type=float, default=RETRY_S)
    ap.add_argument("--stall-s", type=float, default=STALL_S)
    ap.add_argument("--connect-timeout-s", type=float, default=CONNECT_TIMEOUT_S)
    ap.add_argument("--ceiling-ms", type=int, default=DEFAULT_CEILING_MS)
    ap.add_argument("--cycles", type=int, default=None, help="stop after N connect attempts (tests)")
    ap.add_argument("--check-state", metavar="FILE", default=None,
                    help="read-only: grade the state file + the headphone PCM owner (verify-strih), rc 0/1")
    a = ap.parse_args(argv)
    if a.check_state:
        ok, line = check_state(a.check_state, time.time(), a.asound_root, a.proc_root)
        print(line)
        return 0 if ok else 1
    if a.ceiling_ms < 20 or ceiling_bytes(a.ceiling_ms) + CHUNK_BYTES > 65536:
        ap.error("--ceiling-ms must keep the pipe ceiling + one chunk within one 64 KiB pipe buffer")
    mon = Monitor(a.url or default_url(), a.state_file, a.pcm_match, a.ffmpeg, a.aplay, a.asound_root, a.proc_root,
                  a.retry_s, a.connect_timeout_s, a.stall_s, ceiling_bytes(a.ceiling_ms), amixer=a.amixer,
                  speaker_control=a.speaker_control)

    def _stop_signal(_sig, _frm):
        mon.stopping = True

    signal.signal(signal.SIGTERM, _stop_signal)
    signal.signal(signal.SIGINT, _stop_signal)
    mon.log("watching %s -> the headphone PCM matching %r (silent while nothing is published)" % (mon.url, a.pcm_match))
    mon.run(a.cycles)
    return 0


if __name__ == "__main__":
    sys.exit(main())
