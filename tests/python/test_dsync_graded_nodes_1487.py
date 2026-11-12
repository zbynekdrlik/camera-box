"""Issue 1487 -- the nodes dsync GRADES itself leave every camera-box grading roster, keep their roll.

dstack, 12.11.2026 (dsync#179 live): a dev1 user timer grades :8898 every 60 s on mbc, fohabl,
iem-snv, companion-snv, alex-nb and resolume-pp and pages through airuleset notify. So camera-box
  - drops the rows iem-snv + companion-snv (dsync grades AND upgrades both; the version gate's
    outside-fleet scan names them HANDED-OVER via DSYNC_FLEET_HANDED_OVER),
  - keeps alex-nb + resolume-pp as rows for the fleet upgrader and the version gate (dsync cannot log
    in there yet), but `DSYNC_GRADED_NODES` drops them from the clock watchdog's default rosters (and
    so from the handover clock item) and from the config drift check: one grader per node,
  - rolls alex-nb again (it left DSYNC_SELF_UPGRADED_NODES, now `mbc fohabl`).

Tier-0: PATH stubs + fixture seams, no box, no network.
"""
import os
import pathlib
import re
import stat
import subprocess

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_GATE = _ROOT / "scripts" / "dantesync-version-gate.sh"
_UPGRADE = _ROOT / "scripts" / "dantesync-fleet-upgrade.sh"
_WATCHDOG = _ROOT / "scripts" / "dantesync-clock-alert-watchdog.sh"
_DRIFT_SH = _ROOT / "scripts" / "dantesync-config-drift.sh"
_FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "dantesync_config_1372"
_PIN = re.search(r'DANTESYNC_VERSION_PIN="\$\{DANTESYNC_VERSION_PIN:-([0-9.]+)\}"', _GATE.read_text()).group(1)
_CLEAN_PREFIXES = ("DANTESYNC_", "DSYNC_", "OBS_FLEET", "RIG_GRANDMASTER", "CAMBOX_OFFLINE_ACK",
                   "WIN_SSH_USER", "DANTE_CLOCK_")
_DSYNC_GRADED = ("alex-nb", "resolume-pp")


def _env(**over):
    env = {k: v for k, v in os.environ.items() if not k.startswith(_CLEAN_PREFIXES)}
    env.update(over)
    return env


def _bash(tmp_path, body, **env):
    script = tmp_path / "run.sh"
    script.write_text(f"set -euo pipefail\n. '{_LIB}'\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


# --- the lib --------------------------------------------------------------------------------------

def test_the_two_handed_over_nodes_are_no_rows_and_the_two_graded_ones_stay(tmp_path):
    names = _bash(tmp_path, "dantesync_fleet_names").stdout.split()
    assert "iem-snv" not in names and "companion-snv" not in names
    assert "alex-nb" in names and "resolume-pp" in names


def test_the_defaults_of_the_three_dsync_lists(tmp_path):
    r = _bash(tmp_path, 'printf "%s|%s|%s" "$DSYNC_SELF_UPGRADED_NODES" "$DSYNC_GRADED_NODES" '
                        '"$DSYNC_FLEET_HANDED_OVER"')
    assert r.stdout == "mbc fohabl|alex-nb resolume-pp|iem-snv.lan companion-snv.lan", r.stderr


@pytest.mark.parametrize("name,rc", [("alex-nb", 0), ("resolume-pp", 0), ("strih-pp", 1), ("mbc", 1),
                                     ("alex", 1), ("", 1)])
def test_dsync_graded_accessor(tmp_path, name, rc):
    r = _bash(tmp_path, f'set +e; dantesync_fleet_dsync_graded "{name}"; echo "rc=$?"')
    assert r.stdout.strip() == f"rc={rc}", r.stderr


def test_graded_here_drops_only_the_dsync_graded_names_and_keeps_the_order(tmp_path):
    r = _bash(tmp_path, 'printf "%s" "$(dantesync_fleet_graded_here "cam1 resolume-pp strih-pp alex-nb stream-pp")"')
    assert r.stdout == "cam1 strih-pp stream-pp"
    empty = _bash(tmp_path, 'printf "[%s]" "$(dantesync_fleet_graded_here "")"')
    assert empty.stdout == "[]"


def test_an_empty_graded_list_grades_everything_here_again(tmp_path):
    r = _bash(tmp_path, 'printf "%s" "$(dantesync_fleet_graded_here "resolume-pp alex-nb")"', DSYNC_GRADED_NODES="")
    assert r.stdout == "resolume-pp alex-nb"


@pytest.mark.parametrize("host,rc", [("iem-snv.lan", 0), ("IEM-SNV.LAN.", 0), ("iem-snv", 0),
                                     ("companion-snv.lan", 0), ("iem-snv-2.lan", 1), ("stray.lan", 1),
                                     ("iem-snv.pp.lan", 1), ("?", 1), ("", 1)])
def test_handed_over_matches_name_label_case_and_ptr_dot(tmp_path, host, rc):
    r = _bash(tmp_path, f'set +e; dantesync_fleet_handed_over "{host}"; echo "rc=$?"')
    assert r.stdout.strip() == f"rc={rc}", r.stderr


def test_a_bare_handed_over_entry_matches_a_scanned_full_name(tmp_path):
    r = _bash(tmp_path, 'set +e; dantesync_fleet_handed_over iem-snv.lan; echo "rc=$?"; '
                        'dantesync_fleet_handed_over iem-snv.pp.lan; echo "rc=$?"', DSYNC_FLEET_HANDED_OVER="iem-snv")
    assert r.stdout.split() == ["rc=0", "rc=0"], r.stderr


# --- the graders drop them ------------------------------------------------------------------------

def _source_watchdog(var, **env):
    script = f'set -uo pipefail\n. "{_WATCHDOG}"\nprintf "%s" "${{{var}}}"\n'
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=_env(**env))
    assert r.returncode == 0, r.stderr
    return r.stdout


@pytest.mark.parametrize("var", ["DANTE_CLOCK_CAM_NODES", "DANTE_CLOCK_OBS_NODES", "DANTE_CLOCK_FIXED_NODES",
                                 "DANTE_CLOCK_REACH_NODES", "DANTE_CLOCK_LOCAL_NODES"])
def test_no_watchdog_default_roster_grades_a_dsync_graded_node(var):
    assert not set(_source_watchdog(var).split()) & set(_DSYNC_GRADED)


def test_the_reach_roster_keeps_every_other_reach_node():
    assert _source_watchdog("DANTE_CLOCK_REACH_NODES") == "cam1 cam4 cam6 cam7 strih-pp stream-pp"


def test_an_explicit_watchdog_override_is_kept_byte_for_byte():
    assert _source_watchdog("DANTE_CLOCK_REACH_NODES", DANTE_CLOCK_REACH_NODES="resolume-pp alex-nb") \
        == "resolume-pp alex-nb"


def test_the_drift_check_never_reads_a_dsync_graded_node(tmp_path):
    log = tmp_path / "fetched.log"
    stub = tmp_path / "fetch.sh"
    stub.write_text(f'#!/usr/bin/env bash\necho "$1" >> "{log}"\ncp "{_FIX / "cam1.json"}" "$4"\n')
    stub.chmod(0o755)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    r = subprocess.run(["bash", str(_DRIFT_SH), "--fleet-file", str(fleet), "--only", "cam2 alex-nb resolume-pp"],
                       capture_output=True, text=True,
                       env=_env(DANTESYNC_CONFIG_DRIFT_FETCH_CMD=str(stub), OBS_FLEET_HOME="alex-nb resolume-pp",
                                DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env")))
    for name, role in (("alex-nb", "audio"), ("resolume-pp", "video-pp")):
        assert f"node={name} role={role} verdict=SKIPPED (graded by dsync" in r.stdout, r.stdout + r.stderr
    assert log.read_text().split() == ["cam2"], log.read_text()


# --- the roll keeps them ----------------------------------------------------------------------------

def _stub_bin(tmp_path, versions):
    b = tmp_path / "bin"
    b.mkdir()
    arms = "".join(f'    *@{host}) echo "dantesync {v}" ;;\n' for host, v in versions.items())
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\nshift 2\ntarget=''\n"
        'for a in "$@"; do case "$a" in *@*) target="$a"; break ;; esac; done\n'
        f'printf "%s\\n" "$target" >> "{tmp_path}/dials.log"\n'
        'case "$target" in\n' + arms + "    *) exit 255 ;;\nesac\n")
    (b / "dantesync").write_text(f'#!/usr/bin/env bash\necho "dantesync {_PIN}"\n')
    for f in b.iterdir():
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return b


_HOSTS = ["10.77.9.6%d" % n for n in (2, 3, 5)] + ["10.77.9.202", "10.77.9.204", "resolume.lan",
                                                   "10.77.7.232", "10.77.7.30", "resolume-pp.lan", "10.77.7.109"]
_HOME = "strih-snv stream-snv resolume-snv resolume-pp alex-nb"


def test_the_upgrader_still_rolls_alex_nb_and_resolume_pp(tmp_path):
    v = {h: _PIN for h in _HOSTS}
    v["resolume-pp.lan"] = "1.8.10"
    v["10.77.7.109"] = "1.8.10"
    b = _stub_bin(tmp_path, v)
    env = _env(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=_HOME, DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env"),
               SSH_PASS="pw", CAMBOX_OFFLINE_ACK="", DANTESYNC_FOHABL_SSH_PASS="foh")
    r = subprocess.run(["bash", str(_UPGRADE), "--fleet", "--dry-run", "--target", _PIN],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert re.search(r"^\s+resolume-pp\s+win\s+1\.8\.10\s+-> NEWER", r.stdout, re.M), r.stdout
    assert re.search(r"^\s+alex-nb\s+win\s+1\.8\.10\s+-> NEWER", r.stdout, re.M), r.stdout


def test_the_version_gate_still_pins_alex_nb_and_resolume_pp(tmp_path):
    v = {h: _PIN for h in _HOSTS}
    v["10.77.7.109"] = "1.8.10"
    b = _stub_bin(tmp_path, v)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = _env(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=_HOME, DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env"),
               DANTESYNC_VERSION_GATE_SSH_PASS="pw", DANTESYNC_TRAY_EXPECTED_SHA="a" * 64,
               DANTESYNC_NEWEST_RELEASE=_PIN, DANTESYNC_FOHABL_SSH_PASS="foh", DANTESYNC_FLEET_SCAN_CMD="/bin/true")
    for n in ("RESOLUME_SNV", "MBC", "FOHABL", "RESOLUME_PP", "ALEX_NB"):
        env[f"DANTESYNC_TRAY_SHA_{n}"] = "a" * 64
    r = subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet), "--fleet"],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 20, r.stdout + r.stderr
    pinned = r.stdout[:r.stdout.index("-- nodes dsync upgrades itself")]
    assert re.search(r"^\s+alex-nb\s+1\.8\.10\s+DRIFT", pinned, re.M), r.stdout
    assert re.search(rf"^\s+resolume-pp\s+{re.escape(_PIN)}\s+OK", pinned, re.M), r.stdout
