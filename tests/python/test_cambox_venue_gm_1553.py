"""Issue 1553 -- a cambox's dsync grandmaster comes from its VENUE, never the SNV DNS name.

On 22.11.2026 the four PP cameras (strih-pp.env STRIH_CAMERAS) carried `gm_allowlist
["video-clock.lan"]` from setup-device STEP 17: the SNV MikroTik name, which PP has no entry for, so
dsync locked to nothing and the boxes were hand-set to the PP grandmaster. setup-device now writes the
camera's venue grandmaster (`dantesync_fleet_camera_gm_host camN` = the STRIH_DANTE_GM of the venue's
NTP-master fact file), and verify-device's (ba) check grades the box's gm_allowlist against it.

Tier-0: the libs sourced by bash, no box, no network.
"""
import os
import pathlib
import re
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_FLEET = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_SETUP = _ROOT / "scripts" / "setup-device.sh"
_VERIFY = _ROOT / "scripts" / "verify-device.sh"
_BOXES = _ROOT / "scripts" / "strih-boxes"


def _fact(box, key):
    return re.search(rf"^{key}=(.+)$", (_BOXES / f"{box}.env").read_text(), re.M).group(1)


def _bash(body, **env):
    e = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "RIG_GRANDMASTER"))}
    e.update(env)
    return subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{_FLEET}"\n{body}'],
                          capture_output=True, text=True, env=e, timeout=60)


def test_a_pp_camera_gets_the_pp_grandmaster():
    for cam in ("cam1", "cam4", "cam6", "cam7"):
        r = _bash(f"dantesync_fleet_camera_gm_host {cam}")
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == _fact("strih-pp", "STRIH_DANTE_GM") == "video-clock.lan"  # ROZHODNUTÉ 7163831021


def test_an_snv_camera_gets_the_snv_grandmaster():
    for cam in ("cam2", "cam3", "cam5"):
        r = _bash(f"dantesync_fleet_camera_gm_host {cam}")
        assert r.returncode == 0, r.stderr
        assert r.stdout.strip() == _fact("strih-snv", "STRIH_DANTE_GM") == "video-clock.lan"


def test_a_camera_in_no_venue_fails_loud():
    r = _bash("dantesync_fleet_camera_gm_host cam9")
    assert r.returncode != 0 and r.stdout == ""
    assert "cam9" in r.stderr


def test_the_verdict_grades_the_allowlist_exactly():
    cases = [
        ('{"system": {"gm_allowlist": ["10.77.7.200"], "clock_discipline": "ptp_phase_lock"}}', "10.77.7.200", "ok"),
        ('{"system": {"gm_allowlist": ["video-clock.lan"]}}', "10.77.7.200", "other:video-clock.lan"),
        ('{"system": {"gm_allowlist": ["10.77.7.200", "video-clock.lan"]}}', "10.77.7.200",
         "other:10.77.7.200,video-clock.lan"),
        ('{"system": {}}', "10.77.7.200", "missing"),
        ("not json", "10.77.7.200", "unreadable"),
        ("", "10.77.7.200", "unreadable"),
    ]
    for text, want, expect in cases:
        p = subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{_FLEET}"\ndantesync_fleet_gm_allowlist_verdict "$1" "$2"',
                            "h", text, want], capture_output=True, text=True, timeout=60)
        assert p.returncode == 0, p.stderr
        assert p.stdout.strip() == expect, (text, p.stdout)


def test_setup_device_writes_the_venue_grandmaster_not_the_fixed_name():
    s = _SETUP.read_text()
    assert '_RG_GM_HOST="$(dantesync_fleet_camera_gm_host "$CAMERA_NAME")"' in s
    assert '_RG_GM_HOST="$(rig_grandmaster_host)"' not in s
    assert '"${_RG_GM_HOST}"' in s[s.index("cat > /etc/dsync/config.json"):]


def test_verify_device_grades_the_allowlist_in_check_ba():
    v = _VERIFY.read_text()
    assert "# (ba) dsync grandmaster allowlist (issue 1553)" in v
    blk = v[v.index("# (ba) dsync grandmaster allowlist (issue 1553)"):]
    blk = blk[:blk.index("\n# (", 10)]
    assert "dantesync_fleet_camera_gm_host" in blk and "dantesync_fleet_gm_allowlist_verdict" in blk
    assert "/etc/dsync/config.json" in blk and "fail " in blk
    # (q) stays the last check
    assert v.index("# (ba) dsync grandmaster allowlist (issue 1553)") < v.rindex("(q)")
