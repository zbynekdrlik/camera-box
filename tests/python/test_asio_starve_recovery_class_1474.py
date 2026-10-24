"""issue 1474 -- the asio-starve watchdog's default recovery plan follows the watched box's obs-fleet
CLASS. The ASIO sources live on the stream box, which is the Linux stream-snv since issue 1462: the
Windows planner the old default named (`launch-obs-genlock.sh --box stream --force`) refuses a Linux
box (exit 2). On a linux-genlock box the plan is the supervised unit's restart over plain ssh
(`stream_box_obs_restart_hint`, scripts/lib/stream-box.sh); a Windows stream box (an OBS_FLEET
override) keeps the planner line byte for byte. ASIO_STARVE_RECOVERY_PLAN still wins over both.

Driven through the real watchdog in --dry-run (two passes, the 2-pass confirm), with the log read
replaced by ASIO_STARVE_PROBE_CMD and tmp state files. Tier-0: no rig, no network.
"""
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
WATCHDOG = os.path.join(REPO, "scripts", "asio-starve-alert-watchdog.sh")
WIN_FLEET = "strih-snv|10.77.9.202|linux-genlock|always\nstream-snv|10.77.9.204|windows-genlock|always"
STARVED = ("09:00:07.551: asrc: source 'ASIO Input Capture' estimated=0.00ppm starved_blocks=2949 (#803/#806/#960)\n"
           "09:00:08.660: asrc: source 'mbc' estimated=0.00ppm starved_blocks=0 (#803/#806/#960)\n")


def _two_passes(tmp_path, **extra):
    log = tmp_path / "obslog.txt"
    log.write_text(STARVED)
    probe = tmp_path / "probe.sh"
    probe.write_text(f'#!/usr/bin/env bash\ncat "{log}"\n')
    probe.chmod(0o755)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("ASIO_STARVE_", "OBS_FLEET"))}
    env.update({"ASIO_STARVE_PROBE_CMD": f"bash {probe}",
                "ASIO_STARVE_ALERT_STATE_FILE": str(tmp_path / "state"),
                "ASIO_STARVE_NETREACH_STATE_FILE": str(tmp_path / "netreach")})
    env.update(extra)
    out = ""
    for _ in range(2):
        r = subprocess.run(["bash", WATCHDOG, "--dry-run"], env=env, capture_output=True,
                           text=True, timeout=60, cwd=REPO)
        assert r.returncode == 0, r.stdout + r.stderr
        out = r.stdout + r.stderr
    alert = [ln for ln in out.splitlines() if "WOULD alert" in ln]
    assert alert, out
    return alert[0]


def test_a_linux_stream_box_gets_the_unit_restart(tmp_path):
    line = _two_passes(tmp_path)
    assert "recovery=ssh newlevel@10.77.9.204 'systemctl --user restart strih-obs.service'" in line, line
    assert "launch-obs-genlock" not in line, line


def test_a_windows_stream_box_keeps_the_planner_line(tmp_path):
    line = _two_passes(tmp_path, OBS_FLEET=WIN_FLEET)
    assert "recovery=scripts/launch-obs-genlock.sh --box stream --force" in line, line


def test_an_explicit_recovery_plan_wins(tmp_path):
    line = _two_passes(tmp_path, ASIO_STARVE_RECOVERY_PLAN="do the thing")
    assert "recovery=do the thing" in line, line
