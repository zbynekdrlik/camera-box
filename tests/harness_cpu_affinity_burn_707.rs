//! #707 (root-cause follow-up, measured 2026-07-30) — the transient burn-mode `systemd-run` unit
//! in `scripts/recording-e2e.sh` ([2/8] cam1 deploy and [2b/8] ALL_CAMBOX loop) launches the SAME
//! `camera-box` binary production runs under `camera-box.service` (which carries the #289
//! `CPUAffinity=<isolated core>` drop-in written by `scripts/setup-device.sh`), but with NO such
//! property at all — a systemd drop-in only ever applies to a unit literally named
//! `camera-box.service`, never a differently-named transient unit.
//!
//! Measured live on cam2, same binary, minutes apart: production keeps 28/32 threads on the
//! isolated core; the exact same binary under burn-mode `systemd-run` keeps only 1/45 threads
//! there, pushing 44 auxiliary threads (tokio workers, NDI SDK internals, intercom) onto the
//! general cores — which on cam2 (also the dual-QR painter + qpsk-marker box) collide with the
//! painter/marker threads and produced a 53.6% off-nominal emit-cadence rate, vs 0.6% on cam4
//! (no painter). The gate was measuring a CPU environment the operator never runs.
//!
//! Fix: `scripts/lib/cpu-affinity-burn.sh` derives the SAME mask production has from the box's
//! own `/sys/devices/system/cpu/isolated` at deploy time — the same source `src/affinity.rs`'s
//! `read_isolated_cores()` reads — never a hardcoded core number, so burn mode and production
//! cannot drift apart again. Both launch sites embed it via the SAME shared helper, never two
//! independent copies.
//!
//! These tests exercise the pure decision function directly (source the lib, call it with
//! `/sys/devices/system/cpu/isolated` fixture text) — mirroring `tests/harness_v4l2_neutral_744.rs`
//! / `tests/verify_device_pure_functions.rs` — plus static-anchor assertions that BOTH launch
//! sites in `scripts/recording-e2e.sh` wire in the SAME helper, so the two can never silently
//! diverge again.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/cpu-affinity-burn.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Call `cpu_affinity_burn_property_for_isolated` with a fixture `/sys/devices/system/cpu/isolated`
/// transcript passed via an env var (never interpolated into the bash -c script text — a fixture
/// must never need bash-escaping by the test itself).
fn property_for(fixture: &str) -> String {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\ncpu_affinity_burn_property_for_isolated \"$FIXTURE\""
            .to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("FIXTURE", fixture)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "cpu_affinity_burn_property_for_isolated exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn single_isolated_core_707() {
    // The fleet's actual isolcpus=3 shape.
    assert_eq!(property_for("3\n"), "--property=CPUAffinity=3");
}

#[test]
fn no_isolated_core_yields_no_property_707() {
    // An un-isolated box (no isolcpus= on the kernel cmdline, empty /sys file) must get NO
    // CPUAffinity property at all — never a fabricated/default core number.
    assert_eq!(property_for(""), "");
    assert_eq!(property_for("\n"), "");
}

#[test]
fn multi_core_isolation_list_passes_through_verbatim_707() {
    // /sys/devices/system/cpu/isolated uses the kernel cpulist comma/range format, and systemd's
    // CPUAffinity= accepts the EXACT same syntax (systemd.exec(5): "a list of CPU indices or
    // ranges separated by either whitespace or commas") — so a multi-core isolation list passes
    // straight through with no reformatting.
    assert_eq!(property_for("1,3\n"), "--property=CPUAffinity=1,3");
    assert_eq!(property_for("2-3\n"), "--property=CPUAffinity=2-3");
}

#[test]
fn whitespace_in_the_sys_file_is_stripped_707() {
    assert_eq!(property_for("  3  \n"), "--property=CPUAffinity=3");
}

#[test]
fn resolve_cmd_sets_the_shared_variable_from_the_boxs_own_sys_file_707() {
    let harness = "set -uo pipefail\n. \"$SCRIPT\"\ncpu_affinity_burn_resolve_cmd".to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "resolve cmd exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("CPU_AFFINITY_BURN_PROPERTY"),
        "resolve cmd must set CPU_AFFINITY_BURN_PROPERTY: {text}"
    );
    assert!(
        text.contains("/sys/devices/system/cpu/isolated"),
        "resolve cmd must read the box's OWN isolated-core file, never a hardcoded core: {text}"
    );
}

// --- trailing-newline-glue safety (the #746 lesson: `$(...)` unconditionally strips trailing
// newlines from captured output, so a `_cmd` helper embedded mid-string must never rely on its
// own trailing newline to separate it from whatever literal text follows at the embedding site).

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cpu-affinity-burn-707-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[test]
fn resolve_cmd_embedding_never_glues_the_following_command_707() {
    let dir = scratch("resolve");
    let marker = dir.join("marker");
    fs::write(&marker, "").expect("create marker file");
    // Reproduces recording-e2e.sh's EXACT embedding shape: `$(cpu_affinity_burn_resolve_cmd)`
    // mid-string, followed by more literal text via a backslash-newline continuation — exactly
    // what the real sshpass ssh command strings look like.
    let harness = format!(
        r#"set -uo pipefail
. "$SCRIPT"
CMD="echo start; \
   $(cpu_affinity_burn_resolve_cmd) \
   rm -f {marker}; \
   echo done"
eval "$CMD"
"#,
        marker = marker.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("start") && stdout.contains("done"),
        "both echo markers must have run (proves the eval'd script didn't abort): {stdout}"
    );
    assert!(
        !marker.exists(),
        "the `rm -f <marker>` following the resolve cmd's embedding must run as its OWN command \
         — if it got glued onto the resolve cmd's last statement instead, the marker file would \
         still exist: {stdout}"
    );
}

// --- static-anchor: BOTH launch sites must wire in the SAME shared helper, so they can never
// silently diverge again (#707's whole point).

#[test]
fn regression_e2e_script_sources_the_cpu_affinity_burn_lib_707() {
    let text = recording_e2e_text();
    assert!(
        text.contains("lib/cpu-affinity-burn.sh"),
        "recording-e2e.sh must source scripts/lib/cpu-affinity-burn.sh (#707)"
    );
}

#[test]
fn regression_e2e_script_wires_the_helper_into_both_launch_sites_707() {
    let text = recording_e2e_text();
    let resolve_calls = text.matches("cpu_affinity_burn_resolve_cmd").count();
    let property_refs = text.matches("CPU_AFFINITY_BURN_PROPERTY").count();
    // [2/8] cam1 deploy and [2b/8] ALL_CAMBOX loop each call the resolve cmd once and reference
    // the resulting variable once in their own systemd-run invocation — 2 of each in the SCRIPT
    // TEXT (the loop body appears once in the source even though it runs N times at runtime)
    // proves both sites are wired to the SAME helper, not two independent copies.
    assert!(
        resolve_calls >= 2,
        "expected [2/8] + [2b/8] to each call cpu_affinity_burn_resolve_cmd, found {resolve_calls}"
    );
    assert!(
        property_refs >= 2,
        "expected [2/8] + [2b/8] to each reference CPU_AFFINITY_BURN_PROPERTY in their own \
         systemd-run invocation, found {property_refs}"
    );
}

#[test]
fn regression_cam1_systemd_run_carries_the_property_707() {
    let text = recording_e2e_text();
    let deploy_start = text
        .find("CAM1_BURN_BIN=\"/tmp/camera-box-burn-${RUN_ID}\"")
        .expect("#668/#707: expected the [2/8] cam1 burn-binary deploy block");
    let deploy_end = text[deploy_start..]
        .find("sleep 4")
        .map(|i| deploy_start + i)
        .expect("#668/#707: expected the deploy block to end at its post-launch `sleep 4`");
    let block = &text[deploy_start..deploy_end];
    assert!(
        block.contains("cpu_affinity_burn_resolve_cmd") && block.contains("CPU_AFFINITY_BURN_PROPERTY"),
        "#707: the [2/8] cam1 deploy's systemd-run invocation must carry the SAME CPU affinity \
         mask production has, via the shared helper. Block:\n{block}"
    );
    let resolve_idx = block.find("cpu_affinity_burn_resolve_cmd").unwrap();
    let property_idx = block.find("CPU_AFFINITY_BURN_PROPERTY").unwrap();
    assert!(
        resolve_idx < property_idx,
        "the resolve call must come before the property is referenced (the variable must be set \
         before it's read). Block:\n{block}"
    );
    assert!(
        block.contains("systemd-run") && block.contains("--unit="),
        "sanity: the block must still be the systemd-run deploy. Block:\n{block}"
    );
}

#[test]
fn regression_all_cambox_loop_systemd_run_carries_the_property_707() {
    let text = recording_e2e_text();
    let loop_start = text
        .find("for _cn_ip_burn in")
        .expect("#312/#707: recording-e2e.sh must define the [2b/8] ALL_CAMBOX deploy loop");
    let loop_end = text[loop_start..]
        .find("\n  done\n")
        .map(|i| loop_start + i)
        .expect("#312/#707: the [2b/8] loop must be closed by its own `done`");
    let loop_body = &text[loop_start..loop_end];
    assert!(
        loop_body.contains("cpu_affinity_burn_resolve_cmd")
            && loop_body.contains("CPU_AFFINITY_BURN_PROPERTY"),
        "#707: the [2b/8] ALL_CAMBOX loop's systemd-run invocation must carry the SAME CPU \
         affinity mask production has, via the shared helper. Loop body:\n{loop_body}"
    );
    let resolve_idx = loop_body.find("cpu_affinity_burn_resolve_cmd").unwrap();
    let property_idx = loop_body.find("CPU_AFFINITY_BURN_PROPERTY").unwrap();
    assert!(
        resolve_idx < property_idx,
        "the resolve call must come before the property is referenced. Loop body:\n{loop_body}"
    );
}
