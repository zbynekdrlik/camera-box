//! #746 — the Full-path E2E gate's "detect docs-only diff" step (#646) died BEFORE any E2E ran:
//! its first command, `gh pr diff <N> --name-only`, hit GitHub's `PullRequest.diff too_large` API
//! refusal once PR #704 (a long-lived dev->main accumulator train) crossed the API's diff-size
//! limit, and the step's `set -euo pipefail` turned that refusal into an immediate `exit 1` —
//! killing the WHOLE job before the rig-busy-gate or the recording-e2e step ever ran. Net effect:
//! the REQUIRED merge gate could no longer produce a result at all, for any future push to that
//! PR, regardless of rig health (found on gate run 29264394882, commit ed8831da, the #744 push).
//!
//! Fix: derive the changed-file list LOCALLY (`git fetch origin main` + `git diff --name-only
//! origin/main HEAD` — a direct tree-to-tree diff against the fetched tip, deliberately NOT the
//! triple-dot merge-base form, since `actions/checkout@v4`'s default shallow depth=1 checkout
//! has no shared history to compute a merge-base from) instead of the GitHub API, which has no
//! size limit on a local git diff. AND fail OPEN: any failure deriving the list is treated as
//! "NOT docs-only" (the full E2E always runs) rather than ever exiting the step non-zero — a
//! broken OPTIMIZATION (the docs-only skip) must never block, or silently skip, the REQUIRED
//! gate itself.
//!
//! This supersedes (does not merely add to) the #646 mechanism assertions in
//! tests/harness_full_path_e2e_docs_only_skip.rs — see that file's own updated comments for what
//! changed there.

use std::fs;

fn read_workflow() -> String {
    let path = format!(
        "{}/.github/workflows/full-path-e2e.yml",
        env!("CARGO_MANIFEST_DIR")
    );
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Isolate the `id: docs-only` step's own YAML block (from its `- name:` line to the next
/// step's `- name:` line), same slicing convention as harness_full_path_e2e_docs_only_skip.rs.
fn docs_only_step_block(s: &str) -> &str {
    let step_pos = s
        .find("id: docs-only")
        .expect("id: docs-only step must exist");
    let block_start = s[..step_pos].rfind("- name:").unwrap_or(0);
    let next_step_rel = s[step_pos..].find("\n      - name:");
    let block_end = next_step_rel.map(|r| step_pos + r).unwrap_or(s.len());
    &s[block_start..block_end]
}

#[test]
fn docs_only_step_no_longer_uses_the_pr_diff_api_746() {
    let s = read_workflow();
    let block = docs_only_step_block(&s);
    assert!(
        !block.contains("gh pr diff"),
        "#746: the docs-only step must no longer call `gh pr diff` — it hits GitHub's \
         `PullRequest.diff too_large` refusal on an oversized long-lived PR train (e.g. #704): \
         {block}"
    );
}

#[test]
fn docs_only_step_derives_changed_files_via_local_git_diff_746() {
    let s = read_workflow();
    let block = docs_only_step_block(&s);
    assert!(
        block.contains("git fetch origin main"),
        "#746: the docs-only step must fetch main locally (no API size limit): {block}"
    );
    assert!(
        block.contains("git diff --name-only origin/main"),
        "#746: the docs-only step must derive changed files via a local git diff: {block}"
    );
    assert!(
        !block.contains("origin/main...HEAD") && !block.contains("origin/main... HEAD"),
        "#746: must NOT use the triple-dot merge-base form — actions/checkout@v4's default \
         shallow (depth=1) checkout has no shared history to compute a merge-base from, so a \
         triple-dot diff would itself fail on every run: {block}"
    );
}

#[test]
fn docs_only_step_fails_open_never_set_dash_e_746() {
    let s = read_workflow();
    let block = docs_only_step_block(&s);
    assert!(
        !block.contains("set -euo pipefail") && !block.contains("set -eo pipefail"),
        "#746: the docs-only step must NOT enable `-e` — a detection failure must fail OPEN \
         (docs_only=false, full E2E runs), never exit the step non-zero: {block}"
    );
    let lower = block.to_lowercase();
    assert!(
        lower.contains("fail") && lower.contains("open"),
        "#746: the docs-only step must document its fail-open fallback: {block}"
    );
}

#[test]
fn docs_only_step_always_writes_its_output_exactly_once_746() {
    // Structural pin: the docs_only=$DOCS_ONLY GITHUB_OUTPUT write must appear exactly ONCE,
    // AFTER the detection if/else (never duplicated once per branch, which risks one branch
    // forgetting it and leaving the step's output unset).
    let s = read_workflow();
    let block = docs_only_step_block(&s);
    let output_writes = block.matches("docs_only=$DOCS_ONLY").count();
    assert_eq!(
        output_writes, 1,
        "#746: expected exactly one `docs_only=$DOCS_ONLY` GITHUB_OUTPUT write (after the \
         detection if/else, so both the success and the fail-open path reach it): {block}"
    );
}

#[test]
fn permissions_no_longer_require_pull_requests_read_746() {
    let s = read_workflow();
    let perm_pos = s
        .find("permissions:")
        .expect("permissions: block must exist");
    let jobs_pos = s.find("\njobs:").unwrap_or(s.len());
    let perm_block = &s[perm_pos..jobs_pos];
    assert!(
        !perm_block.contains("pull-requests: read"),
        "#746: pull-requests: read is no longer needed once the docs-only step stops calling \
         `gh pr diff` (least privilege): {perm_block}"
    );
}
