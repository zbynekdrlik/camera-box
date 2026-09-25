"""Issue 1372 part B -- the ONE declared dantesync fleet + the per-role config-drift check.

WHY: the version gate, the fleet upgrader, the dev1 clock watchdog and the handover check each
enumerated dantesync nodes from the VIDEO lists only (camera-set.sh + obs-fleet.sh), so the
audio-VLAN PCs mbc and fohabl -- which run dantesync too -- were never version-checked, upgraded,
clock-watched or config-checked. On 25.9.2026 mbc was one release behind and both had a different
config (no gm_allowlist; fohabl without phase_slew). scripts/lib/dantesync-fleet.sh is now the one
list; scripts/dantesync_fleet.py is its python twin and the pure config-drift decision;
scripts/dantesync-config-drift.sh reads every node's config.json and reports a named diff.

Tier-0: the bash lib is driven through `subprocess.run(["bash", <file>])`; the drift check runs
against config.json bytes captured LIVE from the fleet on 25.9.2026 (fixtures/dantesync_config_1372).
"""
import importlib.util
import os
import pathlib
import stat
import subprocess
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_DRIFT_SH = _ROOT / "scripts" / "dantesync-config-drift.sh"
_FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "dantesync_config_1372"

_spec = importlib.util.spec_from_file_location("dantesync_fleet", _ROOT / "scripts" / "dantesync_fleet.py")
df = importlib.util.module_from_spec(_spec)
sys.modules["dantesync_fleet"] = df
_spec.loader.exec_module(df)

# the env a hermetic run must not inherit from the operator's shell
_CLEAN = ("DANTESYNC_FLEET", "OBS_FLEET", "OBS_FLEET_HOME", "DANTESYNC_AUDIO_GM_HOST",
          "RIG_GRANDMASTER_HOST", "RIG_GRANDMASTER_IP", "CAMBOX_OFFLINE_ACK",
          "DANTESYNC_FOHABL_SSH_PASS", "DANTESYNC_FLEET_CRED_FILE")


def _env(**over):
    env = {k: v for k, v in os.environ.items() if k not in _CLEAN}
    env.update(over)
    return env


def _bash(tmp_path, body, **env):
    """Source the lib under the callers' strict mode and run BODY; returns the CompletedProcess."""
    script = tmp_path / "run.sh"
    script.write_text(f"set -euo pipefail\n. '{_LIB}'\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def _with_env(monkeypatch, **env):
    for k in _CLEAN:
        monkeypatch.delenv(k, raising=False)
    for k, v in env.items():
        monkeypatch.setenv(k, v)


# ---------------------------------------------------------------------------------------------
# the node set
# ---------------------------------------------------------------------------------------------

def test_the_fleet_carries_every_dantesync_node_including_the_audio_vlan(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    assert r.returncode == 0, r.stderr
    names = [ln.split("|")[0] for ln in r.stdout.split()]
    assert names == ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7", "dev1", "strih-lx",
                     "stream", "resolume", "mbc", "fohabl"]
    rows = {ln.split("|")[0]: ln for ln in r.stdout.split()}
    assert rows["mbc"] == "mbc|10.77.7.232|windows|audio|always|-|"
    assert rows["fohabl"] == "fohabl|10.77.7.30|windows|audio|always|master|DANTESYNC_FOHABL_SSH_PASS"
    # OBS-box addresses come from obs-fleet.sh (never a second literal); strih-lx is the NTP master
    assert rows["strih-lx"] == "strih-lx|10.77.9.202|linux|ntp-master|obsfleet|-|"
    assert rows["resolume"] == "resolume|resolume.lan|windows|video|obsfleet|-|"
    assert rows["dev1"] == "dev1|local|linux|video|local|-|"


def test_lv1_is_excluded_per_the_owner():
    """ROZHODNUTÉ 25.9.2026: lv1 (10.77.7.100) is an operational PC -- left out for now."""
    txt = _LIB.read_text()
    table = txt[txt.index('DANTESYNC_FLEET="${DANTESYNC_FLEET:-'):txt.index('}"', txt.index('DANTESYNC_FLEET="${'))]
    assert "lv1" not in table and "10.77.7.100" not in table


def test_cameras_come_from_the_camera_resolve_walk_not_a_literal_range(tmp_path):
    """Every camera camera_resolve knows, and nothing else: the count follows camera-set.sh."""
    import re
    arms = re.findall(r"^\s*(cam[0-9]+)\)\s*CAMERA_IP=", (_ROOT / "scripts" / "camera-set.sh").read_text(),
                      re.MULTILINE)
    r = _bash(tmp_path, "dantesync_fleet_names homegate=always os=linux")
    assert r.stdout.split() == arms
    assert "cam1 cam2 cam3" not in _LIB.read_text()  # no literal camera range in the lib


def test_a_retired_obs_box_drops_out_and_returns_with_its_home_check(tmp_path):
    base = "strih-lx|10.77.9.202|linux-genlock|always\nstream|10.77.9.204|windows-genlock|always\n" \
           "resolume|resolume.lan|windows-genlock|traveling\n"
    retired = _bash(tmp_path, "dantesync_fleet_names", OBS_FLEET=base + "imag|10.77.9.182|linux-genlock|retired")
    back = _bash(tmp_path, "dantesync_fleet_names", OBS_FLEET=base + "imag|10.77.9.182|linux-genlock|always")
    assert "imag" not in retired.stdout.split()
    assert "imag" in back.stdout.split()


def test_bash_and_python_rows_are_identical(tmp_path, monkeypatch):
    r = _bash(tmp_path, "dantesync_fleet_rows")
    _with_env(monkeypatch)
    assert r.stdout.strip().split("\n") == df.rows()


def test_bash_and_python_agree_on_an_overridden_table(tmp_path, monkeypatch):
    table = "dev1|local|linux|video|local|-|\nfoh|10.0.0.9|windows|audio|always|master|FOH_PW"
    r = _bash(tmp_path, "dantesync_fleet_rows", DANTESYNC_FLEET=table)
    _with_env(monkeypatch, DANTESYNC_FLEET=table)
    assert r.stdout.strip().split("\n") == df.rows()
    assert r.stdout.strip().split("\n")[-1] == "foh|10.0.0.9|windows|audio|always|master|FOH_PW"


def test_a_malformed_row_fails_loudly_in_both(tmp_path, monkeypatch):
    table = "dev1|local|linux|video|local|-|\nbroken|10.0.0.1|windows|audio"
    r = _bash(tmp_path, "dantesync_fleet_rows || echo RC=$?", DANTESYNC_FLEET=table)
    assert "RC=1" in r.stdout and "malformed" in r.stderr
    _with_env(monkeypatch, DANTESYNC_FLEET=table)
    with pytest.raises(RuntimeError, match="malformed"):
        df.rows()


def test_an_unknown_obs_fleet_name_fails_loudly(tmp_path):
    r = _bash(tmp_path, "dantesync_fleet_rows || echo RC=$?", DANTESYNC_FLEET="ghost|obs:ghost|linux|video|obsfleet|-|")
    assert "RC=1" in r.stdout and "absent from OBS_FLEET" in r.stderr


# ---------------------------------------------------------------------------------------------
# consumer helpers
# ---------------------------------------------------------------------------------------------

def test_version_gate_arms(tmp_path):
    r = _bash(tmp_path, 'echo "L=$(dantesync_fleet_spec linux op)"; echo "W=$(dantesync_fleet_spec win op)";'
                        ' echo "D=$(dantesync_fleet_spec local op)"')
    out = dict(ln.split("=", 1) for ln in r.stdout.splitlines())
    assert out["L"].split() == [f"cam{n}=root@10.77.9.6{n}" for n in range(1, 8)] + ["strih-lx=op@10.77.9.202"]
    assert out["W"].split() == ["stream=op@10.77.9.204", "resolume=op@resolume.lan",
                                "mbc=op@10.77.7.232", "fohabl=master@10.77.7.30"]
    assert out["D"] == "dev1"


def test_present_drops_an_away_traveling_box(tmp_path):
    # OBS_FLEET_HOME pins who is home (the obs-fleet test seam): resolume away
    r = _bash(tmp_path, 'dantesync_fleet_spec win op --present', OBS_FLEET_HOME="strih-lx stream")
    assert "resolume" not in r.stdout
    assert "mbc=op@10.77.7.232" in r.stdout and "stream=op@10.77.9.204" in r.stdout


def test_credential_is_referenced_by_name_never_a_literal(tmp_path):
    r = _bash(tmp_path, 'echo "F=$(dantesync_fleet_cred_var_for_target master@10.77.7.30)";'
                        ' echo "M=$(dantesync_fleet_cred_var_for_target op@10.77.7.232)"')
    assert "F=DANTESYNC_FOHABL_SSH_PASS" in r.stdout
    assert "M=\n" in r.stdout + "\n"
    # the committed lib names the variable, never a password value
    assert "DANTESYNC_FOHABL_SSH_PASS=" not in _LIB.read_text()


def test_credentials_load_from_the_file_parsed_not_sourced(tmp_path):
    cred = tmp_path / "creds.env"
    marker = tmp_path / "executed"
    cred.write_text(f'# comment\nDANTESYNC_FOHABL_SSH_PASS="s3cret $(touch {marker})"\nOTHER_VAR=nope\n')
    r = _bash(tmp_path, 'dantesync_fleet_load_credentials; echo "P=${DANTESYNC_FOHABL_SSH_PASS:-}"; echo "O=${OTHER_VAR:-}"',
              DANTESYNC_FLEET_CRED_FILE=str(cred))
    assert f"P=s3cret $(touch {marker})" in r.stdout
    assert "O=\n" in r.stdout + "\n"         # only row-named keys are taken
    assert not marker.exists()              # the value was never executed
    # an already-set environment value wins over the file
    r = _bash(tmp_path, 'dantesync_fleet_load_credentials; echo "P=$DANTESYNC_FOHABL_SSH_PASS"',
              DANTESYNC_FLEET_CRED_FILE=str(cred), DANTESYNC_FOHABL_SSH_PASS="from-env")
    assert "P=from-env" in r.stdout


def test_role_grandmaster(tmp_path, monkeypatch):
    r = _bash(tmp_path, 'echo "V=$(dantesync_fleet_role_gm_host video)"; echo "A=$(dantesync_fleet_role_gm_ip audio)"')
    assert "V=video-clock.lan" in r.stdout and "A=10.77.7.106" in r.stdout
    _with_env(monkeypatch)
    assert df.role_gm_host("video") == "video-clock.lan"
    assert df.role_gm_host("audio") == "10.77.7.106"
    assert df.role_gm_host("ntp-master") == "video-clock.lan"
    _with_env(monkeypatch, DANTESYNC_AUDIO_GM_HOST="10.77.7.99")
    assert df.role_gm_host("audio") == "10.77.7.99"


# ---------------------------------------------------------------------------------------------
# config drift (the pure decision), on configs read LIVE on 25.9.2026
# ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("name,role", [("cam1", "video"), ("stream", "video"), ("strih-lx", "ntp-master")])
def test_the_live_video_configs_match_their_template(name, role, monkeypatch):
    _with_env(monkeypatch)
    assert df.drift((_FIX / f"{name}.json").read_bytes(), role) == (df.OK, [])


def test_live_mbc_drift_is_named(monkeypatch):
    _with_env(monkeypatch)
    verdict, diffs = df.drift((_FIX / "mbc.json").read_bytes(), "audio")
    assert verdict == df.DRIFT
    assert diffs == ['system.gm_allowlist: missing (canonical ["10.77.7.106"])']


def test_live_fohabl_drift_names_both_missing_keys(monkeypatch):
    _with_env(monkeypatch)
    verdict, diffs = df.drift((_FIX / "fohabl.json").read_bytes(), "audio")
    assert verdict == df.DRIFT
    assert diffs == ['system.gm_allowlist: missing (canonical ["10.77.7.106"])',
                     "system.phase_slew.enabled: missing (canonical true)"]


def test_a_byte_order_mark_is_drift_even_when_the_content_matches(monkeypatch):
    _with_env(monkeypatch)
    raw = b"\xef\xbb\xbf" + (_FIX / "stream.json").read_bytes()
    verdict, diffs = df.drift(raw, "video")
    assert verdict == df.DRIFT and "byte-order mark" in diffs[0] and len(diffs) == 1
    utf16 = (_FIX / "stream.json").read_text().encode("utf-16")
    assert df.drift(utf16, "video")[0] == df.DRIFT


def test_unreadable_and_invalid_configs_are_unknown_never_ok(monkeypatch):
    _with_env(monkeypatch)
    assert df.drift(b"", "video")[0] == df.UNKNOWN
    assert df.drift(None, "video")[0] == df.UNKNOWN
    assert df.drift(b"{not json", "video")[0] == df.UNKNOWN
    assert df.drift(b"[1, 2]", "video")[0] == df.UNKNOWN


def test_value_changes_extra_keys_and_rules(monkeypatch):
    import json
    _with_env(monkeypatch)
    base = json.loads((_FIX / "stream.json").read_text())
    cfg = json.loads(json.dumps(base))
    cfg["system"]["phase_slew"]["enabled"] = False
    cfg["system"]["gm_allowlist"] = ["10.77.9.184"]
    cfg["mystery"] = 1
    verdict, diffs = df.drift(json.dumps(cfg).encode(), "video")
    assert verdict == df.DRIFT
    assert 'system.gm_allowlist: ["10.77.9.184"] (canonical ["video-clock.lan"])' in diffs
    assert "system.phase_slew.enabled: false (canonical true)" in diffs
    assert any(d.startswith("mystery: extra key") for d in diffs)
    # ntp_server is OPTIONAL for the video role (camboxes pass it on the unit command line) ...
    del cfg["mystery"]
    cfg = json.loads(json.dumps(base))
    del cfg["ntp_server"]
    assert df.drift(json.dumps(cfg).encode(), "video") == (df.OK, [])
    # ... but when present it must be the NTP master
    cfg["ntp_server"] = "pool.ntp.org"
    assert df.drift(json.dumps(cfg).encode(), "video")[1] == ['ntp_server: "pool.ntp.org" (canonical "strih.lan")']
    # the NTP master must carry SOME upstream ntp_server
    master = json.loads((_FIX / "strih-lx.json").read_text())
    del master["ntp_server"]
    assert df.drift(json.dumps(master).encode(), "ntp-master")[1] == ["ntp_server: missing (canonical: any value)"]


def test_the_video_template_follows_the_rig_grandmaster_name(monkeypatch):
    _with_env(monkeypatch, RIG_GRANDMASTER_HOST="clock2.lan")
    assert df.template_for("video")["system"]["gm_allowlist"] == ["clock2.lan"]
    assert df.drift((_FIX / "cam1.json").read_bytes(), "video")[0] == df.DRIFT


# ---------------------------------------------------------------------------------------------
# the orchestrator, end to end through its fetch seam (no box)
# ---------------------------------------------------------------------------------------------

def _fetch_stub(tmp_path, mapping, fail=()):
    """A DANTESYNC_CONFIG_DRIFT_FETCH_CMD that copies fixtures/<file> for each node name."""
    stub = tmp_path / "fetch.sh"
    lines = ["#!/usr/bin/env bash", 'name="$1"; out="$4"', 'case "$name" in']
    for name in fail:  # a failing read wins: it half-writes, then exits non-zero
        lines.append(f'  {name}) printf "{{" >"$out"; exit 1 ;;')
    for name, fname in mapping.items():
        lines.append(f'  {name}) cp "{_FIX / fname}" "$out" ;;')
    lines += ['  *) cp "' + str(_FIX / "cam1.json") + '" "$out" ;;', "esac"]
    stub.write_text("\n".join(lines) + "\n")
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
    return stub


def _run_drift(tmp_path, stub, *args, **env):
    fleet_file = tmp_path / "rig-fleet.txt"
    if not fleet_file.exists():
        fleet_file.write_text("")
    full = {"DANTESYNC_CONFIG_DRIFT_FETCH_CMD": str(stub), "OBS_FLEET_HOME": "strih-lx stream resolume",
            "DANTESYNC_FLEET_CRED_FILE": str(tmp_path / "none.env")}
    full.update(env)
    return subprocess.run(["bash", str(_DRIFT_SH), "--fleet-file", str(fleet_file), *args],
                          capture_output=True, text=True, env=_env(**full))


def test_orchestrator_reports_the_live_audio_drift(tmp_path):
    stub = _fetch_stub(tmp_path, {"mbc": "mbc.json", "fohabl": "fohabl.json", "stream": "stream.json",
                                  "resolume": "stream.json", "strih-lx": "strih-lx.json"})
    r = _run_drift(tmp_path, stub)
    assert r.returncode == 20, r.stdout + r.stderr
    assert "node=mbc role=audio verdict=DRIFT" in r.stdout
    assert "system.phase_slew.enabled: missing" in r.stdout
    assert "DANTESYNC CONFIG DRIFT on: mbc fohabl" in r.stdout
    for name in ("cam1", "cam7", "dev1", "strih-lx", "stream", "resolume"):
        assert f"node={name} " in r.stdout and f"node={name} role=" in r.stdout


def test_orchestrator_clean_fleet_exits_0(tmp_path):
    stub = _fetch_stub(tmp_path, {"stream": "stream.json", "resolume": "stream.json", "strih-lx": "strih-lx.json",
                                  "mbc": "cam1.json", "fohabl": "cam1.json"})
    r = _run_drift(tmp_path, stub, "--only", "cam1 stream strih-lx")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "OK: every read dantesync node" in r.stdout
    assert "node=mbc" not in r.stdout


def test_orchestrator_unread_node_is_unknown_not_clean(tmp_path):
    stub = _fetch_stub(tmp_path, {"stream": "stream.json"}, fail=("stream",))
    r = _run_drift(tmp_path, stub, "--only", "cam1 stream")
    assert r.returncode == 11, r.stdout + r.stderr
    assert "node=stream role=video verdict=UNKNOWN" in r.stdout


def test_orchestrator_skips_an_away_traveling_box_and_excludes_an_acked_one(tmp_path):
    stub = _fetch_stub(tmp_path, {"stream": "stream.json", "resolume": "mbc.json"})
    (tmp_path / "rig-fleet.txt").write_text("cam3:battery swap\n")
    r = _run_drift(tmp_path, stub, "--only", "cam3 stream resolume", OBS_FLEET_HOME="strih-lx stream")
    assert r.returncode == 0, r.stdout + r.stderr
    assert "node=resolume role=video verdict=SKIPPED" in r.stdout
    assert "node=cam3 role=video verdict=EXCLUDED (acked offline: battery swap)" in r.stdout


def test_orchestrator_never_writes_to_a_box():
    txt = _DRIFT_SH.read_text()
    body = txt[txt.index('HERE="$('):]
    # reads are `scp <target>:<remote> <local>` only; nothing uploads or runs a remote write
    assert "ssh " not in body.replace("sshpass", "")
    assert '"${target}:${remote}" "$out"' in body
