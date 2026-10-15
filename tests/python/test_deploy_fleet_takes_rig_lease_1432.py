"""issue 1432 follow-up: the post-merge "Deploy to camera fleet" CI job restarts camera-box on every
active cambox, so it must take the shared rig lease first, exactly like the full-path E2E.

On 15.10.2026 03:33Z the job restarted cam2/cam3/cam5 while restreamer held the lease (its YouTube
E2E); the cam2 measurement marker dropped for one window, the program-audio sampler read FOREIGN and
restreamer's run was stopped. The job now runs scripts/rig-busy-gate.sh (waits for a live holder,
refuses while a box streams) before the SNV deploy and releases in an always() step right after it.
"""
from __future__ import annotations

import pathlib

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
CI = ROOT / ".github" / "workflows" / "ci.yml"


def _steps():
    wf = yaml.safe_load(CI.read_text())
    return wf["jobs"]["deploy-fleet"]["steps"]


def _index(steps, pred):
    for i, s in enumerate(steps):
        if pred(s):
            return i
    raise AssertionError("step not found")


def test_the_deploy_takes_the_rig_lease_through_the_busy_gate_before_the_snv_deploy():
    steps = _steps()
    gate = _index(steps, lambda s: s.get("run", "").strip() == "bash scripts/rig-busy-gate.sh")
    deploy = _index(steps, lambda s: "scripts/deploy-fleet.sh" in s.get("run", ""))
    assert gate < deploy
    env = steps[gate].get("env", {})
    assert env.get("RIG_LEASE_REPO") == "${{ github.repository }}"
    assert env.get("RIG_LEASE_RUN_ID") == "${{ github.run_id }}"
    assert "RIG_LEASE_RUN_URL" in env and "RIG_LEASE_JOB" in env
    assert "if" not in steps[gate], "the gate must never be skipped"


def test_the_lease_is_released_always_right_after_the_snv_deploy():
    steps = _steps()
    deploy = _index(steps, lambda s: "scripts/deploy-fleet.sh" in s.get("run", ""))
    release = _index(steps, lambda s: s.get("run", "").strip() == "bash scripts/rig-lease-release.sh")
    venues = _index(steps, lambda s: "dantesync_fleet_away_venues" in s.get("run", ""))
    assert deploy < release < venues, "released right after the SNV deploy, before the other venues"
    assert str(steps[release].get("if", "")).replace("${{", "").replace("}}", "").strip() == "always()"
    assert steps[release].get("env", {}).get("RIG_LEASE_RUN_ID") == "${{ github.run_id }}"
