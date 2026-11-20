"""issue 1538: a standalone `bkshading-deploy-relay.sh --box <cam>` reads the rig lease's issue-1450
`resources` for the camera's OWN venue (`<venue>-cams`, the venue from the strih fact files), so a
lease held only for another box (songplayer `e2e-resolume` holding `resolume-snv`) no longer blocks a
PP relay deploy. A lease that holds the camera's venue cameras still refuses (exit 6), and a holder
without `resources` (today's whole-rig holder) still refuses every box.
"""
import json
import os
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_bkshading_deploy_relay_808 import _local_sha, _rig_env, _run_script  # noqa: E402


def _lease(lease_dir, resources=None):
    os.makedirs(lease_dir, exist_ok=True)
    holder = {"repo": "zbynekdrlik/songplayer", "run_id": "44819268351", "run_url": "u",
              "job": "e2e-resolume", "expected_release_at": ""}
    if resources is not None:
        holder["resources"] = resources
    with open(os.path.join(lease_dir, "holder.json"), "w") as f:
        f.write(json.dumps(holder))
    hb = os.path.join(lease_dir, "heartbeat")
    open(hb, "w").close()
    t = time.time()
    os.utime(hb, (t, t))


def _deploy(box, resources):
    tmp = tempfile.mkdtemp()
    fake_bin = os.path.join(tmp, "bkshading-relay")
    with open(fake_bin, "wb") as f:
        f.write(b"RELAYBINARYCONTENT")
    env, log = _rig_env(tmp, _local_sha(fake_bin))
    env.update(RIG_LEASE_REPO="", RIG_LEASE_RUN_ID="", GITHUB_REPOSITORY="", GITHUB_RUN_ID="",
               BKSHADING_DEPLOY_LEASE_RESOURCE="")
    _lease(env["RIG_LEASE_DIR"], resources)
    r = _run_script(["--host", "10.76.9.66", "--box", box, "--binary", fake_bin], env=env)
    calls = open(log).read() if os.path.exists(log) else ""
    return r, r.stdout + r.stderr, calls


def test_a_lease_holding_only_resolume_snv_lets_a_pp_camera_deploy():
    r, out, calls = _deploy("cam6", ["resolume-snv"])
    assert r.returncode == 0, out
    assert "for other resources, not pp-cams" in out, out
    assert "SCP" in calls, calls


def test_a_lease_holding_the_pp_cameras_refuses_a_pp_camera():
    r, out, calls = _deploy("cam6", ["pp-cams"])
    assert r.returncode == 6, out
    assert "rig lease is held" in out, out
    assert calls == "", calls


def test_a_lease_holding_the_snv_cameras_refuses_an_snv_camera_but_not_a_pp_one():
    r, out, _ = _deploy("cam3", ["snv-cams"])
    assert r.returncode == 6, out
    r, out, _ = _deploy("cam6", ["snv-cams"])
    assert r.returncode == 0, out


def test_a_lease_without_resources_still_refuses_every_box():
    r, out, calls = _deploy("cam6", None)
    assert r.returncode == 6, out
    assert calls == "", calls
