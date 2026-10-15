"""Issue 1436 -- the stream-role headphone monitor: a supervised --user session app that plays the REAL
outgoing stream (the restreamer's local RTMP) to the headphone jack, straight through ALSA.

WHY (owner 15.10.2026): "a spomen si ako to mame riesene v snv ze sa pocuva cez vlc ktore ma nacitany
stream rtmp z obs alebo z restreamera" + "pozadujem hlbkove preverenie nutnosti vyuzivat pipewire" ->
the SNV design (scripts/avsync-vlc-monitor.ps1 plays rtmp://127.0.0.1:1234/live/obs-e2e-test) on every
Linux stream box, writing the card directly (never PipeWire). Main design: issue comment 6189806622.

What this pins:
  * scripts/strih_stream_monitor.py -- the pure parts (the /proc/asound PCM walk, the card resolved by
    its playback PCM NAME, never an index; the PCM owner; the pipe-ceiling relay decision; the ffmpeg /
    aplay argv) and the REAL loop driven with fake ffmpeg / aplay executables: play + end, nothing
    published (silent, logged once), a busy card (never connects), an aplay that fails the open, the
    latency ceiling (drops instead of pausing the RTMP subscriber), a stalled stream; `--check-state`;
  * systemd/strih-stream-monitor.service -- the unit (Restart=always, the kiosk start path, the URL =
    the profile's server + /%l, the state file verify reads);
  * scripts/lib/strih-session-apps.sh + scripts/lib/strih-stream-monitor.sh -- the stream role runs it
    (keeper, monitor, dantesync tray), its packages, and the three extra verify rows (last pass + card
    owner, the OBS stream key, the program audio's OBS monitoring off).

Tier-0: pytest + bash only (no cargo, no root, no rig, no audio device).
"""
import json
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROG = SCRIPTS / "strih_stream_monitor.py"
UNIT = REPO / "systemd" / "strih-stream-monitor.service"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
APPS = SCRIPTS / "lib" / "strih-session-apps.sh"
MONLIB = SCRIPTS / "lib" / "strih-stream-monitor.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_BOXES_DIR", "OBS_FLEET", "NDI_DISCOVERY_SENDER_HOSTS",
         "STRIH_SESSION_APPS_RUNTIME_DIR", "STRIH_SESSION_APPS_EUID", "STRIH_STREAM_MONITOR_ASOUND",
         "STRIH_STREAM_MONITOR_PROC", "STRIH_STREAM_MONITOR_HOSTNAME", "STRIH_STREAM_MONITOR_SCENES",
         "STRIH_STREAM_MONITOR_MANIFEST", "OBS_BOX_STREAM_RTMP_SERVER")


def _mod():
    import importlib.util
    spec = importlib.util.spec_from_file_location("strih_stream_monitor_1436", PROG)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _bash(body, env=None, sources=(FACTS, PROVISION)):
    harness = "set -euo pipefail\n" + "".join('. "%s"\n' % s for s in sources) + body
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    if env:
        e.update(env)
    return subprocess.run(["bash", "-c", harness], capture_output=True, text=True, env=e, cwd=str(REPO),
                          timeout=60)


@pytest.fixture
def boxes(tmp_path):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text().replace("STRIH_NDI_RUNTIME_PEER=TODO_OWNER",
                                                       "STRIH_NDI_RUNTIME_PEER=10.77.9.61"))
    return d


# --- a fake /proc/asound + /proc (stream-pp's two cards, read live 15.10.2026) ---------------------------------

def asound(tmp_path, analog=True, owner=None, second_analog=False):
    """stream-pp's cards: card0 `Generic` (HDMI 0 / HDMI 1), card1 `Generic_1` (ALC257 Analog). OWNER =
    (pid, comm) holding the analog playback PCM, else it reads `closed`."""
    root = tmp_path / "asound"
    proc = tmp_path / "proc"
    proc.mkdir(parents=True, exist_ok=True)

    def pcm(card, dev, name, status="closed", kind="p"):
        d = root / ("card%d" % card) / ("pcm%d%s" % (dev, kind))
        (d / "sub0").mkdir(parents=True, exist_ok=True)
        (d / "info").write_text("card: %d\ndevice: %d\nsubdevice: 0\nstream: PLAYBACK\nid: %s\nname: %s\n"
                                "subname: subdevice #0\n" % (card, dev, name, name))
        (d / "sub0" / "status").write_text(status + "\n")

    (root / "card0").mkdir(parents=True)
    (root / "card0" / "id").write_text("Generic\n")
    pcm(0, 3, "HDMI 0")
    pcm(0, 7, "HDMI 1")
    (root / "card1").mkdir(parents=True)
    (root / "card1" / "id").write_text("Generic_1\n")
    status = "closed"
    if owner:
        status = "state: RUNNING\nowner_pid   : %d\ntrigger_time: 1.0\n" % owner[0]
        (proc / str(owner[0])).mkdir(parents=True, exist_ok=True)
        (proc / str(owner[0]) / "comm").write_text(owner[1] + "\n")
    pcm(1, 0, "ALC257 Analog" if analog else "ALC257 Digital", status)
    pcm(1, 0, "ALC257 Analog", "closed", kind="c")  # the capture side is never a candidate
    if second_analog:
        (root / "card2").mkdir(parents=True)
        (root / "card2" / "id").write_text("Device\n")
        pcm(2, 0, "USB Audio Analog")
    return root, proc


# --- the pure parts ---------------------------------------------------------------------------------------------

def test_the_playback_pcms_are_read_from_proc_asound(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    assert [(p["card"], p["card_id"], p["device"], p["name"]) for p in m.list_playback_pcms(str(root))] == [
        (0, "Generic", 3, "HDMI 0"), (0, "Generic", 7, "HDMI 1"), (1, "Generic_1", 0, "ALC257 Analog")]


def test_the_headphone_pcm_is_resolved_by_its_name_never_an_index(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path)
    pcm, why = m.resolve_pcm(m.list_playback_pcms(str(root)), m.DEFAULT_PCM_MATCH)
    assert why is None and pcm["card_id"] == "Generic_1" and pcm["device"] == 0
    assert m.alsa_device(pcm) == "plughw:CARD=Generic_1,DEV=0"


def test_no_or_two_matching_pcms_resolve_nothing_and_say_why(tmp_path):
    m = _mod()
    root, _proc = asound(tmp_path, analog=False)
    pcm, why = m.resolve_pcm(m.list_playback_pcms(str(root)), m.DEFAULT_PCM_MATCH)
    assert pcm is None and "no playback PCM" in why and "HDMI 0" in why
    root2, _p = asound(tmp_path / "b", second_analog=True)
    pcm, why = m.resolve_pcm(m.list_playback_pcms(str(root2)), m.DEFAULT_PCM_MATCH)
    assert pcm is None and "2 playback PCMs" in why
    assert m.resolve_pcm([], m.DEFAULT_PCM_MATCH)[0] is None


def test_the_pcm_owner(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path)
    assert m.pcm_owner(str(root), str(proc), 1, 0) == {"state": "closed", "pid": None, "comm": None}
    root, proc = asound(tmp_path / "b", owner=(4321, "pipewire"))
    assert m.pcm_owner(str(root), str(proc), 1, 0) == {"state": "open", "pid": 4321, "comm": "pipewire"}
    assert m.pcm_owner(str(root), str(proc), 5, 0)["state"] == "unreadable"


@pytest.mark.parametrize("pending,n,ceiling,want", [
    (0, 3840, 38400, "write"), (34560, 3840, 38400, "write"), (34561, 3840, 38400, "drop"),
    (38400, 1, 38400, "drop"), (None, 3840, 38400, "write")])
def test_the_relay_drops_only_what_would_pass_the_ceiling(pending, n, ceiling, want):
    assert _mod().relay_action(pending, n, ceiling) == want


def test_the_ceiling_is_below_one_pipe_buffer_so_a_write_never_blocks():
    m = _mod()
    assert m.ceiling_bytes(m.DEFAULT_CEILING_MS) + m.CHUNK_BYTES <= 65536
    assert m.ceiling_bytes(200) == 38400


def test_the_argv_plays_only_the_audio_straight_to_alsa():
    m = _mod()
    ff = m.ffmpeg_argv("ffmpeg", "rtmp://127.0.0.1:1234/live/stream-pp")
    assert ff[ff.index("-i") + 1] == "rtmp://127.0.0.1:1234/live/stream-pp"
    assert ff.index("-rw_timeout") < ff.index("-i"), "a socket read timeout is an input option"
    for a in ("-vn", "-nostdin"):
        assert a in ff
    assert ff[ff.index("-map") + 1] == "0:a:0"
    assert ff[ff.index("-f") + 1] == "s16le" and ff[ff.index("-ar") + 1] == "48000" and ff[ff.index("-ac") + 1] == "2"
    assert ff[-1] == "pipe:1"
    ap = m.aplay_argv("aplay", "plughw:CARD=Generic_1,DEV=0")
    assert ap[:2] == ["aplay", "-N"], "a busy PCM must fail the open at once (the hub's AplaySink pattern)"
    assert ap[ap.index("-D") + 1] == "plughw:CARD=Generic_1,DEV=0"
    assert ap[ap.index("-f") + 1] == "S16_LE" and ap[ap.index("-r") + 1] == "48000" and ap[ap.index("-c") + 1] == "2"
    assert "pipewire" not in " ".join(ff + ap) and "pulse" not in " ".join(ff + ap)


def test_the_default_url_is_the_local_restreamer_and_the_short_hostname():
    m = _mod()
    assert m.default_url() == "rtmp://127.0.0.1:1234/live/" + socket.gethostname().split(".")[0]


# --- the loop, with fake ffmpeg / aplay ---------------------------------------------------------------------------

FAKE_FF = r'''
import os, sys, time
log = os.environ["FAKE_LOG"]
with open(log, "a") as f:
    f.write("ffmpeg " + " ".join(sys.argv[1:]) + "\n")
mode = os.environ.get("FAKE_FF_MODE", "play:19200")
kind, _, n = mode.partition(":")
if kind == "none":
    sys.stderr.write("rtmp://127.0.0.1:1234/live/x: Input/output error\n")
    sys.exit(1)
out = sys.stdout.buffer
left = int(n)
while left > 0:
    k = min(3840, left)
    try:
        out.write(b"\x01" * k)
        out.flush()
    except BrokenPipeError:
        sys.exit(1)
    left -= k
    time.sleep(0.002)
if kind == "stall":
    time.sleep(30)
'''

FAKE_AP = r'''
import os, sys, time
log = os.environ["FAKE_LOG"]
with open(log, "a") as f:
    f.write("aplay " + " ".join(sys.argv[1:]) + "\n")
mode = os.environ.get("FAKE_AP_MODE", "read")
if mode == "busy":
    sys.stderr.write("aplay: main:850: audio open error: Device or resource busy\n")
    sys.exit(1)
if mode == "slow":
    time.sleep(1.5)
n = 0
while True:
    b = sys.stdin.buffer.read(4096)
    if not b:
        break
    n += len(b)
with open(os.environ["FAKE_AP_OUT"], "a") as f:
    f.write("%d\n" % n)
'''


def _fakes(tmp_path):
    b = tmp_path / "fakebin"
    b.mkdir(exist_ok=True)
    for name, body in (("ffmpeg", FAKE_FF), ("aplay", FAKE_AP)):
        (b / name).write_text("#!%s\n%s" % (sys.executable, body))
        (b / name).chmod(0o755)
    return b


def run_monitor(tmp_path, cycles=1, ff="play:19200", ap="read", owner=None, analog=True, extra=(), timeout=60):
    root, proc = asound(tmp_path, analog=analog, owner=owner)
    b = _fakes(tmp_path)
    state = tmp_path / "run" / "strih-stream-monitor.json"
    log = tmp_path / "fake.log"
    apout = tmp_path / "aplay.out"
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"FAKE_LOG": str(log), "FAKE_FF_MODE": ff, "FAKE_AP_MODE": ap, "FAKE_AP_OUT": str(apout)})
    argv = [sys.executable, str(PROG), "--url", "rtmp://127.0.0.1:1234/live/stream-pp", "--state-file", str(state),
            "--ffmpeg", str(b / "ffmpeg"), "--aplay", str(b / "aplay"), "--asound-root", str(root),
            "--proc-root", str(proc), "--retry-s", "0.05", "--cycles", str(cycles)] + list(extra)
    t0 = time.monotonic()
    r = subprocess.run(argv, capture_output=True, text=True, env=env, timeout=timeout)
    elapsed = time.monotonic() - t0
    calls = log.read_text() if log.exists() else ""
    played = [int(x) for x in apout.read_text().split()] if apout.exists() else []
    st = json.loads(state.read_text()) if state.exists() else None
    return r, calls, played, st, elapsed


def test_a_published_stream_plays_to_the_resolved_card_and_ends_cleanly(tmp_path):
    r, calls, played, st, _e = run_monitor(tmp_path, ff="play:19200")
    assert r.returncode == 0, r.stderr
    assert played == [19200], "every byte reached aplay (under the ceiling nothing is dropped)"
    assert "aplay -N" in calls and "-D plughw:CARD=Generic_1,DEV=0" in calls
    assert calls.index("ffmpeg ") < calls.index("aplay "), "the card is opened only once audio arrived"
    lines = r.stdout.splitlines()
    assert any(ln.startswith("playing rtmp://127.0.0.1:1234/live/stream-pp -> plughw:CARD=Generic_1,DEV=0")
               for ln in lines), lines
    assert any(ln.startswith("stream ended after") and "; 0 ms dropped" in ln for ln in lines), lines
    assert st["connects"] == 1 and st["url"] == "rtmp://127.0.0.1:1234/live/stream-pp"
    assert st["device"] == "plughw:CARD=Generic_1,DEV=0" and st["card"] == 1 and st["pcm_device"] == 0


def test_nothing_published_is_silent_and_logged_once(tmp_path):
    r, calls, played, st, _e = run_monitor(tmp_path, cycles=3, ff="none")
    assert r.returncode == 0, r.stderr
    assert calls.count("ffmpeg ") == 3 and "aplay " not in calls, "no stream = the card is never opened"
    assert played == []
    waits = [ln for ln in r.stdout.splitlines() if ln.startswith("waiting")]
    assert len(waits) == 1, r.stdout
    assert "Input/output error" in waits[0]
    assert st["state"] == "waiting"


def test_a_card_held_by_another_process_is_never_played_and_never_connected(tmp_path):
    r, calls, played, st, _e = run_monitor(tmp_path, cycles=2, owner=(4321, "pipewire"))
    assert r.returncode == 0, r.stderr
    assert calls == "", "a busy card: no RTMP connection at all (never a paused subscriber)"
    busy = [ln for ln in r.stdout.splitlines() if ln.startswith("busy")]
    assert len(busy) == 1 and "pipewire" in busy[0] and "4321" in busy[0], r.stdout
    assert st["state"] == "busy" and st["owner"] == "pipewire"


def test_an_aplay_that_fails_the_open_ends_the_connection(tmp_path):
    r, calls, played, st, _e = run_monitor(tmp_path, ap="busy", ff="play:400000")
    assert r.returncode == 0, r.stderr
    assert "ffmpeg " in calls and "aplay " in calls
    assert st["state"] == "busy"
    assert any("Device or resource busy" in ln for ln in r.stdout.splitlines()), r.stdout


def test_no_headphone_pcm_is_named_and_never_connects(tmp_path):
    r, calls, played, st, _e = run_monitor(tmp_path, cycles=2, analog=False)
    assert r.returncode == 0, r.stderr
    assert calls == "" and st["state"] == "no-card"
    assert len([ln for ln in r.stdout.splitlines() if ln.startswith("no-card")]) == 1


def test_a_card_slower_than_the_stream_drops_over_the_ceiling_instead_of_pausing(tmp_path):
    # aplay sleeps 1.5 s before it reads: the relay must keep reading ffmpeg and drop past the ceiling
    r, calls, played, st, _e = run_monitor(tmp_path, ff="play:192000", ap="slow", extra=("--ceiling-ms", "40"))
    assert r.returncode == 0, r.stderr
    assert st["dropped_bytes"] > 0
    assert played and played[0] + st["dropped_bytes"] == 192000, (played, st["dropped_bytes"])
    assert played[0] <= 7680 + 3840 + 65536
    ended = [ln for ln in r.stdout.splitlines() if ln.startswith("stream ended")]
    assert ended and "dropped" in ended[0] and "; 0 ms dropped" not in ended[0], ended


def test_a_stalled_stream_is_dropped_not_held(tmp_path):
    r, calls, played, st, elapsed = run_monitor(tmp_path, ff="stall:7680", extra=("--stall-s", "1"))
    assert r.returncode == 0, r.stderr
    assert elapsed < 20, "the stalled ffmpeg (sleeping 30 s) was killed"
    assert any(ln.startswith("stream ended") and "stalled" in ln for ln in r.stdout.splitlines()), r.stdout


# --- --check-state (verify-strih's stream-monitor pass row) ----------------------------------------------------------

def _state(tmp_path, **kw):
    d = {"version": 1, "updated_epoch_s": time.time(), "state": "waiting",
         "url": "rtmp://127.0.0.1:1234/live/stream-pp", "device": "plughw:CARD=Generic_1,DEV=0",
         "card": 1, "pcm_device": 0, "connects": 0, "dropped_bytes": 0, "reason": "nothing published"}
    d.update(kw)
    p = tmp_path / "state.json"
    p.write_text(json.dumps(d))
    return p


def test_check_state_ok_while_waiting_with_the_card_free(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path)
    ok, line = m.check_state(str(_state(tmp_path)), time.time(), str(root), str(proc))
    assert ok, line
    assert "rtmp://127.0.0.1:1234/live/stream-pp" in line and "free" in line


def test_check_state_ok_while_playing_with_aplay_on_the_card(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path, owner=(77, "aplay"))
    ok, line = m.check_state(str(_state(tmp_path, state="playing", connects=2)), time.time(), str(root), str(proc))
    assert ok and "playing" in line and "aplay" in line, line


def test_check_state_fails_pipewire_on_the_headphone_card(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path, owner=(4321, "pipewire"))
    ok, line = m.check_state(str(_state(tmp_path)), time.time(), str(root), str(proc))
    assert not ok and "pipewire" in line and "4321" in line, line


@pytest.mark.parametrize("kw,word", [({"state": "busy"}, "busy"), ({"state": "no-card"}, "no-card"),
                                     ({"updated_epoch_s": 1.0}, "ago")])
def test_check_state_fails(tmp_path, kw, word):
    m = _mod()
    root, proc = asound(tmp_path)
    ok, line = m.check_state(str(_state(tmp_path, **kw)), time.time(), str(root), str(proc))
    assert not ok and word in line, line


def test_check_state_fails_an_unreadable_card_status(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path)
    ok, line = m.check_state(str(_state(tmp_path, card=9)), time.time(), str(root), str(proc))
    assert not ok and "unreadable" in line, line


def test_check_state_fails_a_missing_state_file(tmp_path):
    m = _mod()
    ok, line = m.check_state(str(tmp_path / "none.json"), time.time(), "/nonexistent", "/nonexistent")
    assert not ok and "no state" in line


def test_the_cli_check_state_reads_the_asound_seam_from_the_env(tmp_path):
    root, proc = asound(tmp_path, owner=(4321, "pipewire"))
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"STRIH_STREAM_MONITOR_ASOUND": str(root), "STRIH_STREAM_MONITOR_PROC": str(proc)})
    r = subprocess.run([sys.executable, str(PROG), "--check-state", str(_state(tmp_path))], capture_output=True,
                       text=True, env=env, timeout=30)
    assert r.returncode == 1 and "pipewire" in r.stdout, (r.stdout, r.stderr)


# --- the unit ----------------------------------------------------------------------------------------------------

def _unit():
    return {ln.split("=", 1)[0]: ln.split("=", 1)[1]
            for ln in UNIT.read_text().splitlines() if "=" in ln and not ln.startswith("#")}


def test_the_unit(boxes):
    u = _unit()
    r = _bash('printf "%s|%s" "$OBS_BOX_STREAM_RTMP_SERVER" "$STRIH_STREAM_MONITOR_STATE_FILE"',
              env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    server, state_file = r.stdout.split("|")
    assert u["ExecStart"] == ("/usr/bin/python3 /usr/local/bin/strih_stream_monitor.py --url %s/%%l "
                              "--state-file %%t/%s" % (server, state_file))
    assert u["Restart"] == "always" and u["StartLimitIntervalSec"] == "0"
    assert u["WantedBy"] == "graphical-session.target"
    assert "DISPLAY" not in UNIT.read_text(), "it opens no window"
    assert os.access(PROG, os.X_OK), "commit the program executable (git mode 100755)"


def test_the_stream_key_and_the_unit_host_are_the_same_fact(boxes):
    # setup writes the key = the box name; the unit plays /%l = the short hostname setup step 1 sets to it
    r = _bash('strih_box_load stream-pp\nprintf "%s" "$(obs_box_stream_key)"', env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.stdout == "stream-pp"
    assert "STRIH_HOSTNAME=stream-pp" in (BOXES / "stream-pp.env").read_text()


# --- the session-app entry ------------------------------------------------------------------------------------------

def _applied(boxes, box):
    body = ('strih_box_load %s\nobs_box_role_apply\n'
            'echo "UNITS=${STRIH_SESSION_APP_UNITS[*]}"\necho "ROWS=$(strih_session_apps_row_count)"\n'
            'echo "AUTOSTART=$(strih_session_apps_autostart_lines | tr "\\n" ";")"\n'
            'echo "PKGS=$(strih_session_apps_packages | tr "\\n" " ")"' % box)
    r = _bash(body, env={"STRIH_BOXES_DIR": str(boxes)})
    assert r.returncode == 0, r.stderr
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines())


def test_the_stream_role_runs_the_monitor_between_the_keeper_and_the_tray(boxes):
    a = _applied(boxes, "stream-pp")
    assert a["UNITS"] == "strih-browser-keeper.service strih-stream-monitor.service dantesync-tray.service"
    assert a["ROWS"] == "7", "three unit rows + the keeper's last pass + the monitor's three rows"
    assert "systemctl --user start strih-stream-monitor.service || true" in a["AUTOSTART"]
    assert "ffmpeg" in a["PKGS"].split() and "alsa-utils" in a["PKGS"].split()


def test_the_strih_role_never_runs_the_monitor(boxes):
    for box in ("strih-snv", "strih-pp"):
        a = _applied(boxes, box)
        assert "strih-stream-monitor" not in a["UNITS"]
        assert a["ROWS"] == "8"
        assert "ffmpeg" not in a["PKGS"].split()


def test_the_entry_maps_to_its_program_and_item():
    r = _bash('printf "%s|%s|%s" "$(strih_session_app_script strih-stream-monitor.service)" '
              '"$(strih_session_app_item strih-stream-monitor.service)" '
              '"$(strih_session_app_program strih-stream-monitor.service)"', sources=(APPS,))
    assert r.returncode == 0, r.stderr
    assert r.stdout == "strih_stream_monitor.py|stream-monitor|/usr/local/bin/strih_stream_monitor.py"


# --- the extra verify rows ------------------------------------------------------------------------------------------

FAKE_SCENES = r'''
import os, sys
assert "--audio-monitor-type" in sys.argv
mode = os.environ.get("FAKE_SCENES_MODE", "none")
if mode == "unreachable":
    print("unreachable: ConnectionRefusedError: [Errno 111] Connection refused")
    sys.exit(2)
print({"none": "OBS_MONITORING_TYPE_NONE", "both": "OBS_MONITORING_TYPE_MONITOR_AND_OUTPUT",
       "absent": "absent"}[mode])
'''


def _grade(tmp_path, boxes, key="stream-pp", hostname="stream-pp", scenes="none", owner=None, fresh=True):
    home = tmp_path / "home"
    prof = home / ".config/obs-studio/basic/profiles/stream-pp"
    prof.mkdir(parents=True)
    settings = {"server": "rtmp://127.0.0.1:1234/live", "use_auth": False}
    if key is not None:
        settings["key"] = key
    (prof / "service.json").write_text(json.dumps({"type": "rtmp_custom", "settings": settings}))
    run = tmp_path / "run"
    run.mkdir()
    st = _state(tmp_path, updated_epoch_s=time.time() if fresh else 1.0)
    (run / "strih-stream-monitor.json").write_text(st.read_text())
    root, proc = asound(tmp_path, owner=owner)
    fs = tmp_path / "fake_scenes.py"
    fs.write_text(FAKE_SCENES)
    env = {"STRIH_BOXES_DIR": str(boxes), "STRIH_SESSION_APPS_RUNTIME_DIR": str(run), "STRIH_SESSION_APPS_EUID": "1000",
           "STRIH_STREAM_MONITOR_HOSTNAME": hostname, "STRIH_STREAM_MONITOR_SCENES": str(fs),
           "STRIH_STREAM_MONITOR_MANIFEST": str(tmp_path / "seed.json"), "FAKE_SCENES_MODE": scenes,
           "STRIH_STREAM_MONITOR_ASOUND": str(root), "STRIH_STREAM_MONITOR_PROC": str(proc)}
    r = _bash('strih_box_load stream-pp\nstrih_stream_monitor_grade_rows "%s" "%s" newlevel' % (REPO, home), env=env)
    assert r.returncode == 0, r.stderr
    rows = {}
    for ln in r.stdout.splitlines():
        st, _, text = ln.partition("|")
        rows[text.split(")", 1)[0].lstrip("(")] = (st, text)
    return rows


def test_the_monitor_rows_all_ok(tmp_path, boxes):
    rows = _grade(tmp_path, boxes)
    assert set(rows) == {"stream-monitor-pass", "stream-monitor-key", "stream-monitor-obs"}
    assert all(st == "OK" for st, _t in rows.values()), rows
    assert "rtmp://127.0.0.1:1234/live/stream-pp" in rows["stream-monitor-key"][1]


def test_the_count_matches_the_rows():
    r = _bash('printf "%s" "$STRIH_STREAM_MONITOR_ROWS"', sources=(APPS,))
    assert r.stdout == "3"


@pytest.mark.parametrize("key", [None, "", "obs-e2e-test"])
def test_the_key_row_fails_a_key_the_monitor_does_not_play(tmp_path, boxes, key):
    st, text = _grade(tmp_path, boxes, key=key)["stream-monitor-key"]
    assert st == "FAIL" and "want 'stream-pp'" in text and "step 3" in text, text


def test_the_key_row_fails_a_hostname_that_is_not_the_box_name(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, hostname="localhost")["stream-monitor-key"]
    assert st == "FAIL" and "localhost" in text, text


def test_the_obs_row_fails_a_monitored_program_audio(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, scenes="both")["stream-monitor-obs"]
    assert st == "FAIL" and "MONITOR_AND_OUTPUT" in text and "Zvuk z mixu" in text, text


def test_the_obs_row_notes_an_obs_that_is_not_running(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, scenes="unreachable")["stream-monitor-obs"]
    assert st == "NOTE" and "Connection refused" in text, text


def test_the_obs_row_fails_a_missing_program_audio_input(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, scenes="absent")["stream-monitor-obs"]
    assert st == "FAIL" and "absent" in text, text


def test_the_pass_row_fails_pipewire_on_the_card(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, owner=(4321, "pipewire"))["stream-monitor-pass"]
    assert st == "FAIL" and "pipewire" in text, text


def test_the_pass_row_fails_a_stale_pass(tmp_path, boxes):
    st, text = _grade(tmp_path, boxes, fresh=False)["stream-monitor-pass"]
    assert st == "FAIL" and "journalctl --user -u strih-stream-monitor.service" in text, text


def test_the_session_apps_grader_prints_the_monitor_rows_only_on_the_stream_role(tmp_path, boxes):
    src = APPS.read_text()
    assert '_strih_session_apps_has "$STRIH_STREAM_MONITOR_UNIT"' in src
    assert "strih_stream_monitor_grade_rows" in src
