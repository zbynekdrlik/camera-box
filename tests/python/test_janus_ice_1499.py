"""issue 1499: Janus gathers ICE candidates on the LAN address only, on a box with an audio-VLAN leg.

dstack's two-networks rule (section 4): a box announces a non-Dante service on its LAN or loopback address
only, "a Janus candidate list" included. strih-snv runs Janus beside its Dante leg (enp7s0.70); the package
janus.jcfg gathers on every interface. Pinned here:

  * scripts/janus_ice_cfg.py: the managed `ice_enforce_list` block in janus.jcfg's nat section -- only the
    block is written or removed, the package's `ice_ignore_list` line and the issue-1453 TURN block stay
    byte-identical, a hand-set `ice_enforce_list` is a conflict, a loopback / non-IPv4 value is refused, the
    write keeps the file's mode;
  * scripts/lib/strih-janus-ice.sh: a box declaring a leg (strih-snv, stream-snv) gets STRIH_IP, a box
    without one (strih-pp, stream-pp) gets no block -- nothing Dante-specific acts there (owner 11.11.2026);
  * the grade: the block = STRIH_IP, and a running Janus that started before the jcfg changed is a FAIL;
  * the wiring: setup-strih step 14 applies it before its Janus restart, verify-strih runs the grade only
    for a declared leg, strih-provision.sh sources the lib.

No root, no rig: the real box facts, a temp jcfg through STRIH_JANUS_ICE_JCFG, a fake systemctl on PATH.
"""
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent.parent
SCRIPTS = REPO / "scripts"
TOOL = SCRIPTS / "janus_ice_cfg.py"
LIB = SCRIPTS / "lib" / "strih-janus-ice.sh"
FACTS = SCRIPTS / "lib" / "strih-box-facts.sh"

sys.path.insert(0, str(SCRIPTS))
import janus_ice_cfg as ice  # noqa: E402
from janus_turn_rest_cfg import JcfgError  # noqa: E402
import janus_turn_rest_cfg as turn  # noqa: E402

# The shape of strih-snv's live janus.jcfg nat section (read 11.11.2026), the TURN block in place.
PACKAGE = """general: {
\tconfigs_folder = "/etc/janus"
}

nat: {
\t# camera-box issue 1453: TURN credentials from the intercom hub (setup-strih step 16i) -- managed, do not edit
\tturn_rest_api = "http://127.0.0.1:8790/api/turn-rest"
\tturn_rest_api_method = "GET"
\t# end camera-box issue 1453
\tnice_debug = false
\t#ice_enforce_list = "eth0"
\tice_ignore_list = "vmnet"
}

media: {
\t#ipv6 = true
}
"""


def _nat(text):
    m = re.search(r"^nat: \{\n(.*?)^\}", text, re.S | re.M)
    return m.group(1)


# --- the editor ---------------------------------------------------------------------------------------------

def test_set_puts_the_block_inside_nat_and_leaves_every_other_line_alone():
    new = ice.apply(PACKAGE, "10.77.9.202")
    assert 'ice_enforce_list = "10.77.9.202"' in _nat(new)
    assert new.replace("".join(ice._block("10.77.9.202", "\t")), "") == PACKAGE
    assert ice.check(new, "10.77.9.202") == "ok"
    # the package's ignore line and the TURN block are untouched, the TURN tool still reads its block
    assert '\tice_ignore_list = "vmnet"\n' in new
    assert turn.check(new, "http://127.0.0.1:8790/api/turn-rest") == "ok"


def test_set_is_idempotent_and_a_new_address_replaces_the_old():
    once = ice.apply(PACKAGE, "10.77.9.202")
    assert ice.apply(once, "10.77.9.202") == once
    moved = ice.apply(once, "10.77.8.202")
    assert ice.check(moved, "10.77.8.202") == "ok"
    assert ice.check(moved, "10.77.9.202") == "differs"
    assert moved.count(ice.BEGIN) == 1


def test_clear_restores_the_package_text_byte_for_byte():
    assert ice.apply(ice.apply(PACKAGE, "10.77.9.202"), None) == PACKAGE
    assert ice.apply(PACKAGE, None) == PACKAGE
    assert ice.check(PACKAGE, "10.77.9.202") == "absent"


def test_a_hand_set_enforce_list_is_a_conflict_never_a_duplicate():
    hand = PACKAGE.replace('\t#ice_enforce_list = "eth0"', '\tice_enforce_list = "eth0"')
    with pytest.raises(JcfgError):
        ice.apply(hand, "10.77.9.202")
    with pytest.raises(JcfgError):
        ice.apply(hand, None)


@pytest.mark.parametrize("bad", ["127.0.0.1", "0.0.0.0", "eth0", "10.77.9.202\"", "", "224.0.0.1", "::1"])
def test_an_unusable_address_is_refused(bad):
    with pytest.raises(JcfgError):
        ice.apply(PACKAGE, bad)


def test_a_file_without_a_nat_section_gets_one():
    text = 'general: {\n\tconfigs_folder = "/etc/janus"\n}\n'
    new = ice.apply(text, "10.77.9.202")
    assert ice.check(new, "10.77.9.202") == "ok"
    assert new.startswith(text)


def test_a_half_block_or_an_unclosed_nat_is_refused():
    with pytest.raises(JcfgError):
        ice.apply(PACKAGE.replace("nice_debug", ice.BEGIN + "\n\tnice_debug"), "10.77.9.202")
    with pytest.raises(JcfgError):
        ice.apply("nat: {\n\tnice_debug = false\n", "10.77.9.202")


def test_the_cli_writes_atomically_keeps_the_mode_and_refuses_a_conflict(tmp_path):
    f = tmp_path / "janus.jcfg"
    f.write_text(PACKAGE)
    f.chmod(0o640)

    def run(*a):
        return subprocess.run([sys.executable, str(TOOL), "--file", str(f), *a],
                              capture_output=True, text=True, timeout=30)

    r = run("--set", "10.77.9.202")
    assert (r.returncode, r.stdout.strip()) == (0, "changed")
    assert stat.S_IMODE(f.stat().st_mode) == 0o640
    assert run("--set", "10.77.9.202").stdout.strip() == "unchanged"
    assert run("--check", "10.77.9.202").returncode == 0
    r = run("--check", "10.77.8.202")
    assert (r.returncode, r.stdout.strip()) == (1, "differs")
    assert run("--clear").stdout.strip() == "changed"
    assert f.read_text() == PACKAGE
    f.write_text(PACKAGE.replace('\t#ice_enforce_list = "eth0"', '\tice_enforce_list = "eth0"'))
    before = f.read_text()
    r = run("--set", "10.77.9.202")
    assert r.returncode == 2 and "by hand" in r.stderr
    assert f.read_text() == before
    assert not [p for p in tmp_path.iterdir() if p.name.startswith(".janus.jcfg.")]


# --- the lib ------------------------------------------------------------------------------------------------

_FAKE_SYSTEMCTL = """#!%s
import os, sys
a = [x for x in sys.argv[1:] if x != "--quiet"]
if a[:1] == ["is-active"]:
    sys.exit(0 if os.environ.get("FAKE_JANUS_ACTIVE") == "1" else 3)
if a[:1] == ["show"]:
    print(os.environ.get("FAKE_JANUS_SINCE", ""))
    sys.exit(0)
sys.exit(0)
""" % sys.executable


@pytest.fixture
def rig(tmp_path):
    b = tmp_path / "bin"
    b.mkdir()
    s = b / "systemctl"
    s.write_text(_FAKE_SYSTEMCTL)
    s.chmod(0o755)
    jcfg = tmp_path / "janus.jcfg"
    jcfg.write_text(PACKAGE)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("STRIH_", "FAKE_"))}
    env.update(PATH="%s:%s" % (b, os.environ["PATH"]), STRIH_JANUS_ICE_JCFG=str(jcfg))
    return jcfg, env


def _bash(script, box, env):
    pre = "set -euo pipefail\n. \"%s\"\n. \"%s\"\n" % (FACTS, LIB)
    pre += 'ok() { echo "OK $*"; }\nbad() { echo "FAIL $*"; }\nnote() { echo "NOTE $*"; }\n'
    return subprocess.run(["bash", "-c", pre + 'strih_box_load "$1" || exit 9\n' + script, "h", box],
                          capture_output=True, text=True, env=env, timeout=60)


def _fact(box, name):
    for ln in (SCRIPTS / "strih-boxes" / ("%s.env" % box)).read_text().splitlines():
        if ln.startswith(name + "="):
            return ln.split("=", 1)[1]
    raise AssertionError("no %s in %s" % (name, box))


@pytest.mark.parametrize("box", ["strih-snv", "stream-snv"])
def test_a_box_with_a_leg_gets_its_lan_address(rig, box):
    jcfg, env = rig
    assert _fact(box, "STRIH_PROGRAM_VBAN_LEG") != "none"
    r = _bash("strih_janus_ice_apply", box, env)
    assert r.returncode == 0, r.stderr
    want = _fact(box, "STRIH_IP")
    assert ice.check(jcfg.read_text(), want) == "ok"
    assert want in r.stdout and "changed" in r.stdout
    r = _bash("strih_janus_ice_apply", box, env)
    assert "unchanged" in r.stdout


@pytest.mark.parametrize("box", ["strih-pp", "stream-pp"])
def test_a_box_without_a_leg_gets_no_block_and_a_leftover_is_removed(rig, box):
    jcfg, env = rig
    assert _fact(box, "STRIH_PROGRAM_VBAN_LEG") == "none"
    jcfg.write_text(ice.apply(PACKAGE, "10.77.8.202"))
    r = _bash("strih_janus_ice_apply", box, env)
    assert r.returncode == 0, r.stderr
    assert jcfg.read_text() == PACKAGE
    assert "no audio-VLAN leg" in r.stdout


def test_an_absent_jcfg_is_one_line_never_a_failure(rig):
    jcfg, env = rig
    jcfg.unlink()
    r = _bash("strih_janus_ice_apply; strih_janus_ice_grade", "strih-snv", env)
    assert r.returncode == 0, r.stderr
    assert "absent" in r.stdout and "FAIL" not in r.stdout


def test_a_conflict_fails_the_apply_and_leaves_the_file(rig):
    jcfg, env = rig
    hand = PACKAGE.replace('\t#ice_enforce_list = "eth0"', '\tice_enforce_list = "eth0"')
    jcfg.write_text(hand)
    r = _bash("strih_janus_ice_apply", "strih-snv", env)
    assert r.returncode != 0
    assert jcfg.read_text() == hand


def test_the_grade_fails_a_missing_block_and_a_janus_older_than_the_jcfg(rig):
    jcfg, env = rig
    r = _bash("strih_janus_ice_grade", "strih-snv", env)
    assert "FAIL (janus-ice)" in r.stdout and "absent" in r.stdout
    jcfg.write_text(ice.apply(PACKAGE, _fact("strih-snv", "STRIH_IP")))
    os.utime(jcfg, (2000000000, 2000000000))
    old = dict(env, FAKE_JANUS_ACTIVE="1", FAKE_JANUS_SINCE="Wed 2026-11-11 08:00:00 UTC")
    r = _bash("strih_janus_ice_grade", "strih-snv", old)
    assert "FAIL (janus-ice)" in r.stdout and "restart janus" in r.stdout
    os.utime(jcfg, (1700000000, 1700000000))
    r = _bash("strih_janus_ice_grade", "strih-snv", old)
    assert "OK (janus-ice)" in r.stdout and "loaded it" in r.stdout
    r = _bash("strih_janus_ice_grade", "strih-snv", env)
    assert "OK (janus-ice)" in r.stdout and "not running" in r.stdout


# --- the wiring ---------------------------------------------------------------------------------------------

def test_setup_applies_it_in_step_14_before_the_janus_restart():
    s = (SCRIPTS / "setup-strih.sh").read_text()
    step14 = s[s.index('step 14 "Janus audiobridge'):]
    a = step14.index("strih_janus_ice_apply")
    assert a < step14.index("systemctl restart janus")
    assert a > step14.index("janus.transport.http.jcfg")


def test_verify_grades_it_only_for_a_declared_leg_and_the_lib_is_sourced():
    v = (SCRIPTS / "verify-strih.sh").read_text()
    assert '[ "$(strih_vban_leg_kind)" = none ] || strih_janus_ice_grade' in v
    p = (SCRIPTS / "lib" / "strih-provision.sh").read_text()
    assert '/strih-janus-ice.sh"' in p
