#!/usr/bin/env python3
"""stream_operator_audio.py -- the stream operator's headphones through a LOCKED PipeWire (issue 1465 item 3).

WHY (owner 23.10.2026, ROZHODNUTE, verbatim): "na toto mozme pipewire vyskusat ale nesmie sa babrat do inych
veci nez tych ktore mu jasne povolime a definujeme! A znova len chcem pripomenut ze vlc ma na sebe ovladac
hlasitosti a aj chrome youtube ma v prehravani hlasitost a mute. cize mat dalsi ovladac na dalsiu hlasitost
nie je uplne potrebne". And the requirements (comment 6363676384): ONE fixed headphone device, everything the
operator hears there and never on the notebook speakers, the apps' OWN volume controls as the per-app level,
nothing switches by itself, a fixed output latency so lipsync in VLC and Chrome is truthful. Design: issue
comment 6363778378 (Approach 1). The STREAM role only; the strih role's PipeWire (the hub's program sink, the
MiniFuse rules) is never touched.

WHAT this module declares (everything else is denied):
  * the headphone device = the stream monitor's own rule (scripts/strih_stream_monitor.py resolve_headphone:
    the ONE USB audio interface's playback PCM, else the notebook jack ` Analog$`), named by its WirePlumber
    device name: `alsa_card.` + the udev ID_ID, else ID_PATH, sanitised like WirePlumber 0.5's alsa.lua
    (`[^%w_%-%.]` -> `_`); a running session's own pw-dump name for that card wins when it can be read;
  * the WirePlumber drop-in (render_wireplumber): bluetooth off; every ALSA card disabled, then the headphone
    card enabled on the `pro-audio` profile with automatic profile/port switching off (one node per PCM, no
    ports to switch, and ACP never touches the ALSA mixer: on the jack the monitor's unity + Speaker mute is
    the speaker-path control); every node of that card disabled, then its headphone playback PCM enabled and
    renamed SINK_NAME: 2 ch FL/FR, 48 kHz, period 256 + headroom 256, never suspended (PipeWire always holds
    the PCM); device/stream restore off;
  * the PipeWire drop-in (render_pipewire): rate 48000 only, quantum = min = max = 256 (5.3 ms);
  * WirePlumber 0.5 picks a card's profile by priority (find-preferred-profile), so the drop-in also lists
    `pro-audio` first in `device.profile.priority.rules` for the headphone device (review round 1), next to
    `device.profile`;
  * the jack's speaker path + mixer (mixer_row, review round 1): Speaker off, Master / Headphone at 0 dB
    unmuted -- the monitor's own act before every VLC start, read back; a USB box keeps its own mixer (NOTE);
  * the grader (grade): what a live session reads back -- exactly ONE Audio/Sink = SINK_NAME on the headphone
    device, no other ALSA device, no bluetooth device, no capture source, the sink at 100 % and unmuted, the
    clock, the restore settings.

CLI (std-only; the bash lib scripts/lib/stream-operator-audio.sh runs it for setup-strih step 12 / verify):
  render-pipewire                                 print the PipeWire drop-in
  render-wireplumber [--pw-dump F]                print the WirePlumber drop-in for the resolved device (rc 1)
  grade --pw-dump F --settings F --sm-settings F  print `OK|(...) ...` / `FAIL|(...) ...` rows
Seams: STRIH_STREAM_MONITOR_ASOUND (the monitor's /proc/asound), STREAM_OPERATOR_AUDIO_SYS (/sys),
STREAM_OPERATOR_AUDIO_UDEVADM (udevadm).
"""
import argparse
import json
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import strih_stream_monitor as mon  # noqa: E402

SINK_NAME = mon.PULSE_SINK_DEFAULT
SINK_DESCRIPTION = "Operator headphones"
RATE = 48000
QUANTUM = 256
PERIOD_SIZE = 256
HEADROOM = 256
CHANNELS = 2
POSITION = "FL,FR"
RESTORE_SETTINGS = ("device.restore-profile", "device.restore-routes", "node.stream.restore-props",
                    "node.stream.restore-target")
PIPEWIRE_DROPIN = "60-stream-operator-clock.conf"
WIREPLUMBER_DROPIN = "60-stream-operator-headphones.conf"
SYS_ROOT = os.environ.get("STREAM_OPERATOR_AUDIO_SYS", "/sys")
UDEVADM = os.environ.get("STREAM_OPERATOR_AUDIO_UDEVADM", "udevadm")
AMIXER = os.environ.get("STREAM_OPERATOR_AUDIO_AMIXER", "amixer")
# the jack's controls the monitor sets to 0 dB unmuted before every VLC start (mon.UNITY_CONTROLS, switch ones)
MIXER_UNITY = tuple(name for name, has_switch in mon.UNITY_CONTROLS if has_switch)
DEVICE_PREFIX = "alsa_card."
# the rows grade() prints, in order (scripts/lib/stream-operator-audio.sh counts them)
GRADE_ROWS = ("operator-audio-sink", "operator-audio-cards", "operator-audio-capture", "operator-audio-volume",
              "operator-audio-clock", "operator-audio-restore")


def wp_name_part(value):
    """WirePlumber 0.5 alsa.lua's sanitising of a device name: every byte that is not [A-Za-z0-9_.-] -> `_`."""
    return re.sub(r"[^A-Za-z0-9_.-]", "_", value)


def device_name_from_udev(props):
    """The WirePlumber device name of a sound card from its udev properties: ID_ID (USB: usb-<serial>-<if>),
    else ID_PATH (PCI: pci-<address>); None when neither is set."""
    v = props.get("ID_ID") or props.get("ID_PATH")
    return DEVICE_PREFIX + wp_name_part(v) if v else None


def udev_props(card, sys_root=SYS_ROOT, udevadm=UDEVADM):
    """(props, None) of /sys/class/sound/card<CARD> from `udevadm info --query=property`, else (None, why)."""
    path = "%s/class/sound/card%d" % (sys_root, card)
    try:
        r = subprocess.run([udevadm, "info", "--query=property", "--path=%s" % path], capture_output=True,
                           text=True, timeout=10)
    except (OSError, subprocess.TimeoutExpired) as e:
        return None, "udevadm info %s failed: %s" % (path, e)
    if r.returncode != 0:
        return None, "udevadm info %s failed (rc %d): %s" % (path, r.returncode, (r.stderr or "").strip())
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln), None


def _objects(objs, kind):
    for o in objs or []:
        if isinstance(o, dict) and o.get("type") == "PipeWire:Interface:%s" % kind:
            yield o.get("id"), ((o.get("info") or {}).get("props") or {}), (o.get("info") or {})


def device_name_from_pw_dump(objs, card):
    """The device name a running session gave ALSA card CARD (its `api.alsa.card`), else None."""
    for _i, props, _info in _objects(objs, "Device"):
        if props.get("device.api") == "alsa" and str(props.get("api.alsa.card")) == str(card):
            name = props.get("device.name")
            if isinstance(name, str) and name.startswith(DEVICE_PREFIX):
                return name
    return None


def resolve_device(asound_root=None, pw_objects=None, sys_root=SYS_ROOT, udevadm=UDEVADM):
    """(info, None) for the headphone device: {card, card_id, device, name, usb, device_name, source}, or
    (None, why). The PCM is the stream monitor's own rule (never a second copy of it)."""
    root = asound_root if asound_root is not None else mon.ASOUND_ROOT
    pcm, why, note = mon.resolve_headphone(mon.list_playback_pcms(root), None)
    if pcm is None:
        return None, why
    info = dict(pcm, note=note)
    name = device_name_from_pw_dump(pw_objects, pcm["card"])
    if name:
        info.update(device_name=name, source="pw-dump")
        return info, None
    props, why = udev_props(pcm["card"], sys_root, udevadm)
    if props is None:
        return None, why
    name = device_name_from_udev(props)
    if not name:
        return None, "card%d has neither ID_ID nor ID_PATH in udev -- its WirePlumber name is unknown" % pcm["card"]
    info.update(device_name=name, source="udev")
    return info, None


def render_pipewire():
    """The PipeWire drop-in: one clock rate, one quantum."""
    return """# issue 1465 item 3 (owner 23.10.2026): the stream operator's headphones through a LOCKED PipeWire.
# Installed by setup-strih step 12 on the STREAM role (scripts/stream_operator_audio.py); do not edit by hand.
# One clock: 48 kHz only, the graph quantum fixed at %(q)d frames (%(ms).1f ms) -- a client can neither move the
# rate nor stretch the latency, so what VLC and Chrome report as their output latency stays true.
context.properties = {
    default.clock.rate          = %(r)d
    default.clock.allowed-rates = [ %(r)d ]
    default.clock.quantum       = %(q)d
    default.clock.min-quantum   = %(q)d
    default.clock.max-quantum   = %(q)d
}
""" % {"r": RATE, "q": QUANTUM, "ms": QUANTUM * 1000.0 / RATE}


def render_wireplumber(device_name, pcm_device):
    """The WirePlumber drop-in allowing the ONE headphone output of DEVICE_NAME (PCM PCM_DEVICE)."""
    stem = device_name[len(DEVICE_PREFIX):] if device_name.startswith(DEVICE_PREFIX) else device_name
    restore = "\n".join("    %s = false" % k for k in RESTORE_SETTINGS)
    return """# issue 1465 item 3 (owner 23.10.2026): "nesmie sa babrat do inych veci nez tych ktore mu jasne povolime a
# definujeme" -- the stream operator's headphones through a LOCKED PipeWire, everything else denied.
# Installed by setup-strih step 12 on the STREAM role (scripts/stream_operator_audio.py); do not edit by hand.
# The headphone device (the stream monitor's rule: the ONE USB audio interface, else the jack): %(dev)s

# no bluetooth audio device at all
wireplumber.profiles = {
  main = {
    monitor.bluez = disabled
    monitor.bluez-midi = disabled
  }
}

# nothing is restored by itself: no profile, route, stream volume or stream target memory
wireplumber.settings = {
%(restore)s
}

monitor.alsa.rules = [
  # every ALSA card off (the notebook speakers, HDMI, any other interface) ...
  {
    matches = [ { device.name = "~alsa_card.*" } ]
    actions = { update-props = { device.disabled = true } }
  }
  # ... except the headphone device: pro-audio (one node per PCM, no ports, the ALSA mixer left alone),
  # never switched automatically
  {
    matches = [ { device.name = "%(dev)s" } ]
    actions = {
      update-props = {
        device.disabled = false
        device.profile = "pro-audio"
        api.acp.auto-profile = false
        api.acp.auto-port = false
      }
    }
  }
  # every node of that card off (its capture sources, its other PCMs) ...
  {
    matches = [ { node.name = "~^alsa_(input|output)[.]%(stem)s[.].*$" } ]
    actions = { update-props = { node.disabled = true } }
  }
  # ... except its headphone playback PCM: the operator's ONE sink, fixed format, fixed latency, never suspended
  {
    matches = [ { node.name = "alsa_output.%(stem)s.pro-output-%(pcm)d" } ]
    actions = {
      update-props = {
        node.disabled = false
        node.name = "%(sink)s"
        node.description = "%(desc)s"
        audio.rate = %(rate)d
        audio.channels = %(ch)d
        audio.position = "%(pos)s"
        api.alsa.period-size = %(period)d
        api.alsa.headroom = %(headroom)d
        session.suspend-timeout-seconds = 0
      }
    }
  }
]

# WirePlumber 0.5 chooses the profile by priority (find-preferred-profile): pro-audio for the headphone device
device.profile.priority.rules = [
  {
    matches = [ { device.name = "%(dev)s" } ]
    actions = { update-props = { priorities = [ "pro-audio" ] } }
  }
]
""" % {"dev": device_name, "stem": stem, "pcm": pcm_device, "sink": SINK_NAME, "desc": SINK_DESCRIPTION,
       "rate": RATE, "ch": CHANNELS, "pos": POSITION, "period": PERIOD_SIZE, "headroom": HEADROOM,
       "restore": restore}


# --- the grader --------------------------------------------------------------------------------------------

def parse_metadata(text):
    """`pw-metadata -n NAME` output -> {key: value} (`update: id:N key:'K' value:'V' type:'T'`)."""
    return dict(re.findall(r"key:'([^']*)' value:'([^']*)'", text or ""))


def _props_volume(info):
    for p in ((info.get("params") or {}).get("Props") or []):
        if isinstance(p, dict) and ("channelVolumes" in p or "volume" in p):
            return p
    return None


def grade(objs, settings_text, sm_text, expect_device):
    """[(OK|FAIL, '(operator-audio-...) text')] for a live session's pw-dump OBJS (None = unreadable), the
    `settings` and `sm-settings` metadata texts and the expected headphone device name."""
    rows = []
    if not isinstance(objs, list) or not objs:
        why = "pw-dump of the operator session is unreadable (the session is down?) -- never a pass"
        return [("FAIL", "(%s) %s" % (k, why)) for k in GRADE_ROWS]
    devices = {i: p for i, p, _ in _objects(objs, "Device")}
    nodes = [(i, p, info) for i, p, info in _objects(objs, "Node")]
    sinks = [(i, p, info) for i, p, info in nodes if p.get("media.class") == "Audio/Sink"]
    names = [p.get("node.name") for _i, p, _info in sinks]
    sink = sinks[0] if len(sinks) == 1 and names == [SINK_NAME] else None
    if sink is None:
        rows.append(("FAIL", "(operator-audio-sink) the sinks are %s, not exactly one '%s' -- re-run setup-strih "
                     "step 12 and restart the operator PipeWire" % (names or "none", SINK_NAME)))
    else:
        dev = devices.get(sink[1].get("device.id"), {}).get("device.name")
        if dev != expect_device:
            rows.append(("FAIL", "(operator-audio-sink) '%s' sits on %s, not the headphone device %s -- re-run "
                         "setup-strih step 12 (was the headphone box plugged in at setup?)" % (SINK_NAME, dev, expect_device)))
        else:
            rows.append(("OK", "(operator-audio-sink) the one sink '%s' on the headphone device %s"
                         % (SINK_NAME, expect_device)))
    alsa = sorted(p.get("device.name") or "?" for p in devices.values() if p.get("device.api") == "alsa")
    other = sorted(p.get("device.name") or "?" for p in devices.values()
                   if str(p.get("device.api") or "").startswith("bluez"))
    if alsa == [expect_device] and not other:
        rows.append(("OK", "(operator-audio-cards) the only audio device is %s (no other card, no bluetooth)"
                     % expect_device))
    else:
        rows.append(("FAIL", "(operator-audio-cards) audio devices %s (expected only %s) -- every other card and "
                     "bluetooth must be disabled" % (alsa + other, expect_device)))
    sources = sorted(p.get("node.name") or "?" for _i, p, _info in nodes
                     if str(p.get("media.class") or "").startswith("Audio/Source"))
    if sources:
        rows.append(("FAIL", "(operator-audio-capture) capture sources %s exist -- nothing may record the "
                     "operator's room" % sources))
    else:
        rows.append(("OK", "(operator-audio-capture) no capture source"))
    vol = _props_volume(sink[2]) if sink else None
    if vol is None:
        rows.append(("FAIL", "(operator-audio-volume) the sink's volume is unreadable (no sink '%s')" % SINK_NAME))
    else:
        levels = [vol.get("volume")] + list(vol.get("channelVolumes") or [])
        unity = all(isinstance(v, (int, float)) and abs(v - 1.0) < 1e-3 for v in levels if v is not None)
        if unity and vol.get("mute") is False:
            rows.append(("OK", "(operator-audio-volume) the sink at 100 %, unmuted: the level is each app's own"))
        else:
            rows.append(("FAIL", "(operator-audio-volume) the sink reads volume %s mute %s -- pin it: wpctl set-volume "
                         "%s 1.0 && wpctl set-mute %s 0 (as the operator)"
                         % (levels, vol.get("mute"), sink[0], sink[0])))
    st = parse_metadata(settings_text)
    want = {"clock.rate": str(RATE), "clock.quantum": str(QUANTUM), "clock.min-quantum": str(QUANTUM),
            "clock.max-quantum": str(QUANTUM)}
    bad = ["%s=%s" % (k, st.get(k)) for k, v in want.items() if st.get(k) != v]
    if re.findall(r"\d+", st.get("clock.allowed-rates", "")) != [str(RATE)]:
        bad.append("clock.allowed-rates=%s" % st.get("clock.allowed-rates"))
    if st.get("clock.force-quantum", "0") not in ("0", str(QUANTUM)):
        bad.append("clock.force-quantum=%s" % st.get("clock.force-quantum"))
    if st.get("clock.force-rate", "0") not in ("0", str(RATE)):
        bad.append("clock.force-rate=%s" % st.get("clock.force-rate"))
    if bad:
        rows.append(("FAIL", "(operator-audio-clock) %s (declared: %d Hz only, quantum = min = max = %d) -- restart "
                     "the operator PipeWire so %s applies" % (", ".join(bad), RATE, QUANTUM, PIPEWIRE_DROPIN)))
    else:
        rows.append(("OK", "(operator-audio-clock) %d Hz only, quantum = min = max = %d" % (RATE, QUANTUM)))
    sm = parse_metadata(sm_text)
    left = ["%s=%s" % (k, sm.get(k)) for k in RESTORE_SETTINGS if sm.get(k) != "false"]
    if left:
        rows.append(("FAIL", "(operator-audio-restore) %s (declared false) -- restart wireplumber so %s applies"
                     % (", ".join(left), WIREPLUMBER_DROPIN)))
    else:
        rows.append(("OK", "(operator-audio-restore) no profile / route / stream volume / stream target restore"))
    return rows


def _amixer_sget(amixer, card_id, name):
    """(rc, text) of `amixer -c CARD sget NAME` in the C locale: rc 0 + its output; rc "absent" when the card
    has no such control (amixer's own "Unable to find simple control"); rc None + why for any other failure
    (review round 2: a wrong card id or a broken amixer is never a control the jack lacks)."""
    try:
        r = subprocess.run([amixer, "-c", card_id, "sget", name], capture_output=True, text=True, timeout=10,
                           env=dict(os.environ, LC_ALL="C"))
    except (OSError, subprocess.TimeoutExpired) as e:
        return None, str(e)
    if r.returncode == 0:
        return 0, r.stdout
    err = (r.stderr or r.stdout or "").strip()
    if "Unable to find simple control" in err:
        return "absent", err
    return None, "rc %d: %s" % (r.returncode, err or "no output")


def mixer_row(info, amixer=AMIXER):
    """(OK|FAIL|NOTE, '(operator-audio-mixer) ...'): on the jack, Speaker off and MIXER_UNITY at 0 dB unmuted
    (a control the card lacks is named, never failed); a USB box keeps its own mixer (a NOTE)."""
    tag = "(operator-audio-mixer)"
    card = info.get("card_id") or "?"
    if info.get("usb"):
        return "NOTE", ("%s %s is a USB audio interface: its own mixer (the box's knob) is left as it is"
                        % (tag, card))
    ok, bad = [], []
    rc, out = _amixer_sget(amixer, card, "Speaker")
    if rc is None:
        return "FAIL", "%s amixer failed on %s (%s) -- the speaker path is unread" % (tag, card, out)
    if rc == "absent":
        ok.append("no Speaker control")
    else:
        sw = re.findall(r"\[(on|off)\]", out)
        if sw and "on" not in sw:
            ok.append("Speaker off")
        else:
            bad.append("Speaker %s" % ("on" if sw else "switch unreadable"))
    for name in MIXER_UNITY:
        rc, out = _amixer_sget(amixer, card, name)
        if rc is None:
            return "FAIL", "%s amixer failed on %s (%s) -- %s is unread" % (tag, card, out, name)
        if rc == "absent":
            ok.append("no %s control" % name)
            continue
        dbs = [float(v) for v in re.findall(r"\[(-?\d+(?:\.\d+)?)dB\]", out)]
        sw = re.findall(r"\[(on|off)\]", out)
        if not dbs or any(abs(v) > 0.01 for v in dbs):
            bad.append("%s at %s" % (name, ", ".join("%.2f dB" % v for v in dbs) or "an unread level"))
        elif "off" in sw:
            bad.append("%s muted" % name)
        else:
            ok.append("%s 0 dB" % name)
    if bad:
        return "FAIL", ("%s %s on %s (the monitor sets Speaker off and %s to 0 dB before every VLC start: "
                        "systemctl --user restart strih-stream-monitor)" % (tag, ", ".join(bad), card,
                                                                            " / ".join(MIXER_UNITY)))
    return "OK", "%s the jack %s: %s" % (tag, card, ", ".join(ok))


def _read_objs(path):
    if not path:
        return None
    try:
        with open(path) as fh:
            d = json.load(fh)
    except (OSError, ValueError):
        return None
    return d if isinstance(d, list) else None


def _read_text(path):
    try:
        with open(path) as fh:
            return fh.read()
    except OSError:
        return ""


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("render-pipewire")
    rw = sub.add_parser("render-wireplumber")
    rw.add_argument("--pw-dump", default=None, help="a running session's pw-dump (its device names win)")
    g = sub.add_parser("grade")
    g.add_argument("--pw-dump", required=True)
    g.add_argument("--settings", required=True)
    g.add_argument("--sm-settings", required=True)
    a = ap.parse_args(argv)
    if a.cmd == "render-pipewire":
        sys.stdout.write(render_pipewire())
        return 0
    objs = _read_objs(a.pw_dump)
    info, why = resolve_device(None, objs)
    if info is None:
        print("stream-operator-audio: no headphone device: %s" % why, file=sys.stderr)
        if a.cmd == "grade":
            for k in GRADE_ROWS + ("operator-audio-mixer",):
                print("FAIL|(%s) no headphone device to grade against: %s" % (k, why))
            return 0
        return 1
    if a.cmd == "render-wireplumber":
        if info.get("note"):
            print("stream-operator-audio: %s" % info["note"], file=sys.stderr)
        print("stream-operator-audio: headphone %s:%d %s -> %s (from %s)" % (
            info["card_id"], info["device"], info["name"], info["device_name"], info["source"]), file=sys.stderr)
        sys.stdout.write(render_wireplumber(info["device_name"], info["device"]))
        return 0
    for st, text in grade(objs, _read_text(a.settings), _read_text(a.sm_settings), info["device_name"]):
        print("%s|%s" % (st, text))
    print("%s|%s" % mixer_row(info))
    return 0


if __name__ == "__main__":
    sys.exit(main())
