"""Issue 1553 -- `camera-box-version-gate.sh --other-venues` reads EVERY away-venue camera.

25.11.2026, all four PP cameras reachable on camera-box 1.8.15 (main 1.8.17): the gate printed a row
for cam1 only. The report loop reads its rows from a here-string on stdin, and the per-camera version
read runs ssh, which reads stdin too, so the first camera's read swallowed the remaining rows. The
existing issue-1413 tests had only ONE reachable PP camera, and their fake sshpass never read stdin.

Tier-0: the real gate, a fake sshpass that reads stdin like ssh does, OBS_FLEET_HOME as the reach list.
"""
import os
import pathlib
import stat
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_GATE = _ROOT / "scripts" / "camera-box-version-gate.sh"
_PP = {"cam1": "10.76.9.61", "cam4": "10.76.9.64", "cam6": "10.76.9.66", "cam7": "10.76.9.67"}


def test_every_reachable_away_camera_gets_its_row(tmp_path):
    b = tmp_path / "gbin"
    b.mkdir()
    arms = "".join(f'    *@{ip}) echo "camera-box 1.8.15" ;;\n' for ip in _PP.values())
    (b / "sshpass").write_text(
        "#!/usr/bin/env bash\nshift 2\ntarget=''\n"
        'for a in "$@"; do case "$a" in *@*) target="$a"; break ;; esac; done\n'
        # like ssh without -n: read whatever stdin holds before answering
        "[ -t 0 ] || cat >/dev/null\n"
        'case "$target" in\n    *@192.0.2.62) echo "camera-box 1.8.17" ;;\n' + arms + "    *) exit 255 ;;\nesac\n")
    (b / "sshpass").chmod((b / "sshpass").stat().st_mode | stat.S_IEXEC)
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("")
    env = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "OBS_FLEET", "CAMBOX_OFFLINE_ACK"))}
    env.update(PATH=f"{b}:/usr/bin:/bin", OBS_FLEET_HOME=" ".join(_PP))
    r = subprocess.run(["bash", str(_GATE), "--fleet-file", str(fleet), "--main-pin", "1.8.17",
                        "--linux", "cam2=root@192.0.2.62", "--other-venues"],
                       capture_output=True, text=True, env=env, timeout=180, stdin=subprocess.DEVNULL)
    assert r.returncode == 0, r.stdout + r.stderr
    for cam in _PP:
        rows = [ln for ln in r.stdout.splitlines() if ln.strip().startswith(cam + " ")]
        assert len(rows) == 1, (cam, r.stdout)
        assert "1.8.15" in rows[0] and "PIN-DRIFT" in rows[0], rows[0]
        assert f"{cam} (venue pp" in r.stderr, r.stderr
