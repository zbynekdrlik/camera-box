"""Issue 1412 -- the Poprad (PP) boxes join the ONE declared dantesync fleet.

Owner (verbatim, 11.10.2026): "a tiez tam nie je dantesync tam nie je updatnuty cize potrebujes
zaradit tento pc do updatovania". resolume-pp ran dantesync 1.16.0 but no consumer knew it existed,
and neither did strih-pp (the PP NTP master) or stream-pp. They are now rows of
scripts/lib/dantesync-fleet.sh, so the version gate, the fleet upgrader, the clock watchdog and the
config drift check cover them.

Two rules this file pins:
  * GM per venue: a PP node locks to the PP PTP grandmaster (10.77.8.110), never video-clock.lan
    (SNV). Its role is `<base>-pp` (video-pp, ntp-master-pp), and the grandmaster of a venue role
    is DERIVED from the venue's NTP-master strih fact file (scripts/strih-boxes/*.env:
    STRIH_VENUE=pp + STRIH_DANTESYNC_ROLE=server -> STRIH_DANTE_GM), never a second literal.
  * The reach gate: the PP boxes are reachable from dev1 only while the companion-pp subnet route
    is up, so their homegate is `reach` -- present iff the name resolves AND ssh :22 answers. An
    unreachable PP node is SKIPPED by every consumer, never a page, never a failed gate. The
    OBS_FLEET_HOME force list (the one traveling-box test/ops seam) is authoritative for it too.

Tier-0: bash through `subprocess.run(["bash", <file>])`, probe seams stubbed as bash functions or
PATH stubs, no box and no network.
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
_DRIFT_SH = _ROOT / "scripts" / "dantesync-config-drift.sh"
_FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "dantesync_config_1372"
_STRIH_PP_ENV = _ROOT / "scripts" / "strih-boxes" / "strih-pp.env"

_spec = importlib.util.spec_from_file_location("dantesync_fleet_pp", _PY)
df = importlib.util.module_from_spec(_spec)
sys.modules["dantesync_fleet_pp"] = df
_spec.loader.exec_module(df)

PP_GM = re.search(r"^STRIH_DANTE_GM=(.+)$", _STRIH_PP_ENV.read_text(), re.MULTILINE).group(1)
PP_HOME = "strih-snv stream-snv resolume-snv strih-pp stream-pp resolume-pp"
_CLEAN_PREFIXES = ("DANTESYNC_", "OBS_FLEET", "RIG_GRANDMASTER", "CAMBOX_OFFLINE_ACK", "WIN_SSH_USER",
                   "DANTE_CLOCK_")


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


def _boxes_dir(tmp_path, files):
    d = tmp_path / "strih-boxes"
    d.mkdir()
    for name, text in files.items():
        (d / name).write_text(text)
    return d


# ---------------------------------------------------------------------------------------------
# the rows
# ---------------------------------------------------------------------------------------------

def test_the_pp_boxes_are_fleet_rows(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    assert r.returncode == 0, r.stderr
    rows = {ln.split("|")[0]: ln for ln in r.stdout.split()}
    # strih-pp + stream-pp take their address from obs-fleet.sh (the NAME dev1 dials through the
    # companion-pp subnet router), resolume-pp is no obs-fleet box, so it carries its own name.
    assert rows["strih-pp"] == "strih-pp|strih-pp.lan|linux|ntp-master-pp|reach|-|"
    assert rows["stream-pp"] == "stream-pp|stream-pp.lan|linux|video-pp|reach|-|"
    assert rows["resolume-pp"] == "resolume-pp|resolume-pp.lan|windows|video-pp|reach|-|"


def test_the_pp_rows_reach_the_python_twin_byte_identically(tmp_path, monkeypatch):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    _with_env(monkeypatch)
    assert r.stdout.strip().split("\n") == df.rows()
    assert [x for x in df.rows() if x.split("|")[0].endswith("-pp")] == [
        "strih-pp|strih-pp.lan|linux|ntp-master-pp|reach|-|",
        "stream-pp|stream-pp.lan|linux|video-pp|reach|-|",
        "resolume-pp|resolume-pp.lan|windows|video-pp|reach|-|",
    ]


def test_strih_pp_and_stream_pp_read_their_address_from_obs_fleet(tmp_path):
    """No second literal: a renamed obs-fleet host moves the dantesync row with it."""
    obs = ("strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always\n"
           "imag|10.77.9.182|linux-genlock|retired\nresolume-snv|resolume.lan|windows-genlock|traveling\n"
           "strih-pp|strih-pp.example|linux-genlock|traveling\nstream-pp|10.1.2.3|linux-genlock|traveling")
    r = _bash(tmp_path, 'echo "S=$(dantesync_fleet_field strih-pp addr)"; echo "T=$(dantesync_fleet_field stream-pp addr)"',
              OBS_FLEET=obs)
    assert "S=strih-pp.example" in r.stdout and "T=10.1.2.3" in r.stdout, r.stdout + r.stderr


# ---------------------------------------------------------------------------------------------
# the grandmaster per venue
# ---------------------------------------------------------------------------------------------

def test_the_pp_grandmaster_comes_from_the_strih_pp_fact_file(tmp_path, monkeypatch):
    assert PP_GM == "10.77.8.110"
    r = _bash(tmp_path, 'echo "V=$(dantesync_fleet_role_gm_host video-pp)"; echo "M=$(dantesync_fleet_role_gm_host ntp-master-pp)";'
                        ' echo "I=$(dantesync_fleet_role_gm_ip video-pp)"; echo "S=$(dantesync_fleet_role_gm_host video)"')
    assert r.returncode == 0, r.stderr
    assert f"V={PP_GM}" in r.stdout and f"M={PP_GM}" in r.stdout and f"I={PP_GM}" in r.stdout
    assert "S=video-clock.lan" in r.stdout  # the SNV video roles keep the rig grandmaster
    _with_env(monkeypatch)
    assert df.role_gm_host("video-pp") == PP_GM
    assert df.role_gm_host("ntp-master-pp") == PP_GM
    assert df.role_gm_host("video") == "video-clock.lan"


def test_the_pp_grandmaster_is_never_a_second_literal():
    for path in (_LIB, _PY):
        assert PP_GM not in path.read_text(), path
    assert PP_GM not in (_ROOT / "scripts" / "dantesync-canonical-config.json").read_text()


def test_the_rig_grandmaster_override_never_moves_a_venue_grandmaster(tmp_path, monkeypatch):
    r = _bash(tmp_path, 'echo "I=$(dantesync_fleet_role_gm_ip video-pp)"', RIG_GRANDMASTER_IP="10.77.9.230",
              RIG_GRANDMASTER_HOST="clock2.lan")
    assert f"I={PP_GM}" in r.stdout, r.stdout + r.stderr
    _with_env(monkeypatch, RIG_GRANDMASTER_HOST="clock2.lan")
    assert df.role_gm_host("video-pp") == PP_GM


def test_a_venue_grandmaster_follows_its_fact_file(tmp_path, monkeypatch):
    d = _boxes_dir(tmp_path, {
        "a.env": "# master\nSTRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=server\nSTRIH_DANTE_GM=10.9.9.9\n",
        "b.env": "STRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=client\nSTRIH_DANTE_GM=10.9.9.8\n",
        "c.env": "STRIH_VENUE=yy\nSTRIH_DANTESYNC_ROLE=server\nSTRIH_DANTE_GM=yy-clock.lan\n",
    })
    r = _bash(tmp_path, 'echo "Z=$(dantesync_fleet_role_gm_host video-zz)"; echo "Y=$(dantesync_fleet_role_gm_host ntp-master-yy)"',
              DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert "Z=10.9.9.9" in r.stdout and "Y=yy-clock.lan" in r.stdout, r.stdout + r.stderr
    _with_env(monkeypatch, DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert df.role_gm_host("video-zz") == "10.9.9.9"
    assert df.role_gm_host("ntp-master-yy") == "yy-clock.lan"


@pytest.mark.parametrize("files,why", [
    ({"a.env": "STRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=client\nSTRIH_DANTE_GM=10.9.9.8\n"}, "no NTP-master"),
    ({"a.env": "STRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=server\nSTRIH_DANTE_GM=10.9.9.9\n",
      "b.env": "STRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=server\nSTRIH_DANTE_GM=10.9.9.7\n"}, "2 NTP-master"),
    ({"a.env": "STRIH_VENUE=zz\nSTRIH_DANTESYNC_ROLE=server\n"}, "no STRIH_DANTE_GM"),
])
def test_a_venue_without_exactly_one_master_grandmaster_fails_loudly(tmp_path, monkeypatch, files, why):
    d = _boxes_dir(tmp_path, files)
    r = _bash(tmp_path, 'dantesync_fleet_role_gm_host video-zz || echo "RC=$?"', DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert "RC=1" in r.stdout, r.stdout + r.stderr
    assert why in r.stderr and "zz" in r.stderr
    r = _bash(tmp_path, 'dantesync_fleet_role_gm_ip video-zz || echo "RC=$?"', DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    assert "RC=1" in r.stdout and r.stdout.strip() == "RC=1", r.stdout
    _with_env(monkeypatch, DANTESYNC_FLEET_STRIH_BOXES_DIR=str(d))
    with pytest.raises(ValueError, match=why):
        df.role_gm_host("video-zz")


@pytest.mark.parametrize("role", ["audio-pp", "video-", "videopp", "video-PP", "bogus"])
def test_an_unknown_role_fails_in_both_twins(tmp_path, monkeypatch, role):
    r = _bash(tmp_path, f'dantesync_fleet_role_gm_host "{role}" || echo "RC=$?"')
    assert r.stdout.strip() == "RC=1", r.stdout
    _with_env(monkeypatch)
    with pytest.raises(ValueError):
        df.role_gm_host(role)


def test_the_pp_template_allows_only_the_pp_grandmaster(monkeypatch):
    _with_env(monkeypatch)
    assert {"video-pp", "ntp-master-pp"} <= set(df.ROLES)
    assert df.template_for("video-pp")["system"]["gm_allowlist"] == [PP_GM]
    assert df.template_for("ntp-master-pp")["system"]["gm_allowlist"] == [PP_GM]
    assert df.template_for("ntp-master-pp")["ntp_server_mode"]["enabled"] is True
    assert df.template_for("video-pp")["ntp_server_mode"]["enabled"] is False
    # an SNV-shaped config (gm video-clock.lan) is drift on a PP node
    verdict, diffs = df.drift((_FIX / "cam1.json").read_bytes(), "video-pp")
    assert verdict == df.DRIFT
    assert f'system.gm_allowlist: ["video-clock.lan"] (canonical ["{PP_GM}"])' in diffs


# ---------------------------------------------------------------------------------------------
# the reach gate
# ---------------------------------------------------------------------------------------------

_PROBE = ('obs_fleet_resolve_host() {{ printf "%s" "{ip}"; }}\n'
          'obs_fleet_status_probe() {{ echo "probe $1 $2" >&2; printf "{up}"; }}\n')


def test_a_reach_node_is_present_iff_its_ssh_port_answers(tmp_path):
    up = _bash(tmp_path, _PROBE.format(ip="10.76.8.201", up=1) + 'dantesync_fleet_present resolume-pp && echo PRESENT')
    assert "PRESENT" in up.stdout, up.stdout + up.stderr
    assert "probe 10.76.8.201 22" in up.stderr
    down = _bash(tmp_path, _PROBE.format(ip="10.76.8.201", up=0) + 'dantesync_fleet_present resolume-pp || echo AWAY')
    assert "AWAY" in down.stdout, down.stdout + down.stderr


def test_an_unresolvable_reach_node_is_away_without_a_probe(tmp_path):
    r = _bash(tmp_path, _PROBE.format(ip="", up=1) + 'dantesync_fleet_present strih-pp || echo AWAY')
    assert "AWAY" in r.stdout, r.stdout + r.stderr
    assert "probe" not in r.stderr


def test_the_force_list_is_authoritative_for_a_reach_node(tmp_path):
    body = _PROBE.format(ip="10.76.8.202", up=1) + (
        'dantesync_fleet_present strih-pp && echo S_HOME || echo S_AWAY\n'
        'dantesync_fleet_present stream-pp && echo T_HOME || echo T_AWAY\n')
    r = _bash(tmp_path, body, OBS_FLEET_HOME="strih-pp")
    assert "S_HOME" in r.stdout and "T_AWAY" in r.stdout, r.stdout
    assert "probe" not in r.stderr  # the force list never touches the network


def test_present_drops_an_unreachable_pp_node_from_every_arm(tmp_path):
    body = ('echo "L=$(dantesync_fleet_spec linux op --present)"\n'
            'echo "W=$(dantesync_fleet_spec win op --present)"\n'
            'echo "AL=$(dantesync_fleet_spec linux op)"\n'
            'echo "AW=$(dantesync_fleet_spec win op)"\n')
    r = _bash(tmp_path, body, OBS_FLEET_HOME="strih-snv stream-snv resolume-snv stream-pp")
    out = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    assert "stream-pp=op@stream-pp.lan" in out["L"].split()
    assert "strih-pp=op@strih-pp.lan" not in out["L"].split()
    assert "resolume-pp" not in out["W"]
    assert out["AL"].split()[-2:] == ["strih-pp=op@strih-pp.lan", "stream-pp=op@stream-pp.lan"]
    assert out["AW"].split()[-1] == "resolume-pp=op@resolume-pp.lan"


def test_the_pp_nodes_are_never_fixed_or_obsfleet_nodes(tmp_path):
    r = _bash(tmp_path, 'echo "F=$(dantesync_fleet_fixed_names)"; echo "O=$(dantesync_fleet_names homegate=obsfleet)";'
                        ' echo "R=$(dantesync_fleet_names homegate=reach)"; echo "M=$(dantesync_fleet_master_names)"')
    out = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    assert out["F"] == "mbc fohabl"
    assert out["O"] == "strih-snv stream-snv resolume-snv"
    # issue 1413: the Poprad camboxes are `reach` rows too (test_dantesync_fleet_pp_cams_1413.py)
    assert out["R"] == "cam1 cam4 cam6 cam7 strih-pp stream-pp resolume-pp"
    assert out["M"] == "strih-snv strih-pp"


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
    for f in b.iterdir():
        f.chmod(f.stat().st_mode | stat.S_IEXEC)
    return b


_PIN = re.search(r'DANTESYNC_VERSION_PIN="\$\{DANTESYNC_VERSION_PIN:-([0-9.]+)\}"', _GATE.read_text()).group(1)
_HOSTS = ["10.77.9.6%d" % n for n in range(1, 8)] + ["10.77.9.202", "10.77.9.204", "resolume.lan", "10.77.7.232",
                                                   "10.77.7.30", "strih-pp.lan", "stream-pp.lan", "resolume-pp.lan"]


def _gate(tmp_path, versions, home):
    b = _stub_bin(tmp_path, versions)
    (b / "dantesync").write_text(f'#!/usr/bin/env bash\necho "dantesync {_PIN}"\n')
    (b / "dantesync").chmod(0o755)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = _env(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=home, DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env"),
               DANTESYNC_VERSION_GATE_SSH_PASS="pw", DANTESYNC_TRAY_EXPECTED_SHA="a" * 64,
               DANTESYNC_NEWEST_RELEASE=_PIN, DANTESYNC_FOHABL_SSH_PASS="foh")
    for n in ("STREAM", "RESOLUME", "MBC", "FOHABL", "RESOLUME_PP"):
        env[f"DANTESYNC_TRAY_SHA_{n}"] = "a" * 64
    return subprocess.run(["bash", str(_GATE), "--pin", _PIN, "--fleet-file", str(fleet), "--fleet"],
                          capture_output=True, text=True, env=env)


def test_version_gate_fleet_reads_the_pp_boxes_while_they_are_reachable(tmp_path):
    r = _gate(tmp_path, {h: _PIN for h in _HOSTS}, PP_HOME)
    assert r.returncode == 0, r.stdout + r.stderr
    for name in ("strih-pp", "stream-pp", "resolume-pp"):
        assert f"  {name} " in r.stdout, (name, r.stdout)
    # issue 1413: 3 SNV cameras + dev1 + 5 SNV boxes + the 3 PP boxes; the PP cameras are away here
    # (PP_HOME names only the PP boxes).
    assert "GATE PASS — 12 box(es)" in r.stdout
    dials = (tmp_path / "dials.log").read_text().split()
    assert {"newlevel@strih-pp.lan", "newlevel@stream-pp.lan", "newlevel@resolume-pp.lan"} <= set(dials)


def test_version_gate_fleet_a_stale_pp_box_drifts(tmp_path):
    v = {h: _PIN for h in _HOSTS}
    v["resolume-pp.lan"] = "1.8.10"
    r = _gate(tmp_path, v, PP_HOME)
    assert r.returncode == 20, r.stdout + r.stderr
    assert "resolume-pp" in r.stdout and "1.8.10" in r.stdout


def test_version_gate_fleet_skips_unreachable_pp_boxes_and_still_passes(tmp_path):
    r = _gate(tmp_path, {h: _PIN for h in _HOSTS}, "strih-snv stream-snv resolume-snv")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "GATE PASS — 9 box(es)" in r.stdout
    for name in ("strih-pp", "stream-pp", "resolume-pp"):
        assert f"  {name} SKIPPED (unreachable" in r.stdout, r.stdout
    assert "-pp.lan" not in (tmp_path / "dials.log").read_text()


def _source_upgrade(tmp_path, body, **env):
    script = tmp_path / "src.sh"
    script.write_text(f"set -euo pipefail\n. '{_UPGRADE}'\nset +e\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def test_upgrade_verifies_a_pp_node_against_the_pp_grandmaster(tmp_path):
    r = _source_upgrade(tmp_path, 'echo "S=[$(dantesync_gate_env_for stream-pp)]"; echo "R=[$(dantesync_gate_env_for resolume-pp)]";'
                                  ' echo "M=[$(dantesync_gate_env_for strih-pp)]"; echo "V=[$(dantesync_gate_env_for cam3)]";'
                                  ' echo "A=[$(dantesync_gate_env_for mbc)]"')
    assert f"S=[RIG_GRANDMASTER_IP={PP_GM}]" in r.stdout, r.stdout + r.stderr
    assert f"R=[RIG_GRANDMASTER_IP={PP_GM}]" in r.stdout and f"M=[RIG_GRANDMASTER_IP={PP_GM}]" in r.stdout
    assert "V=[]" in r.stdout and "A=[RIG_GRANDMASTER_IP=10.77.7.104]" in r.stdout


def test_upgrade_verifies_every_venue_master_master_aware(tmp_path):
    r = _source_upgrade(tmp_path, 'm="$(dantesync_upgrade_default_master "")"; echo "D=[$m]";'
                                  ' for n in strih-snv strih-pp stream-pp; do'
                                  ' dantesync_is_ntp_master "$n" "$m" && echo "$n=master" || echo "$n=slave"; done;'
                                  ' echo "R=[$(dantesync_date_role strih-pp "$m")]"')
    assert "D=[strih-snv strih-pp]" in r.stdout, r.stdout + r.stderr
    assert "strih-snv=master" in r.stdout and "strih-pp=master" in r.stdout and "stream-pp=slave" in r.stdout
    assert "R=[ntp-master]" in r.stdout


def test_upgrade_skips_an_unreachable_pp_node_and_rolls_a_reachable_one(tmp_path):
    body = ('for n in strih-pp resolume-pp; do dantesync_skip_away_traveling "$n" && echo "$n=skip" || echo "$n=roll"; done')
    away = _source_upgrade(tmp_path, body, OBS_FLEET_HOME="strih-snv stream-snv resolume-snv")
    assert "strih-pp=skip" in away.stdout and "resolume-pp=skip" in away.stdout, away.stdout + away.stderr
    home = _source_upgrade(tmp_path, body, OBS_FLEET_HOME=PP_HOME)
    assert "strih-pp=roll" in home.stdout and "resolume-pp=roll" in home.stdout, home.stdout + home.stderr


def test_upgrade_fleet_dry_run_lists_the_reachable_pp_boxes(tmp_path):
    v = {h: _PIN for h in _HOSTS}
    v["resolume-pp.lan"] = "1.8.10"
    b = _stub_bin(tmp_path, v)
    (b / "dantesync").write_text(f'#!/usr/bin/env bash\necho "dantesync {_PIN}"\n')
    (b / "dantesync").chmod(0o755)
    env = _env(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=PP_HOME, DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env"),
               SSH_PASS="pw", CAMBOX_OFFLINE_ACK="", DANTESYNC_FOHABL_SSH_PASS="foh")
    r = subprocess.run(["bash", str(_UPGRADE), "--fleet", "--dry-run", "--target", _PIN],
                       capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "resolume-pp  win      1.8.10     -> NEWER" in r.stdout, r.stdout
    assert f"strih-pp     linux    {_PIN:<10} -> SAME" in r.stdout, r.stdout
    assert f"stream-pp    linux    {_PIN:<10} -> SAME" in r.stdout, r.stdout


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
    e = _env(DANTE_CLOCK_LOCAL_NODES="", DANTE_CLOCK_CAM_NODES="", DANTE_CLOCK_OBS_NODES="",
             DANTE_CLOCK_FIXED_NODES="", DANTE_CLOCK_FETCH_CMD=str(fetch), DANTE_CLOCK_BOX_UP_CMD=str(one),
             DANTE_CLOCK_MGMT_SSH_CMD=str(one), RIG_GRANDMASTER_IP="10.77.9.230",
             DANTE_CLOCK_CONFIRM_THRESHOLD="1", DANTE_CLOCK_NOW=str(now), DANTE_CLOCK_VERSION_PIN="1.16.0",
             DANTE_CLOCK_ALERT_STATE_DIR=str(tmp_path), OBS_FLEET_HOME=home)
    e.update(over)
    r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stderr


def test_watchdog_reach_roster_default_is_the_pp_fleet():
    script = f'set -uo pipefail\n. "{_WATCHDOG}"\nprintf "%s" "$DANTE_CLOCK_REACH_NODES"\n'
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=_env())
    assert r.returncode == 0, r.stderr
    assert r.stdout == "cam1 cam4 cam6 cam7 strih-pp stream-pp resolume-pp"  # issue 1413: + the PP camboxes


def test_watchdog_grades_the_pp_nodes_against_the_pp_grandmaster(tmp_path):
    err = _watchdog(tmp_path, {"strih-pp.lan": PP_GM, "stream-pp.lan": PP_GM, "resolume-pp.lan": PP_GM}, PP_HOME)
    assert "reach='cam1 cam4 cam6 cam7 strih-pp stream-pp resolume-pp'" in err, err  # issue 1413
    for name in ("strih-pp", "stream-pp", "resolume-pp"):
        assert f"{name} ({name}.lan): reachable=1 verdict=OK" in err, err
    assert "WOULD alert" not in err, err


def test_watchdog_pages_a_pp_node_locked_to_a_foreign_grandmaster_with_the_pp_remedy(tmp_path):
    err = _watchdog(tmp_path, {"strih-pp.lan": PP_GM, "stream-pp.lan": "10.77.9.230",
                               "resolume-pp.lan": PP_GM}, PP_HOME)
    assert "stream-pp (stream-pp.lan): reachable=1 verdict=NO_CLOCK reason=wrong_gm" in err, err
    assert "WOULD alert" in err and "dante-clock-stream-pp-" in err
    assert f"grandmaster miesta pp ({PP_GM})" in err, err


def test_watchdog_skips_an_unreachable_pp_node_without_a_fetch(tmp_path):
    err = _watchdog(tmp_path, {"strih-pp.lan": PP_GM}, "strih-pp")
    assert "strih-pp (strih-pp.lan): reachable=1 verdict=OK" in err, err
    for name in ("stream-pp", "resolume-pp"):
        assert f"{name} away (unreachable" in err, err
        assert f"{name} ({name}.lan): " not in err
    fetched = (tmp_path / "fetched.log").read_text().split()
    assert fetched == ["strih-pp.lan"]
    assert "WOULD alert" not in err


def test_drift_check_skips_an_unreachable_pp_node(tmp_path):
    stub = tmp_path / "fetch.sh"
    stub.write_text(f'#!/usr/bin/env bash\ncp "{_FIX / "cam1.json"}" "$4"\n')
    stub.chmod(0o755)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    r = subprocess.run(["bash", str(_DRIFT_SH), "--fleet-file", str(fleet), "--only", "cam1 strih-pp resolume-pp"],
                       capture_output=True, text=True,
                       env=_env(DANTESYNC_CONFIG_DRIFT_FETCH_CMD=str(stub), OBS_FLEET_HOME="strih-pp",
                                DANTESYNC_FLEET_CRED_FILE=str(tmp_path / "none.env")))
    assert "node=resolume-pp role=video-pp verdict=SKIPPED (unreachable" in r.stdout, r.stdout + r.stderr
    # strih-pp is reachable and its config is graded against the PP template (an SNV config drifts)
    assert "node=strih-pp role=ntp-master-pp verdict=DRIFT" in r.stdout, r.stdout + r.stderr
