// #1309 -- on-box management-liveness self-heal: pure-function + generated-script tests for
// scripts/lib/mgmt-liveness.sh. The 2026-09-13 half-dead wedge (a cambox loses ssh/MCP/gphoto2 while
// dantesync + the relay HTTP keep answering) left the headless box with no local recovery path; the
// lib's banner classifier + MGMT_OK/MGMT_DEAD/RESTART_ALLOWED/BACKOFF decision drive a systemd timer
// that self-heals sshd. The decision is pure bash so it is exhaustively unit-testable at Tier-0 (#557
// kills local cargo); the on-box script EMBEDS these exact functions via `declare -f`, so this file
// also pins the generated script's shape (no drift, valid bash).

use std::process::Command;

fn manifest_dir() -> String {
    env!("CARGO_MANIFEST_DIR").to_string()
}

fn lib_path() -> String {
    format!("{}/scripts/lib/mgmt-liveness.sh", manifest_dir())
}

/// Source the lib (no `-e`, it is source-only) and run `body`, returning (code, stdout, stderr).
fn run_sourced(body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$LIB\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("LIB", lib_path())
        .current_dir(manifest_dir())
        .output()
        .expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// ----------------------------------------------------------------- banner classifier
#[test]
fn banner_ok_is_1_only_for_an_ssh_protocol_banner() {
    let (c, out, err) = run_sourced("mgmt_liveness_banner_ok 'SSH-2.0-OpenSSH_9.6'");
    assert_eq!(c, 0, "stderr: {err}");
    assert_eq!(out, "1", "a real SSH banner must classify OK");
}

#[test]
fn banner_ok_is_0_for_empty_or_reset() {
    // a plain TCP accept during the wedge yields no banner (empty / reset) -> 0, the discriminator.
    assert_eq!(run_sourced("mgmt_liveness_banner_ok ''").1, "0");
    assert_eq!(
        run_sourced("mgmt_liveness_banner_ok 'garbled reset bytes'").1,
        "0"
    );
}

// ----------------------------------------------------------------- pure decision
fn decide(banner_ok: &str, prev: &str, thr: &str, restarts: &str, maxr: &str) -> String {
    let (c, out, err) = run_sourced(&format!(
        "mgmt_liveness_decide {banner_ok} {prev} {thr} {restarts} {maxr}"
    ));
    assert_eq!(c, 0, "stderr: {err}");
    out
}

#[test]
fn banner_ok_resets_to_mgmt_ok_and_zero_consecutive() {
    let d = decide("1", "2", "3", "0", "3");
    assert!(d.contains("verdict=MGMT_OK"), "{d}");
    assert!(d.contains("consecutive=0"), "{d}");
}

#[test]
fn dead_below_threshold_is_mgmt_dead_and_increments() {
    // a single/second blip must NEVER trigger a restart -- it counts up first.
    let d = decide("0", "1", "3", "0", "3");
    assert!(d.contains("verdict=MGMT_DEAD"), "{d}");
    assert!(d.contains("consecutive=2"), "{d}");
}

#[test]
fn dead_at_threshold_under_cap_is_restart_allowed() {
    let d = decide("0", "2", "3", "0", "3");
    assert!(d.contains("verdict=RESTART_ALLOWED"), "{d}");
    assert!(d.contains("consecutive=3"), "{d}");
}

#[test]
fn dead_at_threshold_at_cap_is_backoff_never_restart_loop() {
    let d = decide("0", "5", "3", "3", "3");
    assert!(d.contains("verdict=BACKOFF"), "{d}");
}

#[test]
fn garbled_prev_consecutive_is_treated_as_zero_never_a_spurious_restart() {
    // a corrupt state file must fail SAFE -- read as 0, so one dead probe stays MGMT_DEAD.
    let d = decide("0", "xx", "3", "0", "3");
    assert!(d.contains("verdict=MGMT_DEAD"), "{d}");
    assert!(d.contains("consecutive=1"), "{d}");
}

// ----------------------------------------------------------------- generated on-box script
fn generated_script() -> String {
    let (c, out, err) = run_sourced("mgmt_liveness_selfcheck_script");
    assert_eq!(c, 0, "stderr: {err}");
    out
}

#[test]
fn generated_script_embeds_the_exact_pure_functions() {
    // one source of truth: the on-box script must EMBED the tested functions (declare -f), never a
    // drifting inlined copy.
    let s = generated_script();
    assert!(
        s.contains("mgmt_liveness_banner_ok ()"),
        "must embed banner_ok: {s}"
    );
    assert!(
        s.contains("mgmt_liveness_decide ()"),
        "must embed decide: {s}"
    );
    assert!(
        s.contains("mgmt_liveness_snapshot_cmds ()"),
        "must embed snapshot cmds: {s}"
    );
}

#[test]
fn generated_script_probes_loopback_and_restarts_the_selfheal_units() {
    let s = generated_script();
    assert!(
        s.contains("/dev/tcp/127.0.0.1/"),
        "must probe the local ssh banner: {s}"
    );
    assert!(
        s.contains("systemctl restart"),
        "must restart on RESTART_ALLOWED: {s}"
    );
    assert!(
        s.contains("ssh remoteos-mcp"),
        "must self-heal both sshd and the remoteos MCP surface: {s}"
    );
}

#[test]
fn generated_script_is_valid_bash() {
    let s = generated_script();
    let dir = std::env::temp_dir();
    let path = dir.join(format!("mgmt-gen-{}.sh", std::process::id()));
    std::fs::write(&path, &s).expect("write generated script");
    let out = Command::new("bash")
        .arg("-n")
        .arg(&path)
        .output()
        .expect("bash -n");
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "generated on-box script must be syntactically valid bash: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ----------------------------------------------------------------- systemd units
#[test]
fn service_unit_is_oneshot_running_the_generated_script() {
    let (_c, out, _e) = run_sourced("mgmt_liveness_service_unit");
    assert!(out.contains("Type=oneshot"), "{out}");
    assert!(
        out.contains("ExecStart=/usr/local/sbin/cambox-mgmt-selfcheck.sh"),
        "{out}"
    );
}

#[test]
fn timer_unit_fires_every_two_minutes() {
    let (_c, out, _e) = run_sourced("mgmt_liveness_timer_unit");
    assert!(out.contains("OnUnitActiveSec=2min"), "{out}");
    assert!(out.contains("WantedBy=timers.target"), "{out}");
}
