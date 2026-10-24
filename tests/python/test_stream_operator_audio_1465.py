"""Issue 1465 item 3 (owner 23.10.2026): the stream operator's headphones through a LOCKED PipeWire.

ROZHODNUTE (owner, verbatim): "na toto mozme pipewire vyskusat ale nesmie sa babrat do inych veci nez tych
ktore mu jasne povolime a definujeme! A znova len chcem pripomenut ze vlc ma na sebe ovladac hlasitosti a aj
chrome youtube ma v prehravani hlasitost a mute. cize mat dalsi ovladac na dalsiu hlasitost nie je uplne
potrebne". Design: issue comment 6363778378 (Approach 1).

What this pins (Tier-0: pytest + bash, fake udevadm / pw-dump / pw-metadata, no rig, no audio device):
  * scripts/stream_operator_audio.py -- the ONE headphone device by the monitor's own rule (the notebook jack,
    never a USB interface, ROZHODNUTE 6383739315), its WirePlumber device name, the two rendered drop-ins (every other ALSA
    card off, bluetooth off, the card's capture and other PCMs off, rate 48000, quantum = min = max = 256,
    the restore settings off) and the grader of what a live session reads back;
  * scripts/lib/stream-operator-audio.sh -- setup-strih step 12 on the STREAM role (the two drop-ins, the
    strih-role leftovers removed) and verify-strih's rows;
  * setup-strih step 12 / verify-strih item 9b -- the role branch (the strih role is untouched).
"""
import importlib.util
import json
import os
import pwd
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import LEAKY, PROVISION, REPO, asound  # noqa: E402
from test_strih_stream_monitor_1465 import add_usb_card  # noqa: E402

SCRIPTS = REPO / "scripts"
SOA = SCRIPTS / "stream_operator_audio.py"
LIB = SCRIPTS / "lib" / "stream-operator-audio.sh"
BASELINE = SCRIPTS / "lib" / "obs-box-baseline.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
ME = pwd.getpwuid(os.getuid()).pw_name
SINK = "stream-operator-headphones"
SCARLETT = "alsa_card.usb-Focusrite_Scarlett_Solo_4th_Gen_S1ABCDEF-00"
JACK_PP = "alsa_card.pci-0000_c4_00.6"
SEAMS = ("STREAM_OPERATOR_AUDIO_UDEVADM", "STREAM_OPERATOR_AUDIO_SYS", "STREAM_OPERATOR_AUDIO_SCENES")


def _soa():
    spec = importlib.util.spec_from_file_location("stream_operator_audio_1465", SOA)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


FAKE_UDEVADM = r'''#!/bin/sh
# udevadm info --query=property --path=<sys>/class/sound/cardN
case "$3" in
  */card2) printf 'DEVPATH=/devices/x/sound/card2\nID_BUS=usb\nID_ID=usb-Focusrite_Scarlett_Solo_4th_Gen_S1ABCDEF-00\nID_PATH=pci-0000:00:14.0-usb-0:1:1.0\n' ;;
  */card1) printf 'DEVPATH=/devices/pci/sound/card1\nID_PATH=pci-0000:c4:00.6\n' ;;
  */card0) printf 'DEVPATH=/devices/pci/sound/card0\nID_PATH=pci-0000:c4:00.1\n' ;;
  *) echo "Unknown device" >&2; exit 4 ;;
esac
'''


def _fakebin(tmp_path, **tools):
    d = tmp_path / "fakebin"
    d.mkdir(exist_ok=True)
    for name, body in tools.items():
        (d / name).write_text(body)
        (d / name).chmod(0o755)
    return d


def _env(tmp_path, root, extra=None, **tools):
    tools.setdefault("udevadm", FAKE_UDEVADM)
    fb = _fakebin(tmp_path, **tools)
    e = {k: v for k, v in os.environ.items() if k not in LEAKY + SEAMS}
    e.update({"PATH": "%s:%s" % (fb, os.environ["PATH"]), "STRIH_STREAM_MONITOR_ASOUND": str(root),
              "STREAM_OPERATOR_AUDIO_SYS": str(tmp_path / "sys")})
    e.update(extra or {})
    return e


# --- the pw-dump / pw-metadata shapes a live 0.5 session prints ------------------------------------------------

def dev(i, name, api="alsa", card=2):
    return {"id": i, "type": "PipeWire:Interface:Device",
            "info": {"props": {"device.name": name, "device.api": api, "api.alsa.card": card}}}


def node(i, name, cls, dev_id, vol=1.0, mute=False):
    return {"id": i, "type": "PipeWire:Interface:Node",
            "info": {"props": {"node.name": name, "media.class": cls, "device.id": dev_id},
                     "params": {"Props": [{"volume": vol, "mute": mute, "channelVolumes": [vol, vol]},
                                          {"params": []}]}}}


def healthy(device=JACK_PP, card=1):
    return [{"id": 0, "type": "PipeWire:Interface:Core", "info": {"props": {}}},
            dev(40, device, card=card), node(41, SINK, "Audio/Sink", 40),
            node(50, "Chromium", "Stream/Output/Audio", None)]


SETTINGS = """Found "settings" metadata 32
update: id:0 key:'log.level' value:'2' type:''
update: id:0 key:'clock.rate' value:'48000' type:''
update: id:0 key:'clock.allowed-rates' value:'[ 48000 ]' type:''
update: id:0 key:'clock.quantum' value:'256' type:''
update: id:0 key:'clock.min-quantum' value:'256' type:''
update: id:0 key:'clock.max-quantum' value:'256' type:''
update: id:0 key:'clock.force-quantum' value:'0' type:''
update: id:0 key:'clock.force-rate' value:'0' type:''
"""

SM = """Found "sm-settings" metadata 35
update: id:0 key:'device.restore-profile' value:'false' type:'Spa:String:JSON'
update: id:0 key:'device.restore-routes' value:'false' type:'Spa:String:JSON'
update: id:0 key:'node.stream.restore-props' value:'false' type:'Spa:String:JSON'
update: id:0 key:'node.stream.restore-target' value:'false' type:'Spa:String:JSON'
update: id:0 key:'node.stream.default-playback-volume' value:'1.000000' type:'Spa:String:JSON'
"""


def snv_asound(tmp_path):
    """A notebook jack (`ALC257 Analog` on Generic_1) + a plugged-in Scarlett Solo 4th Gen (USB, card `Gen`),
    the stream-snv situation: since ROZHODNUTE 6383739315 the jack is the device, the Focusrite never."""
    root, proc = asound(tmp_path)
    add_usb_card(root, 2, "Gen")
    return root, proc


# --- 1. the device + the rendered drop-ins ---------------------------------------------------------------------

def test_the_wireplumber_name_part_is_sanitised_like_alsa_lua():
    m = _soa()
    assert m.wp_name_part("pci-0000:00:1f.3") == "pci-0000_00_1f.3"
    assert m.wp_name_part("usb-Focusrite_Scarlett Solo/x-00") == "usb-Focusrite_Scarlett_Solo_x-00"


def test_the_udev_name_prefers_the_usb_id_then_the_path():
    m = _soa()
    assert m.device_name_from_udev({"ID_ID": "usb-A_B-00", "ID_PATH": "pci-0000:00:14.0-usb-0:1:1.0"}) \
        == "alsa_card.usb-A_B-00"
    assert m.device_name_from_udev({"ID_PATH": "pci-0000:c4:00.6"}) == JACK_PP
    assert m.device_name_from_udev({"DEVPATH": "/x"}) is None


def test_a_plugged_in_focusrite_never_becomes_the_headphone_device(tmp_path):
    m = _soa()
    root, _ = snv_asound(tmp_path)
    fb = _fakebin(tmp_path, udevadm=FAKE_UDEVADM)
    info, why = m.resolve_device(str(root), None, str(tmp_path / "sys"), str(fb / "udevadm"))
    assert why is None, why
    assert (info["card"], info["card_id"], info["device"], info["usb"]) == (1, "Generic_1", 0, False)
    assert info["device_name"] == JACK_PP and info["source"] == "udev"
    assert "Gen:0 USB Audio" in info["note"], info


def test_the_jack_is_the_headphone_device_on_stream_pp(tmp_path):
    m = _soa()
    root, _ = asound(tmp_path)
    fb = _fakebin(tmp_path, udevadm=FAKE_UDEVADM)
    info, why = m.resolve_device(str(root), None, str(tmp_path / "sys"), str(fb / "udevadm"))
    assert why is None, why
    assert (info["card_id"], info["device"], info["usb"], info["device_name"]) == ("Generic_1", 0, False, JACK_PP)


def test_a_running_sessions_own_device_name_wins_over_udev(tmp_path):
    m = _soa()
    root, _ = snv_asound(tmp_path)
    fb = _fakebin(tmp_path, udevadm=FAKE_UDEVADM)
    live = [dev(40, "alsa_card.usb-Focusrite_live-00", card=2), dev(41, "alsa_card.pci-live_jack", card="1")]
    info, why = m.resolve_device(str(root), live, str(tmp_path / "sys"), str(fb / "udevadm"))
    assert why is None and info["device_name"] == "alsa_card.pci-live_jack" and info["source"] == "pw-dump"


def test_no_headphone_device_is_a_reason_never_a_guess(tmp_path):
    m = _soa()
    root, _ = asound(tmp_path, analog=False)
    info, why = m.resolve_device(str(root), None, str(tmp_path / "sys"), "/nonexistent/udevadm")
    assert info is None and "no playback PCM" in why, why
    root2, _ = asound(tmp_path / "b")
    info, why = m.resolve_device(str(root2), None, str(tmp_path / "sys"), "/nonexistent/udevadm")
    assert info is None and "udevadm" in why, why


def test_the_pipewire_dropin_fixes_the_clock():
    text = _soa().render_pipewire()
    for want in ("context.properties", "default.clock.rate          = 48000",
                 "default.clock.allowed-rates = [ 48000 ]", "default.clock.quantum       = 256",
                 "default.clock.min-quantum   = 256", "default.clock.max-quantum   = 256"):
        assert want in text, want
    assert text.endswith("\n") and not text.endswith("\n\n")


def test_the_wireplumber_dropin_allows_the_one_headphone_output_and_nothing_else():
    m = _soa()
    text = m.render_wireplumber(SCARLETT, 0)
    stem = SCARLETT[len("alsa_card."):]
    # bluetooth off, nothing restored
    assert "monitor.bluez = disabled" in text
    for key in ("device.restore-profile", "device.restore-routes", "node.stream.restore-props",
                "node.stream.restore-target"):
        assert "%s = false" % key in text, key
    # every ALSA card off FIRST, then the headphone card on, pro-audio, nothing automatic
    off = text.index('device.name = "~alsa_card.*"')
    on = text.index('device.name = "%s"' % SCARLETT)
    assert off < on
    for want in ('device.profile = "pro-audio"', "api.acp.auto-profile = false", "api.acp.auto-port = false",
                 "device.disabled = false"):
        assert want in text[on:], want
    # review round 1: WirePlumber 0.5 picks the profile by priority (find-preferred-profile): pro-audio first
    pri = text.index("device.profile.priority.rules")
    assert 'device.name = "%s"' % SCARLETT in text[pri:] and 'priorities = [ "pro-audio" ]' in text[pri:]
    # every node of the card off, then the ONE playback PCM on as the operator sink
    all_nodes = text.index('node.name = "~^alsa_(input|output)[.]%s[.].*$"' % stem)
    hp = text.index('node.name = "alsa_output.%s.pro-output-0"' % stem)
    assert on < all_nodes < hp
    tail = text[hp:]
    for want in ("node.disabled = false", 'node.name = "%s"' % SINK, "audio.rate = 48000", "audio.channels = 2",
                 'audio.position = "FL,FR"', "api.alsa.period-size = 256", "api.alsa.headroom = 256",
                 "session.suspend-timeout-seconds = 0"):
        assert want in tail, want
    assert text.endswith("\n") and not text.endswith("\n\n")


def test_the_headphone_pcm_device_number_names_the_node():
    assert 'node.name = "alsa_output.pci-0000_c4_00.6.pro-output-3"' in _soa().render_wireplumber(JACK_PP, 3)


def test_the_cli_renders_the_dropin_for_the_resolved_device(tmp_path):
    root, _ = snv_asound(tmp_path)
    r = subprocess.run([sys.executable, str(SOA), "render-wireplumber"], capture_output=True, text=True,
                       env=_env(tmp_path, root), timeout=30)
    assert r.returncode == 0, r.stderr
    assert r.stdout == _soa().render_wireplumber(JACK_PP, 0)
    assert "Generic_1:0" in r.stderr and JACK_PP in r.stderr and "udev" in r.stderr, r.stderr
    assert SCARLETT not in r.stdout


def test_the_cli_refuses_without_a_headphone_device(tmp_path):
    root, _ = asound(tmp_path, analog=False)
    r = subprocess.run([sys.executable, str(SOA), "render-wireplumber"], capture_output=True, text=True,
                       env=_env(tmp_path, root), timeout=30)
    assert r.returncode == 1 and r.stdout == "" and "no playback PCM" in r.stderr, (r.stdout, r.stderr)


# --- 2. the grader -------------------------------------------------------------------------------------------

def _grade(objects=None, settings=SETTINGS, sm=SM, expect=JACK_PP):
    rows = _soa().grade(healthy() if objects is None else objects, settings, sm, expect)
    out = {}
    for st, text in rows:
        out[text.split(")", 1)[0].lstrip("(")] = (st, text)
    return out


ROWS = ("operator-audio-sink", "operator-audio-cards", "operator-audio-capture", "operator-audio-volume",
        "operator-audio-clock", "operator-audio-restore")
MIXER = "operator-audio-mixer"


def test_a_locked_session_passes_every_row():
    g = _grade()
    assert sorted(g) == sorted(ROWS)
    assert all(st == "OK" for st, _ in g.values()), g


def test_a_second_sink_fails():
    objs = healthy() + [node(60, "alsa_output.pci-x.analog-stereo", "Audio/Sink", 40)]
    st, text = _grade(objs)["operator-audio-sink"]
    assert st == "FAIL" and "alsa_output.pci-x.analog-stereo" in text, text


def test_the_sink_on_another_card_fails():
    st, text = _grade(expect="alsa_card.pci-other")["operator-audio-sink"]
    assert st == "FAIL" and "alsa_card.pci-other" in text, text


def test_no_sink_fails_the_sink_and_the_volume_rows():
    objs = [o for o in healthy() if o["id"] != 41]
    g = _grade(objs)
    assert g["operator-audio-sink"][0] == "FAIL" and g["operator-audio-volume"][0] == "FAIL"


def test_another_card_or_a_bluetooth_device_fails():
    st, text = _grade(healthy() + [dev(70, "alsa_card.pci-0000_01_00.1", card=1)])["operator-audio-cards"]
    assert st == "FAIL" and "alsa_card.pci-0000_01_00.1" in text, text
    st, text = _grade(healthy() + [dev(71, "bluez_card.AA_BB", api="bluez5", card=None)])["operator-audio-cards"]
    assert st == "FAIL" and "bluez_card.AA_BB" in text, text


def test_a_capture_source_fails():
    st, text = _grade(healthy() + [node(80, "alsa_input.x.pro-input-0", "Audio/Source", 40)])["operator-audio-capture"]
    assert st == "FAIL" and "alsa_input.x.pro-input-0" in text, text


@pytest.mark.parametrize("vol,mute,want", [(0.343, False, "OK"), (1.0, True, "NOTE"), (1.2, False, "FAIL")])
def test_the_sink_level_is_the_operators_never_forced(vol, mute, want):
    # ROZHODNUTE 6383739315: the global headphone volume is the operator's (panel item + volume keys), capped
    # at 100 %; verify reads it, never pins it back to 100 % (0.343 linear = 70 % on wpctl's cubic scale)
    objs = [o for o in healthy() if o["id"] != 41] + [node(41, SINK, "Audio/Sink", 40, vol=vol, mute=mute)]
    st, text = _grade(objs)["operator-audio-volume"]
    assert st == want, text
    if want == "OK":
        assert "70 %" in text and "set-volume" not in text, text


@pytest.mark.parametrize("key,value", [("clock.rate", "44100"), ("clock.quantum", "1024"),
                                       ("clock.min-quantum", "1024"), ("clock.max-quantum", "8192"),
                                       ("clock.allowed-rates", "[ 44100 48000 ]"), ("clock.force-quantum", "512")])
def test_a_clock_that_is_not_the_declared_one_fails(key, value):
    lines = [ln if "key:'%s'" % key not in ln else "update: id:0 key:'%s' value:'%s' type:''" % (key, value)
             for ln in SETTINGS.splitlines()]
    st, text = _grade(settings="\n".join(lines))["operator-audio-clock"]
    assert st == "FAIL" and key in text, text


def test_a_restore_setting_left_on_fails():
    st, text = _grade(sm=SM.replace("'node.stream.restore-props' value:'false'",
                                    "'node.stream.restore-props' value:'true'"))["operator-audio-restore"]
    assert st == "FAIL" and "node.stream.restore-props" in text, text


def test_an_unreadable_session_fails_every_row_never_passes():
    rows = _soa().grade(None, "", "", JACK_PP)
    assert len(rows) == len(ROWS) and all(st == "FAIL" for st, _ in rows), rows


# --- 3. the lib: setup step 12 on the stream role + verify rows ----------------------------------------------

def _bash(tmp_path, body, env, *args):
    e = dict(env)
    script = ('set -euo pipefail\n. "%s"\n. "%s"\nfail() { echo "FAIL: $*" >&2; exit 1; }\n%s'
              % (PROVISION, BASELINE, body))
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=120)


FAKE_PW_DUMP = '#!/bin/sh\ncat "$FAKE_PW_DUMP_FILE" 2>/dev/null || exit 1\n'
FAKE_PW_METADATA = '#!/bin/sh\n# pw-metadata -n NAME\ncat "$FAKE_PW_META_DIR/$2" 2>/dev/null || exit 1\n'
FAKE_SCENES = r'''#!/usr/bin/env python3
import os, sys
assert "--pulse-capture-inputs" in sys.argv, sys.argv
mode = os.environ.get("FAKE_SCENES_MODE", "none")
if mode == "down":
    print("unreachable: ConnectionRefusedError: refused"); sys.exit(2)
print({"none": "pulse capture inputs: none",
       "one": "pulse capture inputs: 'Zvuk z mixu' (pulse_input_capture, scene)"}[mode])
'''


FAKE_AMIXER_RW = r'''#!/usr/bin/env python3
# amixer [-q] -c CARD sget|sset NAME [VALUES...]: sget reads $FAKE_MIX_DIR/NAME, sset is logged
import os, sys
args = [a for a in sys.argv[1:] if a != "-q"]
log = os.environ.get("FAKE_AMIXER_LOG")
if log:
    with open(log, "a") as fh:
        fh.write(" ".join(sys.argv[1:]) + "\n")
card, verb, name = args[1], args[2], args[3]
if name == os.environ.get("FAKE_AMIXER_FAIL"):
    sys.stderr.write("amixer: Invalid card number.\n"); sys.exit(1)
missing = "amixer: Unable to find simple control '%s',0\n" % name
if verb == "sget":
    f = os.path.join(os.environ.get("FAKE_MIX_DIR", "/nonexistent"), name)
    if os.path.isfile(f):
        sys.stdout.write(open(f).read()); sys.exit(0)
    sys.stderr.write(missing); sys.exit(1)
if name in os.environ.get("FAKE_AMIXER_ABSENT", "").split(","):
    sys.stderr.write(missing); sys.exit(1)
'''
FAKE_ALSACTL = '#!/bin/sh\necho "alsactl $*" >> "$FAKE_ALSACTL_LOG"\nexit "${FAKE_ALSACTL_RC:-0}"\n'

MIX_HEALTHY = {
    "Speaker": "Simple mixer control 'Speaker',0\n  Front Left: Playback 0 [0%] [-65.25dB] [off]\n"
               "  Front Right: Playback 0 [0%] [-65.25dB] [off]\n",
    "Master": "Simple mixer control 'Master',0\n  Mono: Playback 87 [100%] [0.00dB] [on]\n",
    "Headphone": "Simple mixer control 'Headphone',0\n  Front Left: Playback 87 [100%] [0.00dB] [on]\n"
                 "  Front Right: Playback 87 [100%] [0.00dB] [on]\n",
    "PCM": "Simple mixer control 'PCM',0\n  Front Left: Playback 255 [100%] [0.00dB]\n"
           "  Front Right: Playback 255 [100%] [0.00dB]\n",
    "Auto-Mute Mode": "Simple mixer control 'Auto-Mute Mode',0\n  Items: 'Disabled' 'Enabled'\n"
                      "  Item0: 'Enabled'\n",
}


def asound_state(card_id="Generic_1", speaker="false", automute="Enabled", headphone="true"):
    """/var/lib/alsa/asound.state as `alsactl store` writes it (the controls the mixer row reads back); a
    second card's Speaker on first, so the reader must keep to the headphone card's own block."""
    return ("state.Generic {\n\tcontrol.1 {\n\t\tiface MIXER\n\t\tname 'Speaker Playback Switch'\n"
            "\t\tvalue.0 true\n\t\tvalue.1 true\n\t}\n}\n"
            "state.%s {\n"
            "\tcontrol.12 {\n\t\tiface MIXER\n\t\tname 'Headphone Playback Switch'\n"
            "\t\tvalue.0 %s\n\t\tvalue.1 %s\n\t\tcomment {\n\t\t\taccess 'read write'\n"
            "\t\t\ttype BOOLEAN\n\t\t\tcount 2\n\t\t}\n\t}\n"
            "\tcontrol.14 {\n\t\tiface MIXER\n\t\tname 'Speaker Playback Switch'\n"
            "\t\tvalue.0 %s\n\t\tvalue.1 %s\n\t\tcomment {\n\t\t\taccess 'read write'\n"
            "\t\t\ttype BOOLEAN\n\t\t\tcount 2\n\t\t}\n\t}\n"
            "\tcontrol.20 {\n\t\tiface MIXER\n\t\tname 'Auto-Mute Mode'\n\t\tvalue %s\n"
            "\t\tcomment {\n\t\t\taccess 'read write'\n\t\t\ttype ENUMERATED\n\t\t\tcount 1\n"
            "\t\t\titem.0 Disabled\n\t\t\titem.1 Enabled\n\t\t}\n\t}\n}\n"
            % (card_id, headphone, headphone, speaker, speaker, automute))


STOCK_RC = """<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
<keyboard>
  <keybind key="A-F4"><action name="Close"/></keybind>
</keyboard>
</openbox_config>
"""


def _lib_env(tmp_path, root, objects=None, settings=SETTINGS, sm=SM, scenes="none"):
    mix = tmp_path / "mix"
    mix.mkdir(exist_ok=True)
    for name, text in MIX_HEALTHY.items():
        if not (mix / name).exists():
            (mix / name).write_text(text)
    state = tmp_path / "asound.state"
    if not state.exists():
        state.write_text(asound_state())
    rc_src = tmp_path / "stock-rc.xml"
    rc_src.write_text(STOCK_RC)
    meta = tmp_path / "meta"
    meta.mkdir(exist_ok=True)
    (meta / "settings").write_text(settings)
    (meta / "sm-settings").write_text(sm)
    dump = tmp_path / "pw-dump.json"
    if objects is not False:
        dump.write_text(json.dumps(healthy() if objects is None else objects))
    fs = tmp_path / "fake_scenes.py"
    fs.write_text(FAKE_SCENES)
    fs.chmod(0o755)
    return _env(tmp_path, root, {
        "FAKE_PW_DUMP_FILE": str(dump), "FAKE_PW_META_DIR": str(meta), "FAKE_SCENES_MODE": scenes,
        "STREAM_OPERATOR_AUDIO_SCENES": str(fs), "STRIH_SESSION_APPS_EUID": "1000",
        # ROZHODNUTE 6383739315: the declared jack mixer (amixer + alsactl store), the volume helper, the keys
        "FAKE_MIX_DIR": str(mix), "FAKE_AMIXER_LOG": str(tmp_path / "amixer.log"),
        "FAKE_ALSACTL_LOG": str(tmp_path / "alsactl.log"), "STREAM_OPERATOR_AUDIO_ASOUND_STATE": str(state),
        "OBS_BOX_PANEL_HEADPHONES_HELPER": str(tmp_path / "usr-local-bin" / "obs-box-headphones"),
        "STREAM_OPERATOR_AUDIO_OPENBOX_RC_SRC": str(rc_src)},
        **{"pw-dump": FAKE_PW_DUMP, "pw-metadata": FAKE_PW_METADATA, "amixer": FAKE_AMIXER_RW,
           "alsactl": FAKE_ALSACTL})


LEFTOVERS = ("pipewire/pipewire.conf.d/51-strih-quantum-1024.conf", "pipewire/pipewire.conf.d/strih-program.conf",
             "pipewire/pipewire.conf.d/strih-program-loopback.conf",
             "wireplumber/wireplumber.conf.d/51-minifuse-output-period.conf",
             "wireplumber/wireplumber.conf.d/51-strih-minifuse.conf")


def _home_with_leftovers(tmp_path):
    home = tmp_path / "home"
    for rel in LEFTOVERS:
        p = home / ".config" / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("# strih leftover\n")
    keep = home / ".config" / "pipewire" / "pipewire.conf.d" / "99-operator-own.conf"
    keep.write_text("# not ours\n")
    return home, keep


def test_install_writes_the_two_dropins_and_removes_the_strih_leftovers(tmp_path):
    root, _ = snv_asound(tmp_path)
    home, keep = _home_with_leftovers(tmp_path)
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME)
    assert r.returncode == 0, (r.stdout, r.stderr)
    m = _soa()
    pw = home / ".config/pipewire/pipewire.conf.d/60-stream-operator-clock.conf"
    wp = home / ".config/wireplumber/wireplumber.conf.d/60-stream-operator-headphones.conf"
    assert pw.read_text() == m.render_pipewire()
    assert wp.read_text() == m.render_wireplumber(JACK_PP, 0)
    for rel in LEFTOVERS:
        assert not (home / ".config" / rel).exists(), rel
        assert rel.rsplit("/", 1)[1] in r.stdout, r.stdout
    assert keep.read_text() == "# not ours\n", "a file the stream role does not declare is never touched"
    assert "systemctl --user restart" in r.stdout and "pipewire" in r.stdout and "wireplumber" in r.stdout


def test_install_is_quiet_on_a_rerun(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = tmp_path / "home"
    env = _lib_env(tmp_path, root, objects=False)
    assert _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME).returncode == 0
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME)
    # the two drop-ins, the volume helper and the rc.xml keys
    assert r.returncode == 0 and r.stdout.count("unchanged") == 4 and "written" not in r.stdout, r.stdout


def test_install_prefers_the_running_sessions_device_name(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = tmp_path / "home"
    env = _lib_env(tmp_path, root, objects=[dev(40, "alsa_card.pci-live_jack", card=1)])
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME)
    assert r.returncode == 0, r.stderr
    wp = home / ".config/wireplumber/wireplumber.conf.d/60-stream-operator-headphones.conf"
    assert 'device.name = "alsa_card.pci-live_jack"' in wp.read_text()


def test_install_without_a_headphone_device_fails_and_writes_nothing(tmp_path):
    root, _ = asound(tmp_path, analog=False)
    home, _keep = _home_with_leftovers(tmp_path)
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3" || echo RC=$?', env, REPO, home, ME)
    assert "RC=1" in r.stdout and "no playback PCM" in r.stderr, (r.stdout, r.stderr)
    assert not (home / ".config/wireplumber/wireplumber.conf.d/60-stream-operator-headphones.conf").exists()
    assert all((home / ".config" / rel).exists() for rel in LEFTOVERS), "nothing removed on a refusal"


def _rows(tmp_path, home, env):
    r = _bash(tmp_path, 'stream_operator_audio_grade_rows "$1" "$2" "$3"', env, REPO, home, ME)
    assert r.returncode == 0, r.stderr
    out = {}
    for ln in r.stdout.splitlines():
        st, _, text = ln.partition("|")
        out[text.split(")", 1)[0].lstrip("(")] = (st, text)
    return out


def _installed(tmp_path, root):
    home = tmp_path / "home"
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME)
    assert r.returncode == 0, (r.stdout, r.stderr)
    # step 16h's panel config (the headphone item on the stream role), as setup writes it after step 12
    cfg = home / ".config" / "tint2" / "tint2rc"
    cfg.parent.mkdir(parents=True, exist_ok=True)
    r = _bash(tmp_path, 'strih_box_load stream-pp; obs_box_panel_tint2rc', env)
    assert r.returncode == 0, r.stderr
    cfg.write_text(r.stdout)
    return home


def test_the_grade_rows_of_a_locked_box(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    rows = _rows(tmp_path, home, _lib_env(tmp_path, root))
    assert sorted(rows) == sorted(ROWS + (MIXER, "operator-audio-files", "operator-audio-obs",
                                          "operator-audio-control")), rows
    assert all(st == "OK" for st, _ in rows.values()), rows
    assert "Generic_1" in rows[MIXER][1] and "stored" in rows[MIXER][1], rows[MIXER]
    r = _bash(tmp_path, 'printf "%s" "$STREAM_OPERATOR_AUDIO_ROWS"', _lib_env(tmp_path, root))
    assert r.stdout == str(len(rows))


def test_a_leftover_or_a_drifted_dropin_fails_the_files_row(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    (home / ".config/pipewire/pipewire.conf.d/strih-program.conf").write_text("# back\n")
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root))["operator-audio-files"]
    assert st == "FAIL" and "strih-program.conf" in text, text
    (home / ".config/pipewire/pipewire.conf.d/strih-program.conf").unlink()
    (home / ".config/pipewire/pipewire.conf.d/60-stream-operator-clock.conf").write_text("# drift\n")
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root))["operator-audio-files"]
    assert st == "FAIL" and "60-stream-operator-clock.conf" in text, text


def test_a_pcm_match_dropin_is_refused_by_install_and_fails_the_files_row(tmp_path):
    # review round 1: the player honours --pcm-match, the locked PipeWire the default rule: they must agree
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    d = home / ".config/systemd/user/strih-stream-monitor.service.d"
    d.mkdir(parents=True)
    (d / "10-scarlett.conf").write_text('[Service]\nExecStart=\nExecStart=/usr/bin/python3 x --pcm-match "^USB Audio$"\n')
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3" || echo RC=$?', env, REPO, home, ME)
    assert "RC=1" in r.stdout and "10-scarlett.conf" in r.stderr and "--pcm-match" in r.stderr, (r.stdout, r.stderr)
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root))["operator-audio-files"]
    assert st == "FAIL" and "10-scarlett.conf" in text, text


def test_an_unreadable_session_fails_and_a_down_obs_is_a_note(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    env = _lib_env(tmp_path, root, scenes="down")
    os.unlink(env["FAKE_PW_DUMP_FILE"])
    rows = _rows(tmp_path, home, env)
    assert all(rows[k][0] == "FAIL" for k in ROWS), rows
    assert rows["operator-audio-obs"][0] == "NOTE", rows


def test_a_pulse_capture_input_in_the_stream_obs_fails(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root, scenes="one"))["operator-audio-obs"]
    assert st == "FAIL" and "Zvuk z mixu" in text, text


def test_the_report_counts_its_rows(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = _installed(tmp_path, root)
    body = ('ok() { echo "OK $*"; }\nnote() { echo "NOTE $*"; }\nbad() { echo "BAD $*"; }\n'
            'stream_operator_audio_grade_report "$1" "$2" "$3" && echo REPORT-OK')
    r = _bash(tmp_path, body, _lib_env(tmp_path, root), REPO, home, ME)
    assert r.returncode == 0 and "REPORT-OK" in r.stdout and "BAD" not in r.stdout, (r.stdout, r.stderr)


# --- 4. the role gate in setup-strih step 12 / verify-strih item 9b -----------------------------------------

def _gate(box):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    script = 'set -euo pipefail\n. "%s"\nstrih_box_load "$1"\nif W="$(obs_box_step_applies 12-strih-audio)"; ' \
             'then echo RUN; else echo "SKIP $W"; fi' % PROVISION
    return subprocess.run(["bash", "-c", script, "h", box], capture_output=True, text=True, env=e,
                          cwd=str(REPO), timeout=60).stdout.strip()


@pytest.mark.parametrize("box,want", [("strih-snv", "RUN"), ("strih-pp", "RUN"), ("stream-pp", "SKIP"),
                                      ("stream-snv", "SKIP")])
def test_the_strih_audio_graph_is_the_strih_roles_only(box, want):
    out = _gate(box)
    assert out.split()[0] == want, out
    if want == "SKIP":
        assert "1465" in out, out


def _section(text, start, end):
    a = text.index(start)
    return text[a:text.index(end, a)]


def test_setup_step12_branches_on_the_role_and_keeps_the_strih_graph():
    s = _section(SETUP.read_text(), "\nstep 12 \"", "\nstep 13 \"")
    gate = s.index("obs_box_step_applies 12-strih-audio")
    stream = s.index('stream_operator_audio_install "${HERE}/.." "$USER_HOME" "$DESKTOP_USER"')
    for strih_only in ("strih_pipewire_program_sink_conf", "strih_pipewire_program_loopback_conf",
                       "strih_wireplumber_minifuse_rule", "strih_wireplumber_minifuse_output_period_conf",
                       "strih_pipewire_quantum_conf", "arecord -l"):
        assert s.index(strih_only) > gate, strih_only
    assert gate < stream
    assert "loginctl enable-linger" in s


def test_verify_item_9b_grades_the_operator_audio_on_the_stream_role():
    v = _section(VERIFY.read_text(), "# 9b) ", "# 10) NVENC")
    gate = v.index("obs_box_step_applies 12-strih-audio")
    call = v.index("stream_operator_audio_grade_report")
    assert gate < call
    assert v.index("51-strih-quantum-1024.conf") > gate, "the strih quantum grade stays on the strih role"


def test_both_scripts_stay_within_their_line_budget():
    assert len(SETUP.read_text().splitlines()) <= 1000
    assert len(VERIFY.read_text().splitlines()) <= 1000



# --- 5. review round 1: the jack's speaker path + mixer read back -------------------------------------------

FAKE_AMIXER = r"""#!/bin/sh
# amixer -c CARD sget NAME
f="$FAKE_MIX_DIR/$4"
if [ -f "$f" ]; then cat "$f"; else echo "amixer: Unable to find simple control '$4',0" >&2; exit 1; fi
"""

SPEAKER_OFF = """Simple mixer control 'Speaker',0
  Capabilities: pvolume pswitch
  Front Left: Playback 0 [0%] [-65.25dB] [off]
  Front Right: Playback 0 [0%] [-65.25dB] [off]
"""
MASTER_0DB = """Simple mixer control 'Master',0
  Capabilities: pvolume pvolume-joined pswitch pswitch-joined
  Mono: Playback 87 [100%] [0.00dB] [on]
"""
HEADPHONE_0DB = """Simple mixer control 'Headphone',0
  Front Left: Playback 87 [100%] [0.00dB] [on]
  Front Right: Playback 87 [100%] [0.00dB] [on]
"""


def _mixer(tmp_path, files, state=None):
    d = tmp_path / "mix"
    d.mkdir(exist_ok=True)
    for name, text in files.items():
        (d / name).write_text(text)
    fb = _fakebin(tmp_path, amixer=FAKE_AMIXER)
    st = tmp_path / "asound.state"
    st.write_text(asound_state() if state is None else state)
    os.environ["FAKE_MIX_DIR"] = str(d)
    try:
        return _soa().mixer_row({"card_id": "Generic_1", "usb": False}, str(fb / "amixer"), str(st))
    finally:
        del os.environ["FAKE_MIX_DIR"]


def test_the_jack_with_the_speaker_off_and_unity_passes(tmp_path):
    st, text = _mixer(tmp_path, {"Speaker": SPEAKER_OFF, "Master": MASTER_0DB, "Headphone": HEADPHONE_0DB})
    assert st == "OK" and "(%s)" % MIXER in text and "Speaker off" in text, text


def test_a_speaker_left_on_fails(tmp_path):
    st, text = _mixer(tmp_path, {"Speaker": SPEAKER_OFF.replace("[off]", "[on]", 1), "Master": MASTER_0DB,
                                 "Headphone": HEADPHONE_0DB})
    assert st == "FAIL" and "Speaker" in text, text


def test_a_master_below_unity_or_muted_fails(tmp_path):
    for master in (MASTER_0DB.replace("[0.00dB]", "[-23.25dB]"), MASTER_0DB.replace("[on]", "[off]")):
        st, text = _mixer(tmp_path, {"Speaker": SPEAKER_OFF, "Master": master, "Headphone": HEADPHONE_0DB})
        assert st == "FAIL" and "Master" in text, text


def test_a_control_the_jack_does_not_have_is_named_not_failed(tmp_path):
    st, text = _mixer(tmp_path, {"Master": MASTER_0DB})
    assert st == "OK" and "no Speaker control" in text, text


def test_a_usb_interface_as_the_headphone_device_fails():
    # ROZHODNUTE 6383739315: the headphones are the notebook jack, never a USB interface
    st, text = _soa().mixer_row({"card_id": "Gen", "usb": True}, "/nonexistent/amixer")
    assert st == "FAIL" and "USB" in text and "jack" in text, text


def test_an_amixer_error_that_is_not_a_missing_control_fails(tmp_path):
    # review round 2: only "Unable to find simple control" reads as a control the jack does not have
    fb = _fakebin(tmp_path, amixer="#!/bin/sh\necho 'amixer: Invalid card number.' >&2\nexit 1\n")
    st, text = _soa().mixer_row({"card_id": "Generic_9", "usb": False}, str(fb / "amixer"), "/nonexistent/state")
    assert st == "FAIL" and "Invalid card" in text, text


def test_the_lib_stands_alone_under_set_u(tmp_path):
    # review round 2: the drop-in scan needs the monitor's unit name even when only this lib is sourced
    e = {k: v for k, v in os.environ.items() if k not in LEAKY + ("STRIH_STREAM_MONITOR_UNIT",)}
    script = 'set -euo pipefail\n. "%s"\n_stream_operator_audio_pcm_match_dropins "$1"\necho DONE' % LIB
    r = subprocess.run(["bash", "-c", script, "h", str(tmp_path)], capture_output=True, text=True, env=e, timeout=30)
    assert r.returncode == 0 and "DONE" in r.stdout, r.stderr
