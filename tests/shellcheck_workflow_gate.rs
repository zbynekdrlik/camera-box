//! #545 — the provisioning/ops shell scripts (`setup-device.sh`, `setup-imag.sh`, `rig-mode.sh`,
//! `drift-guard.sh`, ...) run as ROOT on fresh cam boxes + imag-nb during provisioning
//! (#448-#454) and carry non-trivial logic (idempotent GRUB_CMDLINE flag loops, git-arg-injection
//! guards, fail-closed abort guards) with no dedicated lint gate. `.github/workflows/ci.yml`
//! gains a `shellcheck` job (GitHub-hosted `ubuntu-latest`, `-S warning` severity floor — errors
//! and warnings fail the build, the many style/info-level findings stay advisory-only) as a gate:
//! no `continue-on-error`, wired into the same `notify-on-failure` red-alert fan-in as every
//! other gate job.
//!
//! These are STRUCTURAL guards on the workflow YAML — they fail loudly if a future edit drops
//! the job, loosens its severity floor, or detaches it from the failure-notification fan-in. The
//! DEFINITIVE proof that the scripts themselves are clean is the job actually running
//! `shellcheck -S warning scripts/*.sh scripts/lib/*.sh` on a real ubuntu-latest runner.

use std::fs;

fn read_ci_workflow() -> String {
    let path = format!("{}/.github/workflows/ci.yml", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// The job must exist and run on a GitHub-hosted runner — never a self-hosted dev runner (this
/// repo's mutation/coverage/build jobs already occupy those; a lint job tying up self-hosted
/// capacity would be a regression, not a hardening).
#[test]
fn shellcheck_job_exists_on_github_hosted_runner() {
    let wf = read_ci_workflow();
    assert!(
        wf.contains("shellcheck:"),
        "#545: ci.yml must define a `shellcheck:` job."
    );
    let job_start = wf.find("  shellcheck:\n").expect("shellcheck: job header");
    let job_end = wf[job_start..]
        .find("  notify-on-failure:")
        .map(|i| job_start + i)
        .expect("notify-on-failure: job header (the job immediately after shellcheck)");
    let job_body = &wf[job_start..job_end];
    assert!(
        job_body.contains("runs-on: ubuntu-latest"),
        "#545: the shellcheck job must run on GitHub-hosted ubuntu-latest, not a self-hosted \
         dev runner."
    );
}

/// The lint step itself must be a single binary command at `-S warning` severity over the
/// provisioning scripts, with no `continue-on-error` escape hatch (per no-continue-on-error.md —
/// every CI step is succeed-and-continue or fail-and-stop, never advisory-only).
#[test]
fn shellcheck_runs_at_warning_severity_as_a_binary_gate() {
    let wf = read_ci_workflow();
    assert!(
        wf.contains("shellcheck -S warning scripts/*.sh scripts/lib/*.sh"),
        "#545: ci.yml must run `shellcheck -S warning scripts/*.sh scripts/lib/*.sh` — covering \
         the standalone root-run lib/ helpers too; errors and warnings fail the build, the many \
         style/info-level findings stay a deliberate advisory floor."
    );
    assert!(
        !wf.contains("continue-on-error"),
        "#545: no CI step in this workflow may use continue-on-error (no-continue-on-error.md) — \
         a shellcheck step must be a real binary gate, not an informational-only step."
    );
}

/// A red shellcheck job must trip the same Discord `notify-on-failure` alert every other gate
/// job triggers — otherwise a broken provisioning script fails silently off to the side.
#[test]
fn shellcheck_wired_into_the_failure_notification_fanin() {
    let wf = read_ci_workflow();
    let needs_line = wf
        .lines()
        .find(|l| l.trim_start().starts_with("needs:") && l.contains("drift-guard"))
        .expect(
            "notify-on-failure's needs: line (identified via the drift-guard job it already lists)",
        );
    assert!(
        needs_line.contains("shellcheck"),
        "#545: notify-on-failure's `needs:` list must include `shellcheck` so a red shellcheck \
         run posts the same Discord alert as every other gate job. Got: {needs_line}"
    );
}

fn read_repo_file(rel: &str) -> String {
    let path = format!("{}/{rel}", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Every `scripts/*.sh` + `scripts/lib/*.sh` file, the exact set the CI job lints.
fn linted_scripts() -> Vec<(String, String)> {
    let root = env!("CARGO_MANIFEST_DIR");
    let mut out = Vec::new();
    for dir in ["scripts", "scripts/lib"] {
        let entries = fs::read_dir(format!("{root}/{dir}")).expect("read scripts dir");
        for e in entries {
            let p = e.expect("dir entry").path();
            if p.extension().and_then(|x| x.to_str()) == Some("sh") {
                let rel = format!("{dir}/{}", p.file_name().unwrap().to_string_lossy());
                let text = fs::read_to_string(&p).unwrap_or_default();
                out.push((rel, text));
            }
        }
    }
    out.sort();
    out
}

/// Issue 1395: the job lints with the PINNED shellcheck from scripts/lib/shellcheck-pin.sh, put
/// first on PATH before the lint step, never the runner's preinstalled 0.9.0 (which does not know
/// the `extended-analysis` directive the heavy entry scripts carry and passes 12 GB RSS without it).
#[test]
fn shellcheck_job_installs_the_pinned_release_before_the_lint_1395() {
    let wf = read_ci_workflow();
    let job_start = wf.find("  shellcheck:\n").expect("shellcheck: job header");
    let job_end = wf[job_start..]
        .find("\n  deploy-fleet:")
        .map(|i| job_start + i)
        .unwrap_or(wf.len());
    let job = &wf[job_start..job_end];
    let install = job
        .find(". scripts/lib/shellcheck-pin.sh")
        .expect("1395: the shellcheck job must source scripts/lib/shellcheck-pin.sh");
    let path = job
        .find(">> \"$GITHUB_PATH\"")
        .expect("1395: the pinned binary's directory must go on GITHUB_PATH");
    let lint = job
        .find("shellcheck -S warning scripts/*.sh scripts/lib/*.sh")
        .expect("lint step");
    assert!(
        install < path && path < lint,
        "1395: install, then PATH, then the lint step"
    );
    assert!(
        job.contains("shellcheck_pin_install \"$RUNNER_TEMP/shellcheck-pin\""),
        "1395: the job must install through the one pin function"
    );
    assert!(
        !job.contains("apt-get install -y shellcheck"),
        "1395: no fallback to the distro shellcheck (0.9.0 reports the directive as SC1107)"
    );
}

/// Issue 1395: the pin is a real, sha256-checked release, at least 0.10.0 (the first release that
/// knows `extended-analysis`).
#[test]
fn shellcheck_pin_is_a_sha256_checked_release_new_enough_for_the_directive_1395() {
    let lib = read_repo_file("scripts/lib/shellcheck-pin.sh");
    let value = |key: &str| -> String {
        let prefix = format!("{key}=\"");
        let line = lib
            .lines()
            .find(|l| l.starts_with(&prefix))
            .unwrap_or_else(|| panic!("1395: {key} missing"));
        line[prefix.len()..].trim_end_matches('"').to_string()
    };
    let ver = value("SHELLCHECK_PIN_VERSION");
    let parts: Vec<u32> = ver
        .split('.')
        .map(|p| p.parse().expect("numeric version"))
        .collect();
    assert_eq!(parts.len(), 3, "1395: a full x.y.z pin, got {ver}");
    assert!(
        (parts[0], parts[1]) >= (0, 10),
        "1395: extended-analysis needs shellcheck >= 0.10.0, pinned {ver}"
    );
    let sha = value("SHELLCHECK_PIN_SHA256");
    assert!(
        sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
        "1395: the pin carries a 64-hex sha256, got {sha}"
    );
    assert!(
        lib.contains("sha256sum -c"),
        "1395: the download is checked against the pin"
    );
}

/// Issue 1395: the dataflow-off directive stays exactly where the measurement put it. Each use is
/// the second line of its file (file-scoped), carries its reason on the next line, and the set of
/// files is the measured list: a new file needs a measurement, never a copy-paste.
#[test]
fn extended_analysis_off_only_in_the_measured_entry_scripts_with_a_reason_1395() {
    const MEASURED: [&str; 11] = [
        "scripts/deploy-genlock-fleet.sh",
        "scripts/lib/strih-cambox-log-sink.sh",
        "scripts/lib/strih-provision.sh",
        "scripts/lib/strih-snv-deploy.sh",
        "scripts/recording-e2e.sh",
        "scripts/rig-mode.sh",
        "scripts/setup-device.sh",
        "scripts/setup-strih.sh",
        "scripts/verify-device.sh",
        "scripts/verify-imag.sh",
        "scripts/verify-strih.sh",
    ];
    let mut found = Vec::new();
    for (rel, text) in linted_scripts() {
        if !text.contains("shellcheck extended-analysis=") {
            continue;
        }
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.get(1).copied(),
            Some("# shellcheck extended-analysis=false"),
            "1395: {rel}: the directive must be line 2 (file-scoped, right after the shebang)"
        );
        assert!(
            lines.get(2).is_some_and(|l| l.contains("issue 1395")),
            "1395: {rel}: the directive's reason line must follow it"
        );
        assert_eq!(
            text.matches("shellcheck extended-analysis=").count(),
            1,
            "1395: {rel}: one directive per file"
        );
        found.push(rel);
    }
    let mut measured: Vec<String> = MEASURED.iter().map(|s| s.to_string()).collect();
    measured.sort();
    assert_eq!(
        found, measured,
        "1395: the directive set is the measured set"
    );
}
