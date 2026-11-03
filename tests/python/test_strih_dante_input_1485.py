"""Issue 1485: strih-snv takes its program from the Dante device strih-obs (the obs release's dante_input).

Covers the facts (the loader's one-program-source rule), the seed manifest + the seeder's switch to the
dante_input kind (operator state kept), the ~/.camera-box/dante-input.toml render from the facts + the
untracked site file, the install / removal, verify item 9's (program-dante) rows, the PipeWire-purge
condition, and the E2E continuity grade of the Dante source (decide, fold, the gate through its seam).
"""
import importlib.util
import json
import os
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
CONT_LIB = SCRIPTS / "lib" / "program-audio-continuity.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE", "STRIH_DANTE_INPUT_KIND_FILE",
         "STRIH", "STREAM")


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


CONT = _load("dante_input_continuity_1485", SCRIPTS / "dante_input_continuity.py")
PAV = _load("program_audio_vban_1485", SCRIPTS / "program_audio_vban.py")


def _bash(body, *args, env=None):
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _boxes_with(tmp_path, box, pairs):
    d = tmp_path / "boxes"
    d.mkdir(exist_ok=True)
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    f = d / ("%s.env" % box)
    lines = f.read_text().splitlines()
    for k, v in pairs.items():
        lines = ["%s=%s" % (k, v) if l.startswith(k + "=") else l for l in lines]
    f.write_text("\n".join(lines) + "\n")
    return d


SITE = "# test site file (fake names)\nRX1=Program L|foh-tx|MASTER-L\nRX2=Program R|foh-tx|MASTER-R\n"


# --- facts --------------------------------------------------------------------------------------------

def test_only_strih_snv_takes_the_program_from_dante():
    r = _bash('for b in strih-snv strih-pp stream-snv stream-pp; do (strih_box_load "$b" >/dev/null; '
              'if strih_program_dante_declared; then echo "$b yes"; else echo "$b no"; fi); done')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:4] == ["strih-snv yes", "strih-pp no", "stream-snv no", "stream-pp no"]


def test_strih_snv_has_no_vban_program_and_keeps_its_input_name():
    f = dict(l.split("=", 1) for l in (BOXES / "strih-snv.env").read_text().splitlines() if l and l[0] != "#")
    assert (f["STRIH_PROGRAM_VBAN_STREAMS"], f["STRIH_PROGRAM_VBAN_PORT"], f["STRIH_PROGRAM_VBAN_RESERVE_MS"]) == ("none",) * 3
    assert f["STRIH_PROGRAM_AUDIO_INPUT"] == "ASIO zvuk"
    assert "strih-obs=4440:8800:4455:47200-47231" in f["STRIH_DANTE_DEVICES"].split()
    assert "master-asio-dante" not in (BOXES / "strih-snv.env").read_text(), "site data never in the public repo"


def test_a_dante_program_with_a_vban_program_too_is_refused(tmp_path):
    d = _boxes_with(tmp_path, "strih-snv", {"STRIH_PROGRAM_VBAN_STREAMS": "strih-obs", "STRIH_PROGRAM_VBAN_PORT": "6981",
                                           "STRIH_PROGRAM_VBAN_RESERVE_MS": "30"})
    r = _bash('strih_box_load "$1"', "strih-snv", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "strih-obs Dante device" in r.stderr, r.stderr


def test_a_dante_program_needs_its_input_name(tmp_path):
    d = _boxes_with(tmp_path, "strih-snv", {"STRIH_PROGRAM_AUDIO_INPUT": "none"})
    r = _bash('strih_box_load "$1"', "strih-snv", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "issue 1485" in r.stderr, r.stderr


def test_the_seed_manifest_declares_the_dante_program():
    r = _bash('strih_box_load strih-snv >/dev/null; strih_lx_seed_manifest_json')
    assert r.returncode == 0, r.stderr
    m = json.loads(r.stdout)
    assert m["program_audio_input"] == "ASIO zvuk" and m["program_audio_dante"] == {"device": "strih-obs"}
    assert "program_audio_vban" not in m


def test_a_vban_strih_manifest_is_unchanged():
    r = _bash('strih_box_load strih-pp >/dev/null; strih_lx_seed_manifest_json')
    m = json.loads(r.stdout)
    assert "program_audio_dante" not in m and m["program_audio_vban"]["stream_name"] == "strih-obs"


def test_the_purge_condition_counts_a_dante_program(tmp_path):
    toml = tmp_path / "hub.toml"
    toml.write_text('[[participants]]\nname = "x"\nadapter = "alsa"\n')
    r = _bash('strih_box_load strih-snv >/dev/null; strih_pipewire_purge_why', env={"STRIH_PW_PURGE_TOML": str(toml)})
    assert r.stdout.startswith("purge|") and "dante:strih-obs" in r.stdout, r.stdout + r.stderr


# --- the toml -----------------------------------------------------------------------------------------

def _render(tmp_path, site=SITE):
    s = tmp_path / "site.conf"
    s.write_text(site)
    return _bash('strih_box_load strih-snv >/dev/null; rows="$(strih_dante_input_site_rows "$1")"; '
                 'strih_dante_input_toml enp7s0.70 "$(strih_dante_input_ports "$(strih_lx_dante_devices)")" "$rows"', s)


def test_the_toml_is_the_example_format_from_the_facts_and_the_site_file(tmp_path):
    r = _render(tmp_path)
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert d == {"name": "strih-obs", "iface": "enp7s0.70", "network_rate": 96000, "latency_ms": 10,
                 "rx": ["Program L", "Program R"], "arc_port": 4440, "cmc_port": 8800, "flows_port": 4455,
                 "rx_port_base": 47200,
                 "subscribe": [{"rx": 1, "tx_channel": "MASTER-L", "tx_device": "foh-tx"},
                               {"rx": 2, "tx_channel": "MASTER-R", "tx_device": "foh-tx"}]}
    assert "sample_rate" not in d


@pytest.mark.parametrize("site,why", [
    ("RX1=a|b\n", "three fields"),
    ('RX1=a|b"x|c\n', "three fields"),
    ("RX1=a|b|c\nRX3=a|b|c\n", "no RX2"),
    ("RX1=a|b|c\nRX1=a|b|c\n", "twice"),
    ("RX9=a|b|c\n", "not RX<1-8>"),
    ("".join("RX%d=a|b|c\n" % i for i in range(1, 8)), "OBS takes 1 to 6 or 8"),
    ("# nothing\n", "OBS takes 1 to 6 or 8"),
])
def test_a_malformed_site_file_is_refused_by_name(tmp_path, site, why):
    r = _render(tmp_path, site)
    assert r.returncode != 0 and why in r.stderr, r.stderr


def test_install_writes_the_file_and_a_missing_site_file_fails(tmp_path):
    dest = tmp_path / "home" / ".camera-box" / "dante-input.toml"
    site = tmp_path / "site.conf"
    env = {"STRIH_DANTE_INPUT_TOML": str(dest), "STRIH_DANTE_INPUT_IFACE": "enp7s0.70",
           "STRIH_DANTE_INPUT_SITE_FILE": str(site)}
    user = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    body = ('. scripts/lib/obs-box-baseline.sh; fail() { echo "FAIL $*" >&2; exit 1; }; strih_box_load strih-snv >/dev/null; '
            'strih_dante_input_install "$1" "$2"')
    r = _bash(body, user, tmp_path / "home", env=env)
    assert r.returncode == 1 and "is missing" in r.stderr, r.stderr
    site.write_text(SITE)
    r = _bash(body, user, tmp_path / "home", env=env)
    assert r.returncode == 0, r.stderr
    assert tomllib.loads(dest.read_text())["iface"] == "enp7s0.70"
    assert "written" in r.stdout
    r = _bash(body, user, tmp_path / "home", env=env)
    assert "unchanged" in r.stdout
    # a box without the device removes OUR file, never an operator's own
    r = _bash('strih_box_load strih-pp >/dev/null; strih_dante_input_install "$1" "$2"', user, tmp_path / "home", env=env)
    assert r.returncode == 0 and not dest.exists(), r.stdout + r.stderr
    dest.write_text("name = \"mine\"\n")
    r = _bash('strih_box_load strih-pp >/dev/null; strih_dante_input_install "$1" "$2"', user, tmp_path / "home", env=env)
    assert dest.exists() and "not ours" in r.stderr


# --- verify item 9 -----------------------------------------------------------------------------------

def _status(t, f, **k):
    v = dict(clock="media", holdover="0", gm="001dc1fffe0a", polls="1", re_anchors="0", rx_packets="10", rx_loss="0",
             rx_gaps="0", rx_late="0", socket_drops="0", blocks="5", underruns="0", overruns="0", delivered_frames=str(f))
    v.update(k)
    return "%s: dante-input status: %s" % (t, " ".join("%s=%s" % i for i in v.items()))


START = ("12:00:00.000: dante-input start: config=/home/newlevel/.camera-box/dante-input.toml name=strih-obs "
         "iface=enp7s0.70 latency_ms=10 obs_rate=48000 sample_rate=48000 device_rate=96000 halved=true rx=[] "
         "ports=Some(4440)/Some(8800)/Some(4455) rx_port_base=Some(47200) subscriptions=2 state_file=x")


def _grade(tmp_path, log_lines, kind="dante_input", toml=True, grow=True):
    log = tmp_path / "obs.log"
    log.write_text("\n".join(log_lines) + "\n")
    kf = tmp_path / "kind"
    kf.write_text(kind)
    dest = tmp_path / "dante-input.toml"
    if toml:
        dest.write_text('name = "strih-obs"\n[[subscribe]]\n')
    # the "live window": a sleep stand-in that appends one more status line
    nxt = _status("12:00:20.000", 999999)
    sleep_fn = ('sleep() { printf "%%s\\n" %s >> %s; }' % (json.dumps(nxt), json.dumps(str(log)))) if grow else 'sleep() { :; }'
    body = ('ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; note() { echo "NOTE $*"; }; %s; '
            'strih_box_load strih-snv >/dev/null; strih_dante_input_grade "$1" "$2" "$4"') % sleep_fn
    return _bash(body, "scn", "127.0.0.1", "unused", log,
                 env={"STRIH_DANTE_INPUT_KIND_FILE": str(kf), "STRIH_DANTE_INPUT_TOML": str(dest),
                      "STRIH_DANTE_INPUT_WAIT_S": "0"})


def test_item9_passes_a_healthy_dante_source(tmp_path):
    r = _grade(tmp_path, [START, _status("12:00:10.000", 4800)])
    assert r.returncode == 0, r.stderr
    assert "BAD" not in r.stdout, r.stdout
    assert "OK (program-dante) live: delivered_frames 4800 -> 999999" in r.stdout


def test_item9_fails_each_fault_by_name(tmp_path):
    r = _grade(tmp_path, ["x", "12:00:01.000: dante-input error: the device did not start from x: boom"], kind="absent",
               toml=False, grow=False)
    out = r.stdout
    assert "BAD (program-dante) " in out and "is missing or names no strih-obs" in out
    assert "is 'absent', not the dante_input source" in out
    assert "no 'dante-input start: config=' line" in out
    assert "1 'dante-input error:' line(s)" in out and "boom" in out
    assert "no dante-input status: line" in out


def test_item9_fails_a_free_clock_and_a_halved_false_start(tmp_path):
    start = START.replace("halved=true", "halved=false")
    r = _grade(tmp_path, [start, _status("12:00:10.000", 1, clock="free", holdover="1")], grow=False)
    assert "wants 96000 / true" in r.stdout
    assert "clock=free" in r.stdout and "holdover=1" in r.stdout


def test_item9_fails_a_stalled_device(tmp_path):
    r = _grade(tmp_path, [START, _status("12:00:10.000", 4800)], grow=False)
    assert "BAD (program-dante) delivered_frames did not advance" in r.stdout, r.stdout


def test_verify_strih_dispatches_item9_to_the_dante_grade():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert 'if strih_program_audio_declared; then' in v and 'strih_program_audio_grade "$SCN_BIN" "$WS_HOST" "$LOG"' in v
    leg = (SCRIPTS / "lib" / "strih-vban-leg.sh").read_text()
    assert "graded by (program-dante)" in leg


def test_setup_strih_writes_the_toml_after_the_leg():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    a, b = s.index("strih_vban_leg_install ||"), s.index('strih_dante_input_install "$DESKTOP_USER" "$USER_HOME"')
    assert a < b


# --- the seed -----------------------------------------------------------------------------------------

class FakeObs:
    def __init__(self, kind, kinds=("dante_input",)):
        self.kind, self.kinds, self.calls, self.present = kind, list(kinds), [], True

    def req(self, t, d=None, ignore_err=False):
        self.calls.append((t, d))
        if t == "GetInputList":
            return {"inputs": [{"inputName": "ASIO zvuk", "inputKind": self.kind}] if self.present else []}
        if t == "RemoveInput":
            self.present = False
            return {}
        if t == "CreateInput":
            self.present, self.kind = True, d["inputKind"]
            return {"sceneItemId": 7}
        if t == "GetInputKindList":
            return {"inputKinds": self.kinds}
        if t == "GetSceneList":
            return {"currentProgramSceneName": "PRO", "scenes": [{"sceneName": "PRO"}]}
        if t == "GetSceneItemList":
            return {"sceneItems": [{"sourceName": "ASIO zvuk", "sceneItemEnabled": True}]}
        if t == "GetInputVolume":
            return {"inputVolumeMul": 0.5}
        return {}


def _scenes():
    return _load("strih_scenes_1485", SCRIPTS / "strih_scenes.py")


def test_the_seed_switches_a_vban_input_to_dante_keeping_its_state():
    s = _scenes()
    obs = FakeObs("net.nagater.obs-vban.source")
    st = s.seed_program_audio_input(obs, "ASIO zvuk", dict(PAV.DANTE_SEED))
    assert st.startswith("replaced (was net.nagater.obs-vban.source"), st
    created = [d for t, d in obs.calls if t == "CreateInput"]
    assert created and created[-1]["inputKind"] == "dante_input" and created[-1]["inputSettings"] == {}
    assert ("SetInputVolume", {"inputName": "ASIO zvuk", "inputVolumeMul": 0.5}) in obs.calls


def test_the_seed_matches_a_dante_input_and_keeps_one_when_the_kind_is_not_registered():
    s = _scenes()
    assert s.seed_program_audio_input(FakeObs("dante_input"), "ASIO zvuk", dict(PAV.DANTE_SEED)) == "matched"
    obs = FakeObs("net.nagater.obs-vban.source", kinds=())
    st = s.seed_program_audio_input(obs, "ASIO zvuk", dict(PAV.DANTE_SEED))
    assert st.startswith("kept net.nagater.obs-vban.source") and not [c for c in obs.calls if c[0] == "RemoveInput"]


def test_the_manifest_block_parses():
    assert PAV.parse_program_audio_dante('{"program_audio_dante": {"device": "strih-obs"}}') is True
    assert PAV.parse_program_audio_dante('{"program_audio_vban": {}}') is False
    with pytest.raises(ValueError):
        PAV.parse_program_audio_dante('{"program_audio_dante": 1}')


# --- the E2E continuity grade ------------------------------------------------------------------------

def _window(**closing):
    return [_status("12:00:00.000", 100), "mark", _status("12:00:10.000", 200), "stop",
            _status("12:00:20.000", 300), _status("12:00:30.000", 400, **closing)]


def test_a_clean_window_passes():
    r = CONT.decide(_window(), 1, 3)
    assert r["verdict"] == "PASS", r
    assert r["counters"]["delivered_frames"]["delta"] == 300


@pytest.mark.parametrize("key", ["rx_loss", "rx_gaps", "rx_late", "socket_drops", "underruns", "overruns", "re_anchors"])
def test_any_loss_gap_late_drop_underrun_or_overrun_fails(key):
    r = CONT.decide(_window(**{key: "2"}), 1, 3)
    assert r["verdict"] == "FAIL" and r["failures"] == ["%s +2" % key], r


def test_unknown_fails_too():
    assert CONT.decide(_window()[1:], 0, 2)["verdict"] == "UNKNOWN"              # no opening line
    r = CONT.decide(_window()[:5], 1, 3)
    assert r["verdict"] == "UNKNOWN" and r["retryable"]                           # the closing line is not there yet
    w = _window()
    w[5] = _status("12:00:30.000", 400, rx_packets="1")
    assert CONT.decide(w, 1, 3)["verdict"] == "UNKNOWN"                           # a counter went backwards
    w = _window()
    w[5] = _status("12:00:30.000", 100)
    assert CONT.decide(w, 1, 3)["verdict"] == "UNKNOWN"                           # no audio delivered
    w = _window()
    w.insert(3, START)
    assert CONT.decide(w, 1, 4)["verdict"] == "UNKNOWN"                           # the device restarted


def test_a_free_clock_or_an_error_line_in_the_window_fails():
    w = _window()
    w[2] = _status("12:00:10.000", 200, clock="free")
    assert CONT.decide(w, 1, 3)["verdict"] == "FAIL"
    w = _window()
    w.insert(3, "12:00:11.000: dante-input error: the device ended by itself: x")
    assert CONT.decide(w, 1, 4)["verdict"] == "FAIL"


def test_the_fold_takes_the_worse_verdict_and_names_the_dante_reasons():
    f = CONT.fold({"verdict": "PASS", "pass": True, "gates_overall_pass": True, "failures": []},
                  CONT.decide(_window(rx_loss="1"), 1, 3))
    assert f["verdict"] == "FAIL" and f["pass"] is False and f["failures"] == ["strih Dante: rx_loss +1"]
    f = CONT.fold({"verdict": "PASS", "pass": True, "gates_overall_pass": True}, CONT.decide(_window(), 1, 3))
    assert f["verdict"] == "PASS" and f["strih_dante"]["verdict"] == "PASS"


def test_the_gate_marks_and_grades_the_strih_log_through_its_seam(tmp_path):
    log = tmp_path / "strih.log"
    lines = _window(rx_gaps="3")
    log.write_text("\n".join(lines[:2]) + "\n")
    reader = tmp_path / "reader"
    reader.write_text("#!/bin/bash\nset -euo pipefail\nf=%s\nif [ \"$1\" = count ]; then wc -l < \"$f\"; "
                      "else tail -n +\"$(( $3 + 1 ))\" \"$f\"; fi\n" % log)
    reader.chmod(0o755)
    block = tmp_path / "program-audio-continuity.json"
    block.write_text(json.dumps({"verdict": "PASS", "pass": True, "gates_overall_pass": True, "failures": []}))
    env = {"PROGRAM_AUDIO_CONTINUITY_STRIH_READ_CMD": str(reader), "STRIH": "10.77.9.202",
           "PROGRAM_AUDIO_CONTINUITY_STEP_S": "0", "PROGRAM_AUDIO_CONTINUITY_WAIT_S": "0"}
    lib = 'set -euo pipefail\n. "%s"\n' % CONT_LIB
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env)
    run = lambda body: subprocess.run(["bash", "-c", lib + body, "h", str(tmp_path), str(block)],
                                      capture_output=True, text=True, env=e, cwd=str(REPO), timeout=60)
    r = run('program_audio_continuity_mark start "$1" 127.0.0.9 >/dev/null 2>&1 || true; cat "$1/program-audio-continuity-strih-start.mark"')
    assert r.stdout.strip() == "2", r.stdout + r.stderr
    log.write_text("\n".join(lines[:4]) + "\n")
    run('program_audio_continuity_mark stop "$1" 127.0.0.9 >/dev/null 2>&1 || true')
    log.write_text("\n".join(lines) + "\n")
    r = run('_program_audio_continuity_strih_gate "$1" "$2"')
    assert r.returncode == 0, r.stderr
    b = json.loads(block.read_text())
    assert b["verdict"] == "FAIL" and "strih Dante: rx_gaps +3" in b["failures"], b


def test_the_strih_snv_manifest_parses_to_the_dante_seed():
    r = _bash('strih_box_load strih-snv >/dev/null; strih_lx_seed_manifest_json')
    assert PAV.parse_program_audio_vban(r.stdout) == PAV.DANTE_SEED
    assert PAV.program_audio_want(PAV.DANTE_SEED, "pulse", {"d": 1}) == ("dante_input", {})
    with pytest.raises(ValueError):
        PAV.parse_program_audio_vban('{"program_audio_dante": 1}')
