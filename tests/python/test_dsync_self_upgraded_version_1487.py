"""Issue 1487 -- the nodes dsync upgrades itself get a REPORT-ONLY version row.

`DSYNC_SELF_UPGRADED_NODES` (companion-snv mbc fohabl iem-snv alex-nb) are never rolled and never
pinned. On 9.11.2026 mbc ran dsync 1.27.3 below the fleet pin 1.28.0, its PTP lock flapped and, as
the Dante TX of the stream-snv program, it silenced the program for ~20 min; the version was printed
nowhere. Now:
  - `dantesync-version-gate.sh --fleet` reads each of them with the ONE version reader and grades it
    with the pure `dantesync_self_upgraded_verdict` (ordering by `release_version_order_word`):
    below the pin = SELF-BEHIND + a stderr WARNING, at/above = OK, unread = SELF-UNKNOWN (a NOTE).
    The gate exit NEVER changes and the node is never rolled.
  - the handover check (`rig_dev_handover_decision.py`, item `dantesync`) names the node, its
    version and the pin in a Slovak `pozor:` note, also on the summary line; the item status stays
    the gate's exit code.

Tier-0: the gate's PATH stubs + fixture seams, no box, no network.
"""
import os
import pathlib
import re
import stat
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_GATE = _ROOT / "scripts" / "dantesync-version-gate.sh"
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import rig_dev_handover_decision as d  # noqa: E402

_PIN = re.search(r'DANTESYNC_VERSION_PIN="\$\{DANTESYNC_VERSION_PIN:-([0-9.]+)\}"',
                 _GATE.read_text()).group(1)
_SELF = {"mbc": "10.77.7.232", "fohabl": "10.77.7.30", "iem-snv": "iem-snv.lan",
         "companion-snv": "companion-snv.lan"}
_ROLLED = {"cam2": "10.77.9.62", "cam3": "10.77.9.63", "cam5": "10.77.9.65",
           "strih-snv": "10.77.9.202", "stream-snv": "10.77.9.204", "resolume-snv": "resolume.lan"}


def _older(pin):
    maj, mnr, pat = (int(x) for x in pin.split("."))
    return f"{maj}.{mnr - 1}.{pat + 3}" if mnr else f"{maj - 1}.99.0"


def _newer(pin):
    maj, mnr, pat = (int(x) for x in pin.split("."))
    return f"{maj}.{mnr}.{pat + 1}"


# --- the pure verdict (sourced from the gate, above its source guard) ---------------------------
def _verdict(name, version, pin):
    script = '. "$1"; set +e; dantesync_self_upgraded_verdict "$2" "$3" "$4"; echo "RC=$?"'
    r = subprocess.run(["bash", "-c", "set -euo pipefail; " + script, "t", str(_GATE),
                        name, version, pin], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_a_self_upgraded_node_below_the_pin_is_named_behind():
    out = _verdict("mbc", "1.27.3", "1.28.0")
    assert "RC=40" in out
    assert re.search(r"^\s+mbc\s+1\.27\.3\s+SELF-BEHIND \(version=1\.27\.3 pin=1\.28\.0;", out, re.M), out


def test_ordering_is_semver_never_text():
    # 1.9.0 < 1.28.0 as versions, but > as text: a text compare would read OK
    assert "RC=40" in _verdict("mbc", "1.9.0", "1.28.0")
    assert "RC=0" in _verdict("mbc", "1.28.0", "1.9.0")


def test_a_self_upgraded_node_on_or_above_the_pin_is_ok():
    for v in ("1.28.0", "1.28.1", "1.30.0"):
        out = _verdict("fohabl", v, "1.28.0")
        assert "RC=0" in out and " OK" in out and "SELF-BEHIND" not in out, out


def test_an_unread_or_unparseable_version_is_a_note_never_ok():
    for v in ("", "garbage", "1.28"):
        out = _verdict("iem-snv", v, "1.28.0")
        assert "RC=41" in out and "SELF-UNKNOWN" in out, (v, out)


# --- the --fleet run: the row is printed and the gate exit never moves --------------------------
def _stub_bin(tmp_path, versions):
    b = tmp_path / "bin"
    b.mkdir()
    arms = "".join(f'    *@{ip}) echo "dsync {v}" ;;\n' for ip, v in versions.items())
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\n"
        'shift 2\n'
        'target=""\n'
        'for a in "$@"; do case "$a" in *@*) target="$a"; break ;; esac; done\n'
        'case "$target" in\n' + arms + "    *) exit 255 ;;\nesac\n")
    (b / "dsync").write_text(f'#!/usr/bin/env bash\necho "dsync {_PIN}"\n')
    for f in b.iterdir():
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return b


def _fleet(tmp_path, self_versions, **over):
    versions = {ip: _PIN for ip in _ROLLED.values()}
    versions.update({_SELF[n]: v for n, v in self_versions.items() if v is not None})
    bindir = _stub_bin(tmp_path, versions)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("DANTESYNC_", "DSYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "WIN_SSH_USER"))}
    env.update({
        "PATH": f"{bindir}:/usr/bin:/bin",
        "OBS_FLEET_HOME": "strih-snv stream-snv resolume-snv",
        "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env"),
        "DANTESYNC_VERSION_GATE_SSH_PASS": "fleet-default",
        "DANTESYNC_FOHABL_SSH_PASS": "foh-secret",
        "DANTESYNC_TRAY_EXPECTED_SHA": "a" * 64,
        "DANTESYNC_NEWEST_RELEASE": _PIN,
        "DANTESYNC_FLEET_SCAN_CMD": "/bin/true",
        "DANTESYNC_TRAY_SHA_RESOLUME_SNV": "a" * 64,
    })
    env.update(over)
    return subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet), "--fleet"],
                          capture_output=True, text=True, env=env)


def test_fleet_names_a_self_upgraded_node_behind_the_pin_and_still_passes(tmp_path):
    old = _older(_PIN)
    r = _fleet(tmp_path, {"mbc": old, "fohabl": _PIN, "iem-snv": _newer(_PIN), "companion-snv": _PIN})
    assert r.returncode == 0, r.stdout + r.stderr
    assert re.search(rf"^\s+mbc\s+{re.escape(old)}\s+SELF-BEHIND \(version={re.escape(old)} "
                     rf"pin={re.escape(_PIN)};", r.stdout, re.M), r.stdout
    for n in ("fohabl", "iem-snv", "companion-snv"):
        assert re.search(rf"^\s+{n}\s+\S+\s+OK", r.stdout, re.M), (n, r.stdout)
    assert "DSYNC-SELF-UPGRADED WARNING: mbc" in r.stderr and old in r.stderr and _PIN in r.stderr
    # a self-upgraded node is never part of the pinned roll-up
    assert "GATE PASS" in r.stdout
    assert "NOT GATED on the pin" not in r.stdout


def test_fleet_an_unread_self_upgraded_node_is_a_note_and_the_gate_passes(tmp_path):
    r = _fleet(tmp_path, {"mbc": _PIN, "fohabl": _PIN, "iem-snv": None, "companion-snv": _PIN},
               DANTESYNC_FOHABL_SSH_PASS="")
    assert r.returncode == 0, r.stdout + r.stderr
    assert re.search(r"^\s+iem-snv\s+-\s+SELF-UNKNOWN", r.stdout, re.M), r.stdout
    assert re.search(r"^\s+fohabl\s+-\s+SELF-UNKNOWN", r.stdout, re.M), r.stdout
    assert "DSYNC-SELF-UPGRADED NOTE: iem-snv" in r.stderr


def test_fleet_an_unreachable_self_upgraded_reach_node_is_skipped_by_name(tmp_path):
    r = _fleet(tmp_path, {"mbc": _PIN, "fohabl": _PIN, "iem-snv": _PIN, "companion-snv": _PIN})
    assert "  alex-nb SKIPPED (unreachable" in r.stdout, r.stdout


def test_a_self_upgraded_node_named_on_the_command_line_stays_pinned(tmp_path):
    # an explicitly named node is graded on the pin as before (no silent downgrade to report-only)
    bindir = _stub_bin(tmp_path, {"10.77.7.232": _older(_PIN)})
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = dict(os.environ, PATH=f"{bindir}:/usr/bin:/bin", DANTESYNC_TRAY_EXPECTED_SHA="a" * 64,
               DANTESYNC_TRAY_SHA_MBC="a" * 64, DANTESYNC_NEWEST_RELEASE=_PIN)
    r = subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet),
                        "--win", "mbc=newlevel@10.77.7.232"], capture_output=True, text=True, env=env)
    assert r.returncode == 20, r.stdout + r.stderr


# --- the handover relays it, in Slovak, without changing the item status ------------------------
def _item(key):
    return next(i for i in d.ITEMS if i.key == key)


_GATE_OUT_BEHIND = (
    "== dantesync-version-gate (#862): fleet-wide dantesync version parity — pin 1.28.0 ==\n"
    "  cam2           1.28.0       OK\n\n"
    "GATE PASS — 1 box(es) on the pinned dantesync 1.28.0 (any acked-offline box excluded above).\n\n"
    "-- nodes dsync upgrades itself (issue 1487, report-only, never blocks) --\n"
    "  mbc            1.27.3       SELF-BEHIND (version=1.27.3 pin=1.28.0; dsync upgrades this node "
    "itself -- never rolled here)\n"
    "  fohabl         1.28.0       OK\n"
    "  iem-snv        -            SELF-UNKNOWN (version not read -- report-only)\n"
    "!! DSYNC-SELF-UPGRADED WARNING: mbc runs dsync 1.27.3 below the fleet pin 1.28.0\n")


def test_handover_names_the_behind_node_its_version_and_the_pin():
    e = _item("dantesync").decide({"dantesync": (_GATE_OUT_BEHIND, 0)})
    assert e["status"] == d.OK
    assert "pozor:" in e["message"]
    assert "mbc beží dsync 1.27.3 pod pinom 1.28.0" in e["message"]
    assert "iem-snv" in e["message"]
    _, summary, code = d.build_checklist([e])
    assert code == 0
    assert "pozor:" in summary and "mbc beží dsync 1.27.3 pod pinom 1.28.0" in summary


def test_handover_note_rides_on_a_drifted_gate_too():
    e = _item("dantesync").decide({"dantesync": (_GATE_OUT_BEHIND, 20)})
    assert e["status"] == d.FORGOT
    assert "mbc beží dsync 1.27.3 pod pinom 1.28.0" in e["message"]
    _, summary, code = d.build_checklist([e])
    assert code == 1 and "mbc beží dsync 1.27.3" in summary


def test_handover_without_self_rows_is_unchanged():
    e = _item("dantesync").decide({"dantesync": ("GATE PASS\n  fohabl  1.28.0  OK\n", 0)})
    assert e["message"] == "všetky uzly sú na pripnutej dantesync verzii"
    _, summary, _ = d.build_checklist([e])
    assert "pozor" not in summary
