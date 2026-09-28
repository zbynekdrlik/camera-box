"""issue 1367 -- the soak's one opt-in for the restart matrix: `--lease-run-id RUN_ID`.

The restart matrix (scripts/av-restart-matrix.sh) measures every window with the soak itself
(`av-soak.sh --run --hours 0`) while it holds the rig lease across its restarts. Under
`--lease-run-id` the soak must run under THAT lease: verify it is held by that run id, never acquire
it, never release it (neither in cleanup nor through `--stop-leftovers`), and refuse before touching
anything when the caller does not hold it. Without the flag nothing changes (the soak's own tests).

Reuses the soak's fake rig from tests/python/test_av_soak_orchestrator_1367.py (the `rig` fixture:
fake OBS / burns / sshpass / curl / decodes / verdict, a tmp RIG_LEASE_DIR -- never the real one).
"""
import json
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_av_soak_orchestrator_1367 import _calls, _csv_rows, _soak, rig  # noqa: E402,F401

CALLER = "av-matrix-20260928T010000Z-4242"
REPO = "camera-box-av-restart-matrix"
INHERIT = ("--lease-run-id", CALLER, "--lease-repo", REPO)


def hold_lease(p, run_id=CALLER, repo=REPO, age_s=0):
    """The caller's lease, acquired `age_s` ago (the hold ceiling counts from acquired_at)."""
    p["lease"].mkdir()
    acquired = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - age_s))
    (p["lease"] / "holder.json").write_text(json.dumps(
        {"repo": repo, "run_id": run_id, "run_url": "", "job": "av-restart-matrix",
         "acquired_at": acquired, "expected_release_at": "2099-01-01T00:00:00Z"}))
    (p["lease"] / "heartbeat").write_text("")


def holder(p):
    return json.loads((p["lease"] / "holder.json").read_text())["run_id"]


def test_a_window_under_the_callers_lease_never_acquires_or_releases_it(rig):
    env, p = rig
    hold_lease(p)
    r = _soak(env, "--run", *INHERIT)
    assert r.returncode in (0, 1, 2), r.stdout + r.stderr
    assert [row["outcome"] for row in _csv_rows(p["run"])] == ["ok"]
    assert p["lease"].exists() and holder(p) == CALLER, "the caller's lease is left exactly as it was"
    assert "rig lease released" not in r.stdout + r.stderr
    state = (p["run"] / "recording.state").read_text()
    assert "lease=\n" in state, "recording.state names no lease: --stop-leftovers must never release the caller's"
    assert "the caller's rig lease" in r.stdout + r.stderr


def test_the_callers_lease_must_be_held_before_anything_is_touched(rig):
    env, p = rig
    r = _soak(env, "--run", *INHERIT)
    assert r.returncode == 4, r.stdout + r.stderr
    assert "not held by the caller" in r.stderr
    assert p["log"].read_text() == "" and not p["lease"].exists()


def test_a_lease_held_by_another_run_is_refused_and_left_alone(rig):
    env, p = rig
    hold_lease(p, run_id="999", repo="camera-box")
    r = _soak(env, "--run", *INHERIT)
    assert r.returncode == 4, r.stdout + r.stderr
    assert holder(p) == "999"
    assert p["log"].read_text() == ""


def test_the_plan_names_the_callers_lease(rig):
    env, p = rig
    r = _soak(env, "--plan", *INHERIT)
    assert r.returncode == 0, r.stderr
    assert f"the caller's rig lease {CALLER}" in r.stdout
    assert "rig_lease_acquire" not in r.stdout
    assert p["log"].read_text() == "" and not p["lease"].exists()


def test_the_env_form_is_the_same_option(rig):
    env, p = rig
    hold_lease(p)
    r = _soak(dict(env, AV_SOAK_LEASE_RUN_ID=CALLER, AV_SOAK_LEASE_REPO=REPO), "--run")
    assert r.returncode in (0, 1, 2), r.stdout + r.stderr
    assert holder(p) == CALLER


def test_a_stuck_recording_under_the_callers_lease_keeps_it_and_stop_leftovers_never_releases_it(rig):
    env, p = rig
    hold_lease(p)
    r = _soak(dict(env, FAKE_STOP_NEVER="stream"), "--run", *INHERIT)
    assert r.returncode == 5, r.stdout + r.stderr
    assert "RECORDING MAY STILL BE RUNNING on stream" in r.stdout + r.stderr
    assert holder(p) == CALLER
    r = _soak(env, "--stop-leftovers", str(p["run"]))
    assert r.returncode == 0, r.stdout + r.stderr
    assert not (p["state"] / "recording-stream").exists(), "the leftover itself is stopped"
    assert p["lease"].exists() and holder(p) == CALLER, "the caller releases its own lease"
    assert "rig lease released" not in r.stdout + r.stderr


def test_every_slot_still_checks_the_lease_is_the_callers(rig):
    env, p = rig
    hold_lease(p)
    # the lease moves to another run during setup (at the first burn turned on): the slot must
    # refuse to record under a lease that is no longer the caller's, and leave that lease alone
    burn = os.path.join(env["AV_SOAK_OBS_DIR"], "obs_burn_filter.py")
    orig = burn + ".orig"
    os.rename(burn, orig)
    with open(burn, "w") as f:
        f.write(
            "#!/usr/bin/env python3\n"
            "import json, os, subprocess, sys\n"
            "if sys.argv[1:2] == ['add']:\n"
            "    hp = os.path.join(os.environ['RIG_LEASE_DIR'], 'holder.json')\n"
            "    h = json.load(open(hp)); h['run_id'] = 'foreign-run'; json.dump(h, open(hp, 'w'))\n"
            f"sys.exit(subprocess.call([sys.executable, {orig!r}] + sys.argv[1:]))\n")
    os.chmod(burn, 0o755)
    r = _soak(env, "--run", *INHERIT)
    assert r.returncode == 5, r.stdout + r.stderr
    assert "no longer ours" in r.stdout + r.stderr
    starts = [c for c in _calls(p["log"], "obs") if c[:1] == ["record"] and "start" in c]
    assert starts == [], "nothing is recorded under a lease that is not the caller's"
    assert holder(p) == "foreign-run", "the other run's lease is left alone"


def test_the_caller_names_its_lease_repo_too(rig):
    env, p = rig
    hold_lease(p)
    r = _soak(env, "--run", "--lease-run-id", CALLER)
    assert r.returncode == 3, r.stdout + r.stderr
    assert "--lease-repo" in r.stderr
    assert p["log"].read_text() == ""


def test_a_lease_with_the_callers_run_id_but_another_repo_is_refused(rig):
    env, p = rig
    hold_lease(p, repo="camera-box-av-soak")
    r = _soak(env, "--run", *INHERIT)
    assert r.returncode == 4, r.stdout + r.stderr
    assert p["log"].read_text() == ""


def test_the_window_runs_under_the_callers_hold_ceiling_not_its_own(rig):
    # the caller acquired 2 h ago: past the window's own declared hold (slot + 30 min) but inside the
    # caller's exported ceiling -- the window must keep the caller's lease, never stop beating it
    env, p = rig
    hold_lease(p, age_s=7200)
    r = _soak(dict(env, RIG_LEASE_MAX_HOLD_SECS="36000"), "--run", *INHERIT)
    assert r.returncode in (0, 1, 2), r.stdout + r.stderr
    assert [row["outcome"] for row in _csv_rows(p["run"])] == ["ok"]
    assert holder(p) == CALLER
    # and a caller past its OWN ceiling is refused before anything is touched
    env2 = dict(env, RIG_LEASE_MAX_HOLD_SECS="3600")
    p["log"].write_text("")
    import shutil
    shutil.rmtree(p["run"])
    r = _soak(env2, "--run", *INHERIT)
    assert r.returncode == 4, r.stdout + r.stderr
    assert p["log"].read_text() == ""
