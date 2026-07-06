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
