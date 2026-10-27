//! Issue 1476: real release versions. Every main release is a clean `X.Y.Z` with a tag + a GitHub
//! Release; a dev bump is the next patch; a historical `1.7.0-dev.N` still parses and orders OLDER
//! than every 1.8.x.
//!
//! Pins:
//!   * `scripts/lib/release-version.sh` -- the one order rule (semver with pre-release), the
//!     plain-form check, the next-patch bump and the post-merge tag decision;
//!   * the workspace version is a plain `X.Y.Z` (a `-dev.N` on main is the bug this ticket fixes);
//!   * every camera-box version consumer reads BOTH forms (the [0/8] gate, the auto-align,
//!     verify-device) and the gate's PIN-DRIFT row names the direction;
//!   * the CI wiring: ci.yml tags the merge on main and calls release.yml as a reusable workflow
//!     (a tag pushed with GITHUB_TOKEN never starts another workflow), release.yml checks the
//!     binary against the tag and publishes it.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(manifest_dir().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Source `lib` under the caller's real `set -euo pipefail`, then run `body` with `set +e` so a
/// non-zero verdict can be printed. Values reach bash as positional ARGUMENTS, never inside the
/// script text. Returns stdout.
fn run_lib(lib: &str, body: &str, args: &[&str]) -> String {
    run_lib_in(lib, body, args, "C.UTF-8")
}

/// `run_lib` under a chosen locale (a bash bracket RANGE widens under en_US.UTF-8, so the lib's
/// regexes are also run there; a box without that locale falls back to C, which proves less but
/// never fails falsely).
fn run_lib_in(lib: &str, body: &str, args: &[&str], locale: &str) -> String {
    let harness = format!("set -euo pipefail\n. \"$LIB\"\nset +e\n{body}\n");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .arg("harness")
        .args(args)
        .env("LIB", manifest_dir().join(lib))
        .env("LC_ALL", locale)
        .output()
        .expect("run bash harness");
    assert!(
        out.status.success(),
        "harness exited non-zero: stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("unbound variable"),
        "the lib tripped set -u: {stderr}"
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const LIB: &str = "scripts/lib/release-version.sh";

fn cmp(a: &str, b: &str) -> String {
    run_lib(
        LIB,
        r#"out="$(release_version_cmp "$1" "$2")"; echo "rc=$? out=$out""#,
        &[a, b],
    )
    .trim()
    .to_string()
}

#[test]
fn a_historical_dev_build_is_older_than_the_first_real_release() {
    assert_eq!(cmp("1.7.0-dev.830", "1.8.0"), "rc=0 out=-1");
    assert_eq!(cmp("1.8.0", "1.7.0-dev.830"), "rc=0 out=1");
    assert_eq!(cmp("1.7.0-dev.999999", "1.8.0"), "rc=0 out=-1");
}

#[test]
fn a_release_ranks_above_every_pre_release_of_the_same_triple() {
    assert_eq!(cmp("1.7.0-dev.830", "1.7.0"), "rc=0 out=-1");
    assert_eq!(cmp("1.7.0", "1.7.0-dev.830"), "rc=0 out=1");
}

#[test]
fn versions_compare_as_numbers_never_as_text() {
    assert_eq!(cmp("1.8.9", "1.8.10"), "rc=0 out=-1");
    assert_eq!(cmp("1.10.0", "1.9.99"), "rc=0 out=1");
    assert_eq!(cmp("1.7.0-dev.99", "1.7.0-dev.100"), "rc=0 out=-1");
    assert_eq!(cmp("2.0.0", "1.99.99"), "rc=0 out=1");
    assert_eq!(cmp("1.8.0", "1.8.0"), "rc=0 out=0");
    assert_eq!(cmp("1.7.0-dev.5", "1.7.0-dev.5"), "rc=0 out=0");
}

#[test]
fn semver_pre_release_precedence_rules_hold() {
    // numeric below alphanumeric, a shorter equal prefix below a longer one, text order otherwise
    assert_eq!(cmp("1.0.0-1", "1.0.0-alpha"), "rc=0 out=-1");
    assert_eq!(cmp("1.0.0-alpha", "1.0.0-alpha.1"), "rc=0 out=-1");
    assert_eq!(cmp("1.0.0-alpha.beta", "1.0.0-beta"), "rc=0 out=-1");
}

#[test]
fn a_non_version_never_compares() {
    for (a, b) in [
        ("", "1.8.0"),
        ("1.8.0", "unknown"),
        ("v1.8.0", "1.8.0"),
        ("1.8", "1.8.0"),
        ("01.8.0", "1.8.0"),
        ("1.8.0 ", "1.8.0"),
        // a non-ASCII digit (Arabic-Indic 8) must never read as a version under any locale
        ("1.\u{0668}.0", "1.8.0"),
    ] {
        assert_eq!(cmp(a, b), "rc=2 out=", "{a:?} vs {b:?}");
    }
}

#[test]
fn a_non_ascii_digit_never_reads_as_a_version_under_a_utf8_locale() {
    for locale in ["en_US.UTF-8", "C.UTF-8"] {
        let out = run_lib_in(
            LIB,
            r#"out="$(release_version_cmp "$1" 1.8.0)"; echo "rc=$? out=$out""#,
            &["1.\u{0668}.0"],
            locale,
        );
        assert_eq!(out.trim(), "rc=2 out=", "{locale}");
    }
}

#[test]
fn the_lib_regexes_never_use_a_bracket_range() {
    // a range such as [0-9] follows the locale under bash; every set is spelled out
    let lib = read(LIB);
    for (n, line) in lib.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        for (i, _) in line.match_indices('[') {
            let body: String = line[i + 1..].chars().take_while(|c| *c != ']').collect();
            let ranged = body.as_bytes().windows(3).any(|w| {
                w[1] == b'-' && w[0].is_ascii_alphanumeric() && w[2].is_ascii_alphanumeric()
            });
            assert!(!ranged, "line {}: bracket range in {line:?}", n + 1);
        }
    }
}

#[test]
fn an_empty_pre_release_identifier_is_not_a_version() {
    assert_eq!(cmp("1.0.0-a..b", "1.0.0"), "rc=2 out=");
    assert_eq!(cmp("1.0.0-", "1.0.0"), "rc=2 out=");
    assert_eq!(cmp("1.0.0-.a", "1.0.0"), "rc=2 out=");
}

fn dev_check(dev: &str, main: &str) -> String {
    run_lib(
        LIB,
        r#"out="$(release_version_dev_check "$1" "$2")"; echo "rc=$? $out""#,
        &[dev, main],
    )
    .trim()
    .to_string()
}

#[test]
fn dev_must_be_a_clean_version_strictly_above_main() {
    assert_eq!(dev_check("1.8.0", "1.7.0-dev.830"), "rc=0 OK");
    assert_eq!(dev_check("1.8.1", "1.8.0"), "rc=0 OK");
    let out = dev_check("1.8.0", "1.8.0");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("bump") && out.contains("to 1.8.1"),
        "{out}"
    );
    let out = dev_check("1.8.0", "1.8.2");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("to 1.8.3"),
        "{out}"
    );
    let out = dev_check("1.8.1-dev.1", "1.8.0");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("clean X.Y.Z"),
        "{out}"
    );
    let out = dev_check("1.8.1", "");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("unreadable"),
        "{out}"
    );
    let out = dev_check("", "1.8.0");
    assert!(out.starts_with("rc=1 FAIL:"), "{out}");
}

#[test]
fn order_word_names_the_direction_and_never_fails() {
    let out = run_lib(
        LIB,
        r#"for p in "1.7.0-dev.830:1.8.0" "1.8.1:1.8.0" "1.8.0:1.8.0" ":1.8.0"; do
             echo "$(release_version_order_word "${p%%:*}" "${p#*:}")"
           done"#,
        &[],
    );
    assert_eq!(out, "OLDER\nNEWER\nSAME\nUNKNOWN\n");
}

#[test]
fn only_a_clean_x_y_z_is_a_release_version() {
    let out = run_lib(
        LIB,
        r#"for v in 1.8.0 0.0.1 1.80.123 1.7.0-dev.830 1.8.0-rc.1 v1.8.0 1.8 ""; do
             if release_version_is_plain "$v"; then echo "YES $v"; else echo "NO $v"; fi
           done"#,
        &[],
    );
    assert_eq!(
        out,
        "YES 1.8.0\nYES 0.0.1\nYES 1.80.123\nNO 1.7.0-dev.830\nNO 1.8.0-rc.1\nNO v1.8.0\nNO 1.8\nNO \n"
    );
}

#[test]
fn a_dev_bump_is_the_next_patch() {
    let out = run_lib(
        LIB,
        r#"release_version_next_patch 1.8.0; release_version_next_patch 1.8.9
           release_version_next_patch 1.7.0-dev.830; echo "rc=$?""#,
        &[],
    );
    assert_eq!(out, "1.8.1\n1.8.10\nrc=2\n");
}

fn decide(ver: &str, bin: &str, tag_sha: &str, head: &str) -> String {
    run_lib(
        LIB,
        r#"out="$(release_tag_decision "$1" "$2" "$3" "$4")"; echo "rc=$? $out""#,
        &[ver, bin, tag_sha, head],
    )
    .trim()
    .to_string()
}

#[test]
fn the_tag_decision_creates_a_missing_tag_and_accepts_its_own_re_run() {
    assert_eq!(decide("1.8.0", "1.8.0", "", "aaaa"), "rc=0 CREATE");
    assert_eq!(decide("1.8.0", "1.8.0", "aaaa", "aaaa"), "rc=0 EXISTS");
}

#[test]
fn the_tag_decision_never_moves_a_release_to_another_commit() {
    let out = decide("1.8.0", "1.8.0", "bbbb", "aaaa");
    assert!(out.starts_with("rc=1 FAIL:"), "{out}");
    assert!(
        out.contains("bbbb") && out.contains("aaaa") && out.contains("next patch"),
        "{out}"
    );
}

#[test]
fn the_tag_decision_refuses_a_pre_release_or_a_mismatched_binary() {
    let out = decide("1.7.0-dev.830", "1.7.0-dev.830", "", "aaaa");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("1.7.0-dev.830"),
        "{out}"
    );
    let out = decide("1.8.0", "1.7.0-dev.830", "", "aaaa");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("reports 1.7.0-dev.830"),
        "{out}"
    );
    let out = decide("1.8.0", "", "", "aaaa");
    assert!(
        out.starts_with("rc=1 FAIL:") && out.contains("<unreadable>"),
        "{out}"
    );
    let out = decide("", "1.8.0", "", "aaaa");
    assert!(out.starts_with("rc=1 FAIL:"), "{out}");
    let out = decide("1.8.0", "1.8.0", "", "");
    assert!(out.starts_with("rc=1 FAIL:"), "{out}");
}

#[test]
fn the_workspace_version_is_a_clean_release_version() {
    let toml = read("Cargo.toml");
    let line = toml
        .lines()
        .find(|l| l.starts_with("version = \""))
        .expect("the column-0 workspace version line");
    let v = line
        .trim_start_matches("version = \"")
        .trim_end_matches('"');
    let parts: Vec<&str> = v.split('.').collect();
    assert_eq!(parts.len(), 3, "{v} is not X.Y.Z");
    for p in &parts {
        assert!(
            !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()),
            "{v} is not a clean X.Y.Z release version (issue 1476)"
        );
    }
    let (maj, min): (u32, u32) = (parts[0].parse().unwrap(), parts[1].parse().unwrap());
    assert!(
        (maj, min) >= (1, 8),
        "{v} would order below the historical 1.7.0-dev.N builds"
    );
}

// --- the consumers read both forms ------------------------------------------------------------

#[test]
fn the_version_gate_reads_both_forms_from_camera_box_output() {
    let out = run_lib(
        "scripts/camera-box-version-gate.sh",
        r#"camera_box_version_from_version_output "camera-box 1.8.0"
           camera_box_version_from_version_output "camera-box 1.7.0-dev.830"
           camera_box_version_from_cargo_toml 'version = "1.8.0"'"#,
        &[],
    );
    assert_eq!(out, "1.8.0\n1.7.0-dev.830\n1.8.0\n");
}

#[test]
fn the_gate_pin_drift_row_names_older_or_newer_than_main() {
    let out = run_lib(
        "scripts/camera-box-version-gate.sh",
        r#"camera_box_pin_verdict cam3 1.7.0-dev.830 1.8.0; echo "RC=$?"
           camera_box_pin_verdict cam5 1.8.1 1.8.0; echo "RC=$?"
           camera_box_pin_verdict cam2 1.8.0 1.8.0; echo "RC=$?""#,
        &[],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].contains("PIN-DRIFT") && lines[0].contains("OLDER than main 1.8.0"),
        "{out}"
    );
    assert_eq!(lines[1], "RC=20");
    assert!(
        lines[2].contains("PIN-DRIFT") && lines[2].contains("NEWER than main 1.8.0"),
        "{out}"
    );
    assert_eq!(lines[3], "RC=20");
    assert!(
        lines[4].contains("OK") && !lines[4].contains("PIN-DRIFT"),
        "{out}"
    );
    assert_eq!(lines[5], "RC=0");
}

#[test]
fn the_auto_align_reads_both_forms_and_upgrades_a_historical_fleet() {
    let out = run_lib(
        "scripts/lib/camera-box-parity-align.sh",
        r#"cambox_align_version_from_output "camera-box 1.8.0"
           cambox_align_version_from_output "camera-box 1.7.0-dev.830"
           cambox_align_action 1.8.0 cam2=1.7.0-dev.830 cam3=1.7.0-dev.830
           cambox_align_action 1.8.0 cam2=1.8.0 cam3=1.8.0
           cambox_align_action 1.8.1 cam2=1.8.0 cam3=1.7.0-dev.830"#,
        &[],
    );
    assert_eq!(out, "1.8.0\n1.7.0-dev.830\nALIGN\nOK\nMIXED\n");
}

#[test]
fn verify_device_accepts_both_forms() {
    let out = run_lib(
        "scripts/verify-device.sh",
        r#"for v in 1.8.0 1.8.12 1.7.0-dev.830 1.8.0-dev; do
             if version_is_valid_format "$v"; then echo "YES $v"; else echo "NO $v"; fi
           done"#,
        &[],
    );
    assert_eq!(
        out,
        "YES 1.8.0\nYES 1.8.12\nYES 1.7.0-dev.830\nNO 1.8.0-dev\n"
    );
}

// --- CI wiring --------------------------------------------------------------------------------

/// The text of one top-level job of a workflow: its `  <name>:` line up to the next line that
/// starts a sibling job (two spaces, then a key character).
fn job_block(yml: &str, job: &str) -> String {
    let head = format!("  {job}:");
    let mut lines = yml.lines().skip_while(|l| *l != head);
    let first = lines
        .next()
        .unwrap_or_else(|| panic!("job {job} not found"));
    let mut out = vec![first];
    for l in lines {
        let sibling = l.starts_with("  ")
            && l[2..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
        if sibling {
            break;
        }
        out.push(l);
    }
    out.join("\n")
}

#[test]
fn ci_tags_the_merge_on_main_only_after_the_binary_gates() {
    let ci = read(".github/workflows/ci.yml");
    let job = job_block(&ci, "release-tag");
    let job = job.as_str();
    assert!(
        job.contains("if: github.ref == 'refs/heads/main' && github.event_name == 'push'"),
        "{job}"
    );
    assert!(
        job.contains("\n    needs: [lint, test, build, security, shellcheck]\n"),
        "release-tag must wait for every binary gate: {job}"
    );
    assert!(job.contains("contents: write"), "{job}");
    assert!(job.contains("name: camera-box-linux-amd64"), "{job}");
    assert!(job.contains("release_tag_decision"), "{job}");
    assert!(job.contains("scripts/lib/release-version.sh"), "{job}");
    assert!(job.contains("git push origin"), "{job}");
    assert!(job.contains("refs/tags/"), "{job}");
}

#[test]
fn ci_publishes_through_release_yml_as_a_reusable_workflow() {
    let ci = read(".github/workflows/ci.yml");
    let job = job_block(&ci, "release");
    let job = job.as_str();
    assert!(job.contains("needs: [release-tag]"), "{job}");
    assert!(
        job.contains("uses: ./.github/workflows/release.yml"),
        "a tag pushed with GITHUB_TOKEN starts no workflow, so ci.yml must call release.yml: {job}"
    );
    assert!(
        job.contains("tag: ${{ needs.release-tag.outputs.tag }}"),
        "{job}"
    );
    assert!(job.contains("contents: write"), "{job}");
}

#[test]
fn release_yml_runs_on_a_tag_push_and_as_a_reusable_workflow() {
    let rel = read(".github/workflows/release.yml");
    assert!(rel.contains("tags:\n      - 'v*'"), "{rel}");
    assert!(rel.contains("workflow_call:"), "{rel}");
    assert!(rel.contains("inputs.tag"), "{rel}");
    // the release is built from the tag itself and its binary must report the tag's version
    assert!(
        rel.contains("ref: ${{ inputs.tag || github.ref }}"),
        "{rel}"
    );
    assert!(rel.contains("target/release/camera-box --version"), "{rel}");
    assert!(
        rel.contains("tag_name: ${{ inputs.tag || github.ref_name }}"),
        "{rel}"
    );
    assert!(
        rel.contains("camera-box-linux-amd64.tar.gz.sha256"),
        "{rel}"
    );
    assert!(rel.contains("softprops/action-gh-release@v2"), "{rel}");
    // only the per-merge release takes Latest, never a re-issued tag pushed by hand
    assert!(
        rel.contains("make_latest: ${{ inputs.tag != '' }}"),
        "{rel}"
    );
    // the workflow_call path publishes the caller run's gated + deployed build, not a rebuild
    assert!(
        rel.contains("if: inputs.tag != ''\n        uses: actions/download-artifact@v4\n        with:\n          name: camera-box-linux-amd64\n          path: target/release"),
        "{rel}"
    );
    assert!(
        rel.contains("- name: Build release\n        if: inputs.tag == ''\n        run: cargo build --release"),
        "{rel}"
    );
}

#[test]
fn a_main_run_is_never_cancelled_by_the_next_merge() {
    let ci = read(".github/workflows/ci.yml");
    assert!(
        ci.contains("\nconcurrency:\n  group: ci-${{ github.workflow }}-${{ github.ref }}\n"),
        "{ci}"
    );
    assert!(
        ci.contains("  cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}\n"),
        "a cancelled main run leaves a tag without a release"
    );
    assert!(!ci.contains("\n  cancel-in-progress: true\n"));
}

#[test]
fn ci_checks_dev_is_above_main_on_every_dev_push() {
    let ci = read(".github/workflows/ci.yml");
    let job = job_block(&ci, "version-check");
    let job = job.as_str();
    assert!(job.contains("if: github.ref == 'refs/heads/dev'"), "{job}");
    assert!(job.contains(". scripts/lib/release-version.sh"), "{job}");
    assert!(job.contains("git fetch --depth=1 origin main"), "{job}");
    assert!(job.contains("git show FETCH_HEAD:Cargo.toml"), "{job}");
    assert!(
        job.contains("release_version_dev_check \"$dev\" \"$main\""),
        "{job}"
    );
    assert!(!job.contains("continue-on-error"), "{job}");
    let notify = job_block(&ci, "notify-on-failure");
    assert!(
        notify.contains("version-check, release-tag, release]"),
        "a red version check or release must ping: {notify}"
    );
}
