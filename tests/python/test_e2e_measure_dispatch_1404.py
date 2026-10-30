"""issue 1404 -- a MEASURING workflow_dispatch of the full-path E2E (restreamer's release gate).

`measure: true` must run exactly what the pull_request gate runs: the fused ALL_CAMBOX sweep,
E2E_EXECUTE_VERDICT=1 (real decode + verdict + YouTube stage), the #703 verdict-binary fetch for
the dispatched commit, the fail-closed verdict guard and the per-point report. A plain dispatch
stays the operator soak. Parsed from the workflow YAML, never from a text grep of one line.
"""

import pathlib

import yaml

_WF = pathlib.Path(__file__).resolve().parents[2] / ".github" / "workflows" / "full-path-e2e.yml"
MEASURE = "(github.event_name == 'pull_request' || inputs.measure)"


def _wf():
    return yaml.safe_load(_WF.read_text(encoding="utf-8"))


def _steps():
    return _wf()["jobs"]["full-path"]["steps"]


def _step(prefix):
    hits = [s for s in _steps() if s.get("name", "").startswith(prefix)]
    assert len(hits) == 1, (prefix, [s.get("name") for s in _steps()])
    return hits[0]


def test_dispatch_declares_measure_and_caller():
    on = _wf()[True]  # PyYAML reads the bare `on:` key as True
    inputs = on["workflow_dispatch"]["inputs"]
    assert inputs["measure"]["type"] == "boolean" and inputs["measure"]["default"] is False
    assert inputs["caller"]["default"] == ""


def test_measure_turns_on_every_pr_gate_part():
    rec = _step("Recording-based 4-node")
    assert rec["env"]["E2E_EXECUTE_VERDICT"] == "${{ " + MEASURE + " && '1' || '0' }}"
    assert rec["env"]["ALL_CAMBOX"] == "${{ " + MEASURE + " && '1' || '0' }}"
    for prefix in ("Fetch the matching Windows", "Per-point E2E report"):
        assert MEASURE in _step(prefix)["if"], prefix
    guards = [s for s in _steps() if "#703 fail-closed guard" in s.get("run", "")]
    assert len(guards) == 1 and MEASURE in guards[0]["if"]


def test_the_cold_cut_guard_sees_the_same_sweep_flag_as_the_recording_step():
    rec = _step("Recording-based 4-node")["env"]["ALL_CAMBOX"]
    cold = [s for s in _steps() if "cold_cut_bypass_arm_check" in s.get("run", "")]
    assert len(cold) == 1 and cold[0]["env"]["ALL_CAMBOX"] == rec


def test_the_verdict_binary_comes_from_the_dispatched_commit():
    run = _step("Fetch the matching Windows")["run"]
    assert 'SHA="${{ github.event.pull_request.head.sha || github.sha }}"' in run


def test_cg_chain_stays_a_pull_request_only_profile():
    env = _step("Recording-based 4-node")["env"]
    assert "inputs.measure" not in env["CG_CHAIN"]


def test_a_dispatch_report_has_no_pr_comment_and_names_its_caller():
    step = _step("Per-point E2E report")
    assert step["env"]["E2E_CALLER"] == "${{ inputs.caller }}"
    run = step["run"]
    assert 'if [ -n "$PR_NUMBER" ]; then' in run and run.index('if [ -n "$PR_NUMBER" ]') < run.index("gh pr comment")
    # the caller text reaches the shell only through the env, never interpolated into the script
    assert "inputs.caller" not in run
