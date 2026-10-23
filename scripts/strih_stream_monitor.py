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

And (owner 23.10.2026, issue 1465): "operator si chce otovorit vlc aby videl obraz a chce si vediet prepnut
zvuk do sluchatiek aby isiel z vlc tak vidi a pocuje co je na vystupe z obs a teda vie skontrolovat lipsync a
audio vypadky!!!" -- the VLC window stays (the picture), its audio goes to the operator's headphone box, and
the window no longer flickers while nothing is streamed.

And (owner 23.10.2026, issue 1465 item 3, ROZHODNUTE): "na toto mozme pipewire vyskusat ale nesmie sa babrat
do inych veci nez tych ktore mu jasne povolime a definujeme!" -- on the stream role the headphones are a
LOCKED PipeWire's one sink (scripts/stream_operator_audio.py renders it), so VLC and Chrome share them. The
unit runs `--output pulse`: VLC plays through pulse to that sink BY NAME (PULSE_SINK), and the headphone PCM
must be held by PipeWire -- the busy / owner checks and --check-state invert. `--output alsa` (the default)
is the ALSA-direct player of issues 1436 / 1456, kept as the rollback of this trial.

WHAT, one supervised --user unit (strih-stream-monitor.service, Restart=always):
  * resolves the headphone card in /proc/asound, never by a card index (both stream-pp cards are called
    `HD-Audio Generic` and the id `Generic_1` is enumeration order), re-read before every VLC start and
    addressed as plughw:CARD=<id>,DEV=<n>. With no --pcm-match (issue 1465): the playback PCM of the ONE
    USB audio interface (a card with a `usbid` file: stream-snv's Scarlett Solo 4th Gen, card `Gen`, PCM
    `USB Audio`); no USB interface = the jack, the PCM NAME ending in ` Analog` (stream-pp's `ALC257
    Analog`); two or more USB playback PCMs = named once in the log, the jack. An explicit --pcm-match
    is a regex on the PCM name and nothing else;
  * when another process holds that PCM (PipeWire, a second player) VLC is not started; it says who;
  * before each VLC start on the jack: the card's Master / Headphone / PCM to 0 dB unmuted (a unity
    baseline, the level is VLC's slider) and Speaker muted (PipeWire's ACP leaves the laptop speaker route
    on, Auto-Mute disabled: the stream must sound in the headphones only). A control the card does not have
    is named once. A USB headphone box keeps its own mixer (issue 1465): snd-usb-audio names many
    headsets' main output `Speaker`, so that mute would silence the box itself, and a 0 dB write on its own
    volume control is full scale in the headphones;
  * starts VLC on the restreamer's RTMP (default rtmp://127.0.0.1:1234/live/<short hostname>, the key
    setup-strih writes into the stream OBS profile): ALSA straight to the jack, video in a small window
    (--zoom 0.25), 1 s network cache (SNV's), no error dialogs, its http status interface on 127.0.0.1
    with a per-run random password;
  * reads that status every POLL_S: `stopped` (nothing published, a stream end) -> `pl_play` every
    RETRY_S, but ONLY while OBS publishes to the URL (issue 1465): an `obs` process holds an ESTABLISHED
    TCP connection to the URL's host:port (/proc/net/tcp + tcp6 and the obs pids' socket fds). Each
    pl_play on an unpublished URL is a failed RTMP open that flickered the VLC window all day; with
    nothing published VLC idles with its window open and the state reads `waiting: OBS is not
    publishing ...`; `paused` for more than PAUSE_MAX_S -> `pl_stop` (restreamer's per-subscriber channel is
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
import ipaddress
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
import urllib.parse
import urllib.request

DEFAULT_SERVER = "rtmp://127.0.0.1:1234/live"
# The headphone jack's playback PCM: its name ENDS in ` Analog` (stream-pp: `ALC257 Analog`), but a Realtek
# codec's second analog PCM (`ALC... Alt Analog`) is never the jack.
DEFAULT_PCM_MATCH = r"(?<!\bAlt) Analog$"
# RTMP's own port when the URL names none.
RTMP_DEFAULT_PORT = 1935
# /proc/net/tcp state 01 = TCP_ESTABLISHED.
TCP_ESTABLISHED = "01"
# The process that publishes the stream (its /proc comm; the kernel keeps at most COMM_MAX characters).
DEFAULT_OBS_PROCESS = "obs"
COMM_MAX = 15
# A hostname stream URL is looked up at most this often (the default URL is the literal 127.0.0.1).
RESOLVE_TTL_S = 60.0
# The mixer: these to 0 dB (unmuted where they have a switch), Speaker muted (issue 1456).
UNITY_CONTROLS = (("Master", True), ("Headphone", True), ("PCM", False))
DEFAULT_SPEAKER_CONTROL = "Speaker"
# issue 1465 item 3: the locked operator PipeWire's ONE sink (scripts/stream_operator_audio.py renames the
# headphone PCM's node to it) and the process that must hold the headphone PCM in the pulse output.
PULSE_SINK_DEFAULT = "stream-operator-headphones"
OUTPUTS = ("alsa", "pulse")
OWNER_PULSE = ("pipewire",)
PW_DUMP_TIMEOUT_S = 10.0
DEFAULT_HTTP_PORT = 8090  # stream-snv's VLC http port (avsync-vlc-monitor.ps1)
NETWORK_CACHING_MS = 1000
VIDEO_ZOOM = "0.25"
RETRY_S = 3.0
POLL_S = 1.0
PAUSE_MAX_S = 10.0
STARTUP_S = 15.0
HEARTBEAT_S = 5.0
STATE_MAX_AGE_S = 30.0
STATE_VERSION = 3

# FAULT states verify FAILs; the rest (starting / waiting / playing / paused) are healthy
FAULT_STATES = ("no-card", "busy", "vlc-failing", "no-sink")
# the only process allowed to play to the headphone PCM (the alsa output; the pulse output: OWNER_PULSE)
OWNER_OK = ("vlc",)
ASOUND_ROOT = os.environ.get("STRIH_STREAM_MONITOR_ASOUND", "/proc/asound")
PROC_ROOT = os.environ.get("STRIH_STREAM_MONITOR_PROC", "/proc")


def _read(path):
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def list_playback_pcms(asound_root=ASOUND_ROOT):
    """Every playback PCM in /proc/asound: [{card, card_id, device, name, usb}], sorted by card + device.
    usb = the card dir carries `usbid` (snd-usb-audio writes it for every USB audio interface)."""
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
        usb = "usbid" in entries
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
                        "name": nm.group(1).strip() if nm else "", "usb": usb})
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


def _pcm_names(pcms):
    return ", ".join("%s:%d %s" % (p["card_id"], p["device"], p["name"]) for p in pcms) or "none"


def resolve_headphone(pcms, pcm_match=None):
    """The headphone PCM (issue 1465): (pcm, None, note) or (None, the reason, note). NOTE = a line to log
    once (two or more USB playback PCMs), else None.
      * PCM_MATCH given = the operator's regex on the PCM name, exactly resolve_pcm;
      * else the ONE USB audio interface's playback PCM when exactly one exists;
      * else the headphone jack (DEFAULT_PCM_MATCH)."""
    if pcm_match is not None:
        pcm, why = resolve_pcm(pcms, pcm_match)
        return pcm, why, None
    usb = [p for p in pcms if p.get("usb")]
    if len(usb) == 1:
        return usb[0], None, None
    note = None
    if usb:
        note = ("%d USB audio playback PCMs (%s) -- ambiguous, playing to the headphone jack instead "
                "(name one with --pcm-match)" % (len(usb), _pcm_names(usb)))
    pcm, why = resolve_pcm(pcms, DEFAULT_PCM_MATCH)
    return pcm, why, note


def headphone_rule(pcm_match=None):
    """How the headphone PCM is picked, for the `watching ...` line."""
    if pcm_match is not None:
        return "the playback PCM matching %r (--pcm-match)" % pcm_match
    return ("the ONE USB audio interface's playback PCM, else the headphone jack matching %r"
            % DEFAULT_PCM_MATCH)


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
        # issue 1465: the kernel records the opening THREAD; its process (Tgid) is the owner by name
        try:
            tg = re.search(r"^Tgid:\s*(\d+)", _read(os.path.join(proc_root, str(pid), "status")), re.M)
            if tg:
                pid = int(tg.group(1))
        except OSError:
            pass
        try:
            comm = _read(os.path.join(proc_root, str(pid), "comm")).strip()
        except OSError:
            comm = None  # the owner exited between the two reads: named as "?"
    return {"state": "open", "pid": pid, "comm": comm}



def rtmp_endpoint(url):
    """(host, port) the URL connects to; port RTMP_DEFAULT_PORT when it names none."""
    u = urllib.parse.urlsplit(url)
    try:
        port = u.port
    except ValueError:
        port = None
    return u.hostname or "", port or RTMP_DEFAULT_PORT


def _endpoint_ips(host):
    """The IP addresses HOST stands for (a literal is itself; a name through getaddrinfo, may be empty)."""
    try:
        return {ipaddress.ip_address(host)}
    except ValueError:
        pass
    try:
        infos = socket.getaddrinfo(host, None, type=socket.SOCK_STREAM)
    except OSError:
        return set()
    out = set()
    for info in infos:
        try:
            out.add(ipaddress.ip_address(info[4][0].split("%")[0]))
        except ValueError:
            continue
    return out


def _proc_net_addr(hexaddr):
    """A /proc/net/tcp[6] address field `HEX:PORT` -> (ip, port). The address is printed as 32-bit words
    in host byte order, the port as a plain hex number. An IPv4-mapped IPv6 address reads as its IPv4."""
    h, _, p = hexaddr.partition(":")
    raw = bytes.fromhex(h)
    if sys.byteorder == "little":
        raw = b"".join(raw[i:i + 4][::-1] for i in range(0, len(raw), 4))
    ip = ipaddress.ip_address(raw)
    if ip.version == 6 and ip.ipv4_mapped is not None:
        ip = ip.ipv4_mapped
    return ip, int(p, 16)


def established_inodes(proc_root, ips, port):
    """The socket inodes of every ESTABLISHED TCP connection whose REMOTE end is one of IPS at PORT, read
    from <proc_root>/net/tcp and tcp6 (a missing or unreadable table counts as empty)."""
    out = set()
    for table in ("tcp", "tcp6"):
        try:
            lines = _read(os.path.join(proc_root, "net", table)).splitlines()[1:]
        except OSError:
            continue
        for ln in lines:
            f = ln.split()
            if len(f) < 10 or f[3] != TCP_ESTABLISHED:
                continue
            try:
                ip, rport = _proc_net_addr(f[2])
            except ValueError:
                continue
            if rport == port and ip in ips:
                out.add(f[9])
    return out


def _socket_inodes(proc_root, pid):
    """The socket inodes PID holds open (its fd links `socket:[N]`); a vanished or foreign pid = none."""
    fd_dir = os.path.join(proc_root, pid, "fd")
    out = set()
    try:
        fds = os.listdir(fd_dir)
    except OSError:
        return out
    for fd in fds:
        try:
            target = os.readlink(os.path.join(fd_dir, fd))
        except OSError:
            continue
        m = re.fullmatch(r"socket:\[(\d+)\]", target)
        if m:
            out.add(m.group(1))
    return out


def obs_publisher(proc_root, url, process_name=DEFAULT_OBS_PROCESS, ips=None):
    """(True, detail) when a process with comm PROCESS_NAME holds an ESTABLISHED TCP connection to the
    URL's host:port (issue 1465: OBS publishes to the restreamer), else (False, why). Std-only from
    /proc; only the fds of PROCESS_NAME pids are read, a vanished or unreadable pid is skipped. IPS = the
    host's addresses already looked up (None = look them up now).

    Limits: the stream KEY is not checked (OBS publishing under another key still reads True; verify's
    (stream-monitor-key) row grades the key), and PROCESS_NAME must fit the kernel's 15-character comm."""
    host, port = rtmp_endpoint(url)
    if ips is None:
        ips = _endpoint_ips(host)
    where = "%s:%d" % (host, port)
    if not ips:
        return False, "the stream host %r does not resolve" % host
    inodes = established_inodes(proc_root, ips, port)
    if not inodes:
        return False, "nothing holds an ESTABLISHED TCP connection to %s" % where
    try:
        pids = [d for d in os.listdir(proc_root) if d.isdigit()]
    except OSError:
        return False, "%s is unreadable" % proc_root
    for pid in pids:
        try:
            with open(os.path.join(proc_root, pid, "comm"), "rb") as fh:
                comm = fh.read().decode("utf-8", "replace").strip()
        except OSError:
            continue
        if comm != process_name:
            continue
        if inodes & _socket_inodes(proc_root, pid):
            return True, "%s (pid %s) publishes to %s" % (process_name, pid, where)
    return False, "no %s process holds the ESTABLISHED TCP connection to %s" % (process_name, where)


def default_url():
    """The local restreamer + this box's short hostname = the stream key setup-strih writes (issue 1436)."""
    return "%s/%s" % (DEFAULT_SERVER, socket.gethostname().split(".")[0])



def pulse_sink_present(pw_dump, name, card=None, timeout=PW_DUMP_TIMEOUT_S):
    """(True | False | None, detail): does the operator session list the Audio/Sink NAME (`pw-dump`) -- on ALSA
    card CARD when given (its device's `api.alsa.card`, review round 2: a stale drop-in could rename a node of
    another card)? None = the session is unreadable. Review round 1 (issue 1465): an unlocked PipeWire holds the headphone card too
    while something plays, and a PULSE_SINK naming a missing sink falls back to the default sink -- maybe the
    notebook speakers -- so VLC starts only once this sink exists."""
    try:
        r = subprocess.run([pw_dump], capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as e:
        return None, "pw-dump failed: %s" % e
    if r.returncode != 0:
        return None, "pw-dump failed (rc %d)" % r.returncode
    try:
        objs = json.loads(r.stdout)
    except ValueError:
        return None, "pw-dump printed no JSON"
    objs = [o for o in (objs if isinstance(objs, list) else []) if isinstance(o, dict)]

    def props(o):
        p = (o.get("info") or {}).get("props") if isinstance(o.get("info"), dict) else None
        return p if isinstance(p, dict) else {}

    devices = {o.get("id"): props(o) for o in objs if o.get("type") == "PipeWire:Interface:Device"}
    for o in objs:
        p = props(o)
        if (o.get("type") == "PipeWire:Interface:Node" and p.get("node.name") == name
                and p.get("media.class") == "Audio/Sink"):
            if card is None:
                return True, "pw-dump lists the Audio/Sink '%s'" % name
            on = devices.get(p.get("device.id"), {}).get("api.alsa.card")
            if str(on) == str(card):
                return True, "pw-dump lists the Audio/Sink '%s' on card %s" % (name, card)
            return False, "pw-dump lists '%s' on card %s, not the headphone card %s" % (name, on, card)
    return False, "pw-dump lists no Audio/Sink '%s'" % name


def vlc_argv(vlc, device, url, http_port, password, output="alsa"):
    """VLC on URL, ALSA straight to DEVICE (issue 1456) or, OUTPUT "pulse" (issue 1465), through pulse to the
    sink the caller names in PULSE_SINK; its http status interface on loopback. No --repeat: the supervisor
    retries with pl_play, never a tight VLC reconnect loop."""
    aout = ["--aout=pulse"] if output == "pulse" else ["--aout=alsa", "--alsa-audio-device=%s" % device]
    return [vlc, "--no-qt-error-dialogs", "--no-qt-privacy-ask", "--zoom=%s" % VIDEO_ZOOM,
            "--network-caching=%d" % NETWORK_CACHING_MS] + aout + [
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
    def __init__(self, url, state_file, pcm_match=None, vlc="vlc", amixer="amixer",
                 asound_root=ASOUND_ROOT, proc_root=PROC_ROOT, http_port=DEFAULT_HTTP_PORT, retry_s=RETRY_S,
                 poll_s=POLL_S, pause_max_s=PAUSE_MAX_S, startup_s=STARTUP_S,
                 speaker_control=DEFAULT_SPEAKER_CONTROL, log=None, obs_process=DEFAULT_OBS_PROCESS,
                 output="alsa", pulse_sink=PULSE_SINK_DEFAULT, pw_dump="pw-dump"):
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
        self.obs_process = obs_process
        self.output = output
        self.pulse_sink = pulse_sink
        self.pw_dump = pw_dump
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
        self._notes_logged = set()
        self._ips = set()
        self._resolved_at = None
        self._logged = None
        self._last_write = 0.0
        self.st = {"version": STATE_VERSION, "state": "starting", "url": url, "output": output, "device": None,
                   "card": None,
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

    def publisher(self):
        """obs_publisher on this URL, the host looked up at most every RESOLVE_TTL_S (a literal IP never)."""
        now = time.monotonic()
        if self._resolved_at is None or now - self._resolved_at >= RESOLVE_TTL_S:
            self._ips = _endpoint_ips(rtmp_endpoint(self.url)[0])
            self._resolved_at = now
        return obs_publisher(self.proc_root, self.url, self.obs_process, self._ips)

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
        pcm, why, note = resolve_headphone(list_playback_pcms(self.asound_root), self.pcm_match)
        if note and note not in self._notes_logged:
            self._notes_logged.add(note)
            self.log(note)
        if pcm is None:
            self.set_state("no-card", why, device=None, card=None, pcm_device=None)
            return False
        hw = alsa_device(pcm)
        device = "pulse:%s" % self.pulse_sink if self.output == "pulse" else hw
        self.st.update(device=device, card=pcm["card"], pcm_device=pcm["device"])
        owner = pcm_owner(self.asound_root, self.proc_root, pcm["card"], pcm["device"])
        if self.output == "pulse" and owner["state"] != "open":
            # issue 1465: the locked operator PipeWire holds the headphone PCM whenever its sink exists
            self.set_state("no-sink", "PipeWire does not hold the headphone PCM %s (%s): the locked operator "
                           "PipeWire with its sink '%s' is not running -- VLC not started, retrying every %g s"
                           % (hw, owner["state"], self.pulse_sink, self.retry_s))
            return False
        if owner["state"] == "open" and not (self.output == "pulse" and owner["comm"] in OWNER_PULSE):
            self.set_state("busy", "the headphone PCM %s is held by %s (pid %s) -- VLC not started, retrying every %g s"
                           % (hw, owner["comm"] or "?", owner["pid"], self.retry_s))
            return False
        if self.output == "pulse":
            present, detail = pulse_sink_present(self.pw_dump, self.pulse_sink, pcm["card"])
            if not present:
                self.set_state("no-sink", "the operator sink '%s' is not in the operator PipeWire (%s) -- VLC not "
                               "started, retrying every %g s" % (self.pulse_sink, detail, self.retry_s))
                return False
        if pcm.get("usb"):
            key = ("usb", pcm["card_id"])
            if key not in self._mixer_logged:
                self._mixer_logged.add(key)
                self.log("mixer: %s is a USB audio interface -- its own mixer is left as it is, the level is "
                         "VLC's slider and the box's own knob" % pcm["card_id"])
        else:
            self._mixer(pcm["card_id"])
        password = secrets.token_hex(16)
        env = None
        if self.output == "pulse":
            env = dict(os.environ, PULSE_SINK=self.pulse_sink)  # libpulse's default sink for this client
        try:
            self.proc = subprocess.Popen(vlc_argv(self.vlc, hw, self.url, self.http_port, password, self.output),
                                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                         stderr=subprocess.DEVNULL, env=env)
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
            published, detail = self.publisher()
            if not published:
                # issue 1465: no pl_play -- each one on an unpublished URL is a failed RTMP open that
                # flickers the VLC window; VLC idles with its window open until OBS publishes. The reason
                # stays constant (one journal line); what holds the port goes to the state file.
                self.set_state("waiting", "OBS is not publishing to %s -- VLC idles until it does" % self.url,
                               publisher=detail)
                return
            self.set_state("waiting", "OBS publishes to %s, VLC not playing yet -- pl_play every %g s"
                           % (self.url, self.retry_s), publisher=detail)
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
    PipeWire -- or, for the pulse output (issue 1465), held by PipeWire and nothing else."""
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
    if st.get("output") == "pulse":
        if owner["state"] != "open":
            return False, ("PipeWire does not hold the headphone PCM card%d/pcm%dp (closed): the locked operator "
                           "PipeWire is not running or not provisioned" % (card, dev))
        if owner["comm"] not in OWNER_PULSE:
            return False, ("%s (pid %s) holds the headphone PCM card%d/pcm%dp -- only PipeWire may (the pulse "
                           "output plays through its sink)" % (owner["comm"] or "?", owner["pid"], card, dev))
        return True, "%s %s -> %s (start %s), headphone PCM held by PipeWire pid %s" % (
            state, st.get("url"), device, st.get("starts"), owner["pid"])
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
    ap.add_argument("--pcm-match", default=None,
                    help="regex on the playback PCM name (default: the ONE USB audio interface, else the "
                         "headphone jack %r)" % DEFAULT_PCM_MATCH)
    ap.add_argument("--obs-process-name", default=DEFAULT_OBS_PROCESS,
                    help="the /proc comm of the publishing OBS (pl_play only while it publishes)")
    ap.add_argument("--output", choices=OUTPUTS, default="alsa",
                    help="alsa = VLC straight to the headphone PCM (the default, the rollback); pulse = VLC "
                         "through pulse to --pulse-sink, the PCM held by the locked PipeWire (issue 1465)")
    ap.add_argument("--pulse-sink", default=PULSE_SINK_DEFAULT,
                    help="the pulse sink the pulse output plays to (default %s)" % PULSE_SINK_DEFAULT)
    ap.add_argument("--pw-dump", default="pw-dump",
                    help="the pw-dump the pulse output lists the operator session's sinks with")
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
    if not a.obs_process_name or len(a.obs_process_name) > COMM_MAX:
        ap.error("--obs-process-name %r: the kernel reports a process name in 1..%d characters"
                 % (a.obs_process_name, COMM_MAX))
    if a.check_state:
        ok, line = check_state(a.check_state, time.time(), a.asound_root, a.proc_root)
        print(line)
        return 0 if ok else 1
    mon = Monitor(a.url or default_url(), a.state_file, a.pcm_match, a.vlc, a.amixer, a.asound_root, a.proc_root,
                  a.http_port, a.retry_s, a.poll_s, a.pause_max_s, a.startup_s, a.speaker_control,
                  obs_process=a.obs_process_name, output=a.output, pulse_sink=a.pulse_sink, pw_dump=a.pw_dump)

    def _stop_signal(_sig, _frm):
        mon.stopping = True

    signal.signal(signal.SIGTERM, _stop_signal)
    signal.signal(signal.SIGINT, _stop_signal)
    out = ("pulse output to the sink '%s', the headphone PCM held by PipeWire" % a.pulse_sink
           if a.output == "pulse" else "alsa output straight to the headphone PCM")
    mon.log("watching %s -> %s in VLC, %s (http status on 127.0.0.1:%d; pl_play only while %s publishes)"
            % (mon.url, headphone_rule(a.pcm_match), out, a.http_port, a.obs_process_name))
    mon.run(a.run_s)
    return 0


if __name__ == "__main__":
    sys.exit(main())
