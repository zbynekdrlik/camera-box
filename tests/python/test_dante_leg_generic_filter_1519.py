"""issue 1519 (dstack #264, owner decision relayed 13.11.2026: "Dante works on switch-on, one identical filter on
every PC"): the Dante leg runs dstack's GENERIC stateful filter, a pinned artifact, and the devices bind their
fixed slot flow ports (dstack >= 0.6.117), so camera-box writes no RX range any more.

  * scripts/dstack-leg-filter.pin: dstack tag v0.6.129, its commit, the sha256 of tools/linux/dante-leg.nft and
    tools/linux/dante-leg-notrack.nft. strih_dante_leg_filter_ensure fetches both by the exact tag into
    <tree>/dstack-release/<tag>/, proves them against the pin, no fallback;
  * setup-strih step 13 on a Dante box (facts STRIH_DANTE_DEVICES) installs them under /etc/dstack/, loads them
    through ONE oneshot in dstack's order (`nft -D dante_leg=<leg> -f dante-leg.nft`, then the notrack file),
    then removes our per-box `inet vban_dante70` table, its unit and its file; avahi gets deny-interfaces=<leg>;
  * VBAN on the Dante leg is retired: a Dante box declares no leg VBAN sender;
  * STRIH_DANTE_DEVICES = name=arc:cmc:flows (no rx range); the dante-input TOML writes no rx_port_base and
    never a tx_port_base; an ARC port must sit in 4440-4471 (obs#43: the slot block comes from it);
  * verify item 9 on a Dante box grades the two live tables against the pinned files (a netns render of the
    installed files, nft list normalized), the oneshot, the avahi deny, and FAILs a leftover vban_dante70.

The netns render fixture tests/fixtures/dstack_leg_1519/netns-render-zzleg0.70.txt was made with the real nft
1.0.9 on dev1 (`sudo -n unshare -n`, both files loaded with dante_leg=zzleg0.70); the fake nft/unshare of
vban_leg_fakes_1439.py replays it for any leg name.
"""
import hashlib
import shutil
import sys
import tomllib
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import vban_leg_fakes_1439 as fk  # noqa: E402
from test_vban_leg_1439 import (  # noqa: E402
    BOXES, LEG_LIB, STRIH_VLAN_IF, VLAN_PROPS, _bash, _boxes_with, _strih_box)

REPO = HERE.parent.parent
PIN = REPO / "scripts" / "dstack-leg-filter.pin"
FIX = REPO / "tests" / "fixtures" / "dstack_leg_1519"
NFT_SHA = "79a69df5e87680921d3494b53805478f50d0ca322e3e188984d82c0a0fad809f"
NOTRACK_SHA = "81eb810b7102bd7af1e5a8341804af585d367fc4dd353943c034107957e7114e"
UNIT = "dstack-dante-leg-filter.service"
OLD_UNIT = "vban-dante70-filter.service"
AVAHI_CONF = ("[server]\n#host-name=foo\nuse-ipv4=yes\nuse-ipv6=yes\n#allow-interfaces=eth0\n#deny-interfaces=eth1\n"
              "ratelimit-interval-usec=1000000\nratelimit-burst=1000\n\n[wide-area]\nenable-wide-area=yes\n")


def _pin():
    return dict(line.split("=", 1) for line in PIN.read_text().splitlines() if "=" in line)


# --- the pin + the fetch --------------------------------------------------------------------------------

def test_the_pin_names_v0_6_119_its_commit_and_both_files():
    p = _pin()
    assert p == {"DSTACK_LEG_FILTER_TAG": "v0.6.129",
                 "DSTACK_LEG_FILTER_COMMIT": "fb6d292f69f677e16cc83943ff64e28732bcbe81",
                 "DSTACK_LEG_FILTER_NFT_SHA256": NFT_SHA, "DSTACK_LEG_FILTER_NOTRACK_SHA256": NOTRACK_SHA}


def test_the_test_copies_are_the_pinned_bytes():
    assert hashlib.sha256((FIX / "dante-leg.nft").read_bytes()).hexdigest() == NFT_SHA
    assert hashlib.sha256((FIX / "dante-leg-notrack.nft").read_bytes()).hexdigest() == NOTRACK_SHA


def _mirror(tmp_path, tag="v0.6.129", tamper=False):
    """A file:// mirror laid out like raw.githubusercontent.com/<repo>/<tag>/tools/linux/<file>."""
    d = tmp_path / "mirror" / tag / "tools" / "linux"
    d.mkdir(parents=True)
    for f in ("dante-leg.nft", "dante-leg-notrack.nft"):
        shutil.copy(FIX / f, d / f)
    if tamper:
        with open(d / "dante-leg.nft", "a") as fh:
            fh.write("# tampered\n")
    return "file://%s" % (tmp_path / "mirror")


def _ensure(tmp_path, url):
    cache = tmp_path / "cache"
    return _bash('. "%s"\nstrih_dante_leg_filter_ensure "$1"' % LEG_LIB, REPO,
                 env={"STRIH_DANTE_LEG_URL": url, "STRIH_DANTE_LEG_CACHE": str(cache)}), cache


def test_ensure_fetches_both_files_by_the_tag_and_proves_them(tmp_path):
    r, cache = _ensure(tmp_path, _mirror(tmp_path))
    assert r.returncode == 0, r.stderr
    d = cache / "v0.6.129"
    assert r.stdout.strip() == str(d)
    assert hashlib.sha256((d / "dante-leg.nft").read_bytes()).hexdigest() == NFT_SHA
    assert hashlib.sha256((d / "dante-leg-notrack.nft").read_bytes()).hexdigest() == NOTRACK_SHA
    # a cached pair needs no network: a dead mirror still answers
    r2, _ = _ensure(tmp_path, "file://%s/nowhere" % tmp_path)
    assert r2.returncode == 0 and r2.stdout.strip() == str(d), r2.stderr


def test_ensure_refuses_bytes_that_are_not_the_pin_and_keeps_nothing(tmp_path):
    r, cache = _ensure(tmp_path, _mirror(tmp_path, tamper=True))
    assert r.returncode != 0 and "dante-leg.nft" in r.stderr and NFT_SHA in r.stderr, r.stderr
    assert not (cache / "v0.6.129" / "dante-leg.nft").exists()
    assert not list((cache / "v0.6.129").glob("*.part"))


def test_ensure_without_a_mirror_fails_by_name_never_another_filter(tmp_path):
    r, _ = _ensure(tmp_path, "file://%s/nowhere" % tmp_path)
    assert r.returncode != 0 and "v0.6.129" in r.stderr, r.stderr


def test_the_strih_deploy_stages_the_pinned_files_and_protects_them():
    t = (REPO / "scripts" / "lib" / "strih-snv-deploy.sh").read_text()
    assert 'strih_dante_leg_filter_ensure "$w/repo"' in t
    assert "--filter=P /dstack-release/***" in t
    assert "dstack-release/" in (REPO / ".gitignore").read_text().splitlines()


# --- the facts ------------------------------------------------------------------------------------------

def test_the_snv_facts_carry_no_rx_range_and_no_leg_vban_sender():
    real = {"STRIH_BOXES_DIR": str(BOXES)}
    body = 'strih_box_load "$1" || exit 9\nprintf "%s|%s\\n" "$(strih_lx_dante_devices)" "$(strih_lx_program_vban_sender)"'
    assert _bash(body, "strih-snv", env=real).stdout.strip() == \
        "strih-snv-intercom=4441:8801:4456 strih-obs=4440:8800:4455|none"
    assert _bash(body, "stream-snv", env=real).stdout.strip() == "stream-obs=4440:8800:4455|none"


@pytest.mark.parametrize("value,word", [
    ("strih-obs=4440:8800:4455:30000-30031", "name=arc:cmc:flows"),
    ("strih-obs=4472:8800:4455", "4440-4471"),
    ("strih-obs=4439:8800:4455", "4440-4471"),
    ("a=4440:8800:4455 b=4441:8800:4456", "used twice"),
])
def test_the_loader_refuses_a_range_an_arc_outside_the_slots_or_a_shared_port(tmp_path, value, word):
    d = _boxes_with(tmp_path, "strih-snv", [("STRIH_DANTE_DEVICES=strih-snv-intercom=4441:8801:4456 strih-obs=4440:8800:4455",
                                             "STRIH_DANTE_DEVICES=" + value)])
    r = _bash("strih_box_load strih-snv", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and word in r.stderr, r.stderr


def test_the_loader_refuses_a_leg_vban_sender_on_a_dante_box(tmp_path):
    d = _boxes_with(tmp_path, "strih-snv", [("STRIH_PROGRAM_VBAN_SENDER=none", "STRIH_PROGRAM_VBAN_SENDER=10.77.7.30:6980")])
    r = _bash("strih_box_load strih-snv", env={"STRIH_BOXES_DIR": str(d)})
    assert r.returncode != 0 and "STRIH_PROGRAM_VBAN_SENDER" in r.stderr and "Dante" in r.stderr, r.stderr


def test_the_toml_writes_no_rx_or_tx_port_base(tmp_path):
    s = tmp_path / "site.conf"
    s.write_text("RX1=Program L|foh-tx|MASTER-L\nRX2=Program R|foh-tx|MASTER-R\n")
    r = _bash('strih_box_load strih-snv >/dev/null; rows="$(strih_dante_input_site_rows "$1")"; '
              'strih_dante_input_toml enp7s0.70 "$(strih_dante_input_ports "$(strih_lx_dante_devices)")" "$rows"', s,
              env={"STRIH_BOXES_DIR": str(BOXES)})
    assert r.returncode == 0, r.stderr
    d = tomllib.loads(r.stdout)
    assert (d["arc_port"], d["cmc_port"], d["flows_port"]) == (4440, 8800, 4455)
    assert "rx_port_base" not in d and "tx_port_base" not in d, d


# --- setup: the install on a Dante box -------------------------------------------------------------------

def _dante_box(tmp_path, avahi=AVAHI_CONF, leftovers=True, **over):
    """strih-snv as it runs today: the converged /24 leg, our vban_dante70 table loaded from its file + unit."""
    b = _strih_box(tmp_path, VLAN_PROPS, **over)
    etc = b.etc / "dstack"
    b.dstack_dir = etc
    b.dstack_unit = b.etc / "systemd" / "system" / UNIT
    b.avahi = b.etc / "avahi" / "avahi-daemon.conf"
    b.avahi.parent.mkdir(parents=True)
    b.avahi.write_text(avahi)
    if leftovers:
        for f in (b.nft_file, b.unit_file):
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text("old\n")
    b.env.update({"STRIH_DANTE_LEG_URL": _mirror(tmp_path), "STRIH_DANTE_LEG_CACHE": str(tmp_path / "cache"),
                  "STRIH_DANTE_LEG_ETC": str(etc), "STRIH_DANTE_LEG_UNIT_FILE": str(b.dstack_unit),
                  "STRIH_DANTE_AVAHI_CONF": str(b.avahi)})
    s = b.st()
    s["unit_files"] = {UNIT: str(b.dstack_unit)}
    s["nft_fixture"] = str(FIX / "netns-render-zzleg0.70.txt")
    s["nft_fixture_files"] = [str(FIX / "dante-leg.nft"), str(FIX / "dante-leg-notrack.nft")]
    fk.write_state(b.state, s)
    fk.write_proc(b.proc, STRIH_VLAN_IF, 1, 2)
    (b.proc / "net" / "ipv4" / "conf" / STRIH_VLAN_IF / "rp_filter").write_text("2\n")
    return b


def _unit_lines(b):
    return [x for x in b.dstack_unit.read_text().splitlines() if x.startswith("ExecStart=")]


def test_install_loads_the_pinned_generic_filter_then_removes_our_table(tmp_path):
    b = _dante_box(tmp_path)
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    # the pinned bytes under /etc/dstack, loaded in dstack's order by ONE oneshot
    assert (b.dstack_dir / "dante-leg.nft").read_bytes() == (FIX / "dante-leg.nft").read_bytes()
    assert (b.dstack_dir / "dante-leg-notrack.nft").read_bytes() == (FIX / "dante-leg-notrack.nft").read_bytes()
    assert _unit_lines(b) == [
        "ExecStart=/usr/sbin/nft -D dante_leg=%s -f %s/dante-leg.nft" % (STRIH_VLAN_IF, b.dstack_dir),
        "ExecStart=/usr/sbin/nft -D dante_leg=%s -f %s/dante-leg-notrack.nft" % (STRIH_VLAN_IF, b.dstack_dir)]
    unit = b.dstack_unit.read_text()
    assert "Type=oneshot" in unit and "RemainAfterExit=yes" in unit and "Before=network-pre.target" in unit
    assert s["units"][UNIT] == {"enabled": True, "active": True}
    live = s.get("nft_other", {})
    assert '"%s"' % STRIH_VLAN_IF in live.get("dstack_dante_leg", "") and "notrack" in live.get("dstack_dante_leg_notrack", "")
    # then our per-box table, its unit and its file go
    assert s["nft_render"] is None
    assert not s["units"][OLD_UNIT]["enabled"]
    assert not b.nft_file.exists() and not b.unit_file.exists()
    log = s["log"]
    loaded = next(i for i, c in enumerate(log) if c[:3] == ["systemctl", "restart", UNIT])
    dropped = next(i for i, c in enumerate(log) if c[:5] == ["nft", "delete", "table", "inet", "vban_dante70"])
    assert loaded < dropped, "dstack's order: the generic filter first, then our table goes"
    assert not [c for c in log if c[0] == "systemctl" and "avahi-daemon" in " ".join(c) and c[1] != "show"]


def test_install_adds_the_leg_to_avahis_deny_list_keeping_every_other_line(tmp_path):
    b = _dante_box(tmp_path)
    assert b.run("strih_vban_leg_install", box="strih-snv").returncode == 0
    lines = b.avahi.read_text().splitlines()
    assert lines.count("deny-interfaces=%s" % STRIH_VLAN_IF) == 1
    assert lines.index("deny-interfaces=%s" % STRIH_VLAN_IF) < lines.index("[wide-area]")
    assert [x for x in lines if not x.startswith("deny-interfaces=")] == AVAHI_CONF.splitlines()


def test_install_extends_an_existing_deny_list(tmp_path):
    b = _dante_box(tmp_path, avahi=AVAHI_CONF.replace("#deny-interfaces=eth1", "deny-interfaces=docker0"))
    assert b.run("strih_vban_leg_install", box="strih-snv").returncode == 0
    assert "deny-interfaces=docker0,%s" % STRIH_VLAN_IF in b.avahi.read_text().splitlines()


def test_a_second_run_changes_nothing(tmp_path):
    b = _dante_box(tmp_path)
    assert b.run("strih_vban_leg_install", box="strih-snv").returncode == 0
    n = len(fk.calls(b.st(), "systemctl", "restart", UNIT))
    avahi = b.avahi.read_text()
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    assert len(fk.calls(b.st(), "systemctl", "restart", UNIT)) == n
    assert b.avahi.read_text() == avahi
    assert not fk.calls(b.st(), "nft", "delete", "table", "inet", "vban_dante70")[1:], "deleted once only"


def test_a_failed_generic_load_keeps_our_table(tmp_path):
    b = _dante_box(tmp_path)
    s = b.st()
    s["nft_load_fail"] = True
    fk.write_state(b.state, s)
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode != 0 and UNIT in r.stderr, r.stdout + r.stderr
    s = b.st()
    assert s["nft_render"] is not None and b.nft_file.exists(), "the leg is never left unfiltered"


def test_a_non_dante_leg_keeps_its_table_and_loses_a_leftover_generic_filter(tmp_path):
    """A box that drops its Dante devices (a VBAN rollback) gets the issue-1439 table back and no dstack filter."""
    b = _dante_box(tmp_path, leftovers=False)
    b.env["STRIH_BOXES_DIR"] = str(_boxes_with(tmp_path, "strih-snv", [
        ("STRIH_DANTE_DEVICES=strih-snv-intercom=4441:8801:4456 strih-obs=4440:8800:4455", "STRIH_DANTE_DEVICES=none"),
        ("STRIH_PROGRAM_VBAN_STREAMS=none", "STRIH_PROGRAM_VBAN_STREAMS=strih-obs"),
        ("STRIH_PROGRAM_VBAN_PORT=none", "STRIH_PROGRAM_VBAN_PORT=6981"),
        ("STRIH_PROGRAM_VBAN_RESERVE_MS=none", "STRIH_PROGRAM_VBAN_RESERVE_MS=30"),
        ("STRIH_DANTE_NETWORK_LATENCY_MS=1", "STRIH_DANTE_NETWORK_LATENCY_MS=none"),
        ("STRIH_DANTE_RECEIVE_BUFFER_MS=12.041666", "STRIH_DANTE_RECEIVE_BUFFER_MS=none")]))
    b.dstack_dir.mkdir(parents=True)
    for f in ("dante-leg.nft", "dante-leg-notrack.nft"):
        shutil.copy(FIX / f, b.dstack_dir / f)
    b.dstack_unit.parent.mkdir(parents=True, exist_ok=True)
    b.dstack_unit.write_text("[Service]\n")
    s = b.st()
    s["units"][UNIT] = {"enabled": True, "active": True}
    s.setdefault("nft_other", {})["dstack_dante_leg"] = "table inet dstack_dante_leg {\n}\n"
    fk.write_state(b.state, s)
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    assert "dstack_dante_leg" not in s.get("nft_other", {})
    assert not b.dstack_unit.exists() and not (b.dstack_dir / "dante-leg.nft").exists()
    rules = b.nft_file.read_text().split("delete table", 1)[1]
    assert s["nft_render"] is not None and "4440" not in rules and "skuid" not in rules and "10.77.7.0/24" not in rules


# --- verify: item 9 on a Dante box -----------------------------------------------------------------------

def _graded(b):
    r = b.run("strih_vban_leg_grade", box="strih-snv")
    assert r.returncode == 0, r.stderr
    return [x for x in r.stdout.splitlines() if x[:3] in ("OK|", "BAD", "NOT")]


def _installed(tmp_path, **over):
    b = _dante_box(tmp_path, **over)
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    s = b.st()
    s.setdefault("show", {})["avahi-daemon.service"] = "@%d\n" % (int(b.avahi.stat().st_mtime) + 60)
    fk.write_state(b.state, s)
    return b


def test_grade_passes_the_installed_generic_filter(tmp_path):
    rows = _graded(_installed(tmp_path))
    assert not [x for x in rows if x.startswith("BAD|")], rows
    for tag in ("(dante-leg) files", "(dante-leg) filter", "(dante-leg) avahi", "(dante-leg) old table"):
        assert any(x.startswith("OK|" + tag) for x in rows), (tag, rows)
    assert not any("vban_dante70" in x and x.startswith("OK|(vban-leg) filter") for x in rows), rows


def test_grade_fails_a_leftover_vban_dante70(tmp_path):
    b = _installed(tmp_path)
    s = b.st()
    s["nft_render"] = (REPO / "tests" / "fixtures" / "vban_leg_1439" / "nft-list-table-strih-snv-live-23-10.txt").read_text()
    fk.write_state(b.state, s)
    bad = [x for x in _graded(b) if x.startswith("BAD|")]
    assert any("(dante-leg) old table" in x and "vban_dante70" in x for x in bad), bad


def test_grade_fails_a_live_table_that_is_not_the_pinned_render(tmp_path):
    b = _installed(tmp_path)
    s = b.st()
    s["nft_other"]["dstack_dante_leg"] = s["nft_other"]["dstack_dante_leg"].replace("22, 8092", "22")
    fk.write_state(b.state, s)
    bad = [x for x in _graded(b) if x.startswith("BAD|")]
    assert any(x.startswith("BAD|(dante-leg) filter") for x in bad), bad


def test_grade_fails_a_missing_notrack_table(tmp_path):
    b = _installed(tmp_path)
    s = b.st()
    del s["nft_other"]["dstack_dante_leg_notrack"]
    fk.write_state(b.state, s)
    assert any(x.startswith("BAD|(dante-leg) filter") for x in _graded(b))


def test_grade_fails_installed_files_that_are_not_the_pin(tmp_path):
    b = _installed(tmp_path)
    with open(b.dstack_dir / "dante-leg-notrack.nft", "a") as fh:
        fh.write("# edited\n")
    bad = [x for x in _graded(b) if x.startswith("BAD|")]
    assert any(x.startswith("BAD|(dante-leg) files") and "dante-leg-notrack.nft" in x for x in bad), bad


def test_grade_fails_avahi_without_the_deny(tmp_path):
    b = _installed(tmp_path)
    b.avahi.write_text(AVAHI_CONF)
    bad = [x for x in _graded(b) if x.startswith("BAD|")]
    assert any(x.startswith("BAD|(dante-leg) avahi") and STRIH_VLAN_IF in x for x in bad), bad


def test_grade_fails_an_avahi_that_started_before_the_deny(tmp_path):
    """Review round 1: our table's avahi uid drop went; until avahi restarts it still announces on the leg."""
    b = _installed(tmp_path)
    s = b.st()
    s["show"]["avahi-daemon.service"] = "@%d\n" % (int(b.avahi.stat().st_mtime) - 600)
    fk.write_state(b.state, s)
    rows = _graded(b)
    assert any(x.startswith("BAD|(dante-leg) avahi") and "restart avahi-daemon" in x for x in rows), rows


# --- review round 1: dstack's notrack file only on a box with no NAT and no other stateful firewall -------

NAT = "table ip nat {\n\tchain POSTROUTING {\n\t\toifname \"docker0\" masquerade\n\t}\n}\n"


@pytest.mark.parametrize("kind", ["nat", "ufw", "firewalld"])
def test_install_refuses_a_box_with_nat_or_another_firewall_and_changes_nothing(tmp_path, kind):
    b = _dante_box(tmp_path)
    s = b.st()
    if kind == "nat":
        s["nft_ruleset_extra"] = NAT
    elif kind == "firewalld":
        s["units"]["firewalld"] = {"enabled": True, "active": True}
    fk.write_state(b.state, s)
    if kind == "ufw":
        (b.etc / "ufw").mkdir(parents=True)
        (b.etc / "ufw" / "ufw.conf").write_text("# ufw\nENABLED=yes\nLOGLEVEL=low\n")
    r = b.run("strih_vban_leg_install", box="strih-snv")
    assert r.returncode != 0 and "notrack" in r.stderr and "nothing changed" in r.stderr, r.stdout + r.stderr
    s = b.st()
    assert not b.dstack_unit.exists() and not (b.dstack_dir / "dante-leg.nft").exists()
    assert s["nft_render"] is not None and b.nft_file.exists(), "our old table stays"


def test_a_disabled_ufw_and_dstacks_own_tables_never_count_as_nat(tmp_path):
    b = _dante_box(tmp_path)
    (b.etc / "ufw").mkdir(parents=True)
    (b.etc / "ufw" / "ufw.conf").write_text("ENABLED=no\n")
    assert b.run("strih_vban_leg_install", box="strih-snv").returncode == 0
    rows = _graded(b)
    assert any(x.startswith("OK|(dante-leg) nat") for x in rows), rows


def test_grade_fails_nat_that_appeared_later(tmp_path):
    b = _installed(tmp_path)
    s = b.st()
    s["nft_ruleset_extra"] = NAT
    fk.write_state(b.state, s)
    bad = [x for x in _graded(b) if x.startswith("BAD|")]
    assert any(x.startswith("BAD|(dante-leg) nat") and "masquerade" in x for x in bad), bad


def test_an_empty_render_never_reads_as_the_live_tables():
    """Review round 1: two empty strings must never compare equal; the render needs both pipeline stages."""
    src = (REPO / "scripts" / "lib" / "strih-dante-leg-filter.sh").read_text()
    assert src.count('[ "${PIPESTATUS[0]}${PIPESTATUS[1]}" = 00 ]') == 2
    assert '&& [ -n "$want" ]' in src and '|| [ -z "$want" ]' in src


def test_an_unreadable_avahi_conf_is_never_overwritten(tmp_path):
    b = _dante_box(tmp_path)
    b.avahi.chmod(0o000)
    try:
        r = b.run("strih_vban_leg_install", box="strih-snv")
    finally:
        b.avahi.chmod(0o644)
    assert r.returncode != 0 and "avahi-daemon.conf" in r.stderr, r.stdout + r.stderr
    assert b.avahi.read_text() == AVAHI_CONF


def test_grade_fails_a_disabled_loader_unit(tmp_path):
    b = _installed(tmp_path)
    s = b.st()
    s["units"][UNIT]["enabled"] = False
    fk.write_state(b.state, s)
    assert any(x.startswith("BAD|(dante-leg) filter") and UNIT in x for x in _graded(b))


def test_the_per_box_dante_rules_are_gone_from_the_lib():
    leg = (REPO / "scripts" / "lib" / "strih-dante-leg.sh").read_text()
    for gone in ("strih_dante_nft_input", "strih_dante_nft_output", "STRIH_DANTE_EPHEMERAL_PORTS",
                 "STRIH_DANTE_MGMT_TCP_PORTS", "_strih_dante_avahi_uid"):
        assert gone not in leg, gone
    assert "strih_dante_nft_" not in LEG_LIB.read_text()

