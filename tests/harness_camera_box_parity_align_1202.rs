//! issue 1202 — pre-gate auto-align of the active cam fleet to the run's candidate camera-box
//! build, so the `[0/8]` camera-box version-parity gate's existing `--candidate-pin` accept passes
//! without a manual `deploy-fleet` on the version-parity treadmill.
//!
//! ROOT CAUSE the align fixes: `camera-box-version-gate.sh` (#875/#1136) pins the fleet's
//! `/usr/local/bin/camera-box` to `origin/main`, with a candidate-pin accept that passes only when
//! the whole active fleet is uniformly ON this run's candidate. During active dev `origin/main`
//! lags `dev` by dozens of builds, so the candidate-pin accept is the only passing path — but each
//! dev commit bumps the candidate, leaving the fleet one build behind (candidate-1). `[2/8]`/`[2b/8]`
//! scp the candidate binary only to a transient `/tmp` burn path (never `/usr/local/bin/camera-box`)
//! and run AFTER the gate. So the gate refuses every run until a manual `deploy-fleet` (live killed
//! runs 32883434208 / 32892551674).
//!
//! These tests pin the PURE decision `cambox_align_action CANDIDATE ENTRY...` (scripts/lib/
//! camera-box-parity-align.sh) that decides align-vs-refuse. Only `ALIGN` (every active box read AND
//! uniform on ONE version != candidate) authorises a deploy; MIXED / UNKNOWN / NOACTIVE / NOCANDIDATE
//! / already-OK never deploy — so "versions differing BETWEEN boxes stays REFUSED" is preserved in
//! the align itself (and doubly by the untouched gate downstream).
//!
//! RED before issue 1202 (the lib's `cambox_align_action` is a stub that always prints MIXED, so
//! every non-MIXED case fails); GREEN after the real decision lands.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Source the lib under the caller's REAL `set -euo pipefail` (recording-e2e.sh's own opts — a
/// sourced-lib decision function must be safe there, ci-testing-gotchas.md #1133) and run
/// `cambox_align_action CANDIDATE ENTRY...`, returning its printed verdict (trimmed).
/// `ack` (may be empty) is exported as CAMBOX_OFFLINE_ACK for the acked-exclusion cases.
fn action(ack: &str, candidate: &str, entries: &[&str]) -> String {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    assert!(lib.exists(), "{} not found", lib.display());
    // Build the argument list: candidate first, then each name=version entry, each single-quoted.
    let mut args = String::new();
    for a in std::iter::once(candidate).chain(entries.iter().copied()) {
        args.push_str(" '");
        args.push_str(&a.replace('\'', r"'\''"));
        args.push('\'');
    }
    let harness = format!("set -euo pipefail\n. \"$LIB\"\ncambox_align_action{args}\n");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", &lib)
        .env("CAMBOX_OFFLINE_ACK", ack)
        .output()
        .expect("failed to run cambox_align_action harness");
    assert!(
        out.status.success(),
        "harness exited non-zero for candidate={candidate:?} entries={entries:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn align_action_all_boxes_already_on_candidate_is_ok() {
    assert_eq!(
        action("", "1.7.0-dev.551", &["cam3=1.7.0-dev.551"]),
        "OK",
        "a fleet already on the candidate needs no align (the gate passes via candidate-pin)"
    );
    assert_eq!(
        action(
            "",
            "1.7.0-dev.551",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.551"]
        ),
        "OK"
    );
}

#[test]
fn align_action_uniform_stale_fleet_is_align() {
    // The exact live treadmill shape: fleet uniformly one build behind the candidate.
    assert_eq!(
        action("", "1.7.0-dev.551", &["cam3=1.7.0-dev.550"]),
        "ALIGN",
        "a fleet uniformly on ONE stale build != candidate must auto-align to the candidate"
    );
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.551"]
        ),
        "ALIGN"
    );
}

#[test]
fn align_action_mixed_fleet_is_refused_never_aligned() {
    // Versions differing BETWEEN boxes must NEVER auto-deploy — mixed protection (issue 1202 HARD
    // constraint). The untouched gate then refuses it.
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.550", "cam4=1.7.0-dev.551"]
        ),
        "MIXED",
        "a fleet with versions differing BETWEEN boxes must stay REFUSED, never auto-aligned"
    );
}

#[test]
fn align_action_any_unread_box_is_unknown_never_aligned() {
    // An unread box (empty version) — even if every OTHER box agrees — must fail closed: deploying
    // would target an unreachable box. The gate then fails CLOSED (UNKNOWN=11).
    assert_eq!(
        action("", "1.7.0-dev.552", &["cam3=1.7.0-dev.551", "cam4="]),
        "UNKNOWN",
        "a uniform-but-partially-unread fleet must NOT auto-align (fail closed)"
    );
    // Unknown takes precedence even when the read boxes also disagree.
    assert_eq!(
        action(
            "",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.550", "cam4=1.7.0-dev.549", "cam5="]
        ),
        "UNKNOWN"
    );
}

#[test]
fn align_action_empty_candidate_is_nocandidate() {
    assert_eq!(
        action("", "", &["cam3=1.7.0-dev.550"]),
        "NOCANDIDATE",
        "no resolvable candidate -> no align; the gate decides"
    );
}

#[test]
fn align_action_all_acked_offline_is_noactive() {
    assert_eq!(
        action(
            "cam3:card-swap,cam4:battery",
            "1.7.0-dev.551",
            &["cam3=", "cam4="]
        ),
        "NOACTIVE",
        "every listed box acked-offline -> nothing to align (the gate vacuous-passes)"
    );
}

#[test]
fn align_action_excludes_acked_box_from_the_uniformity_check() {
    // An acked-offline box is excluded from the align decision exactly as the gate excludes it.
    // cam4 acked+unread -> ignored; cam3 on the candidate -> OK (no align needed).
    assert_eq!(
        action(
            "cam4:battery",
            "1.7.0-dev.551",
            &["cam3=1.7.0-dev.551", "cam4="]
        ),
        "OK",
        "an acked box must not force UNKNOWN, and must not break the active-fleet verdict"
    );
    // cam4 acked (any version) -> ignored; cam3 uniformly stale -> ALIGN off cam3 alone.
    assert_eq!(
        action(
            "cam4:battery",
            "1.7.0-dev.552",
            &["cam3=1.7.0-dev.551", "cam4=1.7.0-dev.400"]
        ),
        "ALIGN",
        "an acked box's version must not enter the uniformity check"
    );
}

// ---- orchestrator (cambox_parity_align_before_gate) — reads versions via the gate's fixture seam,
// decides, and deploys ONLY on ALIGN. Exercised end-to-end (read seam + CAMBOX_ALIGN_DEPLOY_CMD
// deploy seam) with NO real ssh / deploy. ------------------------------------------------------

/// Runs `cambox_parity_align_before_gate NODE_LIST` under the caller's real `set -euo pipefail`.
/// `versions` is (cam_name, camera-box-version) — each written to a temp fixture file wired via the
/// gate's own `CAMERA_BOX_VERSION_GATE_VERSION_<NAME>` seam (an empty version means "no fixture" =
/// unread). `no_main_pin` sets the operator-soak skip. Returns (orchestrator_rc_is_zero,
/// combined_output, Some(deploy_marker_contents) | None). The deploy seam writes `set=<CAMERA_SET>
/// cand=<candidate>` to a marker file iff the orchestrator invokes a deploy.
fn run_orchestrator(
    candidate: &str,
    no_main_pin: bool,
    node_list: &str,
    versions: &[(&str, &str)],
) -> (bool, String, Option<String>) {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    assert!(lib.exists(), "{} not found", lib.display());
    // tempfile::tempdir() — kernel-atomic O_EXCL random name, cannot collide across parallel test
    // threads (the #975 pid+timestamp hazard), auto-removed on drop.
    let work_dir = tempfile::tempdir().expect("create tempdir");
    let work = work_dir.path();
    let marker = work.join("deploy-marker");

    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_parity_align_before_gate \"$NODE_LIST\"\n");
    cmd.env("LIB", &lib)
        .env("NODE_LIST", node_list)
        .env("CAMBOX_ALIGN_CANDIDATE", candidate)
        .env(
            "CAMBOX_ALIGN_DEPLOY_CMD",
            format!(
                "printf 'set=%s cand=%s' \"$CAMERA_SET\" \"$CAMBOX_ALIGN_CANDIDATE\" > {}",
                marker.display()
            ),
        );
    if no_main_pin {
        cmd.env("CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN", "1");
    }
    for (name, ver) in versions {
        if ver.is_empty() {
            continue; // no fixture -> unread
        }
        let fx = work.join(format!("ver-{name}"));
        std::fs::write(&fx, format!("camera-box {ver}\n")).unwrap();
        let var = format!(
            "CAMERA_BOX_VERSION_GATE_VERSION_{}",
            name.to_uppercase().replace('-', "_")
        );
        cmd.env(var, &fx);
    }
    let out = cmd.output().expect("failed to run orchestrator harness");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let deploy = std::fs::read_to_string(&marker).ok();
    (out.status.success(), combined, deploy)
}

#[test]
fn orchestrator_aligns_a_uniform_stale_fleet_and_deploys_the_candidate() {
    let (ok, out, deploy) = run_orchestrator(
        "1.7.0-dev.551",
        false,
        "cam3=root@10.0.0.3",
        &[("cam3", "1.7.0-dev.550")],
    );
    assert!(
        ok,
        "orchestrator must return 0 (never abort the harness): {out}"
    );
    assert_eq!(
        deploy.as_deref(),
        Some("set=cam3 cand=1.7.0-dev.551"),
        "a uniform-stale fleet must deploy the candidate scoped to CAMERA_SET=cam3. out={out}"
    );
    assert!(
        out.contains("deploying the candidate"),
        "must log the deploy: {out}"
    );
}

#[test]
fn orchestrator_does_not_deploy_when_fleet_already_on_candidate() {
    let (ok, out, deploy) = run_orchestrator(
        "1.7.0-dev.551",
        false,
        "cam3=root@10.0.0.3 cam4=root@10.0.0.4",
        &[("cam3", "1.7.0-dev.551"), ("cam4", "1.7.0-dev.551")],
    );
    assert!(ok, "{out}");
    assert_eq!(
        deploy, None,
        "an already-on-candidate fleet must NOT deploy. out={out}"
    );
    assert!(out.contains("already on the candidate"), "{out}");
}

#[test]
fn orchestrator_never_deploys_a_mixed_fleet() {
    // The HARD issue-1202 constraint: versions differing BETWEEN boxes must NEVER auto-deploy.
    let (ok, out, deploy) = run_orchestrator(
        "1.7.0-dev.552",
        false,
        "cam3=root@10.0.0.3 cam4=root@10.0.0.4",
        &[("cam3", "1.7.0-dev.550"), ("cam4", "1.7.0-dev.551")],
    );
    assert!(ok, "{out}");
    assert_eq!(
        deploy, None,
        "a MIXED fleet must stay REFUSED — never auto-aligned (the gate refuses it). out={out}"
    );
}

#[test]
fn orchestrator_never_deploys_when_a_box_is_unread() {
    let (ok, out, deploy) = run_orchestrator(
        "1.7.0-dev.552",
        false,
        "cam3=root@10.0.0.3 cam4=root@10.0.0.4",
        &[("cam3", "1.7.0-dev.551"), ("cam4", "")], // cam4 unread
    );
    assert!(ok, "{out}");
    assert_eq!(
        deploy, None,
        "an unread box must fail CLOSED — never auto-align a partially-unreachable fleet. out={out}"
    );
}

#[test]
fn orchestrator_skips_align_entirely_under_no_main_pin_operator_soak() {
    let (ok, out, deploy) = run_orchestrator(
        "1.7.0-dev.551",
        true, // --no-main-pin
        "cam3=root@10.0.0.3",
        &[("cam3", "1.7.0-dev.550")], // uniform-stale — would ALIGN if the pin were on
    );
    assert!(ok, "{out}");
    assert_eq!(
        deploy, None,
        "a --no-main-pin operator soak must NEVER auto-realign over a deliberately-deployed build. out={out}"
    );
    assert!(out.contains("SKIPPED"), "must log the soak skip: {out}");
}

// ---- static-anchor wiring: recording-e2e.sh must run the align BEFORE the [0/8] gate ----------

#[test]
fn recording_e2e_sources_and_calls_the_parity_align_before_the_gate() {
    let s = read_e2e();
    assert!(
        s.contains(r#". "$HERE/lib/camera-box-parity-align.sh""#),
        "recording-e2e.sh must source the parity-align lib"
    );
    let call = s
        .find("cambox_parity_align_before_gate \"$CAMBOX_VERSION_LINUX\"")
        .expect(
            "recording-e2e.sh must call cambox_parity_align_before_gate on the gate's node list",
        );
    let gate = s
        .find("camera-box-version-gate.sh")
        .expect("recording-e2e.sh must invoke the camera-box version gate");
    assert!(
        call < gate,
        "issue 1202: the parity auto-align must run BEFORE the [0/8] camera-box version-parity gate \
         (so the gate's candidate-pin accept then passes)"
    );
    // The align must scope to the SAME node list the gate reads.
    let banner = s
        .find("[0/8] camera-box version-parity gate")
        .expect("the [0/8] camera-box gate banner must exist");
    assert!(
        banner < call && call < gate,
        "issue 1202: the align call must sit inside the [0/8] gate block, after CAMBOX_VERSION_LINUX \
         is built and before the gate invocation"
    );
}

fn read_e2e() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

// ---- real cambox_align_deploy path (issue 1202 review 🔴/🟡 fix) — the candidate binary's version
// GUARD (never ship a stale build) + acked exclusion from the deploy CAMERA_SET, via the
// CAMBOX_ALIGN_CANDIDATE_BIN + CAMBOX_ALIGN_DEPLOY_FLEET seams (no gh / no ssh). ----------------

/// Writes an executable fake `camera-box` that prints `camera-box <ver>` for `--version`.
fn write_fake_cbox(dir: &std::path::Path, name: &str, ver: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(
        &p,
        format!("#!/usr/bin/env bash\necho \"camera-box {ver}\"\n"),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
    p
}

/// A fake deploy-fleet.sh that records `set=<CAMERA_SET> bin=<$2>` to a marker and exits 0.
fn write_fake_fleet(dir: &std::path::Path, marker: &std::path::Path) -> PathBuf {
    let p = dir.join("deploy-fleet.sh");
    std::fs::write(
        &p,
        format!(
            "#!/usr/bin/env bash\nprintf 'set=%s bin=%s\\n' \"${{CAMERA_SET:-}}\" \"$2\" > {}\nexit 0\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
    p
}

#[test]
fn align_binary_version_reads_the_last_field() {
    let d = tempfile::tempdir().unwrap();
    let bin = write_fake_cbox(d.path(), "camera-box", "1.7.0-dev.551");
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_align_binary_version \"$BIN\"\n")
        .env("LIB", &lib)
        .env("BIN", &bin)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "1.7.0-dev.551");
}

/// Runs `cambox_align_deploy CANDIDATE NAMES` with a fake candidate binary (version `bin_ver`) and a
/// fake deploy-fleet. Returns (rc_is_zero, Some(marker) | None).
fn run_deploy(candidate: &str, bin_ver: Option<&str>, names: &str) -> (bool, Option<String>) {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    let d = tempfile::tempdir().unwrap();
    let marker = d.path().join("fleet-invoked");
    let fleet = write_fake_fleet(d.path(), &marker);
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_align_deploy \"$CAND\" \"$NAMES\"\n")
        .env("LIB", &lib)
        .env("CAND", candidate)
        .env("NAMES", names)
        .env("CAMBOX_ALIGN_DEPLOY_FLEET", &fleet);
    match bin_ver {
        Some(v) => {
            let bin = write_fake_cbox(d.path(), "camera-box", v);
            cmd.env("CAMBOX_ALIGN_CANDIDATE_BIN", &bin);
        }
        None => {
            cmd.env(
                "CAMBOX_ALIGN_CANDIDATE_BIN",
                d.path().join("does-not-exist"),
            );
        }
    }
    let out = cmd.output().unwrap();
    let deploy = std::fs::read_to_string(&marker).ok();
    (out.status.success(), deploy)
}

#[test]
fn deploy_ships_the_candidate_only_when_the_binary_version_matches() {
    let (ok, marker) = run_deploy("1.7.0-dev.551", Some("1.7.0-dev.551"), "cam3 cam4");
    assert!(ok, "a matching-version candidate must deploy (rc=0)");
    let m = marker.expect("deploy-fleet must be invoked on a version match");
    assert!(
        m.contains("set=cam3 cam4 bin="),
        "deploy-fleet must be invoked with --binary + CAMERA_SET='cam3 cam4': {m}"
    );
}

#[test]
fn deploy_never_ships_a_stale_build_on_version_mismatch() {
    // The review 🔴: a stale build must NEVER be deployed to "align" (it just needlessly redeploys
    // and the gate still refuses). Guarded by the candidate-version match.
    let (ok, marker) = run_deploy("1.7.0-dev.551", Some("1.7.0-dev.550"), "cam3");
    assert!(
        !ok,
        "a version mismatch must return non-zero (deploy refused)"
    );
    assert_eq!(
        marker, None,
        "deploy-fleet must NOT be invoked when the newest build != candidate (no stale deploy)"
    );
}

#[test]
fn deploy_returns_nonzero_and_skips_when_the_candidate_binary_is_missing() {
    let (ok, marker) = run_deploy("1.7.0-dev.551", None, "cam3");
    assert!(!ok, "a missing candidate binary must return non-zero");
    assert_eq!(
        marker, None,
        "deploy-fleet must NOT be invoked with no binary"
    );
}

#[test]
fn orchestrator_excludes_acked_boxes_from_the_deploy_camera_set() {
    // issue 1202 review 🟡: deploy-fleet.sh does not consult CAMBOX_OFFLINE_ACK, so an acked box must
    // never enter the deploy CAMERA_SET. cam3 uniform-stale -> ALIGN; cam4 acked -> deploy scope=cam3.
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    let d = tempfile::tempdir().unwrap();
    let marker = d.path().join("fleet-invoked");
    let fleet = write_fake_fleet(d.path(), &marker);
    let cand_bin = write_fake_cbox(d.path(), "camera-box", "1.7.0-dev.551");
    let f3 = d.path().join("ver-cam3");
    std::fs::write(&f3, "camera-box 1.7.0-dev.550\n").unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_parity_align_before_gate \"$NODES\"\n")
        .env("LIB", &lib)
        .env("NODES", "cam3=root@10.0.0.3 cam4=root@10.0.0.4")
        .env("CAMBOX_ALIGN_CANDIDATE", "1.7.0-dev.551")
        .env("CAMBOX_OFFLINE_ACK", "cam4:battery")
        .env("CAMERA_BOX_VERSION_GATE_VERSION_CAM3", &f3)
        .env("CAMBOX_ALIGN_CANDIDATE_BIN", &cand_bin)
        .env("CAMBOX_ALIGN_DEPLOY_FLEET", &fleet)
        .output()
        .unwrap();
    assert!(out.status.success(), "orchestrator must return 0");
    let m = std::fs::read_to_string(&marker).expect("deploy-fleet must be invoked (cam3 is ALIGN)");
    assert!(
        m.contains("set=cam3 bin="),
        "deploy scope must be exactly cam3: {m}"
    );
    assert!(
        !m.contains("cam4"),
        "the acked-offline cam4 must NOT appear in the deploy CAMERA_SET: {m}"
    );
}

// ---- issue 1244 — commit-scoped ci.yml artifact resolution (never "newest on branch") --------
//
// ROOT CAUSE: cambox_align_deploy() used to resolve the candidate ci.yml artifact via "newest
// successful ci.yml run on branch dev" (`gh run list --branch dev ...`). That resolution was
// PROVEN non-deterministic *inside the E2E job's own runner environment* (self-hosted runner,
// GITHUB_TOKEN) -- live runs 33400360170/33425283884 both resolved a PREHISTORIC build
// (.428/.439) while the real newest build (.591/.594) was already published; the identical query
// from an interactive shell (a different token) resolved correctly. Resolving BY THE CANDIDATE'S
// OWN COMMIT is deterministic regardless of that anomaly -- a commit has at most ONE successful
// ci.yml run, or none (not yet published), so "which run is newest" stops being a question that
// can go wrong. No fallback to the old branch-based query when --commit finds nothing (issue 1244
// is explicit: that would silently reopen the exact non-determinism this fix removes) -- the
// existing refuse/self-heal path (candidate's own ci.yml not published yet) covers it.
//
// RED before issue 1244 (the lib still resolves via `--branch dev`, so every assertion below that
// requires `--commit` / rejects `--branch` fails); GREEN after the commit-scoped resolution lands.

/// A fake `gh` that appends every invocation's full argv (one line) to `$GH_ARGV_LOG`, then always
/// reports "no run found" (empty stdout, exit 0) -- these tests only assert on the EMITTED command
/// shape, never on a downloaded artifact.
fn write_fake_gh_logging_argv(dir: &std::path::Path, log: &std::path::Path) -> PathBuf {
    let p = dir.join("gh");
    std::fs::write(
        &p,
        format!(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> {}\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
    p
}

/// Runs `cambox_align_deploy CANDIDATE NAMES` with NO `CAMBOX_ALIGN_CANDIDATE_BIN` (forcing the
/// real `gh`-resolution path) and a PATH-stubbed `gh` that logs its argv. Returns the logged
/// `gh run list ...` invocation line, or `None` if `gh` was never invoked at all.
fn resolve_run_list_argv(extra_env: &[(&str, &str)]) -> Option<String> {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    let d = tempfile::tempdir().unwrap();
    let bindir = d.path().join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    let log = d.path().join("gh-argv.log");
    write_fake_gh_logging_argv(&bindir, &log);

    let path_env = format!(
        "{}:{}",
        bindir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_align_deploy \"$CAND\" \"$NAMES\"\n")
        .env("LIB", &lib)
        .env("CAND", "1.7.0-dev.551")
        .env("NAMES", "cam3")
        .env("PATH", &path_env)
        .env_remove("CAMBOX_ALIGN_CANDIDATE_BIN")
        .env_remove("CAMBOX_ALIGN_DEPLOY_CMD")
        .env_remove("GITHUB_SHA")
        .env_remove("CAMBOX_ALIGN_CANDIDATE_SHA");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    // The command is expected to return non-zero (no run "found") -- only the logged argv matters.
    let _ = cmd.output().unwrap();
    std::fs::read_to_string(&log).ok().and_then(|s| {
        s.lines()
            .find(|l| l.starts_with("run list"))
            .map(|l| l.to_string())
    })
}

#[test]
fn deploy_resolves_ci_run_by_explicit_candidate_sha_seam_1244() {
    let line = resolve_run_list_argv(&[("CAMBOX_ALIGN_CANDIDATE_SHA", "deadbeefcafe")])
        .expect("gh run list must be invoked when no CAMBOX_ALIGN_CANDIDATE_BIN is set");
    assert!(
        line.contains("--commit deadbeefcafe"),
        "the ci.yml run resolution must be scoped to the candidate's own commit sha (issue 1244 \
         -- 'newest on branch' resolved a prehistoric run inside the E2E runner environment): \
         {line}"
    );
    assert!(
        !line.contains("--branch"),
        "the resolution must NEVER fall back to a branch-based 'newest' query (issue 1244 \
         explicitly rejects a --commit-empty fallback to the old non-deterministic path): {line}"
    );
}

#[test]
fn deploy_resolves_ci_run_by_github_sha_fallback_when_seam_unset_1244() {
    let line = resolve_run_list_argv(&[("GITHUB_SHA", "cafebabe1234")])
        .expect("gh run list must be invoked when no CAMBOX_ALIGN_CANDIDATE_BIN is set");
    assert!(
        line.contains("--commit cafebabe1234"),
        "with no CAMBOX_ALIGN_CANDIDATE_SHA override, the resolution must fall back to \
         $GITHUB_SHA (set by every GitHub Actions job -- recording-e2e.sh runs as a plain \
         workflow `run:` step and inherits it with no explicit wiring): {line}"
    );
    assert!(
        !line.contains("--branch"),
        "must never fall back to the branch-based query: {line}"
    );
}

#[test]
fn candidate_sha_prefers_the_explicit_seam_over_github_sha_1244() {
    let lib = manifest_dir().join("scripts/lib/camera-box-parity-align.sh");
    let out = Command::new("bash")
        .arg("-c")
        .arg("set -euo pipefail\n. \"$LIB\"\ncambox_align_candidate_sha\n")
        .env("LIB", &lib)
        .env("CAMBOX_ALIGN_CANDIDATE_SHA", "explicit-sha")
        .env("GITHUB_SHA", "actions-sha")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "explicit-sha");
}
