"""issue 1439, ROZHODNUTÉ 6360417876 (owner 23.10.2026): the VBAN-only leg on a stream box's second NIC.

Owner, verbatim: "na stream-snv je druha sietovka usb-c 2.5gbits a ta je pripojena do vlan 70 audio
siete, vies nastavit aby na stream bola ta sietovka na 10.77.7.204 a islo cez nu iba ten vbani nic ine
aby cez nu ani nic ine neodchadazlo, vsetko ostatne streamy ndi video internet naech ide ce tu sietovku
klasicku". The hand fix of 23.10.2026 10:04 is codified into setup-strih / verify-strih the same day:
  * the facts STRIH_PROGRAM_VBAN_LEG_MAC / _LEG_ADDR / STRIH_PROGRAM_VBAN_SENDER
    (scripts/lib/strih-box-facts.sh): stream-snv's values, `none` elsewhere, the refusals;
  * scripts/lib/strih-vban-leg.sh: the NM profile (a /32, no gateway, the policy route for ping replies),
    the nft table (only the sender's VBAN + ping in, only echo replies out, plus a counter of VBAN
    arriving OFF the leg), its boot oneshot, the per-interface ARP sysctls;
  * setup-strih step 13 (stream role) = strih_vban_leg_install, verify-strih item 9 = strih_vban_leg_grade.

Tier-0: bash + pytest only; nmcli / ip / nft / systemctl / sysctl are the fakes of vban_leg_fakes_1439.py.
The nft ruleset is pinned to the real nft 1.0.9 render (captured in a throwaway network namespace) and
to the live stream-snv table read on 23.10.2026.
"""
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vban_leg_fakes_1439 as fk  # noqa: E402

REPO = HERE.parent.parent
SCRIPTS = REPO / "scripts"
PROVISION = SCRIPTS / "lib" / "strih-provision.sh"
LEG_LIB = SCRIPTS / "lib" / "strih-vban-leg.sh"
SETUP = SCRIPTS / "setup-strih.sh"
VERIFY = SCRIPTS / "verify-strih.sh"
BOXES = SCRIPTS / "strih-boxes"
FIX = REPO / "tests" / "fixtures" / "vban_leg_1439"

LEG_MAC = "00:e0:4c:68:01:85"
LEG_IF = "enx00e04c680185"
LEG_ADDR = "10.77.7.204"
SENDER = "10.77.7.232"
RIG_IF = "enp56s0"
RIG_UUID = "872cb3ca-c619-31f0-b224-66ef70bbeab0"
LEAKY = ("STRIH_LX_IP", "STRIH_LX_HOST", "STRIH_LX_DANTESYNC_ROLE", "STRIH_LX_NTP_SERVER", "STRIH_NIC_IFACE",
         "STRIH_BOXES_DIR", "OBS_FLEET", "RIG_GRANDMASTER_IP", "GH_TOKEN", "NDI_DISCOVERY_SENDER_HOSTS",
         "STRIH_VBAN_LEG_SYSFS", "STRIH_VBAN_LEG_PROC_SYS", "STRIH_VBAN_LEG_NFT_FILE", "STRIH_VBAN_LEG_UNIT_FILE",
         "STRIH_VBAN_LEG_SYSCTL_FILE", "STRIH_VBAN_LEG_WAIT_S")
GRADE_FNS = 'ok() { printf "OK|%s\\n" "$*"; }\nbad() { printf "BAD|%s\\n" "$*"; }\nnote() { printf "NOTE|%s\\n" "$*"; }\n'


def _bash(body, *args, env=None, path_prefix=None):
    """BODY under the callers' strict mode with the provisioning libs sourced; values reach it as $1.. only."""
    e = fk.os_env_without(LEAKY)
    if path_prefix:
        e["PATH"] = "%s:%s" % (path_prefix, e["PATH"])
    e.update(env or {})
    script = 'set -euo pipefail\n. "%s"\n%s' % (PROVISION, body)
    return subprocess.run(["bash", "-c", script, "harness", *[str(a) for a in args]], capture_output=True,
                          text=True, env=e, cwd=str(REPO), timeout=60)


def _boxes_with(tmp_path, box, edits):
    d = tmp_path / "boxes"
    if not d.exists():
        import shutil
        shutil.copytree(BOXES, d)
    f = d / ("%s.env" % box)
    t = f.read_text()
    for old, new in edits:
        assert t.count(old) == 1, (box, old)
        t = t.replace(old, new)
    f.write_text(t)
    return d


def _norm(text):
    """The verify-side normalization: trimmed lines, counters dropped, blank lines gone."""
    out = []
    for line in text.split("\n"):
        line = re.sub(r"counter packets [0123456789]+ bytes [0123456789]+", "counter", line.strip())
        if line:
            out.append(line)
    return out


# --- the facts -----------------------------------------------------------------------------------------

def test_the_three_leg_facts_follow_the_program_audio_input():
    r = _bash("strih_box_fact_keys")
    keys = r.stdout.split()
    i = keys.index("STRIH_PROGRAM_AUDIO_INPUT")
    assert keys[i + 1:i + 5] == ["STRIH_PROGRAM_VBAN_LEG_MAC", "STRIH_PROGRAM_VBAN_LEG_ADDR",
                                 "STRIH_PROGRAM_VBAN_SENDER", "STRIH_PROGRAM_SENDER"]


def test_stream_snv_declares_the_live_leg_and_every_other_box_none():
    body = ('strih_box_load "$1" || exit 9\n'
            'printf "%s|%s|%s\\n" "$(strih_lx_program_vban_leg_mac)" "$(strih_lx_program_vban_leg_addr)" '
            '"$(strih_lx_program_vban_sender)"')
    assert _bash(body, "stream-snv").stdout.strip() == "%s|%s|%s" % (LEG_MAC, LEG_ADDR, SENDER)
    for box in sorted(p.stem for p in BOXES.glob("*.env")):
        if box == "stream-snv":
            continue
        r = _bash(body, box)
        assert r.returncode == 0, (box, r.stderr)
        assert r.stdout.strip() == "none|none|none", box


@pytest.mark.parametrize("box,edits,word", [
    ("strih-snv", [("STRIH_PROGRAM_VBAN_LEG_MAC=none", "STRIH_PROGRAM_VBAN_LEG_MAC=" + LEG_MAC),
                   ("STRIH_PROGRAM_VBAN_LEG_ADDR=none", "STRIH_PROGRAM_VBAN_LEG_ADDR=" + LEG_ADDR),
                   ("STRIH_PROGRAM_VBAN_SENDER=none", "STRIH_PROGRAM_VBAN_SENDER=" + SENDER)], "strih role"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_SENDER=" + SENDER, "STRIH_PROGRAM_VBAN_SENDER=none")], "all three"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_LEG_MAC=" + LEG_MAC, "STRIH_PROGRAM_VBAN_LEG_MAC=00:E0:4C:68:01:85")],
     "MAC"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_LEG_MAC=" + LEG_MAC, "STRIH_PROGRAM_VBAN_LEG_MAC=00:e0:4c:68:01")], "MAC"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_LEG_ADDR=" + LEG_ADDR, "STRIH_PROGRAM_VBAN_LEG_ADDR=10.77.7.256")], "IPv4"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_SENDER=" + SENDER, "STRIH_PROGRAM_VBAN_SENDER=10.77.8.232")], "/24"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_SENDER=" + SENDER, "STRIH_PROGRAM_VBAN_SENDER=" + LEG_ADDR)], "differ"),
    ("stream-snv", [("STRIH_PROGRAM_VBAN_LEG_ADDR=" + LEG_ADDR, "STRIH_PROGRAM_VBAN_LEG_ADDR=10.77.9.204"),
                    ("STRIH_PROGRAM_VBAN_SENDER=" + SENDER, "STRIH_PROGRAM_VBAN_SENDER=10.77.9.232")], "STRIH_IP"),
])
def test_the_loader_refuses_a_malformed_leg(tmp_path, box, edits, word):
    d = _boxes_with(tmp_path, box, edits)
    r = _bash('strih_box_load "$1"', box, env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0
    assert "STRIH_PROGRAM_VBAN_" in r.stderr and word in r.stderr, r.stderr


# --- the rendered files ----------------------------------------------------------------------------------

def _render(fn, *args):
    r = _bash('. "%s"\n%s "$@"' % (LEG_LIB, fn), *args)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_the_ruleset_file_is_the_real_nft_render():
    text = _render("strih_vban_leg_nft_text", LEG_IF, SENDER, "6980")
    assert "flush ruleset" not in text
    body = text.split("delete table inet vban_dante70\n", 1)[1]
    assert _norm(re.sub(r"(?m)^\s*#.*$", "", body)) == _norm((FIX / "nft-list-table-netns.txt").read_text())
    assert text.splitlines()[0] == "#!/usr/sbin/nft -f"
    assert "table inet vban_dante70\ndelete table inet vban_dante70\n" in text


def test_the_ruleset_reproduces_the_live_hand_fix_plus_the_off_leg_counter():
    want = _norm((FIX / "nft-list-table-netns.txt").read_text())
    live = _norm((FIX / "nft-list-table-live-23-10.txt").read_text())
    off = 'iifname != "%s" iifname != "lo" udp dport 6980 counter' % LEG_IF
    assert [line for line in want if line != off] == live
    assert off in want


def test_the_expected_listing_is_derived_from_the_ruleset_file():
    got = _render("strih_vban_leg_nft_expected", LEG_IF, SENDER, "6980").strip().split("\n")
    assert got == _norm((FIX / "nft-list-table-netns.txt").read_text())


def test_the_boot_unit_applies_the_file_before_the_network_never_the_distro_service():
    unit = _render("strih_vban_leg_unit_text", "/etc/nftables.d/vban-dante70.nft")
    for want in ("Type=oneshot", "RemainAfterExit=yes", "ExecStart=/usr/sbin/nft -f /etc/nftables.d/vban-dante70.nft",
                 "Before=network-pre.target", "Wants=network-pre.target", "WantedBy=multi-user.target"):
        assert want in unit.splitlines(), want
    assert "nftables.service" not in unit


def test_the_sysctl_dropin_is_keyed_on_the_interface_name():
    text = _render("strih_vban_leg_sysctl_text", LEG_IF)
    lines = [x for x in text.splitlines() if x and not x.startswith("#")]
    assert lines == ["net/ipv4/conf/%s/arp_ignore = 1" % LEG_IF, "net/ipv4/conf/%s/arp_announce = 2" % LEG_IF]


# --- setup: strih_vban_leg_install ----------------------------------------------------------------------------

class Box:
    def __init__(self, tmp_path, **state):
        self.tmp = tmp_path
        self.bin = tmp_path / "bin"
        self.state = tmp_path / "state.json"
        self.proc = tmp_path / "proc-sys"
        self.sysfs = tmp_path / "sys"
        self.etc = tmp_path / "etc"
        self.nft_file = self.etc / "nftables.d" / "vban-dante70.nft"
        self.unit_file = self.etc / "systemd" / "system" / "vban-dante70-filter.service"
        self.sysctl_file = self.etc / "sysctl.d" / "60-vban-dante70.conf"
        state.setdefault("unit_runs_nft", str(self.nft_file))
        fk.new_state(self.state, **state)
        self.env = fk.install(self.bin, self.state, self.proc)
        self.env.update({"STRIH_VBAN_LEG_SYSFS": str(self.sysfs), "STRIH_VBAN_LEG_PROC_SYS": str(self.proc),
                         "STRIH_VBAN_LEG_NFT_FILE": str(self.nft_file),
                         "STRIH_VBAN_LEG_UNIT_FILE": str(self.unit_file),
                         "STRIH_VBAN_LEG_SYSCTL_FILE": str(self.sysctl_file), "STRIH_VBAN_LEG_WAIT_S": "0"})
        self.nic(RIG_IF, "a8:a1:59:00:00:01")

    def nic(self, name, mac):
        d = self.sysfs / "class" / "net" / name
        d.mkdir(parents=True, exist_ok=True)
        (d / "address").write_text(mac + "\n")

    def run(self, fn, box="stream-snv"):
        body = 'strih_box_load "$1" || exit 9\n%s%s' % (GRADE_FNS, fn)
        return _bash(body, box, env=self.env, path_prefix=self.bin)

    def st(self):
        return fk.read_state(self.state)


def _rig_profile():
    return {"uuid": RIG_UUID, "type": "802-3-ethernet",
            "props": {"connection.id": "stream-lan", "connection.interface-name": RIG_IF, "ipv4.method": "manual",
                      "ipv4.addresses": "10.77.9.204/23", "ipv4.gateway": "10.77.8.1"}}


def _dhcp_profile(uuid="11111111-2222-3333-4444-555555555555", name="Wired connection 1", ifname=""):
    return {"uuid": uuid, "type": "802-3-ethernet",
            "props": {"connection.id": name, "connection.interface-name": ifname, "ipv4.method": "auto",
                      "802-3-ethernet.mac-address": LEG_MAC, "connection.autoconnect": "yes", "ipv6.method": "auto"}}


LIVE_PROPS = {"connection.id": "vban-dante70", "connection.interface-name": LEG_IF, "connection.autoconnect": "yes",
              "ipv4.method": "manual", "ipv4.addresses": "10.77.7.204/32", "ipv4.gateway": "",
              "ipv4.never-default": "yes", "ipv4.ignore-auto-routes": "yes", "ipv4.ignore-auto-dns": "yes",
              "ipv4.dns": "", "ipv4.routes": "10.77.7.0/24 table=70",
              "ipv4.routing-rules": "priority 100 from 10.77.7.204 table 70", "ipv6.method": "disabled",
              "802-3-ethernet.mac-address": ""}


def _fresh_box(tmp_path):
    """A freshly installed stream-snv: the USB-C NIC came up by DHCP on VLAN 70 with a second default route."""
    dhcp = _dhcp_profile()
    b = Box(tmp_path, profiles=[_rig_profile(), dhcp], active={RIG_IF: RIG_UUID, LEG_IF: dhcp["uuid"]},
            applied={RIG_IF: _rig_profile()["props"], LEG_IF: dict(dhcp["props"])},
            dhcp={LEG_IF: "10.77.7.108/24"}, nft_counters=[[100, 0], [350, 0]])
    b.nic(LEG_IF, LEG_MAC)
    return b, dhcp


def _live_box(tmp_path, **over):
    """stream-snv as the hand fix left it at 10:07: profile right + active, the table loaded, nothing on disk."""
    prof = {"uuid": "565ed2bb-cc72-31b4-88bb-d937e55c3c2d", "type": "802-3-ethernet", "props": dict(LIVE_PROPS)}
    kw = dict(profiles=[_rig_profile(), prof], active={RIG_IF: RIG_UUID, LEG_IF: prof["uuid"]},
              applied={RIG_IF: _rig_profile()["props"], LEG_IF: dict(LIVE_PROPS)},
              nft_counters=[[100, 0], [350, 0]])
    kw.update(over)
    b = Box(tmp_path, **kw)
    b.nic(LEG_IF, LEG_MAC)
    return b, prof


def test_install_on_a_fresh_box_adopts_the_dhcp_profile_and_leaves_one_vban_route(tmp_path):
    b, dhcp = _fresh_box(tmp_path)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    leg = [p for p in s["profiles"] if p["uuid"] == dhcp["uuid"]]
    assert leg, "the existing DHCP profile on the leg is adopted, never a second one created"
    props = leg[0]["props"]
    for k, v in LIVE_PROPS.items():
        if k != "802-3-ethernet.mac-address":
            assert props.get(k) == v, (k, props.get(k))
    assert len(s["profiles"]) == 2 and not fk.calls(s, "nmcli", "connection", "add")
    assert s["applied"][LEG_IF]["ipv4.addresses"] == "10.77.7.204/32"
    # the rig NIC's profile is never CHANGED (reading every profile to find the leg's is expected)
    for c in fk.calls(s, "nmcli"):
        if c[:1] != ["-g"]:
            assert RIG_UUID not in c and RIG_IF not in c and "stream-lan" not in c, c
    # the filter: written, enabled, applied before the address moved
    assert b.nft_file.read_text() == _render("strih_vban_leg_nft_text", LEG_IF, SENDER, "6980")
    assert s["units"]["vban-dante70-filter.service"] == {"enabled": True, "active": True}
    log = s["log"]
    restart = next(i for i, c in enumerate(log) if c[:3] == ["systemctl", "restart", "vban-dante70-filter.service"])
    moved = [i for i, c in enumerate(log) if c[0] == "nmcli" and ("reapply" in c or "up" in c)]
    assert moved and restart < moved[0], "the filter is in place before the leg takes its address"
    # the ARP sysctls: the drop-in and the live values
    assert "arp_ignore = 1" in b.sysctl_file.read_text()
    conf = b.proc / "net" / "ipv4" / "conf" / LEG_IF
    assert (conf / "arp_ignore").read_text().strip() == "1" and (conf / "arp_announce").read_text().strip() == "2"


def test_a_refused_reapply_falls_back_to_bringing_the_leg_up(tmp_path):
    b, dhcp = _fresh_box(tmp_path)
    s = fk.read_state(b.state)
    s["reapply_fails"] = True
    b.state.write_text(json.dumps(s))
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert fk.calls(s, "nmcli", "device", "reapply", LEG_IF)
    assert fk.calls(s, "nmcli", "connection", "up", dhcp["uuid"])
    assert s["applied"][LEG_IF]["ipv4.addresses"] == "10.77.7.204/32"


def test_a_second_run_changes_nothing(tmp_path):
    b, _ = _fresh_box(tmp_path)
    assert b.run("strih_vban_leg_install").returncode == 0
    n = len(b.st()["log"])
    mt = (b.nft_file.stat().st_mtime_ns, b.unit_file.stat().st_mtime_ns, b.sysctl_file.stat().st_mtime_ns)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stderr
    s = b.st()
    new = s["log"][n:]
    for c in new:
        assert c[:3] != ["nmcli", "connection", "modify"] and "up" not in c[1:3] and "reapply" not in c, c
        assert c[:2] not in (["systemctl", "restart"], ["systemctl", "enable"], ["systemctl", "daemon-reload"]), c
    assert mt == (b.nft_file.stat().st_mtime_ns, b.unit_file.stat().st_mtime_ns, b.sysctl_file.stat().st_mtime_ns)


def test_install_on_the_live_box_never_bounces_the_leg(tmp_path):
    b, prof = _live_box(tmp_path, nft_render=(FIX / "nft-list-table-live-23-10.txt").read_text())
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert not fk.calls(s, "nmcli", "connection", "modify")
    assert not fk.calls(s, "nmcli", "connection", "up") and not fk.calls(s, "nmcli", "device", "reapply")
    assert not fk.calls(s, "nmcli", "connection", "delete")
    assert s["active"][LEG_IF] == prof["uuid"]
    # the hand-applied table lacks the off-leg counter: the file is written and the table replaced
    assert fk.calls(s, "systemctl", "restart", "vban-dante70-filter.service")
    assert b.sysctl_file.exists()


def test_a_second_profile_on_the_leg_is_deleted(tmp_path):
    b, prof = _live_box(tmp_path)
    s = fk.read_state(b.state)
    s["profiles"].append(_dhcp_profile(uuid="99999999-2222-3333-4444-555555555555", ifname=LEG_IF))
    b.state.write_text(json.dumps(s))
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert fk.calls(s, "nmcli", "connection", "delete", "99999999-2222-3333-4444-555555555555")
    assert [p["uuid"] for p in s["profiles"]] == [RIG_UUID, prof["uuid"]]


def test_an_absent_adapter_fails_by_its_mac_and_changes_nothing(tmp_path):
    b = Box(tmp_path, profiles=[_rig_profile()], active={RIG_IF: RIG_UUID}, applied={RIG_IF: _rig_profile()["props"]})
    r = b.run("strih_vban_leg_install")
    assert r.returncode != 0
    assert LEG_MAC in r.stderr
    s = b.st()
    assert not fk.calls(s, "nmcli", "connection", "modify") and not fk.calls(s, "systemctl", "restart")
    assert not b.nft_file.exists() and not b.sysctl_file.exists()


def test_an_uncabled_leg_keeps_the_profile_and_warns(tmp_path):
    b = Box(tmp_path, profiles=[_rig_profile()], active={RIG_IF: RIG_UUID}, applied={RIG_IF: _rig_profile()["props"]},
            carrier={LEG_IF: False})
    b.nic(LEG_IF, LEG_MAC)
    r = b.run("strih_vban_leg_install")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARN" in r.stdout and LEG_IF in r.stdout
    s = b.st()
    made = [p for p in s["profiles"] if p["props"].get("connection.id") == "vban-dante70"]
    assert len(made) == 1 and made[0]["props"]["ipv4.addresses"] == "10.77.7.204/32"
    add = fk.calls(s, "nmcli", "connection", "add")
    assert add and "ipv4.method" in add[0] and "manual" in add[0], "created already static: never a DHCP moment"


def test_a_box_without_a_leg_skips(tmp_path):
    b = Box(tmp_path)
    r = b.run("strih_vban_leg_install", box="stream-pp")
    assert r.returncode == 0, r.stderr
    assert "SKIP" in r.stdout
    assert not fk.calls(b.st(), "nmcli") and not fk.calls(b.st(), "nft")


# --- verify: strih_vban_leg_grade --------------------------------------------------------------------------

def _rows(r):
    assert r.returncode == 0, r.stderr
    return [line for line in r.stdout.splitlines() if line[:3] in ("OK|", "BAD", "NOT")]


def _healthy(tmp_path, **over):
    kw = dict(nft_render=(FIX / "nft-list-table-netns.txt").read_text(),
              units={"vban-dante70-filter.service": {"enabled": True, "active": True}})
    kw.update(over)
    b, prof = _live_box(tmp_path, **kw)
    fk.write_proc(b.proc, LEG_IF, 1, 2)
    return b


def test_grade_passes_the_codified_leg(tmp_path):
    rows = _rows(_healthy(tmp_path).run("strih_vban_leg_grade"))
    assert rows and all(x.startswith("OK|(vban-leg)") for x in rows), rows
    assert any("250 packets" in x and SENDER in x for x in rows), rows


def test_grade_fails_when_no_vban_arrives_on_the_leg(tmp_path):
    rows = _rows(_healthy(tmp_path, nft_counters=[[500, 0], [500, 0]]).run("strih_vban_leg_grade"))
    bad = [x for x in rows if x.startswith("BAD|")]
    assert len(bad) == 1 and SENDER in bad[0] and LEG_ADDR in bad[0] and "stream" in bad[0], rows


def test_grade_fails_when_vban_still_arrives_off_the_leg(tmp_path):
    rows = _rows(_healthy(tmp_path, nft_counters=[[500, 10], [700, 60]]).run("strih_vban_leg_grade"))
    bad = [x for x in rows if x.startswith("BAD|")]
    assert len(bad) == 1 and "off the leg" in bad[0] and "50" in bad[0], rows


def test_grade_fails_a_dhcp_leg_with_its_default_route(tmp_path):
    dhcp = _dhcp_profile(ifname=LEG_IF)
    b = _healthy(tmp_path, profiles=[_rig_profile(), dhcp], active={RIG_IF: RIG_UUID, LEG_IF: dhcp["uuid"]},
                 applied={RIG_IF: _rig_profile()["props"], LEG_IF: dict(dhcp["props"])},
                 dhcp={LEG_IF: "10.77.7.108/24"})
    rows = _rows(b.run("strih_vban_leg_grade"))
    text = "\n".join(x for x in rows if x.startswith("BAD|"))
    assert "default route" in text and "10.77.7.108/24" in text and "vban-dante70" in text, rows


def test_grade_fails_the_live_table_until_setup_ran(tmp_path):
    rows = _rows(_healthy(tmp_path, nft_render=(FIX / "nft-list-table-live-23-10.txt").read_text(),
                          units={}).run("strih_vban_leg_grade"))
    bad = [x for x in rows if x.startswith("BAD|")]
    assert any("(vban-leg) filter" in x for x in bad), rows


def test_grade_fails_missing_arp_sysctls(tmp_path):
    b = _healthy(tmp_path)
    fk.write_proc(b.proc, LEG_IF, 0, 0)
    bad = [x for x in _rows(b.run("strih_vban_leg_grade")) if x.startswith("BAD|")]
    assert len(bad) == 1 and "arp_ignore" in bad[0], bad


def test_grade_names_the_mac_of_an_absent_adapter(tmp_path):
    b = Box(tmp_path)
    rows = _rows(b.run("strih_vban_leg_grade"))
    assert len(rows) == 1 and rows[0].startswith("BAD|") and LEG_MAC in rows[0], rows


def test_grade_says_run_as_root_when_the_table_is_unreadable(tmp_path):
    rows = _rows(_healthy(tmp_path, nft_unreadable=True).run("strih_vban_leg_grade"))
    bad = [x for x in rows if x.startswith("BAD|")]
    assert len(bad) == 1 and "root" in bad[0], rows


def test_grade_notes_a_box_without_a_leg(tmp_path):
    rows = _rows(Box(tmp_path).run("strih_vban_leg_grade", box="stream-pp"))
    assert len(rows) == 1 and rows[0].startswith("NOTE|(vban-leg)"), rows


# --- the wiring ---------------------------------------------------------------------------------------------

def test_setup_step_13_installs_the_leg_on_the_stream_role():
    s = SETUP.read_text()
    i = s.index("strih_intercom_hub_remove ||")
    j = s.index("step 14 ")
    assert "strih_vban_leg_install || fail" in s[i:j]
    assert s.count("strih_vban_leg_install") == 1


def test_verify_item_9_grades_the_leg_on_the_stream_role():
    v = VERIFY.read_text()
    line = next(x for x in v.splitlines() if 'strih_program_vban_grade "$SCN_BIN"' in x)
    assert "strih_vban_leg_grade" in line
    assert v.count("strih_vban_leg_grade") == 1


def test_the_provision_lib_sources_the_leg_lib():
    assert "strih-vban-leg.sh" in PROVISION.read_text()


def test_both_orchestrators_stay_within_their_line_budget():
    for f in (SETUP, VERIFY):
        assert len(f.read_text().splitlines()) <= 1000, f
