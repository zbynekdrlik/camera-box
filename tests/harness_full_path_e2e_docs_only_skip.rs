//! #646 — full-path-e2e gate: skip busy-gate+E2E on docs-only PRs (runtime if:, never
//! on.pull_request.paths).
//!
//! ## The gap
//!
//! Every dev->main PR triggers the full rig E2E (30-min busy-wait budget + a real recording
//! run), even a docs-only change with zero code/script impact — correct-but-wasteful.
//!
//! ## The trap this MUST avoid
//!
//! `on.pull_request.paths` (or `paths-ignore`) at the TRIGGER level means a docs-only PR produces
//! NO check run at all for this workflow. A required status check with no check run ever created
//! reads as eternally "pending" to branch protection — it can NEVER go green, and the PR is
//! blocked from merging forever. The fix MUST be a runtime `if:` condition on the STEPS (the job
//! still runs and reports a genuine "skipped, docs-only" result), never a trigger-level path
//! filter. Same static-read content-assert style as tests/harness_full_path_e2e_workflow.rs.

use std::fs;

fn read_workflow() -> String {
    let path = format!(
        "{}/.github/workflows/full-path-e2e.yml",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The exact trap named in the issue: NEVER `on.pull_request.paths` / `paths-ignore` anywhere in
/// the workflow — those silently produce no check run for a docs-only PR, permanently blocking
/// branch protection's required check.
#[test]
fn full_path_e2e_yml_never_uses_trigger_level_path_filter() {
    let s = read_workflow();
    assert!(
        !s.contains("paths-ignore:"),
        "#646: full-path-e2e.yml must NEVER use `paths-ignore:` at the trigger level — a \
         docs-only PR would get no check run, permanently blocking branch protection"
    );
    // `paths:` under the pull_request trigger specifically (not a stray unrelated key elsewhere).
    let pr_pos = s.find("pull_request:").expect("pull_request trigger must exist");
    let jobs_pos = s.find("\njobs:").unwrap_or(s.len());
    let trigger_block = &s[pr_pos..jobs_pos];
    assert!(
        !trigger_block.contains("paths:"),
        "#646: the pull_request trigger must NEVER be path-filtered (paths:) — that produces no \
         check run for a docs-only PR: {trigger_block}"
    );
}

/// A docs-only-detection step must exist, gated to pull_request events only (a workflow_dispatch
/// manual soak has no PR diff to inspect and must never be treated as docs-only), and must derive
/// the changed-files list via the GitHub API/CLI (`gh pr diff`), never a trigger-level filter.
#[test]
fn full_path_e2e_yml_has_docs_only_detection_step() {
    let s = read_workflow();
    assert!(
        s.contains("id: docs-only"),
        "#646: a step with id: docs-only must exist so later steps can reference its output"
    );
    assert!(
        s.contains("gh pr diff"),
        "#646: the docs-only detection must derive the changed-files list via `gh pr diff` \
         (runtime, not a trigger-level path filter)"
    );
    assert!(
        s.contains("docs_only=") && s.contains("GITHUB_OUTPUT"),
        "#646: the docs-only step must write its verdict to GITHUB_OUTPUT (docs_only=...)"
    );
}

/// The docs-only detection step itself must run only for `pull_request` events — a
/// workflow_dispatch (manual operator soak) has no PR to diff and must always run in full.
#[test]
fn full_path_e2e_yml_docs_only_step_is_pull_request_only() {
    let s = read_workflow();
    let step_pos = s
        .find("id: docs-only")
        .expect("id: docs-only step must exist");
    // Look at the step's own block: from its `- name:` line (search backwards a little) to the
    // next `- name:` line.
    let block_start = s[..step_pos].rfind("- name:").unwrap_or(0);
    let next_step_rel = s[step_pos..].find("\n      - name:");
    let block_end = next_step_rel.map(|r| step_pos + r).unwrap_or(s.len());
    let block = &s[block_start..block_end];
    assert!(
        block.contains("github.event_name == 'pull_request'"),
        "#646: the docs-only detection step must be gated to pull_request events only: {block}"
    );
}

/// The docs-only step must run BEFORE the busy-gate step (its output is consumed by both the
/// busy-gate and the recording step's `if:` conditions).
#[test]
fn full_path_e2e_yml_docs_only_check_runs_before_busy_gate() {
    let s = read_workflow();
    let docs_only_pos = s
        .find("id: docs-only")
        .expect("id: docs-only step must exist");
    let busy_gate_pos = s
        .find("run: bash scripts/rig-busy-gate.sh")
        .expect("rig-busy-gate.sh step must still exist");
    assert!(
        docs_only_pos < busy_gate_pos,
        "#646: the docs-only detection step must run BEFORE rig-busy-gate.sh (its output gates \
         that step): docs_only_pos={docs_only_pos}, busy_gate_pos={busy_gate_pos}"
    );
}

/// The rig-busy-gate step must be SKIPPED (not just "does nothing useful") on a confirmed
/// docs-only PR, via a runtime `if:` referencing the docs-only step's output — but must still
/// run on a workflow_dispatch (manual soak) regardless.
#[test]
fn full_path_e2e_yml_busy_gate_step_is_conditioned_on_docs_only() {
    let s = read_workflow();
    let step_name_pos = s
        .find("name: Rig-busy gate")
        .expect("the rig-busy-gate step must still exist");
    let run_pos = s
        .find("run: bash scripts/rig-busy-gate.sh")
        .expect("rig-busy-gate.sh must still be invoked");
    let block = &s[step_name_pos..run_pos];
    assert!(
        block.contains("steps.docs-only.outputs.docs_only"),
        "#646: the rig-busy-gate step must have an `if:` condition referencing \
         steps.docs-only.outputs.docs_only: {block}"
    );
    assert!(
        block.contains("workflow_dispatch"),
        "#646: the rig-busy-gate step's if: must still allow workflow_dispatch (manual soak \
         always runs in full, never skipped as docs-only): {block}"
    );
}

/// The recording-e2e step must be equally conditioned — a docs-only PR skips BOTH the busy-gate
/// AND the E2E recording, never just one.
#[test]
fn full_path_e2e_yml_recording_step_is_conditioned_on_docs_only() {
    let s = read_workflow();
    let step_name_pos = s
        .find("Recording-based 4-node cam2")
        .expect("the recording-e2e step must still exist");
    let run_pos = s
        .find("run: bash scripts/recording-e2e.sh")
        .expect("recording-e2e.sh must still be invoked");
    let block = &s[step_name_pos..run_pos];
    assert!(
        block.contains("steps.docs-only.outputs.docs_only"),
        "#646: the recording-e2e step must have an `if:` condition referencing \
         steps.docs-only.outputs.docs_only: {block}"
    );
    assert!(
        block.contains("workflow_dispatch"),
        "#646: the recording-e2e step's if: must still allow workflow_dispatch: {block}"
    );
}

/// `gh pr diff` needs read access to the PR — the workflow's permissions block must grant it
/// explicitly (least-privilege, not relying on an implicit default).
#[test]
fn full_path_e2e_yml_permissions_include_pull_requests_read() {
    let s = read_workflow();
    let perm_pos = s.find("permissions:").expect("permissions: block must exist");
    let jobs_pos = s.find("\njobs:").unwrap_or(s.len());
    let perm_block = &s[perm_pos..jobs_pos];
    assert!(
        perm_block.contains("pull-requests: read"),
        "#646: permissions must include pull-requests: read (gh pr diff needs it): {perm_block}"
    );
}
