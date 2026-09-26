"""Issue 1372 part B -- the version gate and the fleet upgrader derive their nodes from the ONE fleet.

`dantesync-version-gate.sh --fleet` and `dantesync-fleet-upgrade.sh --fleet` add every node of
scripts/lib/dantesync-fleet.sh (every camera, dev1, the OBS boxes, the audio-VLAN PCs mbc + fohabl)
that the command line did not already name; the explicit --linux/--win/--local arms are unchanged
(byte-compatible). A node whose fleet row names its own ssh credential (fohabl, user `master`) is
reached with THAT variable's value, never the fleet default -- proven here with a PATH `sshpass`
stub that records the password each target was dialled with.

Tier-0: fixture seams + PATH stubs, no box, no network.
"""
import os
import pathlib
import stat
import re
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_GATE = _ROOT / "scripts" / "dantesync-version-gate.sh"
_UPGRADE = _ROOT / "scripts" / "dantesync-fleet-upgrade.sh"
# The ONE pin, read from the gate script itself so a fleet roll (a pin bump) never strands this test.
_PIN = re.search(r'DANTESYNC_VERSION_PIN="\$\{DANTESYNC_VERSION_PIN:-([0-9.]+)\}"', _GATE.read_text()).group(1)
_WIN = ("stream", "resolume", "mbc", "fohabl")


def _stub_bin(tmp_path, versions):
    """PATH stubs: `sshpass` records `<target> <password>` and answers `dantesync <v>` per target
    ip (versions: ip -> version); `dantesync` answers the local read; gh is absent (tray/lag UNKNOWN)."""
    b = tmp_path / "bin"
    b.mkdir()
    log = tmp_path / "sshpass.log"
    arms = "".join(f'    *@{ip}) echo "dantesync {v}" ;;\n' for ip, v in versions.items())
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\n"
        'pw="$2"; shift 2\n'
        'target=""\n'
        'for a in "$@"; do case "$a" in *@*) target="$a"; break ;; esac; done\n'
        f'printf "%s %s\\n" "$target" "$pw" >> "{log}"\n'
        'case "$target" in\n' + arms + "    *) exit 255 ;;\nesac\n")
    (b / "dantesync").write_text(f'#!/usr/bin/env bash\necho "dantesync {versions.get("local", _PIN)}"\n')
    for f in b.iterdir():
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return b, log


_IPS = {"cam1": "10.77.9.61", "cam2": "10.77.9.62", "cam3": "10.77.9.63", "cam4": "10.77.9.64",
        "cam5": "10.77.9.65", "cam6": "10.77.9.66", "cam7": "10.77.9.67", "strih-lx": "10.77.9.202",
        "stream": "10.77.9.204", "resolume": "resolume.lan", "mbc": "10.77.7.232", "fohabl": "10.77.7.30"}


def _env(tmp_path, bindir, **over):
    e = {k: v for k, v in os.environ.items()
         if not k.startswith(("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK", "WIN_SSH_USER"))}
    e.update({
        "PATH": f"{bindir}:/usr/bin:/bin",
        "OBS_FLEET_HOME": "strih-lx stream resolume",
        "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env"),
        "DANTESYNC_VERSION_GATE_SSH_PASS": "fleet-default",
        "DANTESYNC_TRAY_EXPECTED_SHA": "a" * 64,
        "DANTESYNC_NEWEST_RELEASE": _PIN,
        "DANTESYNC_FOHABL_SSH_PASS": "foh-secret",
    })
    for n in _WIN:
        e[f"DANTESYNC_TRAY_SHA_{n.upper()}"] = "a" * 64
    e.update(over)
    return e


def _gate(tmp_path, versions, *args, **env):
    bindir, log = _stub_bin(tmp_path, versions)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    r = subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet), *args],
                       capture_output=True, text=True, env=_env(tmp_path, bindir, **env))
    return r, (log.read_text() if log.exists() else "")


def _all_on_pin(**over):
    v = {ip: _PIN for ip in _IPS.values()}
    v.update(over)
    return v


def test_gate_fleet_reads_every_node_including_the_audio_vlan(tmp_path):
    r, _ = _gate(tmp_path, _all_on_pin(), "--fleet")
    assert r.returncode == 0, r.stdout + r.stderr
    for name in _IPS:
        assert f"  {name} " in r.stdout, (name, r.stdout)
    assert "  dev1 " in r.stdout
    assert "GATE PASS — 13 box(es)" in r.stdout


def test_gate_fleet_fohabl_is_dialled_with_its_own_credential(tmp_path):
    r, log = _gate(tmp_path, _all_on_pin(), "--fleet")
    assert r.returncode == 0, r.stdout + r.stderr
    dials = dict(line.split(" ", 1) for line in log.splitlines())
    assert dials["master@10.77.7.30"] == "foh-secret"
    assert dials["root@10.77.9.61"] == "fleet-default"
    assert dials["newlevel@10.77.7.232"] == "fleet-default"


def test_gate_fleet_missing_fohabl_credential_is_unknown_never_the_default(tmp_path):
    r, log = _gate(tmp_path, _all_on_pin(), "--fleet", DANTESYNC_FOHABL_SSH_PASS="")
    assert r.returncode == 11, r.stdout + r.stderr
    assert "fohabl" in r.stdout and "UNKNOWN" in r.stdout
    assert "master@10.77.7.30 fleet-default" not in log


def test_gate_fleet_a_stale_audio_node_drifts(tmp_path):
    r, _ = _gate(tmp_path, _all_on_pin(**{"10.77.7.232": "1.8.53"}), "--fleet")
    assert r.returncode == 20, r.stdout + r.stderr
    assert "mbc" in r.stdout and "1.8.53" in r.stdout and "DRIFT" in r.stdout


def test_gate_fleet_skips_an_away_traveling_box(tmp_path):
    r, log = _gate(tmp_path, _all_on_pin(), "--fleet", OBS_FLEET_HOME="strih-lx stream")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "resolume SKIPPED (traveling box away" in r.stdout
    assert "resolume.lan" not in log


def test_gate_fleet_never_duplicates_an_explicitly_named_node(tmp_path):
    r, log = _gate(tmp_path, _all_on_pin(), "--linux", "cam1=root@10.77.9.61", "--fleet")
    assert r.returncode == 0, r.stdout + r.stderr
    assert r.stdout.count("  cam1 ") == 1
    assert log.count("root@10.77.9.61 ") == 1


def test_gate_explicit_arms_are_unchanged_without_fleet(tmp_path):
    r, log = _gate(tmp_path, _all_on_pin(), "--linux", "cam3=root@10.77.9.63")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "GATE PASS — 1 box(es)" in r.stdout
    assert "mbc" not in r.stdout
    assert log.strip() == "root@10.77.9.63 fleet-default"


def test_upgrade_fleet_dry_run_covers_the_audio_vlan_with_their_credentials(tmp_path):
    bindir, log = _stub_bin(tmp_path, _all_on_pin(**{"10.77.7.232": "1.8.53"}))
    env = _env(tmp_path, bindir, SSH_PASS="fleet-default", CAMBOX_OFFLINE_ACK="")
    r = subprocess.run(["bash", str(_UPGRADE), "--fleet", "--dry-run", "--target", _PIN],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "mbc          win      1.8.53     -> NEWER" in r.stdout, r.stdout
    assert f"fohabl       win      {_PIN:<10} -> SAME" in r.stdout, r.stdout
    assert "dev1         local" in r.stdout
    assert "DRY-RUN: would upgrade" in r.stdout
    dials = dict(line.split(" ", 1) for line in log.read_text().splitlines())
    assert dials["master@10.77.7.30"] == "foh-secret"
    assert dials["newlevel@10.77.7.232"] == "fleet-default"


def test_upgrade_skips_a_node_whose_own_credential_is_missing(tmp_path):
    bindir, log = _stub_bin(tmp_path, _all_on_pin())
    env = _env(tmp_path, bindir, SSH_PASS="fleet-default", CAMBOX_OFFLINE_ACK="", DANTESYNC_FOHABL_SSH_PASS="")
    r = subprocess.run(["bash", str(_UPGRADE), "--fleet", "--dry-run", "--target", _PIN],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "fohabl SKIPPED (its ssh credential DANTESYNC_FOHABL_SSH_PASS is not set" in r.stdout
    assert "master@10.77.7.30" not in log.read_text()


def _source_upgrade(tmp_path, body, **env):
    script = tmp_path / "src.sh"
    script.write_text(f"set -euo pipefail\n. '{_UPGRADE}'\nset +e\n{body}\n")
    e = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "OBS_FLEET", "RIG_GRANDMASTER"))}
    e.update(env)
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=e)


def test_upgrade_verify_grades_an_audio_node_against_the_audio_grandmaster(tmp_path):
    """Review round 1: verify_node ran dantesync-gate.sh against the VIDEO grandmaster for mbc/fohabl
    too -- harmless only while GM enforcement is off. The gate env now carries the node's ROLE
    grandmaster for an audio node, and nothing for a video node (the gate keeps its own default)."""
    r = _source_upgrade(tmp_path, 'echo "A=[$(dantesync_gate_env_for mbc)]"; echo "V=[$(dantesync_gate_env_for cam1)]";'
                                  ' echo "U=[$(dantesync_gate_env_for nosuchbox)]"')
    assert "A=[RIG_GRANDMASTER_IP=10.77.7.104]" in r.stdout, r.stdout + r.stderr
    assert "V=[]" in r.stdout and "U=[]" in r.stdout
    src = _UPGRADE.read_text()
    assert src.count('"${gate_env[@]}" "$HERE/dantesync-gate.sh"') == 2


def test_upgrade_verify_env_fails_loudly_when_the_audio_grandmaster_does_not_resolve(tmp_path):
    """Review round 2: an unresolvable audio grandmaster (only via a hostname override) must fail
    the audio node's verify loudly -- never fall back to the video grandmaster silently."""
    r = _source_upgrade(tmp_path, 'dantesync_gate_env_for mbc; echo "rc=$?"',
                        DANTESYNC_AUDIO_GM_HOST="no-such-grandmaster.invalid")
    assert "rc=1" in r.stdout, r.stdout + r.stderr
    assert "cannot resolve the audio grandmaster" in r.stderr
    assert "gate_env_for" in _UPGRADE.read_text() and 'genv="$(dantesync_gate_env_for "$name")" || {' in _UPGRADE.read_text()


def test_upgrade_without_nodes_names_the_fleet_flag(tmp_path):
    bindir, _ = _stub_bin(tmp_path, {})
    r = subprocess.run(["bash", str(_UPGRADE), "--dry-run"], capture_output=True, text=True,
                       env=_env(tmp_path, bindir, CAMBOX_OFFLINE_ACK=""))
    assert r.returncode == 1
    assert "pass --fleet or --linux/--win/--local" in r.stderr
