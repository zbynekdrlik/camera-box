"""Issue 1465 item 3 revision (owner 24.10.2026): the notebook jack + a global headphone volume.

ROZHODNUTE 6383739315 (owner, verbatim): "na snv stream pc chcem aby sa uz nepouzivala focusrite a isli
sluchatka priamo z jack notebooku ako to je aj v stream-pp a tiez musi byt zrozumitelne ako si ma operator
regulovat hlasitost sluchatiek, nie len tu localnu v vlc/chrome yt ale aj tu globalnu". Design: comment
6383742509 (Approach 1).

What this pins (Tier-0: pytest + bash, fake pw-dump / wpctl / amixer / alsactl, no rig):
  1. the resolver picks the notebook's analog PCM on every stream box and NEVER a USB interface
     (stream-snv: `ALC256 Analog` on card `PCH`, the Focusrite `Gen` plugged in but unused);
  2. the jack's HDA mixer is DECLARED: Speaker off, Auto-Mute Mode Enabled, Master / Headphone / PCM at 0 dB
     unmuted -- setup writes it and stores it with `alsactl store` (alsa-restore puts it back at boot),
     verify reads it live AND in /var/lib/alsa/asound.state;
  3. the global volume: scripts/stream_operator_volume.py (installed as /usr/local/bin/obs-box-headphones) on
     the ONE sink `stream-operator-headphones`, resolved by name from pw-dump, wpctl on its id, capped at 1.0;
     the tint2 item `Slúchadlá NN %` / `Slúchadlá stlmené` (wheel = +-5 %, click = mute) and the openbox
     XF86AudioRaiseVolume / LowerVolume / Mute keys; the operator's level kept across restarts, unmuted at
     the start of a session; stream role only;
  4. verify item 9b: the device is the jack, the speaker muted, the item + keys present, the level read,
     never forced to 100 %.
"""
import importlib.util
import json
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import LEAKY, PROVISION, REPO  # noqa: E402
from test_strih_stream_monitor_1465 import add_usb_card  # noqa: E402
from test_stream_operator_audio_1465 import (  # noqa: E402
    BASELINE, FAKE_AMIXER_RW, ME, MIX_HEALTHY, SINK, _bash, _installed, _lib_env, _rows, asound_state, healthy, node,
    snv_asound)

SCRIPTS = REPO / "scripts"
VOL = SCRIPTS / "stream_operator_volume.py"
PCH = "alsa_card.pci-0000_00_1f.3"
HELPER_DEFAULT = "/usr/local/bin/obs-box-headphones"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _mon():
    return _load("strih_stream_monitor_jack_1465", SCRIPTS / "strih_stream_monitor.py")


def _soa():
    return _load("stream_operator_audio_jack_1465", SCRIPTS / "stream_operator_audio.py")


def _vol():
    return _load("stream_operator_volume_1465", VOL)


def _pcm(root, card, dev, name):
    d = root / ("card%d" % card) / ("pcm%dp" % dev)
    (d / "sub0").mkdir(parents=True, exist_ok=True)
    (d / "info").write_text("card: %d\ndevice: %d\nsubdevice: 0\nstream: PLAYBACK\nid: %s\nname: %s\n"
                            % (card, dev, name, name))
    (d / "sub0" / "status").write_text("closed\n")


def snv_cards(tmp_path):
    """stream-snv as read live 24.10.2026: card0 `NVidia` (HDMI only), card1 `Gen` (the Scarlett Solo 4th Gen,
    USB), card2 `PCH` (`ALC256 Analog` + HDMI)."""
    root = tmp_path / "asound"
    for card, cid in ((0, "NVidia"), (2, "PCH")):
        (root / ("card%d" % card)).mkdir(parents=True)
        (root / ("card%d" % card) / "id").write_text(cid + "\n")
    for i, dev in enumerate((3, 7, 8, 9)):
        _pcm(root, 0, dev, "HDMI %d" % i)
    add_usb_card(root, 1, "Gen")
    _pcm(root, 2, 0, "ALC256 Analog")
    _pcm(root, 2, 3, "HDMI 0")
    return root


SNV_UDEVADM = r'''#!/bin/sh
case "$3" in
  */card0) printf 'ID_PATH=pci-0000:01:00.1\n' ;;
  */card1) printf 'ID_ID=usb-Focusrite_Scarlett_Solo_4th_Gen_S1ABCDEF-00\nID_PATH=pci-0000:00:14.0-usb-0:5:1.0\n' ;;
  */card2) printf 'ID_PATH=pci-0000:00:1f.3\n' ;;
  *) echo "Unknown device" >&2; exit 4 ;;
esac
'''


# --- 1. the resolver: the jack, never a USB interface -----------------------------------------------------------

def test_stream_snv_resolves_the_notebook_jack_never_the_focusrite(tmp_path):
    m = _mon()
    pcm, why, note = m.resolve_headphone(m.list_playback_pcms(str(snv_cards(tmp_path))))
    assert why is None and m.alsa_device(pcm) == "plughw:CARD=PCH,DEV=0", (why, pcm)
    assert pcm["usb"] is False
    assert "Gen:0 USB Audio" in note and "never" in note, note


def test_a_usb_pcm_named_analog_is_never_the_jack(tmp_path):
    m = _mon()
    root = snv_cards(tmp_path)
    add_usb_card(root, 3, "Device", name="USB Audio Analog")
    pcm, why, _note = m.resolve_headphone(m.list_playback_pcms(str(root)))
    assert why is None and m.alsa_device(pcm) == "plughw:CARD=PCH,DEV=0", why
    # with no notebook jack at all, a USB analog PCM is still no headphone device
    only_usb = tmp_path / "b"
    add_usb_card(only_usb / "asound", 0, "Device", name="USB Audio Analog")
    pcm, why, _note = m.resolve_headphone(m.list_playback_pcms(str(only_usb / "asound")))
    assert pcm is None and "no playback PCM" in why, why


def test_the_watching_rule_names_the_jack():
    rule = _mon().headphone_rule(None)
    assert "never a USB audio interface" in rule and "ONE USB" not in rule, rule


# --- 2. the device + the WirePlumber drop-in: the PCH card on, the Focusrite off ---------------------------------

def test_the_rendered_dropin_enables_only_the_jack_card(tmp_path):
    root = snv_cards(tmp_path)
    fb = tmp_path / "fb"
    fb.mkdir()
    (fb / "udevadm").write_text(SNV_UDEVADM)
    (fb / "udevadm").chmod(0o755)
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"STRIH_STREAM_MONITOR_ASOUND": str(root), "STREAM_OPERATOR_AUDIO_SYS": str(tmp_path / "sys"),
                "STREAM_OPERATOR_AUDIO_UDEVADM": str(fb / "udevadm")})
    r = subprocess.run([sys.executable, str(SCRIPTS / "stream_operator_audio.py"), "render-wireplumber"],
                       capture_output=True, text=True, env=env, timeout=30)
    assert r.returncode == 0, r.stderr
    text = r.stdout
    assert text == _soa().render_wireplumber(PCH, 0)
    assert 'device.name = "%s"' % PCH in text and "Focusrite" not in text and "usb-" not in text
    assert 'node.name = "alsa_output.pci-0000_00_1f.3.pro-output-0"' in text
    # every ALSA card (the Focusrite and the NVidia HDMI included) is off before the jack card comes back on
    assert text.index('device.name = "~alsa_card.*"') < text.index('device.name = "%s"' % PCH)
    assert "PCH:0 ALC256 Analog" in r.stderr and "never" in r.stderr, r.stderr


# --- 3. the declared jack mixer: written, stored, read back --------------------------------------------------------

def _mix_env(tmp_path, root, **extra):
    fb = tmp_path / "mixbin"
    fb.mkdir(exist_ok=True)
    (fb / "amixer").write_text(FAKE_AMIXER_RW)
    (fb / "amixer").chmod(0o755)
    (fb / "alsactl").write_text('#!/bin/sh\necho "alsactl $*" >> "$FAKE_ALSACTL_LOG"\nexit "${FAKE_ALSACTL_RC:-0}"\n')
    (fb / "alsactl").chmod(0o755)
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"PATH": "%s:%s" % (fb, os.environ["PATH"]), "STRIH_STREAM_MONITOR_ASOUND": str(root),
                "FAKE_AMIXER_LOG": str(tmp_path / "amixer.log"), "FAKE_ALSACTL_LOG": str(tmp_path / "alsactl.log")})
    env.update(extra)
    return env


def _apply(tmp_path, **extra):
    root = snv_cards(tmp_path)
    r = subprocess.run([sys.executable, str(SCRIPTS / "stream_operator_audio.py"), "apply-mixer"],
                       capture_output=True, text=True, env=_mix_env(tmp_path, root, **extra), timeout=30)
    amixer = (tmp_path / "amixer.log").read_text().splitlines() if (tmp_path / "amixer.log").exists() else []
    alsactl = (tmp_path / "alsactl.log").read_text().splitlines() if (tmp_path / "alsactl.log").exists() else []
    return r, amixer, alsactl


def test_apply_mixer_writes_the_declared_state_on_the_jack_then_stores_it(tmp_path):
    r, amixer, alsactl = _apply(tmp_path)
    assert r.returncode == 0, (r.stdout, r.stderr)
    for want in ("-q -c PCH sset Master 0dB unmute", "-q -c PCH sset Headphone 0dB unmute", "-q -c PCH sset PCM 0dB",
                 "-q -c PCH sset Speaker mute", "-q -c PCH sset Auto-Mute Mode Enabled"):
        assert want in amixer, (want, amixer)
    assert not [a for a in amixer if " Gen " in " %s " % a], "the Focusrite's mixer is never touched"
    assert alsactl == ["alsactl store PCH"], alsactl
    assert "PCH" in r.stdout and "stored" in r.stdout, r.stdout


def test_apply_mixer_names_a_control_the_codec_lacks_and_still_stores(tmp_path):
    r, amixer, alsactl = _apply(tmp_path, FAKE_AMIXER_ABSENT="PCM")
    assert r.returncode == 0, r.stderr
    assert "PCM" in r.stdout and "no " in r.stdout, r.stdout
    assert alsactl == ["alsactl store PCH"], alsactl


def test_apply_mixer_fails_on_a_real_amixer_error_and_stores_nothing(tmp_path):
    r, _amixer, alsactl = _apply(tmp_path, FAKE_AMIXER_FAIL="Speaker")
    assert r.returncode == 1 and "Speaker" in r.stderr and "Invalid card" in r.stderr, (r.stdout, r.stderr)
    assert alsactl == [], "a mixer that was not fully written is never stored"


def test_apply_mixer_fails_when_alsactl_cannot_store(tmp_path):
    r, _amixer, alsactl = _apply(tmp_path, FAKE_ALSACTL_RC="1")
    assert r.returncode == 1 and "alsactl" in r.stderr, (r.stdout, r.stderr)
    assert alsactl == ["alsactl store PCH"]


def _mixer_row(tmp_path, live=None, state=None):
    d = tmp_path / "mixrow"
    d.mkdir(exist_ok=True)
    for name, text in dict(MIX_HEALTHY, **(live or {})).items():
        if text is not None:
            (d / name).write_text(text)
    st = tmp_path / "asound.state"
    st.write_text(asound_state() if state is None else state)
    fb = tmp_path / "rowbin"
    fb.mkdir(exist_ok=True)
    (fb / "amixer").write_text(FAKE_AMIXER_RW)
    (fb / "amixer").chmod(0o755)
    os.environ["FAKE_MIX_DIR"] = str(d)
    try:
        return _soa().mixer_row({"card_id": "Generic_1", "usb": False}, str(fb / "amixer"), str(st))
    finally:
        del os.environ["FAKE_MIX_DIR"]


def test_the_mixer_row_passes_with_the_declared_state_live_and_stored(tmp_path):
    st, text = _mixer_row(tmp_path)
    assert st == "OK", text
    assert "Speaker off" in text and "Auto-Mute" in text and "stored" in text, text


def test_auto_mute_left_disabled_fails(tmp_path):
    st, text = _mixer_row(tmp_path, {"Auto-Mute Mode": "Simple mixer control 'Auto-Mute Mode',0\n"
                                                       "  Items: 'Disabled' 'Enabled'\n  Item0: 'Disabled'\n"})
    assert st == "FAIL" and "Auto-Mute" in text, text


@pytest.mark.parametrize("state,why", [
    (asound_state(speaker="true"), "Speaker"),
    (asound_state(automute="Disabled"), "Auto-Mute"),
    (asound_state(headphone="false"), "Headphone"),
    (asound_state(card_id="Other"), "Generic_1"),
    (asound_state(headphone_db=-2000), "Headphone Playback Volume -20.00 dB"),
    (asound_state(master_db=-2325), "Master Playback Volume -23.25 dB"),
    (asound_state(pcm_db=-2000), "PCM Playback Volume -20.00 dB"),
], ids=["speaker-on", "auto-mute-off", "headphone-off", "card-not-stored", "headphone-level", "master-level",
        "pcm-level"])
def test_a_stored_state_that_differs_fails_it_would_come_back_after_a_reboot(tmp_path, state, why):
    st, text = _mixer_row(tmp_path, state=state)
    assert st == "FAIL" and why in text and "reboot" in text, text


def test_a_live_pcm_level_below_0_db_fails(tmp_path):
    # review round 2: apply-mixer writes PCM at 0 dB, so the live row grades it (no switch: only its dB)
    st, text = _mixer_row(tmp_path, {"PCM": "Simple mixer control 'PCM',0\n  Front Left: Playback 203 [80%] "
                                            "[-10.20dB]\n  Front Right: Playback 203 [80%] [-10.20dB]\n"})
    assert st == "FAIL" and "PCM at -10.20 dB" in text, text
    (tmp_path / "ok").mkdir()
    st, text = _mixer_row(tmp_path / "ok")
    assert st == "OK" and "PCM 0 dB" in text, text


def test_no_stored_state_fails(tmp_path):
    fb = tmp_path / "rowbin"
    fb.mkdir()
    (fb / "amixer").write_text(FAKE_AMIXER_RW)
    (fb / "amixer").chmod(0o755)
    d = tmp_path / "mixrow"
    d.mkdir()
    for name, text in MIX_HEALTHY.items():
        (d / name).write_text(text)
    os.environ["FAKE_MIX_DIR"] = str(d)
    try:
        st, text = _soa().mixer_row({"card_id": "Generic_1", "usb": False}, str(fb / "amixer"),
                                    str(tmp_path / "missing.state"))
    finally:
        del os.environ["FAKE_MIX_DIR"]
    assert st == "FAIL" and "missing.state" in text and "setup-strih" in text, text


# --- 4. the volume helper ------------------------------------------------------------------------------------------

def test_the_helper_names_the_monitors_sink():
    assert _vol().SINK_NAME == _mon().PULSE_SINK_DEFAULT == SINK


def test_the_label_is_plain_slovak():
    v = _vol()
    assert v.label(None) == "Slúchadlá nedostupné"
    assert v.label({"id": 41, "volume": 0.7, "mute": False}) == "Slúchadlá 70 %"
    assert v.label({"id": 41, "volume": 0.7, "mute": True}) == "Slúchadlá stlmené"


def test_the_sink_is_found_by_name_on_the_cubic_scale():
    v = _vol()
    objs = [{"id": 0, "type": "PipeWire:Interface:Core", "info": {"cookie": 2306208623, "name": "pipewire-0"}},
            {"id": 30, "type": "PipeWire:Interface:Node",
             "info": {"props": {"node.name": "alsa_output.x", "media.class": "Audio/Sink"},
                      "params": {"Props": [{"channelVolumes": [1.0, 1.0], "mute": False}]}}},
            {"id": 41, "type": "PipeWire:Interface:Node",
             "info": {"props": {"node.name": SINK, "media.class": "Audio/Sink", "object.serial": 140},
                      "params": {"Props": [{"volume": 1.0, "mute": False, "channelVolumes": [0.343, 0.343]},
                                           {"params": []}]}}}]
    s = v.read_sink(objs)
    assert s["id"] == 41 and abs(s["volume"] - 0.7) < 1e-3 and s["mute"] is False, s
    # review rounds 1 + 2: PipeWire hands node ids out again (after a PipeWire restart, and when only
    # WirePlumber restarts and re-creates the node); the core cookie (random per daemon start) + the node's
    # object.serial (never handed out twice in one run) name ONE sink
    assert s["key"] == "2306208623:140", s
    objs[2]["info"]["props"]["object.serial"] = 175
    assert v.read_sink(objs)["key"] == "2306208623:175" and v.read_sink(objs)["id"] == 41
    assert v.read_sink(objs[:1]) is None and v.read_sink(None) is None


FAKE_PW_DUMP_VOL = '#!/bin/sh\ncat "$FAKE_VOL_DUMP" 2>/dev/null || exit 1\n'
FAKE_WPCTL = r'''#!/usr/bin/env python3
# wpctl set-volume [-l LIMIT] ID VOL[%][+-] | set-mute ID 1|0|toggle: logged, then applied to the fake dump
import json, os, sys
with open(os.environ["FAKE_WPCTL_LOG"], "a") as fh:
    fh.write(" ".join(sys.argv[1:]) + "\n")
path = os.environ["FAKE_VOL_DUMP"]
objs = json.load(open(path))
args = sys.argv[1:]
limit = None
if "-l" in args:
    i = args.index("-l"); limit = float(args[i + 1]); del args[i:i + 2]
verb, nid, val = args[0], int(args[1]), args[2]
for o in objs:
    if o.get("id") != nid:
        continue
    p = o["info"]["params"]["Props"][0]
    cur = (sum(p["channelVolumes"]) / len(p["channelVolumes"])) ** (1.0 / 3)
    if verb == "set-volume":
        if val.endswith("%+") or val.endswith("%-"):
            new = cur + (1 if val.endswith("+") else -1) * float(val[:-2]) / 100.0
        else:
            new = float(val)
        new = max(0.0, new if limit is None else min(new, limit))
        p["channelVolumes"] = [new ** 3] * len(p["channelVolumes"])
    elif verb == "set-mute":
        p["mute"] = (not p["mute"]) if val == "toggle" else val == "1"
json.dump(objs, open(path, "w"))
'''


def _vol_rig(tmp_path, volume=0.7, mute=False, sink=True, saved=None):
    objs = [{"id": 30, "type": "PipeWire:Interface:Node",
             "info": {"props": {"node.name": "alsa_output.x", "media.class": "Audio/Sink"},
                      "params": {"Props": [{"channelVolumes": [1.0, 1.0], "mute": False}]}}}]
    if sink:
        objs.append({"id": 41, "type": "PipeWire:Interface:Node",
                     "info": {"props": {"node.name": SINK, "media.class": "Audio/Sink"},
                              "params": {"Props": [{"mute": mute, "channelVolumes": [volume ** 3] * 2}]}}})
    dump = tmp_path / "dump.json"
    dump.write_text(json.dumps(objs))
    fb = tmp_path / "volbin"
    fb.mkdir(exist_ok=True)
    for name, body in (("pw-dump", FAKE_PW_DUMP_VOL), ("wpctl", FAKE_WPCTL)):
        (fb / name).write_text(body)
        (fb / name).chmod(0o755)
    state = tmp_path / "state" / "operator-headphones.json"
    if saved is not None:
        state.parent.mkdir(parents=True, exist_ok=True)
        state.write_text(json.dumps({"volume": saved}))
    env = {k: v for k, v in os.environ.items() if k not in LEAKY}
    env.update({"PATH": "%s:%s" % (fb, os.environ["PATH"]), "FAKE_VOL_DUMP": str(dump),
                "FAKE_WPCTL_LOG": str(tmp_path / "wpctl.log"), "STREAM_OPERATOR_VOLUME_STATE": str(state)})
    return env, dump, state


def _run_vol(tmp_path, env, *args):
    r = subprocess.run([sys.executable, str(VOL), *args], capture_output=True, text=True, env=env, timeout=30)
    log = tmp_path / "wpctl.log"
    return r, (log.read_text().splitlines() if log.exists() else [])


def test_up_steps_5_percent_capped_unmutes_and_keeps_the_level(tmp_path):
    env, _dump, state = _vol_rig(tmp_path, volume=0.98, mute=True)
    r, calls = _run_vol(tmp_path, env, "up")
    assert r.returncode == 0, r.stderr
    assert calls == ["set-volume -l 1.0 41 5%+", "set-mute 41 0"], calls
    assert r.stdout.strip() == "Slúchadlá 100 %", r.stdout
    assert json.loads(state.read_text())["volume"] == 1.0


def test_down_steps_5_percent(tmp_path):
    env, _dump, state = _vol_rig(tmp_path, volume=0.7)
    r, calls = _run_vol(tmp_path, env, "down")
    assert r.returncode == 0 and calls == ["set-volume -l 1.0 41 5%-"], (calls, r.stderr)
    assert r.stdout.strip() == "Slúchadlá 65 %" and json.loads(state.read_text())["volume"] == 0.65


def test_toggle_mutes_and_unmutes(tmp_path):
    env, _dump, _state = _vol_rig(tmp_path, volume=0.7)
    r, calls = _run_vol(tmp_path, env, "toggle")
    assert r.stdout.strip() == "Slúchadlá stlmené" and calls == ["set-mute 41 toggle"], (r.stdout, calls)
    r, _calls = _run_vol(tmp_path, env, "toggle")
    assert r.stdout.strip() == "Slúchadlá 70 %", r.stdout


def test_no_operator_sink_touches_nothing(tmp_path):
    env, _dump, state = _vol_rig(tmp_path, sink=False)
    for cmd in ("up", "down", "toggle"):
        r, calls = _run_vol(tmp_path, env, cmd)
        assert r.returncode == 1 and calls == [] and r.stdout.strip() == "Slúchadlá nedostupné", (cmd, r.stdout)
    assert not state.exists()


def test_print_reads_without_writing(tmp_path):
    env, _dump, state = _vol_rig(tmp_path, volume=0.5)
    r, calls = _run_vol(tmp_path, env, "print")
    assert r.returncode == 0 and r.stdout.strip() == "Slúchadlá 50 %" and calls == [] and not state.exists()


def test_watch_restores_the_kept_level_unmuted_when_the_sink_appears(tmp_path):
    # a reboot or an operator-audio restart brings the sink back at 100 % (no restore in WirePlumber):
    # the panel item puts the operator's kept level back, unmuted at the start of the session
    env, _dump, _state = _vol_rig(tmp_path, volume=1.0, mute=True, saved=0.4)
    r, calls = _run_vol(tmp_path, env, "watch", "--polls", "1", "--interval", "0")
    assert r.returncode == 0, r.stderr
    assert calls == ["set-volume -l 1.0 41 0.40", "set-mute 41 0"], calls
    assert r.stdout.splitlines() == ["Slúchadlá 40 %"], r.stdout


def test_watch_without_a_kept_level_only_unmutes(tmp_path):
    env, _dump, _state = _vol_rig(tmp_path, volume=0.8, mute=True)
    r, calls = _run_vol(tmp_path, env, "watch", "--polls", "1", "--interval", "0")
    assert calls == ["set-mute 41 0"] and r.stdout.splitlines() == ["Slúchadlá 80 %"], (calls, r.stdout)


def _sink(sid, volume, mute=False, run="c1", serial=None):
    return {"id": sid, "key": "%s:%s" % (run, sid if serial is None else serial), "volume": volume, "mute": mute}


def _watch(tmp_path, seq, kept=None, fail_calls=0):
    """A Watcher over SEQ (the scripted looks; a wpctl call changes the current sink and the next look sees it;
    the first FAIL_CALLS wpctl calls fail and change nothing). Returns (watcher, calls, state path)."""
    v = _vol()
    state = tmp_path / "kept.json"
    if kept is not None:
        state.write_text(json.dumps(kept))
    calls = []
    reads = iter(seq)
    box = {"cur": None, "changed": False}

    def read():
        if box["changed"]:
            box["changed"] = False
            return dict(box["cur"])
        s = next(reads)
        box["cur"] = None if s is None else dict(s)
        return None if s is None else dict(s)

    def wpctl(*args):
        calls.append(" ".join(args))
        if len(calls) <= fail_calls:
            return False
        if args[0] == "set-volume":
            box["cur"]["volume"] = float(args[4])
        elif args[0] == "set-mute":
            box["cur"]["mute"] = args[2] == "1"
        box["changed"] = True
        return True

    w = v.Watcher(read, wpctl, str(state))
    return w, calls, state


def test_a_pipewire_restart_that_reuses_the_node_id_still_restores(tmp_path):
    # review round 1 (the 🔴): the restart finished between two looks and the sink came back under the SAME id
    # at 100 % -- a new core cookie makes it a new sink: the kept 70 % comes back, the 100 % is never kept
    w, calls, state = _watch(tmp_path, [_sink(41, 0.7, run="c1"), _sink(41, 1.0, run="c2")])
    assert w.poll() == "Slúchadlá 70 %"
    assert json.loads(state.read_text())["volume"] == 0.7
    w.poll()
    assert w.shown == "Slúchadlá 70 %", calls
    assert "set-volume -l 1.0 41 0.70" in calls, calls
    assert json.loads(state.read_text())["volume"] == 0.7, "the 100 % default is never kept"


def test_a_wireplumber_only_restart_that_reuses_the_node_id_still_restores(tmp_path):
    # review round 2: only WirePlumber restarted -- the same PipeWire run (same cookie), the node re-created under
    # its old id at 100 %; its new object.serial makes it a new sink
    w, calls, state = _watch(tmp_path, [_sink(41, 0.7, serial=100), _sink(41, 1.0, serial=140)])
    w.poll()
    w.poll()
    assert w.shown == "Slúchadlá 70 %" and "set-volume -l 1.0 41 0.70" in calls, calls
    assert json.loads(state.read_text())["volume"] == 0.7


def test_a_failed_restore_is_retried_never_marked_done(tmp_path):
    # review round 2: a wpctl that fails at login must not leave the sink at 100 % for the session
    w, calls, state = _watch(tmp_path, [_sink(41, 1.0), _sink(41, 1.0)], kept={"volume": 0.4}, fail_calls=1)
    w.poll()
    assert json.loads(state.read_text()).get("restored") is None, "a failed restore is not recorded"
    w.poll()
    assert calls.count("set-volume -l 1.0 41 0.40") == 2 and w.shown == "Slúchadlá 40 %", calls
    assert json.loads(state.read_text())["restored"] == "c1:41"


def test_a_sink_with_no_core_cookie_is_restored_but_never_recorded(tmp_path):
    # review round 2: with no Core object in pw-dump the key has no run part; restore, but never record it
    w, calls, state = _watch(tmp_path, [{"id": 41, "key": "?:41", "volume": 1.0, "mute": False}],
                             kept={"volume": 0.5})
    w.poll()
    assert "set-volume -l 1.0 41 0.50" in calls and "restored" not in json.loads(state.read_text()), calls


def test_a_restarted_watcher_adopts_the_sink_and_keeps_the_operators_mute(tmp_path):
    # review round 1: a panel restart (step 16h) restarts the watcher; the sink it already restored is adopted
    # as it is -- the operator's mute stays, nothing is set
    w, calls, _state = _watch(tmp_path, [_sink(41, 0.6, mute=True)], kept={"volume": 0.6, "restored": "c1:41"})
    assert w.poll() == "Slúchadlá stlmené" and calls == [], calls


def test_one_missed_look_neither_flashes_the_label_nor_restores(tmp_path):
    # review round 1: one slow pw-dump read None; the sink only counts as gone after GONE_AFTER misses in a row
    w, calls, _state = _watch(tmp_path, [_sink(41, 0.6, mute=True), None, _sink(41, 0.6, mute=True)],
                              kept={"volume": 0.6, "restored": "c1:41"})
    assert [w.poll() for _ in range(3)] == ["Slúchadlá stlmené", None, None] and calls == [], calls
    assert _vol().GONE_AFTER >= 2


def test_a_kept_zero_level_is_restored_audible(tmp_path):
    w, calls, _state = _watch(tmp_path, [_sink(41, 1.0)], kept={"volume": 0.0})
    w.poll()
    assert calls[0] == "set-volume -l 1.0 41 0.10", calls


def test_a_kept_level_that_is_not_a_number_is_ignored(tmp_path):
    v = _vol()
    for bad in (True, "0.5", None):
        p = tmp_path / "k.json"
        p.write_text(json.dumps({"volume": bad}))
        assert v.load_level(str(p)) is None, bad


def test_a_failed_wpctl_call_is_a_nonzero_exit(tmp_path):
    env, _dump, _state = _vol_rig(tmp_path, volume=0.7)
    (tmp_path / "volbin" / "wpctl").write_text("#!/bin/sh\necho 'Translate ID error' >&2\nexit 1\n")
    r, _calls = _run_vol(tmp_path, env, "down")
    assert r.returncode == 1 and "wpctl" in r.stderr, (r.returncode, r.stderr)


def test_the_watcher_keeps_a_level_changed_elsewhere_and_restores_after_a_restart(tmp_path):
    v = _vol()
    state = tmp_path / "kept.json"
    seq = [_sink(41, 0.7), _sink(41, 0.55), _sink(41, 0.55)] + [None] * v.GONE_AFTER + [_sink(57, 1.0, run="c2")]
    calls = []
    reads = iter(seq)
    box = {"cur": None, "changed": False}

    def read():
        # after a wpctl call the next look sees its effect, else the next scripted state
        if box["changed"]:
            box["changed"] = False
            return dict(box["cur"])
        s = next(reads)
        box["cur"] = None if s is None else dict(s)
        return None if s is None else dict(s)

    def wpctl(*args):
        calls.append(" ".join(args))
        if args[0] == "set-volume":
            box["cur"]["volume"] = float(args[4])
        elif args[0] == "set-mute":
            box["cur"]["mute"] = args[2] == "1"
        box["changed"] = True

    w = v.Watcher(read, wpctl, str(state))
    out = [w.poll() for _ in range(len(seq))]
    # first sight: nothing kept yet -> only the unmute; then a change made elsewhere (a key press) is kept
    assert out[0] == "Slúchadlá 70 %" and out[1] == "Slúchadlá 55 %" and out[2] is None, out
    assert json.loads(state.read_text())["volume"] == 0.55
    assert out[2 + v.GONE_AFTER] == "Slúchadlá nedostupné" and set(out[3:2 + v.GONE_AFTER]) == {None}, out
    # a new sink (PipeWire restarted): the kept 55 % comes back
    assert "set-volume -l 1.0 57 0.55" in calls and out[-1] == "Slúchadlá 55 %", (calls, out)


# --- 5. the panel item + the keys (stream role only) ----------------------------------------------------------------

def _panel(box):
    script = 'set -euo pipefail\n. "$1"\nstrih_box_load "$2"\nobs_box_panel_tint2rc'
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    r = subprocess.run(["bash", "-c", script, "h", str(PROVISION), box], capture_output=True, text=True, env=e,
                       timeout=120)
    assert r.returncode == 0, r.stderr
    return [ln for ln in r.stdout.splitlines() if not ln.startswith("#")]


@pytest.mark.parametrize("box", ["stream-pp", "stream-snv"])
def test_the_stream_panel_carries_the_headphone_item_before_the_keyboard_item(box):
    cfg = _panel(box)
    assert "panel_items = TSEEC" in cfg, cfg
    watch = cfg.index("execp_command = /usr/bin/python3 %s watch" % HELPER_DEFAULT)
    kbd = cfg.index("execp_command = /usr/bin/python3 /usr/local/lib/camera-box/obs_box_kbd_layout.py watch")
    assert watch < kbd
    block = cfg[watch:kbd]
    for want in ("execp_continuous = 1",
                 "execp_lclick_command = /usr/bin/python3 %s toggle" % HELPER_DEFAULT,
                 "execp_uwheel_command = /usr/bin/python3 %s up" % HELPER_DEFAULT,
                 "execp_dwheel_command = /usr/bin/python3 %s down" % HELPER_DEFAULT):
        assert want in block, (want, block)
    tip = [ln for ln in block if ln.startswith("execp_tooltip = ")]
    assert tip and "Slúchadlá" in tip[0] and "koliesko" in tip[0] and "klik" in tip[0], tip


@pytest.mark.parametrize("box", ["strih-snv", "strih-pp"])
def test_the_strih_panel_has_no_headphone_item(box):
    cfg = _panel(box)
    assert "panel_items = TSEC" in cfg and not [ln for ln in cfg if "obs-box-headphones" in ln], cfg


RC = """<?xml version="1.0" encoding="UTF-8"?>
<openbox_config xmlns="http://openbox.org/3.4/rc">
<keyboard>
  <keybind key="A-F4"><action name="Close"/></keybind>
  <!-- OBS-box kiosk: panel brightness keys (openbox has no brightness handler) -->
  <keybind key="XF86MonBrightnessUp"><action name="Execute"><command>/usr/local/bin/obs-box-brightness up</command></action></keybind>
  <keybind key="XF86MonBrightnessDown"><action name="Execute"><command>/usr/local/bin/obs-box-brightness down</command></action></keybind>
</keyboard>
</openbox_config>
"""


def _merge(rc):
    script = ('set -euo pipefail\n. "$1"\n. "$2"\nfail() { echo "FAIL: $*" >&2; exit 1; }\n'
              'printf "%s" "$3" | obs_box_openbox_rc_with_keys obs_box_panel_headphones_keybinds_xml')
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    return subprocess.run(["bash", "-c", script, "h", str(PROVISION), str(BASELINE), rc], capture_output=True,
                          text=True, env=e, timeout=120)


def test_the_volume_keys_merge_before_keyboard_close_and_keep_everything_else():
    r = _merge(RC)
    assert r.returncode == 0, r.stderr
    out = r.stdout
    for key, verb in (("XF86AudioRaiseVolume", "up"), ("XF86AudioLowerVolume", "down"), ("XF86AudioMute", "toggle")):
        line = ('  <keybind key="%s"><action name="Execute"><command>/usr/bin/python3 %s %s</command></action>'
                '</keybind>' % (key, HELPER_DEFAULT, verb))
        assert line in out.splitlines() and out.index(line) < out.index("</keyboard>"), (key, out)
    for kept in RC.splitlines():
        assert kept in out.splitlines(), kept
    again = _merge(out)
    assert again.returncode == 0 and again.stdout == out, "the merge is idempotent"


def test_the_brightness_merge_is_the_same_filter():
    k = (SCRIPTS / "lib" / "obs-box-kiosk.sh").read_text()
    line = k[k.index("obs_box_openbox_rc_with_brightness_keys() {"):]
    line = line[:line.index("\n")]
    assert line.endswith("{ obs_box_openbox_rc_with_keys obs_box_brightness_keybinds_xml; }"), line


# --- 6. setup step 12 + verify item 9b on the stream role ------------------------------------------------------------

def test_install_applies_the_mixer_installs_the_helper_and_merges_the_keys(tmp_path):
    root, _ = snv_asound(tmp_path)
    home = tmp_path / "home"
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3"', env, REPO, home, ME)
    assert r.returncode == 0, (r.stdout, r.stderr)
    amixer = (tmp_path / "amixer.log").read_text()
    assert "-c Generic_1 sset Speaker mute" in amixer and "-c Generic_1 sset Auto-Mute Mode Enabled" in amixer
    assert " Gen " not in amixer
    assert (tmp_path / "alsactl.log").read_text().splitlines() == ["alsactl store Generic_1"]
    helper = tmp_path / "usr-local-bin" / "obs-box-headphones"
    assert helper.read_bytes() == VOL.read_bytes() and os.access(helper, os.X_OK)
    rc = (home / ".config" / "openbox" / "rc.xml").read_text()
    assert "XF86AudioRaiseVolume" in rc and "%s up" % helper in rc and "A-F4" in rc, rc
    assert "openbox --reconfigure" in r.stdout, r.stdout


def _installed_home(tmp_path):
    root, _ = snv_asound(tmp_path)
    return root, _installed(tmp_path, root)


def test_install_refuses_a_symlinked_rc_xml(tmp_path):
    # review round 1: root never reads or writes through a link the desktop user placed
    root, _ = snv_asound(tmp_path)
    home = tmp_path / "home"
    (home / ".config" / "openbox").mkdir(parents=True)
    target = tmp_path / "elsewhere.xml"
    target.write_text("<keyboard>\n</keyboard>\n")
    (home / ".config" / "openbox" / "rc.xml").symlink_to(target)
    env = _lib_env(tmp_path, root, objects=False)
    r = _bash(tmp_path, 'stream_operator_audio_install "$1" "$2" "$3" || echo RC=$?', env, REPO, home, ME)
    assert "RC=1" in r.stdout and "symlink" in r.stderr, (r.stdout, r.stderr)
    assert target.read_text() == "<keyboard>\n</keyboard>\n"


def test_verify_grades_ten_rows_with_the_control_row(tmp_path):
    root, home = _installed_home(tmp_path)
    rows = _rows(tmp_path, home, _lib_env(tmp_path, root))
    st, text = rows["operator-audio-control"]
    assert st == "OK" and "panel" in text and "keys" in text, text
    r = _bash(tmp_path, 'printf "%s" "$STREAM_OPERATOR_AUDIO_ROWS"', _lib_env(tmp_path, root))
    assert r.stdout == "10"


@pytest.mark.parametrize("piece", ["helper", "keys", "panel"])
def test_each_missing_control_piece_fails_the_control_row(tmp_path, piece):
    root, home = _installed_home(tmp_path)
    if piece == "helper":
        (tmp_path / "usr-local-bin" / "obs-box-headphones").write_text("# old\n")
    elif piece == "keys":
        rc = home / ".config" / "openbox" / "rc.xml"
        rc.write_text("\n".join(ln for ln in rc.read_text().splitlines() if "XF86AudioMute" not in ln) + "\n")
    else:
        cfg = home / ".config" / "tint2" / "tint2rc"
        cfg.write_text("\n".join(ln for ln in cfg.read_text().splitlines() if "uwheel" not in ln) + "\n")
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root))["operator-audio-control"]
    assert st == "FAIL" and {"helper": "obs-box-headphones", "keys": "XF86AudioMute",
                             "panel": "tint2rc"}[piece] in text, text


def test_verify_reads_the_operator_level_and_never_pins_it(tmp_path):
    root, home = _installed_home(tmp_path)
    objs = [o for o in healthy() if o["id"] != 41] + [node(41, SINK, "Audio/Sink", 40, vol=0.125)]
    st, text = _rows(tmp_path, home, _lib_env(tmp_path, root, objects=objs))["operator-audio-volume"]
    assert st == "OK" and "50 %" in text, text


def test_the_lib_says_how_to_apply_the_keys_and_the_level_lives_in_the_helper():
    lib = (SCRIPTS / "lib" / "stream-operator-audio.sh").read_text()
    assert "openbox --reconfigure" in lib
    assert "set-volume" not in (SCRIPTS / "stream_operator_audio.py").read_text().split("def grade(")[1].split(
        "def _amixer_sget")[0], "verify never tells the operator to pin the level"


def test_scripts_stay_within_their_line_budget():
    for name in ("setup-strih.sh", "verify-strih.sh"):
        assert len((SCRIPTS / name).read_text().splitlines()) <= 1000, name
