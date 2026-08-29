//! #406/#312 item5 — full-path-e2e.yml is now an ACTUAL BLOCKING merge gate (pull_request
//! trigger + rig-busy-gate step + fork guard), not merely a manual `workflow_dispatch` soak.
//!
//! Content-assert guards (YAML text, not a live GH Actions run — matches this repo's existing
//! workflow-file test style, e.g. tests/appliance_boot_hardening.rs / probe_windows_buildable.rs)
//! so a future edit cannot silently drop the automatic trigger, the fork-PR guard, the busy-gate
//! step, or reorder the busy-gate AFTER the recording step — any one of those would either
//! re-open the "operator is the guard" gap this ticket closes, or let an untrusted fork PR touch
//! production hardware.

use std::fs;

fn read_workflow() -> String {
    let path = format!(
        "{}/.github/workflows/full-path-e2e.yml",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The automatic BLOCKING merge gate needs a `pull_request` trigger targeting main — without
/// it, the busy-gate + E2E never run automatically and nothing can be required at merge time.
#[test]
fn full_path_e2e_yml_has_pull_request_trigger_targeting_main() {
    let s = read_workflow();
    assert!(
        s.contains("pull_request:"),
        "#406/#312 item5: full-path-e2e.yml must have a `pull_request:` trigger"
    );
    let pr_pos = s.find("pull_request:").unwrap();
    let jobs_pos = s.find("\njobs:").unwrap_or(s.len());
    let trigger_block = &s[pr_pos..jobs_pos];
    assert!(
        trigger_block.contains("branches: [main]") || trigger_block.contains("branches:\n"),
        "the pull_request trigger must target main: {trigger_block}"
    );
    for expected in ["opened", "synchronize", "reopened", "ready_for_review"] {
        assert!(
            trigger_block.contains(expected),
            "pull_request trigger must include type {expected:?}: {trigger_block}"
        );
    }
    // workflow_dispatch must still exist (manual operator soak path preserved).
    assert!(
        s.contains("workflow_dispatch:"),
        "workflow_dispatch must remain available for a manual operator soak"
    );
}

/// A fork PR must never trigger this job — it reroutes + records PRODUCTION OBS on a LAN-only
/// self-hosted runner, which must never run untrusted fork code.
#[test]
fn full_path_e2e_yml_job_has_fork_pr_guard() {
    let s = read_workflow();
    assert!(
        s.contains("github.event.pull_request.head.repo.full_name == github.repository"),
        "the `full-path` job must guard against fork PRs (compare head.repo.full_name to \
         github.repository) — an automatic trigger on untrusted fork code would touch \
         production hardware."
    );
    assert!(
        s.contains("github.event_name == 'workflow_dispatch'"),
        "the fork guard must still allow workflow_dispatch (always same-repo, operator-run)"
    );
}

/// The rig-busy-gate step must be the FIRST step (after checkout) — running it AFTER the
/// recording step would defeat the entire point of the gate.
#[test]
fn full_path_e2e_yml_busy_gate_runs_before_the_recording_step() {
    let s = read_workflow();
    // Match the actual step INVOCATION lines (`run: bash scripts/...`), not any mention of the
    // script names in header prose/comments (which appear earlier and would false-order this).
    let busy_gate_pos = s
        .find("run: bash scripts/rig-busy-gate.sh")
        .expect("full-path-e2e.yml must invoke scripts/rig-busy-gate.sh as a job step");
    let recording_pos = s
        .find("run: exec bash scripts/recording-e2e.sh")
        .expect("full-path-e2e.yml must still invoke scripts/recording-e2e.sh (via `exec`, #657)");
    assert!(
        busy_gate_pos < recording_pos,
        "#406/#312 item5: rig-busy-gate.sh MUST run BEFORE recording-e2e.sh in the job — \
         busy_gate_pos={busy_gate_pos}, recording_pos={recording_pos}"
    );
}

/// The busy-gate + E2E must be steps of the SAME job (not split into a separate `needs:`-
/// dependent job) — a skipped dependency job would be treated as "passing" by branch
/// protection, which is the exact fail-open hole this design avoids.
#[test]
fn full_path_e2e_yml_busy_gate_and_recording_are_one_job_not_split() {
    let s = read_workflow();
    // Exactly one top-level job key under `jobs:` (2-space indented `<name>:` lines).
    let jobs_pos = s.find("\njobs:").expect("workflow must have jobs:");
    let job_section = &s[jobs_pos..];
    // A top-level job key is indented EXACTLY 2 spaces and ends with ':' (its own properties —
    // name, if, runs-on, timeout-minutes, concurrency, steps — are indented 4+ spaces).
    let job_key_lines: Vec<&str> = job_section
        .lines()
        .skip(1) // skip the "jobs:" line itself
        .filter(|l| {
            let trimmed = l.trim_start();
            let indent = l.len() - trimmed.len();
            indent == 2 && trimmed.ends_with(':') && !trimmed.starts_with('#')
        })
        .collect();
    assert_eq!(
        job_key_lines.len(),
        1,
        "the busy-gate and the recording E2E must stay in ONE job (never split so a skipped \
         dependency reads as passing to branch protection); found job key lines: {job_key_lines:?}"
    );
    assert!(job_key_lines[0].trim().starts_with("full-path:"));
    assert!(
        !s.contains("needs:"),
        "no job in this workflow should depend on another via needs: (single-job design)"
    );
}

/// The job needs headroom for the 30-min busy-wait budget PLUS the recording + decode; 60 min
/// (the old manual-only budget) is no longer enough.
#[test]
fn full_path_e2e_yml_job_timeout_has_busy_gate_headroom() {
    let s = read_workflow();
    assert!(
        s.contains("timeout-minutes: 75"),
        "timeout-minutes must have headroom for the 30-min busy-gate budget + recording + \
         decode (>= 75 recommended)"
    );
}

/// A workflow-level concurrency group (PR/ref-scoped, cancel-in-progress) supersedes a stale
/// re-push, AND the job-level hardware-serialization group (shared with loopback-e2e.yml) must
/// still exist — losing either reintroduces a real risk (wasted runner churn, or two hardware
/// workflows racing the same physical rig).
#[test]
fn full_path_e2e_yml_has_both_workflow_and_job_level_concurrency() {
    let s = read_workflow();

    // Workflow-level concurrency block appears BEFORE `jobs:`.
    let jobs_pos = s.find("\njobs:").expect("workflow must have jobs:");
    let wf_section = &s[..jobs_pos];
    assert!(
        wf_section.contains("group: full-path-e2e-"),
        "workflow-level concurrency group must be scoped to this workflow (PR/ref-keyed): \
         {wf_section}"
    );
    assert!(
        wf_section.contains("cancel-in-progress: true"),
        "workflow-level concurrency must cancel a superseded run on the same PR/ref"
    );

    // Job-level concurrency block appears AFTER `jobs:`, inside the `full-path` job.
    let job_section = &s[jobs_pos..];
    assert!(
        job_section.contains("group: loopback-e2e-hardware"),
        "job-level concurrency must keep serializing with loopback-e2e.yml's exact hardware group"
    );
    assert!(
        job_section.contains("cancel-in-progress: false"),
        "the hardware-serialization group must NEVER cancel-in-progress (queue instead, never \
         kill a run mid-hardware-operation)"
    );
}

/// The job must have an explicit `name:` — the required-status-check context branch protection
/// keys on is exactly this name; an unnamed job's auto-generated context could drift on refactor.
#[test]
fn full_path_e2e_yml_job_has_explicit_name() {
    let s = read_workflow();
    let jobs_pos = s.find("\njobs:").expect("workflow must have jobs:");
    let job_section = &s[jobs_pos..];
    let full_path_pos = job_section
        .find("full-path:")
        .expect("the `full-path` job must exist");
    let after = &job_section[full_path_pos..];
    let name_line = after
        .lines()
        .find(|l| l.trim_start().starts_with("name:"))
        .expect("the full-path job must have an explicit `name:` (branch protection's required check keys on it)");
    assert!(
        !name_line.trim_end().ends_with("name:"),
        "name: must not be empty"
    );
}

/// The recording step's DURATION must default to the 300s acceptance floor when no
/// workflow_dispatch input exists (a pull_request-triggered run) — never fall through to
/// recording-e2e.sh's own internal 1800s default (a needlessly long PR-gate run).
#[test]
fn full_path_e2e_yml_duration_defaults_to_300_for_pr_triggered_runs() {
    let s = read_workflow();
    assert!(
        s.contains("DURATION: ${{ inputs.duration_secs || '300' }}"),
        "the recording step's DURATION env must fall back to '300' when inputs.duration_secs is \
         empty (a pull_request-triggered run has no workflow_dispatch inputs)"
    );
}

// ---------------------------------------------------------------------------------------------
// #406 — the EPIC's own acceptance bar: "the REQUIRED pull_request job never sets ALL_CAMBOX=1,
// so the blocking gate exercises NEITHER the multi-camera cross-camera spread (#624/#286) NOR
// the #641 A/V-offset gate — both exist in code but only in the on-demand sweep." User directive
// 2026-07-11 evening: flip it NOW (prerequisite #681 measurement-trust work already landed) —
// every PR must pass the fused 6-camera zero-loss + delivery-latency-spread + A/V-offset gate
// (fail-closed) to merge. recording-e2e.sh's ALL_CAMBOX=1 path self-wires everything else it
// needs (switch schedule, cam2's continuous QPSK audio marker via its own AV_SYNC_MARKER_DEVICE/
// CADENCE defaults, AV_EXPECTED_MS defaulting to the #1178 calibrated rig video-leg) — the workflow only needs to set the ONE
// flag. Scoped to `pull_request` only (via the ternary) so a manual `workflow_dispatch` operator
// soak keeps its existing single-camera behavior unless a future ticket adds an explicit input.
// ---------------------------------------------------------------------------------------------

#[test]
fn full_path_e2e_yml_sets_all_cambox_for_pull_request_triggered_runs() {
    let s = read_workflow();
    assert!(
        s.contains("ALL_CAMBOX: ${{ github.event_name == 'pull_request' && '1' || '0' }}"),
        "#406: the required pull_request job must set ALL_CAMBOX=1 so the automatic merge gate \
         runs the FUSED multi-camera sweep (cross-camera spread + #641 A/V-offset gate) — not \
         just the single-camera path. Must be scoped to pull_request (leaving workflow_dispatch's \
         manual-soak behavior unchanged)."
    );
}

// ---------------------------------------------------------------------------------------------
// #830 — the shared cross-repo rig lease: camera-box's full-path-e2e.yml and restreamer's Rust
// CI both drive the SAME physical rig from the SAME self-hosted dev1 runner with no mutual
// exclusion (live collision 2026-07-27). The busy-gate step must identify THIS run to the lease
// (repo/run_id/run_url/job), and a SEPARATE `always()` step must release it — never only on
// rig-busy-gate.sh's own success path (that would leave the lease dangling on cancellation or a
// later step's failure).
// ---------------------------------------------------------------------------------------------

/// The busy-gate step must pass its OWN run identity to the shared lease — otherwise
/// scripts/rig-busy-gate.sh falls back to a local-only identity that a foreign gate can't use to
/// find the run (no run_url to report, no way to check "is this run still going").
#[test]
fn full_path_e2e_yml_busy_gate_step_sets_rig_lease_identity_env() {
    let s = read_workflow();
    let step_pos = s
        .find("name: Rig-busy gate")
        .expect("the rig-busy-gate step must exist");
    let run_pos = s[step_pos..]
        .find("run: bash scripts/rig-busy-gate.sh")
        .map(|p| p + step_pos)
        .expect("the rig-busy-gate step must invoke scripts/rig-busy-gate.sh");
    let step_block = &s[step_pos..run_pos];
    for expected in [
        "RIG_LEASE_REPO:",
        "RIG_LEASE_RUN_ID:",
        "RIG_LEASE_RUN_URL:",
        "RIG_LEASE_JOB:",
    ] {
        assert!(
            step_block.contains(expected),
            "#830: the rig-busy-gate step's env: block must set {expected} so the shared rig \
             lease can identify THIS run: {step_block}"
        );
    }
}

/// A dedicated release step must exist, run `if: always()` (success, failure, AND cancellation),
/// and invoke the release wrapper — never rely solely on rig-busy-gate.sh's own success path,
/// which deliberately leaves the lease HELD for the recording step that follows it.
#[test]
fn full_path_e2e_yml_has_always_release_lease_step() {
    let s = read_workflow();
    let step_pos = s
        .find("name: Release the shared rig lease")
        .expect("#830: a dedicated 'Release the shared rig lease' step must exist");
    let after = &s[step_pos..];
    let step_end = after
        .find("\n      - name:")
        .map(|p| p + step_pos)
        .unwrap_or(s.len());
    let step_block = &s[step_pos..step_end];
    assert!(
        step_block.contains("if: always()"),
        "#830: the lease-release step must run `if: always()` (success, earlier-step failure, \
         AND cancellation) — never gated on the earlier steps' own outcome: {step_block}"
    );
    assert!(
        step_block.contains("run: bash scripts/rig-lease-release.sh"),
        "the release step must invoke scripts/rig-lease-release.sh: {step_block}"
    );
}

/// The ALL_CAMBOX env line must live in the SAME step (env: block) as DURATION/
/// NDI_RUNTIME_DIR_V6 — i.e. it actually reaches recording-e2e.sh's invocation, not just sit
/// somewhere else in the file where it would never be read by the script.
#[test]
fn full_path_e2e_yml_all_cambox_is_in_the_recording_steps_env_block() {
    let s = read_workflow();
    let step_pos = s
        .find("name: Recording-based 4-node cam2")
        .expect("the recording step must exist");
    let run_pos = s[step_pos..]
        .find("run: exec bash scripts/recording-e2e.sh")
        .map(|p| p + step_pos)
        .expect("the recording step must invoke recording-e2e.sh");
    let step_block = &s[step_pos..run_pos];
    assert!(
        step_block.contains("ALL_CAMBOX:"),
        "#406: ALL_CAMBOX must be set inside the recording step's own env: block (between its \
         `name:` and its `run:` line), not elsewhere in the file — recording-e2e.sh only sees \
         env vars set on ITS OWN step. step_block:\n{step_block}"
    );
}
