"""Issue 1553, ROZHODNUTÉ 7163831021 (owner 25.11.2026, option 2): one clock name at both venues.

Every box's dsync allows `video-clock.lan` (each venue's MikroTik points it at its own clock: SNV the
Yamaha card, PP dante-ptp 10.77.7.200). dev1 sits on SNV DNS, so the address dev1 grades a venue's nodes
against is a separate fact, `STRIH_DANTE_GM_ADDR`: `none` = resolve the name on dev1 (SNV), else an IPv4.

Tier-0: the fact files and libs as committed, a fake getent that answers the SNV address.
"""
import os
import pathlib
import re
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_BOXES = _ROOT / "scripts" / "strih-boxes"
_FLEET = _ROOT / "scripts" / "lib" / "dantesync-fleet.sh"
_FACTS = _ROOT / "scripts" / "lib" / "strih-box-facts.sh"


def _fact(box, key):
    m = re.search(rf"^{key}=(.*)$", (_BOXES / f"{box}.env").read_text(), re.M)
    return m.group(1) if m else None


def test_every_box_allows_the_same_clock_name():
    for box in ("strih-snv", "stream-snv", "strih-pp", "stream-pp"):
        assert _fact(box, "STRIH_DANTE_GM") == "video-clock.lan", box


def test_the_grading_address_is_none_at_snv_and_the_pp_clock_at_pp():
    assert _fact("strih-snv", "STRIH_DANTE_GM_ADDR") == "none"
    assert _fact("stream-snv", "STRIH_DANTE_GM_ADDR") == "none"
    assert _fact("strih-pp", "STRIH_DANTE_GM_ADDR") == "10.77.7.200"
    assert _fact("stream-pp", "STRIH_DANTE_GM_ADDR") == "10.77.7.200"


def _bash(body, tmp_path):
    fake = tmp_path / "getent"
    fake.write_text('#!/usr/bin/env bash\necho "10.77.9.230     STREAM video-clock.lan"\n')
    fake.chmod(0o755)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("DANTESYNC_", "RIG_GRANDMASTER"))}
    env["RIG_GRANDMASTER_GETENT"] = str(fake)
    return subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{_FLEET}"\n{body}'],
                          capture_output=True, text=True, env=env, timeout=60)


def test_dev1_grades_pp_against_the_pp_address_never_its_own_dns(tmp_path):
    r = _bash("dantesync_fleet_role_gm_ip video-pp; dantesync_fleet_role_gm_ip ntp-master-pp", tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["10.77.7.200", "10.77.7.200"]


def test_dev1_grades_snv_by_resolving_the_name(tmp_path):
    r = _bash("dantesync_fleet_role_gm_ip video", tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "10.77.9.230"


def test_a_pp_camera_allows_the_name(tmp_path):
    r = _bash("dantesync_fleet_camera_gm_host cam7", tmp_path)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == "video-clock.lan"


def _validate(tmp_path, value):
    src = (_BOXES / "strih-pp.env").read_text()
    f = tmp_path / "strih-pp.env"
    f.write_text(re.sub(r"^STRIH_DANTE_GM_ADDR=.*$", f"STRIH_DANTE_GM_ADDR={value}", src, flags=re.M))
    return subprocess.run(["bash", "-c", f'set -euo pipefail\n. "{_FACTS}"\nSTRIH_BOXES_DIR="$1" strih_box_load strih-pp >/dev/null',
                           "h", str(tmp_path)], capture_output=True, text=True, timeout=60)


def test_the_grading_address_must_be_none_or_ipv4(tmp_path):
    assert _validate(tmp_path, "10.77.7.200").returncode == 0
    bad = _validate(tmp_path, "video-clock.lan")
    assert bad.returncode != 0 and "STRIH_DANTE_GM_ADDR" in bad.stderr, bad.stderr
