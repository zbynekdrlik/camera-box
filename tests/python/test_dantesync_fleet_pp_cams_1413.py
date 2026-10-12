"""Issue 1413 -- the SNV rig shrank to cam2/cam3/cam5; the Poprad (PP) camboxes join the fleet.

Owner (verbatim, 12.10.2026): "v snv rigu uz je menej camboxov lebo sa ostatne preniesli do pp,
cize rig uz bude mat mensi pocet camboxov a imag nb ja odstaveny dlhsie ... imag nb sa pouziva len v
lete" and "chcem aby si sa staral ... aj o dantesync upgrady v pp sieti a tiez o obs".

Each venue's strih fact file (scripts/strih-boxes/<box>.env) says which cameras the venue HAS
(STRIH_CAMERAS) and how dev1 dials them (STRIH_CAMERA_DEV1_NET). scripts/lib/dantesync-fleet.sh
derives its camera rows from that, so:
  * an SNV camera (strih-lx.env: 2 3 5) is a plain `video` / `always` row at its camera_resolve
    address -- the clock watchdog's CAM arm;
  * a PP camera (strih-pp.env: 1 4 6 7) is a `video-pp` / `reach` row at 10.76.9.<octet> (dev1
    reaches the PP LAN through the companion-pp subnet router), locked to the PP grandmaster and
    SKIPPED while unreachable -- never a failed gate, never a page;
  * every camera is in exactly one venue; one listed twice fails loudly in both twins.

Tier-0: bash through `subprocess.run(["bash", <file>])`, probe seams stubbed, no box, no network
(the OBS_FLEET_HOME force list decides reachability).
"""
import importlib.util
import os
import pathlib
import re
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_PY = _ROOT / "scripts" / "dantesync_fleet.py"
_GATE = _ROOT / "scripts" / "dantesync-version-gate.sh"
_UPGRADE = _ROOT / "scripts" / "dantesync-fleet-upgrade.sh"
_WATCHDOG = _ROOT / "scripts" / "dantesync-clock-alert-watchdog.sh"
_BOXES = _ROOT / "scripts" / "strih-boxes"

_spec = importlib.util.spec_from_file_location("dantesync_fleet_pp_cams", _PY)
df = importlib.util.module_from_spec(_spec)
sys.modules["dantesync_fleet_pp_cams"] = df
_spec.loader.exec_module(df)

PP_GM = re.search(r"^STRIH_DANTE_GM=(.+)$", (_BOXES / "strih-pp.env").read_text(), re.MULTILINE).group(1)
_CLEAN_PREFIXES = ("DANTESYNC_", "OBS_FLEET", "RIG_GRANDMASTER", "CAMBOX_OFFLINE_ACK", "WIN_SSH_USER",
                   "DANTE_CLOCK_")
_PIN = re.search(r'DANTESYNC_VERSION_PIN="\$\{DANTESYNC_VERSION_PIN:-([0-9.]+)\}"', _GATE.read_text()).group(1)

SNV_CAMS = ["cam2", "cam3", "cam5"]
PP_CAMS = ["cam1", "cam4", "cam6", "cam7"]
PP_CAM_ROWS = [f"{c}|10.76.9.6{c[3:]}|linux|video-pp|reach|root|" for c in PP_CAMS]
SNV_CAM_ROWS = [f"{c}|10.77.9.6{c[3:]}|linux|video|always|root|" for c in SNV_CAMS]


def _env(**over):
    env = {k: v for k, v in os.environ.items() if not k.startswith(_CLEAN_PREFIXES)}
    env.update(over)
    return env


def _bash(tmp_path, body, **env):
    """Source the lib under the callers' strict mode and run BODY."""
    script = tmp_path / "run.sh"
    script.write_text(f"set -euo pipefail\n. '{_LIB}'\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def _with_env(monkeypatch, **env):
    for k in list(os.environ):
        if k.startswith(_CLEAN_PREFIXES):
            monkeypatch.delenv(k, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)


def _facts(path):
    return dict(ln.split("=", 1) for ln in path.read_text().splitlines() if ln and not ln.startswith("#"))


def _boxes_copy(tmp_path, edits):
    """A copy of the real strih-boxes dir with per-file (old, new) line edits."""
    d = tmp_path / "strih-boxes"
    d.mkdir()
    for f in _BOXES.glob("*.env"):
        body = f.read_text()
        for old, new in edits.get(f.name, []):
            assert body.count(old + "\n") == 1, (f.name, old)
            body = body.replace(old + "\n", new + "\n")
        (d / f.name).write_text(body)
    return d


# ---------------------------------------------------------------------------------------------
# the venue facts
# ---------------------------------------------------------------------------------------------

def test_the_venue_fact_files_split_the_fleet_snv_235_and_pp_1467():
    assert _facts(_BOXES / "strih-lx.env")["STRIH_CAMERAS"].split() == ["2", "3", "5"]
    assert _facts(_BOXES / "strih-lx.env")["STRIH_CAMERA_DEV1_NET"] == "direct"
    assert _facts(_BOXES / "strih-pp.env")["STRIH_CAMERAS"].split() == ["1", "4", "6", "7"]
    assert _facts(_BOXES / "strih-pp.env")["STRIH_CAMERA_DEV1_NET"] == "10.76.9"
    assert _facts(_BOXES / "stream-pp.env")["STRIH_CAMERA_DEV1_NET"] == "none"


# ---------------------------------------------------------------------------------------------
# the rows
# ---------------------------------------------------------------------------------------------

def test_every_camera_row_follows_its_venue(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    assert r.returncode == 0, r.stderr
    cams = [ln for ln in r.stdout.split() if ln.startswith("cam")]
    # camera_resolve order, each at its venue's address + role + gate
    assert cams == sorted(SNV_CAM_ROWS + PP_CAM_ROWS, key=lambda ln: int(ln.split("|")[0][3:]))


def test_the_python_twin_prints_the_same_camera_rows(tmp_path, monkeypatch):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    _with_env(monkeypatch)
    assert r.stdout.strip().split("\n") == df.rows()
    assert [x for x in df.camera_rows() if "|reach|" in x] == PP_CAM_ROWS


def test_the_cam_arm_is_the_home_venue_and_the_pp_cameras_are_reach_nodes(tmp_path):
    r = _bash(tmp_path, 'echo "C=$(dantesync_fleet_camera_names)"; echo "R=$(dantesync_fleet_names homegate=reach)";'
                        ' echo "F=$(dantesync_fleet_fixed_names)"')
    out = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    assert out["C"].split() == SNV_CAMS
    assert out["R"].split()[:4] == PP_CAMS
    assert out["F"] == "mbc fohabl"  # a camera is never a fixed node, whatever its venue


def test_a_pp_camera_locks_to_the_pp_grandmaster(tmp_path, monkeypatch):
    r = _bash(tmp_path, 'dantesync_fleet_role_gm_host "$(dantesync_fleet_field cam7 role)"')
    assert r.stdout.strip() == PP_GM, r.stderr
    _with_env(monkeypatch)
    assert df.role_gm_host("video-pp") == PP_GM


def test_a_camera_moved_between_venues_is_one_edit_per_fact_file(tmp_path, monkeypatch):
    d = _boxes_copy(tmp_path, {"strih-lx.env": [("STRIH_CAMERAS=2 3 5", "STRIH_CAMERAS=2 3 5 7")],
                               "strih-pp.env": [("STRIH_CAMERAS=1 4 6 7", "STRIH_CAMERAS=1 4 6")]})
    r = _bash(tmp_path, "dantesync_fleet_rows", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert r.returncode == 0, r.stderr
    assert "cam7|10.77.9.67|linux|video|always|root|" in r.stdout.split()
    _with_env(monkeypatch, DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert r.stdout.strip().split("\n") == df.rows()


def test_a_camera_listed_by_two_venues_fails_loudly_in_both(tmp_path, monkeypatch):
    d = _boxes_copy(tmp_path, {"strih-lx.env": [("STRIH_CAMERAS=2 3 5", "STRIH_CAMERAS=2 3 5 7")]})
    r = _bash(tmp_path, "dantesync_fleet_rows || echo RC=$?", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert "RC=1" in r.stdout and "cam7 is listed by two venue fact files" in r.stderr, r.stderr
    assert "cam7|" not in r.stdout  # never a guessed row
    _with_env(monkeypatch, DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    with pytest.raises(RuntimeError, match="cam7 is listed by two venue fact files"):
        df.rows()


@pytest.mark.parametrize("net", ["10.76", "none", "10.76.9.0", "ten.76.9"])
def test_a_bad_dev1_camera_network_fails_loudly_in_both(tmp_path, monkeypatch, net):
    d = _boxes_copy(tmp_path, {"strih-pp.env": [("STRIH_CAMERA_DEV1_NET=10.76.9", f"STRIH_CAMERA_DEV1_NET={net}")]})
    r = _bash(tmp_path, "dantesync_fleet_rows || echo RC=$?", DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert "RC=1" in r.stdout and "STRIH_CAMERA_DEV1_NET" in r.stderr, r.stderr
    assert not any(ln.startswith(("cam1|", "cam7|")) for ln in r.stdout.split())
    _with_env(monkeypatch, DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    with pytest.raises(RuntimeError, match="STRIH_CAMERA_DEV1_NET"):
        df.rows()


def test_the_camera_rows_survive_a_callers_noglob(tmp_path):
    """The version gate builds its specs under `set -f`; the fact-file glob must still expand
    (the first cut of this change silently dropped every camera from the gated set)."""
    r = _bash(tmp_path, 'set -f\necho "L=$(dantesync_fleet_spec linux op)"\necho "G=$(dantesync_fleet_venue_gm_host pp)"',
              OBS_FLEET_HOME="")
    out = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    assert "cam2=root@10.77.9.62" in out["L"].split() and "cam7=root@10.76.9.67" in out["L"].split(), r.stderr
    assert out["G"] == PP_GM


# ---------------------------------------------------------------------------------------------
# the consumers
# ---------------------------------------------------------------------------------------------

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


_HOSTS = (["10.77.9.6%d" % n for n in (2, 3, 5)] + ["10.76.9.6%d" % n for n in (1, 4, 6, 7)]
          + ["10.77.9.202", "10.77.9.204", "resolume.lan", "10.77.7.232", "10.77.7.30",
             "strih-pp.lan", "stream-pp.lan", "resolume-pp.lan"])


def _gate(tmp_path, versions, home):
    b = _stub_bin(tmp_path, versions)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = _env(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=home, DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env"),
               DANTESYNC_VERSION_GATE_SSH_PASS="pw", DANTESYNC_TRAY_EXPECTED_SHA="a" * 64,
               DANTESYNC_NEWEST_RELEASE=_PIN, DANTESYNC_FOHABL_SSH_PASS="foh")
    for n in ("STREAM", "RESOLUME", "MBC", "FOHABL", "RESOLUME_PP"):
        env[f"DANTESYNC_TRAY_SHA_{n}"] = "a" * 64
    return subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet), "--fleet"],
                          capture_output=True, text=True, env=env)


def test_version_gate_reads_a_reachable_pp_camera_at_its_pp_address(tmp_path):
    r = _gate(tmp_path, {h: _PIN for h in _HOSTS}, "strih-lx stream resolume cam7")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "  cam7 " in r.stdout and "  cam7 SKIPPED" not in r.stdout, r.stdout
    for name in ("cam1", "cam4", "cam6"):
        assert f"  {name} SKIPPED (unreachable" in r.stdout, r.stdout
    for name in SNV_CAMS:
        assert f"  {name} " in r.stdout, r.stdout
    dials = (tmp_path / "dials.log").read_text().split()
    assert "root@10.76.9.67" in dials
    assert not any(d.endswith(("@10.77.9.61", "@10.77.9.64", "@10.77.9.66", "@10.77.9.67")) for d in dials)
    assert "GATE PASS — 10 box(es)" in r.stdout  # cam2 cam3 cam5 cam7 + dev1 + 5 SNV boxes


def test_version_gate_drifts_on_a_stale_pp_camera(tmp_path):
    v = {h: _PIN for h in _HOSTS}
    v["10.76.9.67"] = "1.8.10"
    r = _gate(tmp_path, v, "strih-lx stream resolume cam7")
    assert r.returncode == 20, r.stdout + r.stderr
    assert "cam7" in r.stdout and "1.8.10" in r.stdout


def test_version_gate_never_fails_on_the_pp_cameras_while_they_are_away(tmp_path):
    r = _gate(tmp_path, {h: _PIN for h in _HOSTS}, "strih-lx stream resolume")
    assert r.returncode == 0, r.stdout + r.stderr
    for name in PP_CAMS:
        assert f"  {name} SKIPPED (unreachable" in r.stdout, r.stdout
    assert "10.76.9." not in (tmp_path / "dials.log").read_text()


def _source_upgrade(tmp_path, body, **env):
    script = tmp_path / "src.sh"
    script.write_text(f"set -euo pipefail\n. '{_UPGRADE}'\nset +e\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def test_upgrade_verifies_a_pp_camera_against_the_pp_grandmaster_and_skips_it_while_away(tmp_path):
    r = _source_upgrade(tmp_path, 'echo "P=[$(dantesync_gate_env_for cam7)]"; echo "S=[$(dantesync_gate_env_for cam3)]";'
                                  ' for n in cam1 cam7 cam3; do dantesync_skip_away_traveling "$n" && echo "$n=skip" || echo "$n=roll"; done',
                        OBS_FLEET_HOME="strih-lx stream resolume cam7")
    assert f"P=[RIG_GRANDMASTER_IP={PP_GM}]" in r.stdout and "S=[]" in r.stdout, r.stdout + r.stderr
    assert "cam1=skip" in r.stdout and "cam7=roll" in r.stdout and "cam3=roll" in r.stdout


def _status(gm, now):
    return ("{" + f'"offset_ns":1,"settled":true,"updated_ts":{now},"is_locked":true,'
            f'"mode":"LOCK","gm_source_ip":"{gm}","ntp_step_storm":false,"ntp_steps_last_hour":null' + "}")


def _watchdog(tmp_path, by_host, home, **over):
    now = 1790349506
    fetch = tmp_path / "fetch.sh"
    arms = "".join(f"  {h}) cat <<'JSON'\n{_status(gm, now)}\nJSON\n  ;;\n" for h, gm in by_host.items())
    fetch.write_text(f'#!/usr/bin/env bash\necho "$1" >> "{tmp_path}/fetched.log"\ncase "$1" in\n{arms}  *) exit 1 ;;\nesac\n')
    one = tmp_path / "one.sh"
    one.write_text("#!/usr/bin/env bash\nprintf 1\n")
    for f in (fetch, one):
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    e = _env(DANTE_CLOCK_LOCAL_NODES="", DANTE_CLOCK_OBS_NODES="", DANTE_CLOCK_FIXED_NODES="",
             DANTE_CLOCK_FETCH_CMD=str(fetch), DANTE_CLOCK_BOX_UP_CMD=str(one), DANTE_CLOCK_MGMT_SSH_CMD=str(one),
             RIG_GRANDMASTER_IP="10.77.9.230", DANTE_CLOCK_CONFIRM_THRESHOLD="1", DANTE_CLOCK_NOW=str(now),
             DANTE_CLOCK_VERSION_PIN=_PIN, DANTE_CLOCK_ALERT_STATE_DIR=str(tmp_path), OBS_FLEET_HOME=home)
    e.update(over)
    r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stderr


def test_watchdog_rosters_split_the_cameras_by_venue():
    script = f'set -uo pipefail\n. "{_WATCHDOG}"\nprintf "%s|%s" "$DANTE_CLOCK_CAM_NODES" "$DANTE_CLOCK_REACH_NODES"\n'
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=_env())
    assert r.returncode == 0, r.stderr
    cam, reach = r.stdout.split("|")
    assert cam.split() == SNV_CAMS
    assert reach.split()[:4] == PP_CAMS and not set(cam.split()) & set(reach.split())


def test_watchdog_grades_a_pp_camera_at_its_pp_address_against_the_pp_grandmaster(tmp_path):
    hosts = {f"10.77.9.6{c[3:]}": "10.77.9.230" for c in SNV_CAMS}
    hosts["10.76.9.67"] = PP_GM
    err = _watchdog(tmp_path, hosts, "cam7")
    for c in SNV_CAMS:
        assert f"{c} (10.77.9.6{c[3:]}): reachable=1 verdict=OK" in err, err
    assert "cam7 (10.76.9.67): reachable=1 verdict=OK" in err, err
    for name in ("cam1", "cam4", "cam6", "strih-pp"):
        assert f"{name} away (unreachable" in err, err
    fetched = (tmp_path / "fetched.log").read_text().split()
    assert "10.76.9.67" in fetched and "10.77.9.67" not in fetched
    assert "WOULD alert" not in err, err


def test_watchdog_pages_a_pp_camera_on_the_snv_grandmaster_with_the_pp_remedy(tmp_path):
    err = _watchdog(tmp_path, {"10.76.9.67": "10.77.9.230"}, "cam7", DANTE_CLOCK_CAM_NODES="")
    assert "cam7 (10.76.9.67): reachable=1 verdict=NO_CLOCK reason=wrong_gm" in err, err
    assert "WOULD alert" in err and "dante-clock-cam7-" in err
    assert f"grandmaster miesta pp ({PP_GM})" in err, err
