"""issue 1439 phase 2, the strih role: the console's VBAN program straight into the strih OBS obs-vban source.

Addendum 6338457001 (owner ROZHODNUTE 22.10.2026: "interkom-hub ma byt separatna vec, nema nic preposielat do
strih obs") + the PHASE 2 inputs 6361250274 (fohabl 10.77.7.30 sends the NEW stream `strih-obs` to
strih-snv 10.77.7.202:6981 over the VLAN-70 leg, 96 kHz, 239 frames per packet, 2 ch, PCM24). One unified
design: the program VBAN is a box FACT on every role, and every consumer follows the facts, never the role:

  * the loader takes the four program facts on the strih role too -- all `none` (a hub-fed strih; none is
    deployed since 24.10.2026) or all set (strih-snv: `strih-obs` / 6981 / 30 / `ASIO zvuk`; strih-pp, design
    6385519443: lv1's `strih-obs` / 6981 / 40 / `ASIO zvuk`), never a mix;
  * the strih seed manifest then carries `program_audio_input` + `program_audio_vban`, which the role-free
    seeder (strih_scenes.py -> program_audio_vban.py) already switches, keeping the operator's mixer state;
  * verify item 9 grades the obs-vban source on every box that declares it;
  * the hub forwards nothing into OBS: the VB-Matrix converter no longer emits `program_out`, and a box that
    declares a program VBAN stream has no `program_out` in its hub TOML.

Tier-0: bash + pytest only, no OBS, no rig.
"""
import importlib.util
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from strih_pp_hub_fed_1439 import HUB_FED_PAIRS  # noqa: E402

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"
HEADER = REPO / "vendor" / "obs-vban" / "src" / "vban-receive-timeline.h"
XML = REPO / "intercom" / "tests" / "fixtures" / "vbmatrix-coconut-today.xml"
VBAN_KIND = "net.nagater.obs-vban.source"

LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "RIG_GRANDMASTER_IP", "GH_TOKEN", "NDI_DISCOVERY_SENDER_HOSTS",
         "STRIH_PROGRAM_VBAN_WAIT_S", "STRIH_PROGRAM_VBAN_KIND_FILE")


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1439_strih", SCRIPTS / "program_audio_vban.py")


def _scenes():
    return _load("strih_scenes_1439_strih", SCRIPTS / "strih_scenes.py")


def _bash(body, *args, env=None):
    """BODY under the callers' strict mode, the provisioning libs sourced; values reach it as $1.. only."""
    e = {k: v for k, v in os.environ.items() if k not in LEAKY}
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _facts(box):
    out = {}
    for line in (BOXES / ("%s.env" % box)).read_text().splitlines():
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    return out


def _boxes_with(tmp_path, box, pairs):
    d = tmp_path / "boxes"
    d.mkdir()
    for f in BOXES.glob("*.env"):
        (d / f.name).write_text(f.read_text())
    f = d / ("%s.env" % box)
    t = f.read_text()
    for old, new in pairs:
        assert t.count(old) == 1, (box, old)
        t = t.replace(old, new)
    f.write_text(t)
    return d



def _header_define(name):
    m = re.search(r"#define\s+%s\s+([0-9]+)" % name, HEADER.read_text())
    assert m, name
    return int(m.group(1))


# --- the facts ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("box,want", [("strih-snv", "strih-obs|6981|30|ASIO zvuk"),
                                      ("strih-pp", "strih-obs|6981|40|ASIO zvuk")])
def test_each_strih_box_declares_its_program_vban(box, want):
    r = _bash('strih_box_load "$1"\nprintf "%s|%s|%s|%s" "$(strih_lx_program_vban_streams)" '
              '"$(strih_lx_program_vban_port)" "$(strih_lx_program_vban_reserve_ms)" '
              '"$(strih_lx_program_audio_input)"', box)
    assert r.returncode == 0, r.stderr
    assert r.stdout == want


def test_strih_snv_receives_on_6981_from_the_console_on_its_leg():
    # one UDP port has one receiver: the hub owns 6980 (fohabl-strih, the camboxes), OBS takes 6981, and
    # the leg filter admits the console on both (STRIH_PROGRAM_VBAN_SENDER, comment 6361250274)
    f = _facts("strih-snv")
    assert f["STRIH_PROGRAM_VBAN_PORT"] == "6981"
    assert f["STRIH_PROGRAM_VBAN_SENDER"] == "10.77.7.30:6980,6981"
    assert f["STRIH_PROGRAM_VBAN_LEG"] == "vlan:70"


def test_strih_pp_receives_lv1s_strih_obs_on_6981_with_no_leg():
    # issue 1439 on strih-pp (design 6385519443): lv1 10.77.8.230 sends `strih-obs` to the rig NIC
    # 10.77.8.202:6981 directly (tcpdump 24.10.2026: 48 kHz, 103 frames, 2 ch, PCM16), as stream-pp's lv1
    # leg: no VBAN-only leg; the hub keeps 6980 for lv1's `strih` (the cans + the vestibule)
    f = _facts("strih-pp")
    assert (f["STRIH_PROGRAM_VBAN_STREAMS"], f["STRIH_PROGRAM_VBAN_PORT"]) == ("strih-obs", "6981")
    assert (f["STRIH_PROGRAM_VBAN_LEG"], f["STRIH_PROGRAM_VBAN_LEG_ADDR"], f["STRIH_PROGRAM_VBAN_SENDER"]) == (
        "none", "none", "none")
    hub = tomllib.loads((REPO / f["STRIH_INTERCOM_CONFIG"]).read_text())
    assert hub["hub"]["vban_bind"].endswith(":6980")


def test_strih_pps_r_sits_above_lv1s_worst_gap():
    # R = 40 ms, the value lv1 is proven at on stream-pp (lost 0, ~27 ms spare); lv1's worst gap there
    # read 26-29 ms. A packet is in time with up to R - one packet (103 frames at 48 kHz) - the 2 ms
    # playout margin of delay over the earliest arrival.
    r = _bash('strih_box_load strih-pp\nprintf "%s" "$(strih_lx_program_vban_reserve_ms)"')
    reserve_ms = int(r.stdout)
    budget_ms = reserve_ms - 103 * 1000.0 / 48_000 - _header_define("VBAN_RECV_DEADLINE_MARGIN_MS")
    assert budget_ms > 29, (reserve_ms, budget_ms)
    assert reserve_ms <= 40, "the strih path's R stays low (owner 22.10.2026: low latency against double sound)"


def test_strih_snvs_low_r_sits_above_the_consoles_measured_gap_on_the_leg():
    # Validation 6372931419: fohabl's fohabl-strih over the same enp7s0.70 leg read max_gap_ms_10min
    # 11.4-11.5 ms. A packet is in time with up to R - one packet - the playout deadline margin of delay;
    # strih-obs = 239 frames at 96 kHz. R stays LOW (the strih path's double-sound budget, issue-1437
    # item 4): at most 25 ms.
    r = _bash('strih_box_load strih-snv\nprintf "%s" "$(strih_lx_program_vban_reserve_ms)"')
    reserve_ms = int(r.stdout)
    budget_ms = reserve_ms - 239 * 1000.0 / 96_000 - _header_define("VBAN_RECV_DEADLINE_MARGIN_MS")
    assert budget_ms > 11.5, (reserve_ms, budget_ms)
    # Live 23.10.2026 22:59:48 on strih-snv at R = 20: one packet arrived 1.5 ms past its deadline
    # (`obs-vban receive late: ... late_ms=1.5`, 23 s after the OBS start), so the in-time window must
    # reach 20 ms + 1.5 ms with headroom; the steady state read max_late_ms -5 to -7 (13-15 ms used).
    # The cap keeps R low for the strih path (owner 22.10.2026: low audio latency against double sound).
    in_time_ms = reserve_ms - _header_define("VBAN_RECV_DEADLINE_MARGIN_MS")
    assert in_time_ms >= 20 + 1.5 + 5, (reserve_ms, in_time_ms)
    assert reserve_ms <= 30


@pytest.mark.parametrize("pairs", [
    [("STRIH_PROGRAM_VBAN_STREAMS=strih-obs\n", "STRIH_PROGRAM_VBAN_STREAMS=none\n")],
    [("STRIH_PROGRAM_AUDIO_INPUT=ASIO zvuk\n", "STRIH_PROGRAM_AUDIO_INPUT=none\n")],
    [("STRIH_PROGRAM_VBAN_STREAMS=strih-obs\n", "STRIH_PROGRAM_VBAN_STREAMS=none\n"),
     ("STRIH_PROGRAM_VBAN_PORT=6981\n", "STRIH_PROGRAM_VBAN_PORT=none\n"),
     ("STRIH_PROGRAM_VBAN_RESERVE_MS=40\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=none\n")],
])
def test_a_strih_box_declares_all_four_program_facts_or_none(tmp_path, pairs):
    d = _boxes_with(tmp_path, "strih-pp", pairs)
    r = _bash('strih_box_load "$1"', "strih-pp", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0
    assert "all none" in r.stderr and "strih role" in r.stderr, r.stderr


def test_a_hub_fed_strih_box_with_all_four_program_facts_none_still_loads(tmp_path):
    d = _boxes_with(tmp_path, "strih-pp", HUB_FED_PAIRS)
    r = _bash('strih_box_load "$1"\nprintf "%s" "$(strih_lx_program_vban_port)"', "strih-pp",
              env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout == "none"


@pytest.mark.parametrize("old,new,word", [
    ("STRIH_PROGRAM_VBAN_RESERVE_MS=30\n", "STRIH_PROGRAM_VBAN_RESERVE_MS=9\n", "10..500"),
    ("STRIH_PROGRAM_VBAN_PORT=6981\n", "STRIH_PROGRAM_VBAN_PORT=80\n", "1024..65535"),
    ("STRIH_PROGRAM_VBAN_STREAMS=strih-obs\n", "STRIH_PROGRAM_VBAN_STREAMS=strih-obs lv1-obs\n", "ONE VBAN stream"),
])
def test_a_strih_boxs_program_facts_are_checked_like_the_stream_roles(tmp_path, old, new, word):
    d = _boxes_with(tmp_path, "strih-snv", [(old, new)])
    r = _bash('strih_box_load "$1"', "strih-snv", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0
    assert word in r.stderr, r.stderr


# --- the seed manifest + the seeder ------------------------------------------------------------------------

def _manifest(box):
    r = _bash('strih_box_load "$1"\nobs_box_seed_manifest_json', box)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_strih_snv_manifest_switches_asio_zvuk_to_the_vban_source():
    text = _manifest("strih-snv")
    scn = _scenes()
    assert scn.parse_program_audio_input(text) == "ASIO zvuk"
    assert PAV.parse_program_audio_vban(text) == PAV.vban_source_settings(6981, "strih-obs", 30)
    # the strih role keeps the operator's monitoring and its collection's other audio inputs
    assert scn.parse_program_audio_monitor(text) is None
    assert scn.parse_pulse_capture_rule(text) is None
    # the strih manifest's own content is unchanged: update-only, the camera inputs, the outputs
    assert scn.parse_seed_mode(text) == "update-only"
    inputs, outputs, latency = scn.parse_seed_manifest(text)
    assert latency == 3 and "STRIH-SNV (2ME PGM)" in outputs


def test_the_strih_pp_manifest_switches_asio_zvuk_to_lv1s_vban_source():
    text = _manifest("strih-pp")
    assert _scenes().parse_program_audio_input(text) == "ASIO zvuk"
    assert PAV.parse_program_audio_vban(text) == PAV.vban_source_settings(6981, "strih-obs", 40)


def test_a_hub_fed_strih_manifest_carries_no_program_keys(tmp_path):
    d = _boxes_with(tmp_path, "strih-pp", HUB_FED_PAIRS)
    r = _bash('strih_box_load "$1"\nobs_box_seed_manifest_json', "strih-pp", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    text = r.stdout
    assert "program_audio_vban" not in text and "program_audio_input" not in text
    assert PAV.parse_program_audio_vban(text) is None


def test_the_stream_and_strih_manifests_render_the_vban_block_through_one_function():
    s = PROVISION.read_text() + (SCRIPTS / "lib" / "obs-box-role.sh").read_text()
    # the block's settings are rendered by ONE printf format, never a second copy in either manifest
    assert s.count('{"port": %s, "stream_name": "%s", "receive_buffer_ms": %s}') == 1, "one renderer"
    assert '"program_audio_vban": {"port"' not in s, "no manifest renders the block inline"


# --- verify item 9 -----------------------------------------------------------------------------------------

@pytest.mark.parametrize("box,want", [("strih-snv", "yes"), ("strih-pp", "yes"), ("stream-pp", "yes"),
                                      ("stream-snv", "yes")])
def test_the_program_vban_declared_predicate_follows_the_facts(box, want):
    r = _bash('strih_box_load "$1"\nif strih_program_vban_declared; then echo yes; else echo no; fi', box)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == want


LINE = ("21:30:00.000: [obs-vban] obs-vban receive: received=%d lost=0 late=0 re_anchors=0 stale=0 reordered=0 "
        "concealed=0 silence_ms=0.0 max_late_ms=-13.1 sender_ppm=+0.300 phase_ms=+0.01 fit=locked "
        "emit_late_ms=-12.0 emit_missed=0 receive_buffer_ms=30 rate=96000 ch=2 port=6981 stream='strih-obs' "
        "source='ASIO zvuk'\n")


def test_item_9_grades_strih_snvs_asio_zvuk_on_the_facts(tmp_path):
    log = tmp_path / "obs.txt"
    log.write_text(LINE % 100)
    (tmp_path / "later.txt").write_text(LINE % 4100)
    kf = tmp_path / "kind"
    kf.write_text(VBAN_KIND)
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_box_load strih-snv\n( sleep 0.3; cat "$2" >> "$1" ) &\n'
            'strih_program_vban_grade /nonexistent 127.0.0.1 "$1"\nwait')
    r = _bash(body, log, tmp_path / "later.txt",
              env={"STRIH_PROGRAM_VBAN_KIND_FILE": str(kf), "STRIH_PROGRAM_VBAN_WAIT_S": "1"})
    assert r.returncode == 0, r.stderr
    rows = r.stdout.splitlines()
    assert not [x for x in rows if x.startswith(("FAIL", "NOTE"))], rows
    assert any("OBS 'ASIO zvuk' is the obs-vban receive source" in x for x in rows), rows
    assert any("receiving stream 'strih-obs' on UDP 6981: received 100 -> 4100" in x for x in rows), rows


def test_verify_item_9_follows_the_facts_not_the_role():
    v = VERIFY.read_text()
    assert len(v.splitlines()) <= 1000
    item9 = v[v.index("# 9) program audio"):v.index("# 9b)")]
    assert item9.count("strih_program_vban_grade") == 1
    # the obs-vban grade whenever the facts declare a program VBAN stream, on every role
    assert 'if strih_program_vban_declared; then\n  strih_program_vban_grade "$SCN_BIN" "$WS_HOST" "$LOG"\n' \
        in item9
    # the strih-program sink / pulse / hub-rx verdict only on a box that declares none
    assert item9.index("strih_program_vban_grade") < item9.index("AUDIO_SINK=0") \
        < item9.index('AUDIO_VERDICT="$(strih_lx_program_audio_verdict')
    # the hub-only rows (the HDMI leg, the cutters' card leg) stay on every hub box
    gate = item9.index('if WHY="$(obs_box_step_applies 13)"; then')
    assert item9.index("AUDIO_VERDICT") < gate < item9.index("strih_intercom_hub_hdmi_leg_grade") \
        < item9.index("strih_intercom_hub_alsa_leg_grade")
    assert item9.rstrip("\n").splitlines()[-1].startswith("strih_vban_leg_grade")


# --- the hub forwards nothing into OBS ---------------------------------------------------------------------

sys.path.insert(0, str(SCRIPTS))
import vbmatrix_to_intercom_toml as conv  # noqa: E402


def test_the_converter_emits_no_program_out():
    data = tomllib.loads(conv.convert(XML.read_text()))
    assert not any(p["role"] == "program_out" or p["name"] == "program_out" for p in data["participant"])
    assert not any(pt["dst"] == "program_out" for pt in data["point"])
    # the console still feeds the cutters' cans and the speakers (the intercom's own program references)
    assert any(pt["src"] == "fohabl" and pt["dst"] == "cutters" for pt in data["point"])
    assert any(pt["src"] == "fohabl" and pt["dst"] == "speakers" for pt in data["point"])
    assert any(pt["src"] == "lv1" and pt["dst"] == "cutters" for pt in data["point"])


# the strih-role boxes only: the stream role runs no intercom hub (setup step 13 moves one aside)
STRIH_ROLE_BOXES = sorted(p.stem for p in BOXES.glob("*.env") if _facts(p.stem)["STRIH_ROLE"] == "strih")


def test_both_strih_boxes_are_checked():
    assert STRIH_ROLE_BOXES == ["strih-pp", "strih-snv"]


@pytest.mark.parametrize("box", STRIH_ROLE_BOXES)
def test_a_box_that_takes_the_program_as_vban_has_no_hub_program_out(box):
    f = _facts(box)
    toml = tomllib.loads((REPO / f["STRIH_INTERCOM_CONFIG"]).read_text())
    has_out = any(p["role"] == "program_out" for p in toml["participant"])
    if f["STRIH_PROGRAM_VBAN_STREAMS"] == "none":
        assert has_out, "%s: a hub-fed strih keeps its program_out until its console sends a stream" % box
    else:
        assert not has_out, "%s: the program comes as VBAN into OBS; the hub forwards nothing into OBS" % box


def _grade_once(tmp_path, log_text):
    log = tmp_path / "obs.txt"
    log.write_text(log_text)
    kf = tmp_path / "kind"
    kf.write_text(VBAN_KIND)
    body = ('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
            'strih_box_load strih-snv\nstrih_program_vban_grade /nonexistent 127.0.0.1 "$1"')
    r = _bash(body, log, env={"STRIH_PROGRAM_VBAN_KIND_FILE": str(kf), "STRIH_PROGRAM_VBAN_WAIT_S": "0"})
    assert r.returncode == 0, r.stderr
    return [x for x in r.stdout.splitlines() if x.startswith("FAIL")]


def test_item_9_names_the_leg_address_when_nothing_arrived(tmp_path):
    # review round 1: the console sends to the box's VBAN-only leg (STRIH_PROGRAM_VBAN_LEG_ADDR,
    # 10.77.7.202), never to the rig address -- the "nothing arrived" hint names where it must send
    fails = _grade_once(tmp_path, "21:30:00.000: info: nothing from obs-vban yet\n")
    assert any("sending stream 'strih-obs' to 10.77.7.202:6981?" in x for x in fails), fails


def test_item_9_bind_failure_names_the_port_not_the_hub_on_6981(tmp_path):
    # review round 1: strih-snv's hub binds 6980, so a 6981 bind failure is "another process", named by ss
    fails = _grade_once(tmp_path, "12:00:00.000: [obs-vban] Failed to bind port 6981\n")
    assert any("could not bind UDP 6981" in x and "sport = :6981" in x for x in fails), fails
