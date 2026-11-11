"""Issue 1506: the rig-busy guard reads the strih/stream of the VENUE of the box it touches.

On 11.11.2026 `cambox-ro-units-apply.sh --venue pp --apply` and a direct
`bkshading-deploy-relay.sh --box camN` guarded PP cameras with the SNV strih/stream (fixed
10.77.9.202/.204 defaults) and restarted dsync + the relay while stream-pp streamed. The guard's
OBS hosts now come from the box's venue: remote_log_camera_venue (strih fact files) +
obs_fleet_host strih-/stream-<venue>. An unresolvable venue refuses; an explicit caller env wins.
"""
import os
import pathlib
import subprocess

REPO = pathlib.Path(__file__).resolve().parents[2]
LIB = REPO / "scripts" / "lib"


def hosts(cam: str) -> subprocess.CompletedProcess:
    script = (
        f'. "{LIB}/obs-fleet.sh"; . "{LIB}/remote-logging.sh"; . "{LIB}/stray-session-check.sh"; '
        f'stray_session_hosts_for_camera "{cam}"'
    )
    return subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                          env={**os.environ, "REMOTE_LOG_LIB_DIR": str(LIB)})


def test_a_pp_camera_is_guarded_by_the_pp_strih_and_stream():
    r = hosts("cam1")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["strih-pp.lan", "stream-pp.lan"]


def test_an_snv_camera_is_guarded_by_the_snv_strih_and_stream():
    r = hosts("cam5")
    assert r.returncode == 0, r.stderr
    assert r.stdout.split() == ["10.77.9.202", "10.77.9.204"]


def test_a_camera_with_no_venue_refuses_never_an_snv_fallback():
    r = hosts("cam99")
    assert r.returncode != 0
    assert r.stdout.strip() == ""


def _guard_block(path: str) -> str:
    return (REPO / path).read_text()


def test_ro_units_apply_resolves_the_guard_hosts_per_box():
    s = _guard_block("scripts/cambox-ro-units-apply.sh")
    body = s.split("rig_refused() {", 1)[1].split("\n}\n", 1)[0]
    assert "stray_session_hosts_for_camera" in body
    assert 'STRIH_HOST="${STRIH_HOST:-10.77.9.202}"' not in s


def test_relay_deploy_resolves_the_guard_hosts_from_its_box():
    s = _guard_block("scripts/bkshading-deploy-relay.sh")
    assert "stray_session_hosts_for_camera" in s
    assert 'STRIH_HOST="${STRIH_HOST:-10.77.9.202}"' not in s
