"""Issue 1465 item 3 (owner 23.10.2026): the headphone monitor and the stream OBS seed under the locked
PipeWire (design comment 6363778378, Approach 1).

What this pins (Tier-0, no rig):
  * scripts/strih_stream_monitor.py `--output pulse` (the stream role's unit): VLC plays through pulse to the
    operator sink by name, the headphone PCM must be held by PipeWire (the busy / owner checks and
    `--check-state` inverted), the jack's mixer unity writes stay (PipeWire's pro-audio profile never touches
    the ALSA mixer), a USB box keeps its own mixer. `--output alsa` (the default) is unchanged: the rollback;
  * scripts/program_audio_vban.py + scripts/strih_scenes.py: on the stream role (the manifest's
    `"pulse_capture": "forbid"`) the launch seed removes every scene pulse capture input (stream-snv's
    `Zvuk z mixu` on strih-program-source), never the program-audio input and never a global channel, which
    the read-only `--pulse-capture-inputs` reader names for verify.
"""
import importlib.util
import json
import os
import sys
import time
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_strih_stream_monitor_1436 import _applied, _mod, _state, asound, boxes  # noqa: E402,F401
from test_strih_stream_monitor_1465 import add_usb_card  # noqa: E402
from test_strih_stream_monitor_vlc_1456 import Rig  # noqa: E402

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
UNIT = REPO / "systemd" / "strih-stream-monitor.service"
SINK = "stream-operator-headphones"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1465", SCRIPTS / "program_audio_vban.py")


# --- 1. the monitor's pulse output ------------------------------------------------------------------------------

def test_the_pulse_argv_names_no_alsa_device():
    m = _mod()
    argv = m.vlc_argv("vlc", "plughw:CARD=Gen,DEV=0", "rtmp://127.0.0.1:1234/live/stream-snv", 8090, "pw",
                      output="pulse")
    assert "--aout=pulse" in argv and not [a for a in argv if "alsa" in a], argv
    assert argv[-1] == "rtmp://127.0.0.1:1234/live/stream-snv"
    assert m.PULSE_SINK_DEFAULT == SINK
    alsa = m.vlc_argv("vlc", "plughw:CARD=Gen,DEV=0", "rtmp://x/live/y", 8090, "pw")
    assert "--aout=alsa" in alsa, "the default stays the ALSA-direct rollback"


def _pulse_rig(tmp_path, owner=(4242, "pipewire"), usb=False):
    rig = Rig(tmp_path, owner=owner)
    if usb:
        add_usb_card(rig.root, 2, "Gen")
        st = rig.root / "card2" / "pcm0p" / "sub0" / "status"
        st.write_text("state: RUNNING\nowner_pid   : %d\ntrigger_time: 1.0\n" % owner[0])
    return rig


FAKE_PW_DUMP = r'''#!/usr/bin/env python3
import json, os, sys
mode = os.environ.get("FAKE_PW_DUMP_MODE", "sink")
if mode == "fail":
    sys.stderr.write("pw-dump: failed to connect\n"); sys.exit(1)
# review round 2: the sink must sit on the resolved headphone card (its device's api.alsa.card)
card = os.environ.get("FAKE_PW_CARD", "1")
objs = [7, {"id": 40, "type": "PipeWire:Interface:Device",
            "info": {"props": {"device.api": "alsa", "api.alsa.card": int(card), "device.name": "alsa_card.x"}}},
        {"id": 30, "type": "PipeWire:Interface:Node",
         "info": {"props": {"node.name": "alsa_output.pci-x.analog-stereo", "media.class": "Audio/Sink"}}}]
if mode == "sink":
    objs.append({"id": 41, "type": "PipeWire:Interface:Node",
                 "info": {"props": {"node.name": "stream-operator-headphones", "media.class": "Audio/Sink",
                                    "device.id": 40}}})
print(json.dumps(objs))
'''


def _run_pulse(rig, run_s=2.0, pw="sink", card="1"):
    # FAKE_PCM_STATUS empty: in the pulse output VLC never opens the PCM, PipeWire holds it throughout.
    # review round 1: the operator sink must exist in the session (pw-dump) before VLC starts.
    fake = rig.bin / "pw-dump"
    fake.write_text("#!%s\n%s" % (sys.executable, FAKE_PW_DUMP))
    fake.chmod(0o755)
    return rig.run(run_s, "--output", "pulse", "--pw-dump", str(fake), FAKE_PCM_STATUS="", FAKE_PW_DUMP_MODE=pw,
                   FAKE_PW_CARD=card)


def test_pulse_plays_to_the_operator_sink_while_pipewire_holds_the_jack(tmp_path):
    rig = _pulse_rig(tmp_path)
    rig.publish()
    r = _run_pulse(rig)
    assert r.returncode == 0, r.stdout + r.stderr
    argv = rig.vlc_argv()
    assert len(argv) == 1 and "--aout=pulse" in argv[0] and not [a for a in argv[0] if "alsa" in a], argv
    assert "env PULSE_SINK=%s" % SINK in rig.log(), rig.log()
    st = rig.st()
    assert st["output"] == "pulse" and st["device"] == "pulse:%s" % SINK, st
    assert any(ln.startswith("playing rtmp://127.0.0.1:1234/live/stream-pp -> pulse:%s" % SINK)
               for ln in r.stdout.splitlines()), r.stdout
    # the jack: PipeWire's pro-audio profile leaves the ALSA mixer alone, so the unity + Speaker mute stay
    assert ["-q", "-c", "Generic_1", "sset", "Speaker", "mute"] in [a.split() for a in rig.amixer()]
    assert "pulse output" in r.stdout.splitlines()[0], r.stdout


def test_pulse_with_a_focusrite_plugged_in_still_plays_the_jack(tmp_path):
    # ROZHODNUTE 6383739315 (owner 24.10.2026): the notebook jack, never a USB interface; the jack keeps its
    # unity + Speaker mute, the Focusrite's mixer is never touched
    rig = _pulse_rig(tmp_path, usb=True)
    rig.publish()
    r = _run_pulse(rig, card="1")
    assert r.returncode == 0, r.stderr
    assert ["-q", "-c", "Generic_1", "sset", "Speaker", "mute"] in [a.split() for a in rig.amixer()]
    assert not [a for a in rig.amixer() if " Gen " in " %s " % a], rig.amixer()
    assert rig.st()["card"] == 1 and rig.st()["state"] == "playing", rig.st()


def test_pulse_starts_vlc_on_a_closed_pcm_when_the_operator_sink_exists(tmp_path):
    # live stream-snv 23.10.2026: the locked PipeWire keeps the operator sink SUSPENDED (the PCM reads
    # `closed`) until its first client plays -- `session.suspend-timeout-seconds = 0` only stops a running
    # node from suspending. Requiring PipeWire to hold the PCM first was a deadlock: VLC never started.
    rig = Rig(tmp_path)  # the jack reads `closed`
    rig.publish()
    r = _run_pulse(rig)
    assert r.returncode == 0, r.stderr
    assert len(rig.vlc_argv()) == 1, rig.log()
    assert rig.st()["state"] == "playing", rig.st()


def test_pulse_on_a_closed_pcm_without_the_operator_sink_starts_nothing(tmp_path):
    rig = Rig(tmp_path)  # closed, and no operator sink in the session: the locked PipeWire is not running
    rig.publish()
    r = _run_pulse(rig, 1.5, pw="other")
    assert r.returncode == 0, r.stderr
    assert rig.vlc_argv() == [], rig.log()
    st = rig.st()
    assert st["state"] == "no-sink" and SINK in st["reason"], st


def test_pulse_without_the_operator_sink_never_starts_vlc(tmp_path):
    # review round 1: an unlocked PipeWire also holds the card while something plays; PULSE_SINK naming a
    # sink that does not exist would send VLC to the default sink -- maybe the notebook speakers
    for pw in ("other", "fail"):
        rig = _pulse_rig(tmp_path / pw)
        rig.publish()
        r = _run_pulse(rig, 1.5, pw=pw)
        assert r.returncode == 0, r.stderr
        assert rig.vlc_argv() == [], (pw, rig.log())
        st = rig.st()
        assert st["state"] == "no-sink" and SINK in st["reason"] and "pw-dump" in st["reason"], (pw, st)


def test_pulse_with_another_process_on_the_headphone_pcm_is_busy(tmp_path):
    rig = Rig(tmp_path, owner=(4343, "aplay"))
    r = _run_pulse(rig, 1.5)
    assert r.returncode == 0, r.stderr
    assert rig.vlc_argv() == []
    assert rig.st()["state"] == "busy" and "aplay" in rig.st()["reason"], rig.st()


def test_the_alsa_output_still_refuses_pipewire_on_the_card(tmp_path):
    rig = Rig(tmp_path, owner=(4242, "pipewire"))
    rig.run(1.5)
    assert rig.st()["state"] == "busy" and rig.st()["output"] == "alsa", rig.st()


def _pulse_state(tmp_path):
    p = _state(tmp_path)
    d = json.loads(p.read_text())
    d.update(output="pulse", device="pulse:%s" % SINK)
    p.write_text(json.dumps(d))
    return p


def test_check_state_pulse_needs_pipewire_on_the_headphone_pcm(tmp_path):
    m = _mod()
    for comm, ok_want in (("pipewire", True), ("vlc", False), ("aplay", False)):
        sub = tmp_path / comm
        root, proc = asound(sub, owner=(777, comm))
        ok, line = m.check_state(str(_pulse_state(sub)), time.time(), str(root), str(proc))
        assert ok is ok_want, (comm, line)
        if not ok_want:
            assert comm in line and "PipeWire" in line, line
    # a closed PCM is the operator sink suspended until its first sound (live 23.10.2026): healthy
    root, proc = asound(tmp_path / "closed")
    ok, line = m.check_state(str(_pulse_state(tmp_path / "closed")), time.time(), str(root), str(proc))
    assert ok and "suspended" in line, line


def test_check_state_fails_the_no_sink_fault(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path, owner=(777, "pipewire"))
    p = _pulse_state(tmp_path)
    d = json.loads(p.read_text())
    d.update(state="no-sink", reason="PipeWire does not hold it")
    p.write_text(json.dumps(d))
    ok, line = m.check_state(str(p), time.time(), str(root), str(proc))
    assert not ok and "no-sink" in line, line


def test_the_pcm_owner_is_the_thread_groups_process(tmp_path):
    m = _mod()
    root, proc = asound(tmp_path, owner=(5001, "data-loop.0"))
    (proc / "5001" / "status").write_text("Name:\tdata-loop.0\nTgid:\t5000\nPid:\t5001\n")
    (proc / "5000").mkdir()
    (proc / "5000" / "comm").write_text("pipewire\n")
    assert m.pcm_owner(str(root), str(proc), 1, 0) == {"state": "open", "pid": 5000, "comm": "pipewire"}


def test_the_stream_unit_plays_through_pulse():
    lines = [ln for ln in UNIT.read_text().splitlines() if ln.startswith("ExecStart=")]
    assert len(lines) == 1 and " --output pulse" in lines[0], lines


def test_the_stream_role_installs_the_pulse_tools(boxes):
    pkgs = _applied(boxes, "stream-pp")["PKGS"].split()
    for p in ("vlc", "alsa-utils", "pipewire-pulse", "pipewire-bin", "wireplumber"):
        assert p in pkgs, (p, pkgs)


# --- 2. the stream OBS seed: no pulse capture input ----------------------------------------------------------

class FakeObs:
    """An OBS listing INPUTS ({name: (kind, settings)}); SPECIAL = the global audio channels' names."""

    def __init__(self, inputs, special=None, removal_ignored=()):
        self.inputs = dict(inputs)
        self.special = special or {}
        self.removal_ignored = set(removal_ignored)
        self.calls = []

    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if t == "GetInputList":
            return {"inputs": [{"inputName": n, "inputKind": k} for n, (k, _s) in self.inputs.items()]}
        if t == "GetSpecialInputs":
            return dict({"desktop1": None, "desktop2": None, "mic1": None, "mic2": None, "mic3": None,
                         "mic4": None}, **self.special)
        if t == "GetInputSettings":
            return {"inputSettings": self.inputs[d["inputName"]][1]}
        if t == "RemoveInput":
            if d["inputName"] not in self.removal_ignored:
                self.inputs.pop(d["inputName"], None)
            return {}
        if t == "GetSceneList":
            return {"scenes": [{"sceneName": "PRO"}]}
        if t == "GetSceneItemList":
            return {"sceneItems": [{"sourceName": n} for n in self.inputs]}
        return {}

    def removed(self):
        return [d["inputName"] for t, d in self.calls if t == "RemoveInput"]


SNV = {"mbc": ("net.nagater.obs-vban.source", {"port": 6980}),
       "Zvuk z mixu": ("pulse_input_capture", {"device_id": "strih-program-source"}),
       "NDI 2ME PGM": ("ndi_source", {}),
       "Zaznam": ("pulse_output_capture", {"device_id": "default"})}


def test_the_seed_removes_every_scene_pulse_capture_and_names_it(monkeypatch):
    monkeypatch.setattr(PAV.time, "sleep", lambda s: None)
    obs = FakeObs(SNV)
    status = PAV.drop_pulse_capture_inputs(obs, keep="mbc")
    assert sorted(obs.removed()) == ["Zaznam", "Zvuk z mixu"], obs.calls
    assert "removed 'Zvuk z mixu' (pulse_input_capture on strih-program-source)" in status, status
    assert "removed 'Zaznam' (pulse_output_capture on default)" in status, status
    assert set(obs.inputs) == {"mbc", "NDI 2ME PGM"}


def test_the_program_audio_input_is_never_removed_by_this_rule(monkeypatch):
    monkeypatch.setattr(PAV.time, "sleep", lambda s: None)
    obs = FakeObs({"Zvuk z mixu": ("pulse_input_capture", {"device_id": "x"})})
    status = PAV.drop_pulse_capture_inputs(obs, keep="Zvuk z mixu")
    assert obs.removed() == [] and status == "no pulse capture input", status


def test_a_global_channel_is_named_never_removed(monkeypatch):
    monkeypatch.setattr(PAV.time, "sleep", lambda s: None)
    obs = FakeObs({"Desktop Audio": ("pulse_output_capture", {"device_id": "default"})},
                  special={"desktop1": "Desktop Audio"})
    status = PAV.drop_pulse_capture_inputs(obs, keep="mbc")
    assert obs.removed() == []
    assert "global channel 'Desktop Audio' (pulse_output_capture)" in status and "Settings" in status, status


def test_a_removal_obs_never_applies_is_a_loud_failure(monkeypatch):
    monkeypatch.setattr(PAV.time, "sleep", lambda s: None)
    obs = FakeObs(SNV, removal_ignored={"Zvuk z mixu"})
    status = PAV.drop_pulse_capture_inputs(obs, keep="mbc")
    assert "remove FAILED: 'Zvuk z mixu'" in status, status


def test_the_reader_lists_every_pulse_capture_for_verify():
    obs = FakeObs(dict(SNV, **{"Desktop Audio": ("pulse_output_capture", {})}), special={"desktop1": "Desktop Audio"})
    line = PAV.pulse_capture_report(obs)
    assert line == ("pulse capture inputs: 'Desktop Audio' (pulse_output_capture, global channel); "
                    "'Zaznam' (pulse_output_capture, scene); 'Zvuk z mixu' (pulse_input_capture, scene)"), line
    assert PAV.pulse_capture_report(FakeObs({"mbc": SNV["mbc"]})) == "pulse capture inputs: none"


def test_the_manifest_rule_is_lenient():
    assert PAV.parse_pulse_capture_rule('{"pulse_capture": "forbid"}') == "forbid"
    for text in ("{not json", "[]", '{"pulse_capture": "allow"}', '{"pulse_capture": 1}', "{}"):
        assert PAV.parse_pulse_capture_rule(text) is None, text


def test_the_stream_manifests_forbid_pulse_capture_the_strih_ones_do_not(boxes):
    from test_obs_box_stream_role_1412 import _manifest
    for box, want in (("stream-pp", "forbid"), ("stream-snv", "forbid"), ("strih-snv", None), ("strih-pp", None)):
        text = _manifest(boxes, box)
        assert PAV.parse_pulse_capture_rule(text) == want, box


def test_bootstrap_drops_the_pulse_capture_only_when_the_manifest_forbids_it(monkeypatch):
    monkeypatch.setattr(PAV.time, "sleep", lambda s: None)
    s = _load("strih_scenes_1465", SCRIPTS / "strih_scenes.py")
    monkeypatch.setattr(s, "_obs_phase2_module", lambda: None)
    obs = FakeObs(SNV)
    res = s.bootstrap(obs, [], studio=False, update_only=True, audio_name="mbc", pulse_capture="forbid")
    assert "Zvuk z mixu" in obs.removed() and "removed 'Zvuk z mixu'" in res["pulse capture"], res
    obs2 = FakeObs({"ASIO zvuk": ("pulse_input_capture", {"device_id": "strih-program-source"})})
    res2 = s.bootstrap(obs2, [], studio=False, update_only=True)
    assert obs2.removed() == [] and "pulse capture" not in res2, res2


def test_main_wires_the_rule_and_the_reader():
    src = (SCRIPTS / "strih_scenes.py").read_text()
    assert "pulse_capture=parse_pulse_capture_rule(manifest_text)," in src
    assert "audio_vban=audio_vban)" in src
    assert '"--pulse-capture-inputs"' in src and "pulse_capture_report(" in src


def test_pulse_with_the_operator_sink_on_another_card_never_starts_vlc(tmp_path):
    # review round 2: a stale drop-in could rename a node of another card; the sink must sit on the headphone card
    rig = _pulse_rig(tmp_path)
    rig.publish()
    r = _run_pulse(rig, 1.5, card="7")
    assert r.returncode == 0, r.stderr
    assert rig.vlc_argv() == [], rig.log()
    st = rig.st()
    assert st["state"] == "no-sink" and "card 1" in st["reason"], st
