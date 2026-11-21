"""Issue 1548: a re-run attempt reclaims the rig lease its OWN dead previous attempt left behind.

A GitHub re-run keeps the run id, so the dead attempt's holder (fresh heartbeat, run in progress)
used to read as a live foreign holder: the 21.11.2026 release E2E attempt 5 waited its whole
1800 s budget on attempt 4's lease. rig_lease_acquire now treats a holder with the caller's own
repo + run_id as its own earlier attempt and reclaims it. Any other holder is unchanged.
"""
import json
import subprocess
from pathlib import Path

LIB = Path(__file__).resolve().parents[2] / "scripts" / "lib" / "rig-lease.sh"


def _acquire(lease_dir, repo, run_id):
    script = (
        'set -euo pipefail\n. "$1"\nset +e\n'
        'rig_lease_acquire "$2" "$3" "https://example.invalid/run" full-path 2099-01-01T00:00:00Z\n'
        'echo "RC=$?"\n'
    )
    env = {"PATH": "/usr/bin:/bin", "RIG_LEASE_DIR": str(lease_dir), "HOME": str(lease_dir.parent)}
    out = subprocess.run(["bash", "-c", script, "t", str(LIB), repo, run_id],
                         capture_output=True, text=True, env=env, timeout=60)
    return out.stdout, out.stderr


def _holder(lease_dir):
    return json.loads((lease_dir / "holder.json").read_text())


def test_a_rerun_reclaims_its_own_previous_attempts_lease(tmp_path):
    lease = tmp_path / "rig-lease"
    out, _ = _acquire(lease, "zbynekdrlik/camera-box", "44881510158")
    assert "RIG_LEASE_ACQUIRED" in out and "RC=0" in out
    out, err = _acquire(lease, "zbynekdrlik/camera-box", "44881510158")
    assert "RIG_LEASE_RECLAIMED" in out and "RC=0" in out, (out, err)
    assert "same run" in err
    assert _holder(lease)["run_id"] == "44881510158"


def test_a_different_run_of_the_same_repo_still_waits(tmp_path):
    lease = tmp_path / "rig-lease"
    _acquire(lease, "zbynekdrlik/camera-box", "111")
    out, _ = _acquire(lease, "zbynekdrlik/camera-box", "222")
    assert "RIG_LEASE_HELD_BY=zbynekdrlik/camera-box#111" in out and "RC=1" in out
    assert _holder(lease)["run_id"] == "111"


def test_the_same_run_id_of_another_repo_still_waits(tmp_path):
    lease = tmp_path / "rig-lease"
    _acquire(lease, "zbynekdrlik/restreamer", "555")
    out, _ = _acquire(lease, "zbynekdrlik/camera-box", "555")
    assert "RIG_LEASE_HELD_BY=zbynekdrlik/restreamer#555" in out and "RC=1" in out
    assert _holder(lease)["repo"] == "zbynekdrlik/restreamer"
