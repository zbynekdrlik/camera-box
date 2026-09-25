"""Issue 1372 part B -- the dev1 dante-clock watchdog derives its roster from the ONE dantesync fleet.

WHY: the watchdog watched only the video fleet (a literal `cam1 ... cam7` + the OBS boxes + dev1).
The audio-VLAN PCs mbc (10.77.7.232) and fohabl (10.77.7.30) run dantesync too and were never
clock-watched. Now every default roster list comes from scripts/lib/dantesync-fleet.sh, and each
node is graded against ITS role's grandmaster: an audio-VLAN node locks to the audio grandmaster
(10.77.7.106, the Audinate device -- the same clock as video-clock.lan on the other VLAN), so a
correctly locked mbc must read OK, never a false `wrong_gm` page against the video grandmaster.

Drives the REAL watchdog in --dry-run through its seams (fetch stub keyed by IP), no live rig.
"""
import os
import pathlib
import stat
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_WATCHDOG = _ROOT / "scripts" / "dantesync-clock-alert-watchdog.sh"
_NOW = 1790349506
VIDEO_GM = "10.77.9.230"
AUDIO_GM = "10.77.7.106"


def _status(gm, is_locked="true", mode="LOCK"):
    return ("{" + f'"offset_ns":1,"settled":true,"updated_ts":{_NOW},"is_locked":{is_locked},'
            f'"mode":"{mode}","gm_source_ip":"{gm}","ntp_step_storm":false,"ntp_steps_last_hour":null' + "}")


def _source_var(var, **env):
    """The value of VAR after sourcing the watchdog (the roster defaults are evaluated at source)."""
    e = {k: v for k, v in os.environ.items() if not k.startswith(("DANTE_CLOCK_", "DANTESYNC_", "OBS_FLEET"))}
    e.update(env)
    script = f'set -uo pipefail\n. "{_WATCHDOG}"\nprintf "%s" "${{{var}}}"\n'
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stdout


def test_roster_defaults_come_from_the_dantesync_fleet():
    assert _source_var("DANTE_CLOCK_CAM_NODES") == "cam1 cam2 cam3 cam4 cam5 cam6 cam7"
    assert _source_var("DANTE_CLOCK_OBS_NODES") == "strih-lx stream resolume"
    assert _source_var("DANTE_CLOCK_FIXED_NODES") == "mbc fohabl"
    assert _source_var("DANTE_CLOCK_LOCAL_NODES") == "dev1"


def test_env_overrides_stay_byte_compatible():
    assert _source_var("DANTE_CLOCK_CAM_NODES", DANTE_CLOCK_CAM_NODES="cam3") == "cam3"
    assert _source_var("DANTE_CLOCK_OBS_NODES", DANTE_CLOCK_OBS_NODES="") == ""
    assert _source_var("DANTE_CLOCK_FIXED_NODES", DANTE_CLOCK_FIXED_NODES="") == ""
    assert _source_var("DANTE_CLOCK_LOCAL_NODES", DANTE_CLOCK_LOCAL_NODES="") == ""


def test_no_literal_camera_range_in_the_watchdog():
    assert "cam1 cam2 cam3 cam4 cam5 cam6 cam7" not in _WATCHDOG.read_text()


def _run_dry(tmp_path, by_ip, **env):
    fetch = tmp_path / "fetch.sh"
    arms = "".join(f"  {ip}) cat <<'JSON'\n{body}\nJSON\n  ;;\n" for ip, body in by_ip.items())
    fetch.write_text(f'#!/usr/bin/env bash\ncase "$1" in\n{arms}  *) exit 1 ;;\nesac\n')
    fetch.chmod(fetch.stat().st_mode | stat.S_IEXEC)
    boxup = tmp_path / "boxup.sh"
    boxup.write_text("#!/usr/bin/env bash\nprintf 0\n")
    boxup.chmod(boxup.stat().st_mode | stat.S_IEXEC)
    # The #1309 management axis probes the REAL ssh banner of a cam node unless stubbed -- hermetic
    # runs stub it to "banner read back" (1) so the verdict never depends on the runner's network.
    mgmt = tmp_path / "mgmt.sh"
    mgmt.write_text("#!/usr/bin/env bash\nprintf 1\n")
    mgmt.chmod(mgmt.stat().st_mode | stat.S_IEXEC)
    e = {k: v for k, v in os.environ.items() if not k.startswith(("DANTE_CLOCK_", "DANTESYNC_", "OBS_FLEET"))}
    e.update({
        "DANTE_CLOCK_LOCAL_NODES": "", "DANTE_CLOCK_CAM_NODES": "", "DANTE_CLOCK_OBS_NODES": "",
        "DANTE_CLOCK_FETCH_CMD": str(fetch), "DANTE_CLOCK_BOX_UP_CMD": str(boxup),
        "DANTE_CLOCK_MGMT_SSH_CMD": str(mgmt),
        "RIG_GRANDMASTER_IP": VIDEO_GM, "DANTE_CLOCK_CONFIRM_THRESHOLD": "1",
        "DANTE_CLOCK_NOW": str(_NOW), "DANTE_CLOCK_VERSION_PIN": "1.8.54",
        "DANTE_CLOCK_ALERT_STATE_DIR": str(tmp_path),
    })
    e.update(env)
    r = subprocess.run(["bash", str(_WATCHDOG), "--dry-run"], capture_output=True, text=True, env=e)
    assert r.returncode == 0, r.stderr
    return r.stderr


def test_audio_vlan_nodes_are_watched_and_graded_against_the_audio_grandmaster(tmp_path):
    err = _run_dry(tmp_path, {"10.77.7.232": _status(AUDIO_GM), "10.77.7.30": _status(AUDIO_GM)})
    assert "fixed='mbc fohabl'" in err
    assert "mbc (10.77.7.232): reachable=1 verdict=OK" in err, err
    assert "fohabl (10.77.7.30): reachable=1 verdict=OK" in err, err
    assert "WOULD alert" not in err, err


def test_an_audio_node_on_the_wrong_grandmaster_pages_with_the_audio_remedy(tmp_path):
    err = _run_dry(tmp_path, {"10.77.7.232": _status(VIDEO_GM), "10.77.7.30": _status(AUDIO_GM)})
    assert "mbc (10.77.7.232): reachable=1 verdict=NO_CLOCK reason=wrong_gm" in err, err
    assert "WOULD alert" in err and "dante-clock-mbc-" in err
    assert "audio grandmaster na audio VLAN (10.77.7.106" in err
    assert "fohabl (10.77.7.30): reachable=1 verdict=OK" in err


def test_a_video_node_keeps_the_video_grandmaster(tmp_path):
    err = _run_dry(tmp_path, {"10.77.9.204": _status(AUDIO_GM)}, DANTE_CLOCK_FIXED_NODES="",
                   DANTE_CLOCK_OBS_NODES="stream")
    assert "stream (10.77.9.204): reachable=1 verdict=NO_CLOCK reason=wrong_gm" in err, err
    assert "DNS video-clock.lan" in err


def test_a_down_audio_node_is_skip_never_a_page(tmp_path):
    err = _run_dry(tmp_path, {"10.77.7.30": _status(AUDIO_GM)})
    assert "mbc (10.77.7.232): reachable=0 verdict=SKIP" in err, err
    assert "WOULD alert" not in err, err


def test_an_explicit_three_field_roster_still_parses(tmp_path):
    """DANTE_CLOCK_NODES keeps its NAME|IP|HOMEGATE shape (no role -> the video grandmaster)."""
    err = _run_dry(tmp_path, {"10.77.9.61": _status(VIDEO_GM)}, DANTE_CLOCK_NODES="cam1|10.77.9.61|always")
    assert "cam1 (10.77.9.61): reachable=1 verdict=OK" in err, err
