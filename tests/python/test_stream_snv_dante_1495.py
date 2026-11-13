"""issue 1495 (owner 9.11.2026: "prechod stream na dlink v obs" + "stream nb ... jeden ethernet"): stream-snv
takes its program audio from its own Dante device `stream-obs` (the obs release's dante_input source, the
strih-snv issue-1485 design parametrized by role) and its audio leg rides VLAN 70 on the rig NIC.

Pinned here:
  * the program device is `<STRIH_ROLE>-obs` (strih-obs unchanged on a strih box, stream-obs on a stream box),
    the loader takes it on either role and refuses a Dante program next to a VBAN one;
  * stream-snv's facts: VLAN 70, the /24 leg address, no VBAN program, the input name `mbc` kept, the
    device's ports, and no Dante transmitter name in the public repo (the untracked site file);
  * the dante-input.toml for the stream box (name stream-obs, the leg's interface);
  * rig-mode #901 item 2's pure decision + its obs_phase2 read for a Dante stream box;
  * dante_input_continuity.py's stream mode (`stream_dante`, `stream Dante:`), the strih defaults unchanged.
"""
import argparse
import importlib.util
import os
import subprocess
import sys
import tomllib
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
BOXES = SCRIPTS / "strih-boxes"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_NIC_IFACE", "STRIH_BOXES_DIR", "OBS_FLEET", "GH_TOKEN",
         "STRIH_DANTE_INPUT_SITE_FILE", "STRIH_DANTE_INPUT_TOML", "STRIH_DANTE_INPUT_IFACE", "STRIH_DANTE_INPUT_KIND_FILE")
SITE = "# test site file (fake names)\nRX1=Program L|mix-tx|MIX-L\nRX2=Program R|mix-tx|MIX-R\n"


def _load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    sys.modules[name] = m
    spec.loader.exec_module(m)
    return m


PAV = _load("program_audio_vban_1495", SCRIPTS / "program_audio_vban.py")
CONT = _load("dante_input_continuity_1495", SCRIPTS / "dante_input_continuity.py")
OBS = _load("obs_phase2_1495", SCRIPTS / "obs_phase2.py")


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
        assert any(line.startswith(k + "=") for line in lines), k
        lines = ["%s=%s" % (k, v) if line.startswith(k + "=") else line for line in lines]
    f.write_text("\n".join(lines) + "\n")
    return d


def _facts(box):
    out = {}
    for line in (BOXES / ("%s.env" % box)).read_text().splitlines():
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    return out


# --- the facts -----------------------------------------------------------------------------------------------

def test_stream_snv_is_one_nic_and_dante():
    f = _facts("stream-snv")
    assert f["STRIH_PROGRAM_VBAN_LEG"] == "vlan:70", "the USB-C NIC leg is retired"
    assert f["STRIH_PROGRAM_VBAN_LEG_ADDR"] == "10.77.7.204"
    assert (f["STRIH_PROGRAM_VBAN_STREAMS"], f["STRIH_PROGRAM_VBAN_PORT"], f["STRIH_PROGRAM_VBAN_RESERVE_MS"]) == ("none",) * 3
    assert f["STRIH_PROGRAM_AUDIO_INPUT"] == "mbc", "the SNV stream tooling keys on the name"
    assert f["STRIH_DANTE_DEVICES"] == "stream-obs=4440:8800:4455"   # issue 1519: no RX range
    assert f["STRIH_PROGRAM_VBAN_SENDER"] == "none", "issue 1519: no VBAN on the Dante leg"
    # issue 1502: the ONE rig NIC is now the USB 2.5 GbE adapter (r8152) renamed rig0; still one NIC
    assert f["STRIH_NIC_DRIVER"] == "r8152" and f["STRIH_NIC_RENAME"] == "00:e0:4c:68:01:85=rig0"
    text = (BOXES / "stream-snv.env").read_text()
    for site in ("mbc-snv", "STREAM master"):
        assert site not in text, "the Dante transmitter names live only in the untracked site file"


def test_the_device_follows_the_role():
    r = _bash('for b in strih-snv stream-snv; do (strih_box_load "$b" >/dev/null; echo "$b $(strih_dante_input_device)"); done')
    assert r.returncode == 0, r.stderr
    assert r.stdout.split("\n")[:2] == ["strih-snv strih-obs", "stream-snv stream-obs"]


def test_a_device_of_the_other_role_is_no_program_device(tmp_path):
    # strih-obs named on the stream box is not its program device: with the VBAN program facts none the load
    # refuses (the stream role needs ONE program source)
    d = _boxes_with(tmp_path, "stream-snv", {"STRIH_DANTE_DEVICES": "strih-obs=4440:8800:4455"})
    r = _bash('strih_box_load stream-snv', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "STRIH_PROGRAM_VBAN_STREAMS" in r.stderr, r.stderr


def test_a_dante_program_next_to_a_vban_program_is_refused(tmp_path):
    d = _boxes_with(tmp_path, "stream-snv", {"STRIH_PROGRAM_VBAN_STREAMS": "stream", "STRIH_PROGRAM_VBAN_PORT": "6980",
                                             "STRIH_PROGRAM_VBAN_RESERVE_MS": "80"})
    r = _bash('strih_box_load stream-snv', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "stream-obs Dante device" in r.stderr, r.stderr


def test_the_vban_rollback_is_the_three_program_facts_and_no_device(tmp_path):
    d = _boxes_with(tmp_path, "stream-snv", {"STRIH_PROGRAM_VBAN_STREAMS": "stream", "STRIH_PROGRAM_VBAN_PORT": "6980",
                                             "STRIH_PROGRAM_VBAN_RESERVE_MS": "80", "STRIH_DANTE_DEVICES": "none",
                                             # issues 1510 + 1513: the four Dante terms go none with the device
                                             "STRIH_DANTE_NETWORK_LATENCY_MS": "none", "STRIH_DANTE_RECEIVE_BUFFER_MS": "none",
                                             "STRIH_DANTE_TX": "none", "STRIH_DANTE_TX_TRACK": "none"})
    r = _bash('strih_box_load stream-snv >/dev/null; if strih_program_dante_declared; then echo dante; else echo vban; fi; '
              'strih_lx_program_vban_leg', env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["vban", "vlan:70"], "the leg stays on the VLAN; only the program source changes"


def test_the_leg_is_the_dante_24_on_the_rig_nics_vlan():
    r = _bash('strih_box_load stream-snv >/dev/null; . "%s"; strih_vban_leg_nm_props enp56s0.70 10.77.7.204 enp56s0 70'
              % (SCRIPTS / "lib" / "strih-vban-leg.sh"))
    assert r.returncode == 0, r.stderr
    props = dict(line.split("\t", 1) for line in r.stdout.splitlines())
    assert props["ipv4.addresses"] == "10.77.7.204/24" and props["ipv4.route-table"] == "70"
    assert props["vlan.parent"] == "enp56s0" and props["vlan.id"] == "70" and props["ipv4.never-default"] == "yes"


# --- the dante-input.toml ----------------------------------------------------------------------------------------

def test_the_stream_box_toml_names_its_own_device_on_the_vlan(tmp_path):
    s = tmp_path / "site.conf"
    s.write_text(SITE)
    r = _bash('strih_box_load stream-snv >/dev/null; rows="$(strih_dante_input_site_rows "$1")"; '
              'strih_dante_input_toml enp56s0.70 "$(strih_dante_input_ports "$(strih_lx_dante_devices)")" "$rows"', s)
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert d["name"] == "stream-obs" and d["iface"] == "enp56s0.70" and d["network_rate"] == 96000
    assert (d["arc_port"], d["cmc_port"], d["flows_port"]) == (4440, 8800, 4455)
    assert "rx_port_base" not in d and "tx_port_base" not in d   # issue 1519: the fixed slot block
    assert [x["tx_channel"] for x in d["subscribe"]] == ["MIX-L", "MIX-R"]
    assert "sample_rate" not in d and "latency_ms" not in d
    # issues 1510 + 1513: the named terms (P kept at 13.041666 ms) + the program TX to mbc
    assert (d["network_latency_ms"], d["receive_buffer_ms"]) == (1.0, 12.041666)
    assert (d["tx"], d["tx_track"]) == (["OBS program L", "OBS program R"], 1)


def test_the_install_writes_the_stream_toml(tmp_path):
    s = tmp_path / "site.conf"
    s.write_text(SITE)
    dest = tmp_path / "home" / ".camera-box" / "dante-input.toml"
    env = {"STRIH_DANTE_INPUT_SITE_FILE": str(s), "STRIH_DANTE_INPUT_TOML": str(dest),
           "STRIH_DANTE_INPUT_IFACE": "enp56s0.70", "STRIH_DANTE_RX_PIN_ROOT": str(tmp_path / "root")}
    user = subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()
    dest.parent.mkdir(parents=True)
    r = _bash('strih_box_load stream-snv >/dev/null\n'
              'install() { :; }\n'
              'obs_box_write_if_changed_rendered() { local d="$1"; shift 4; "$@" > "$d"; }\n'
              'strih_dante_ptp_port_install() { :; }\n'
              'strih_dante_input_install "$1" "$2"', user, tmp_path / "home", env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert tomllib.loads(dest.read_text())["name"] == "stream-obs"
    assert "stream-obs on enp56s0.70" in r.stdout


# --- rig-mode #901 item 2: the Dante stream box ---------------------------------------------------------------------

def test_the_dante_decision_passes_the_dante_input_unmuted():
    assert PAV.mbc_dante_transport_problem(PAV.DANTE_SOURCE_KIND, False) is None


def test_the_dante_decision_names_mute_first():
    p = PAV.mbc_dante_transport_problem("net.nagater.obs-vban.source", True)
    assert "MUTED" in p and "obs-vban" not in p


@pytest.mark.parametrize("kind", ["net.nagater.obs-vban.source", "pulse_input_capture", ""])
def test_the_dante_decision_fails_any_other_kind(kind):
    p = PAV.mbc_dante_transport_problem(kind, False)
    assert p and repr(kind) in p and PAV.DANTE_SOURCE_KIND in p


class _WS:
    def close(self):
        pass


def _patch(monkeypatch, kind, muted):
    seen = []

    def rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        seen.append(rtype)
        if rtype == "GetInputSettings":
            return {"inputKind": kind, "inputSettings": {}}
        if rtype == "GetInputMute":
            return {"inputMuted": muted}
        raise AssertionError("read-only: %s" % rtype)

    monkeypatch.setattr(OBS, "_conn", lambda host, password="": _WS())
    monkeypatch.setattr(OBS, "_rpc", rpc)
    return seen


def _args(**kw):
    a = dict(host="10.77.9.204", password="", input="mbc", expected_device_id=OBS.DANTE_MBC_DEVICE_ID,
             vban_stream=None, vban_port=None, dante_device="stream-obs")
    a.update(kw)
    return argparse.Namespace(**a)


def test_the_handler_passes_the_dante_input_with_two_reads(monkeypatch, capsys):
    seen = _patch(monkeypatch, PAV.DANTE_SOURCE_KIND, False)
    OBS.mbc_input_check(_args())
    out = capsys.readouterr().out
    assert out.startswith("PASS:") and "Dante input OK" in out and "stream-obs" in out
    assert seen == ["GetInputSettings", "GetInputMute"]


def test_the_handler_fails_the_old_vban_input(monkeypatch):
    _patch(monkeypatch, "net.nagater.obs-vban.source", False)
    with pytest.raises(SystemExit) as e:
        OBS.mbc_input_check(_args())
    assert "issue 1495" in str(e.value) and "dante_input" in str(e.value)


def test_the_dante_flag_excludes_the_vban_flags(monkeypatch):
    _patch(monkeypatch, PAV.DANTE_SOURCE_KIND, False)
    with pytest.raises(SystemExit, match="excludes"):
        OBS.mbc_input_check(_args(vban_stream="stream", vban_port=6980))


def test_the_cli_parses_the_dante_flag(monkeypatch):
    got = {}
    monkeypatch.setattr(OBS, "mbc_input_check", lambda a: got.update(vars(a)))
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "mbc-input-check", "--host", "h", "--dante-device", "stream-obs"])
    OBS.main()
    assert got["dante_device"] == "stream-obs"
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "mbc-input-check", "--host", "h"])
    OBS.main()
    assert got["dante_device"] is None


# --- dante_input_continuity.py: the stream mode --------------------------------------------------------------------

def _status(t, delivered, **over):
    v = dict(clock="media", holdover=0, gm="00aabb", polls=10, re_anchors=0, rx_packets=1, rx_loss=0, rx_gaps=0,
             rx_late=0, socket_drops=0, blocks=1, underruns=0, overruns=0, delivered_frames=delivered, rx_holes=0,
             rx_written_late=0, late_blocks=0, rebootstraps=0, max_process_us=40, obs_dropped=0, max_obs_us=30)
    v.update(over)
    return "%s: dante-input status: %s" % (t, " ".join("%s=%s" % i for i in v.items()))


def _window(after_loss=0):
    return [_status("10:00:00.000", 1000), "x", "x", _status("10:00:10.000", 2000),
            _status("10:00:20.000", 3000, rx_loss=after_loss)]


def test_the_stream_mode_folds_under_its_own_key(tmp_path):
    log = tmp_path / "log.txt"
    log.write_text("\n".join(_window(after_loss=2)) + "\n")
    block = tmp_path / "block.json"
    block.write_text('{"verdict": "PASS", "pass": true, "failures": [], "unknown_reasons": []}')
    rc = CONT.main(["--box", "stream", "--device", "stream-obs", "--log", str(log), "--start-line", "1",
                    "--end-line", "3", "--fold-into", str(block)])
    import json
    b = json.loads(block.read_text())
    assert rc == 1 and b["verdict"] == "FAIL" and "strih_dante" not in b
    assert b["stream_dante"]["device"] == "stream-obs" and "stream Dante: rx_loss +2" in b["failures"]


@pytest.mark.parametrize("verdict,row", [("silent-channel", "BAD"), ("ok", "OK"), ("quiet", "NOTE")])
def test_the_dante_grade_reads_the_per_channel_meter_of_mbc(tmp_path, verdict, row):
    # review round 1: verify item 9's per-channel row is source-agnostic, so the Dante grade runs it too
    ch = tmp_path / "channels"
    ch.write_text("channel_verdict=%s\nchannel_detail=ch2 -100.0 dB while ch1 -30.0 dB\n" % verdict)
    kf = tmp_path / "kind"
    kf.write_text("dante_input")
    r = _bash('ok() { echo "OK $*"; }; bad() { echo "BAD $*"; }; note() { echo "NOTE $*"; }\n'
              'strih_box_load stream-snv >/dev/null; strih_dante_input_grade scn 127.0.0.1 ""',
              env={"STRIH_PROGRAM_VBAN_CHANNELS_FILE": str(ch), "STRIH_DANTE_INPUT_KIND_FILE": str(kf),
                   "STRIH_DANTE_INPUT_TOML": str(tmp_path / "none.toml")})
    assert r.returncode == 0, r.stderr
    rows = [line for line in r.stdout.splitlines() if "ch2 -100.0 dB" in line]
    assert len(rows) == 1 and rows[0].startswith(row + " (program-dante)"), r.stdout
    assert "'mbc' is the dante_input source" in r.stdout


def test_the_per_point_report_names_the_dante_device_on_a_dante_stream_box():
    rep = _load("e2e_report_youtube_1495", SCRIPTS / "e2e_report_youtube.py")
    pa = {"verdict": "PASS", "pass": True, "stream_dante": {"verdict": "PASS", "device": "stream-obs"}}
    text = rep.points_markdown({"program_audio_continuity": pa})
    assert "dante_input (stream-obs) PASS" in text and "prijímač obs-vban" not in text
    text = rep.points_markdown({"program_audio_continuity": {"verdict": "PASS", "pass": True}})
    assert "prijímač obs-vban PASS" in text


def test_the_strih_defaults_are_unchanged(tmp_path, capsys):
    res = CONT.decide(_window(), 1, 3)
    assert res["device"] == "strih-obs" and res["verdict"] == "PASS"
    assert CONT.fold({}, res)["strih_dante"] is res
    assert CONT.summary_lines(res)[0].startswith("strih Dante program input 'strih-obs': PASS")
