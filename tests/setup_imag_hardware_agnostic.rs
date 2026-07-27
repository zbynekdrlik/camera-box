//! `scripts/setup-imag.sh` must provision ANY notebook that takes the imag role — not only the
//! ONE box it was written against (#816, child of the #791 imag-nb umbrella).
//!
//! Three assumptions were baked in as literals and each one breaks on a different machine:
//!   1. CPU isolation `isolcpus=2..11 nohz_full=10,11 irqaffinity=0,1,12,13,14,15` — verified by
//!      hand against the OLD box's 16-thread topology. The replacement notebook (i5-13420H,
//!      live-profiled 2026-07-27) has only 12 threads: `irqaffinity=…,12,13,14,15` names CPUs that
//!      do not exist, and isolating 2-11 would leave NOTHING but cpu0,1 for openbox/Xorg/sshd.
//!   2. `nvidia-driver-595-open` + `prime-select nvidia`, both `fail`-hard. The replacement box has
//!      NO discrete GPU at all (Intel UHD only, live-checked `lspci`) — a mandatory NVIDIA step
//!      aborts provisioning on a box that is otherwise perfectly fine.
//!   3. `STATIC_IP=10.77.9.182` — the address of the box being REPLACED. Two imag notebooks cannot
//!      hold the same address while the old one is still running.
//!
//! Same convention as `tests/setup_imag_pure_functions.rs`: SOURCE the real script (its
//! `BASH_SOURCE[0] != $0` guard skips the destructive provisioning flow) and run the pure
//! functions directly, plus a few textual guards that the CALL SITES actually use them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/setup-imag.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn body() -> String {
    fs::read_to_string(script()).expect("read setup-imag.sh")
}

/// Source the REAL script and run `body` against its pure functions.
fn run_sourced(body: &str) -> (i32, String, String) {
    run_sourced_env(body, &[])
}

fn run_sourced_env(body: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `/sys/devices/system/cpu/cpuN/topology/thread_siblings_list` as gathered on the ORIGINAL imag
/// notebook: 6 SMT-paired P-cores (cpu0-11) + 4 unpaired E-cores (cpu12-15).
const OLD_BOX_SIBLINGS: &str = "\
0 0-1
1 0-1
2 2-3
3 2-3
4 4-5
5 4-5
6 6-7
7 6-7
8 8-9
9 8-9
10 10-11
11 10-11
12 12
13 13
14 14
15 15
";

/// The REPLACEMENT notebook, live-profiled on 10.77.9.187 (2026-07-27, i5-13420H):
/// 4 SMT-paired P-cores (cpu0-7) + 4 unpaired E-cores (cpu8-11). 12 threads total.
const NEW_BOX_SIBLINGS: &str = "\
0 0-1
1 0-1
2 2-3
3 2-3
4 4-5
5 4-5
6 6-7
7 6-7
8 8
9 9
10 10
11 11
";

/// The derived plan must reproduce the hand-verified OLD-box values BYTE-FOR-BYTE — the whole
/// point of deriving it is to generalise WITHOUT changing the machine it was tuned on.
#[test]
fn cpu_isolation_plan_reproduces_the_old_box_values_exactly() {
    let (code, out, err) = run_sourced(&format!(
        "printf '%s' {} | imag_cpu_isolation_plan",
        shell_quote(OLD_BOX_SIBLINGS)
    ));
    assert_eq!(code, 0, "plan should resolve. stderr: {err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines,
        vec!["2,3,4,5,6,7,8,9,10,11", "10,11", "0,1,12,13,14,15"],
        "OLD-box plan must equal the hand-tuned #483 values (isolated / nohz_full / housekeeping)"
    );
}

/// The replacement box has 4 fewer threads and its E-cores sit at 8-11, so every one of the three
/// hardcoded lists is wrong there.
#[test]
fn cpu_isolation_plan_adapts_to_the_replacement_notebook() {
    let (code, out, err) = run_sourced(&format!(
        "printf '%s' {} | imag_cpu_isolation_plan",
        shell_quote(NEW_BOX_SIBLINGS)
    ));
    assert_eq!(code, 0, "plan should resolve. stderr: {err}");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        lines,
        vec!["2,3,4,5,6,7", "6,7", "0,1,8,9,10,11"],
        "the i5-13420H plan must isolate the P-core block minus P-core0, keep every E-core for \
         housekeeping, and put nohz_full on the LAST isolated P-core pair"
    );
    assert!(
        !out.contains("12") && !out.contains("13") && !out.contains("14") && !out.contains("15"),
        "the plan must never name a CPU the box does not have: {out}"
    );
}

/// A box with too few P-cores cannot host the OBS thread pool AND keep a housekeeping core —
/// fail LOUD instead of silently isolating everything (script-failure-policy).
#[test]
fn cpu_isolation_plan_fails_loud_on_a_box_with_too_few_p_cores() {
    let tiny = "0 0-1\n1 0-1\n2 2\n3 3\n";
    let (code, _out, err) = run_sourced(&format!(
        "printf '%s' {} | imag_cpu_isolation_plan",
        shell_quote(tiny)
    ));
    assert_ne!(code, 0, "a 1-P-core box must fail, not produce a plan");
    assert!(
        err.to_lowercase().contains("core"),
        "the error must explain the topology is too small: {err}"
    );
}

/// The NVIDIA step must run only when a discrete NVIDIA GPU is actually present.
#[test]
fn nvidia_presence_is_detected_from_lspci_not_assumed() {
    let with_gpu = "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n\
                    01:00.0 3D controller [0302]: NVIDIA Corporation GB207M [GeForce RTX 5050] [10de:2dd8]\n";
    let (code, _o, err) = run_sourced(&format!(
        "printf '%s' {} | imag_has_discrete_nvidia",
        shell_quote(with_gpu)
    ));
    assert_eq!(code, 0, "an NVIDIA 3D controller must be detected: {err}");

    let igpu_only =
        "00:02.0 VGA compatible controller [0300]: Intel Corporation Raptor Lake-P [8086:a7a8]\n\
         00:06.2 PCI bridge [0604]: Intel Corporation Device [8086:a73d]\n";
    let (code, _o, _e) = run_sourced(&format!(
        "printf '%s' {} | imag_has_discrete_nvidia",
        shell_quote(igpu_only)
    ));
    assert_ne!(
        code, 0,
        "an Intel-iGPU-only box must NOT be reported as having a discrete NVIDIA GPU"
    );
}

/// Call-site guards: the derived plan and the GPU probe must actually be USED — a pure function
/// nothing calls fixes nothing.
#[test]
fn the_provisioning_flow_uses_the_derived_values_not_literals() {
    let b = body();
    assert!(
        b.contains("imag_cpu_isolation_plan"),
        "step 8 must derive the isolation plan"
    );
    assert!(
        !b.contains("isolcpus=2,3,4,5,6,7,8,9,10,11"),
        "the hardcoded 16-thread isolcpus list must be gone"
    );
    assert!(
        !b.contains("irqaffinity=0,1,12,13,14,15"),
        "the hardcoded 16-thread irqaffinity list must be gone"
    );
    assert!(
        !b.contains("taskset -c 2-11 obs"),
        "both OBS launches must pin to the DERIVED isolated set, not the literal 2-11"
    );
    assert!(
        b.contains("imag_has_discrete_nvidia"),
        "the NVIDIA step must be gated on a real discrete GPU"
    );
    // the driver install itself must sit INSIDE the gate, not run unconditionally
    let gate = b
        .find("imag_has_discrete_nvidia")
        .expect("gate call must exist");
    let install = b
        .find("nvidia-driver-595-open install failed")
        .expect("driver install must exist");
    assert!(
        gate < install,
        "the discrete-GPU gate must precede the nvidia driver install"
    );
}

/// The address must be overridable — the replacement box cannot take 10.77.9.182 while the box it
/// replaces is still live on it.
#[test]
fn static_ip_is_overridable_and_defaults_to_the_incumbent() {
    let (code, out, err) = run_sourced("printf '%s\\n' \"$STATIC_IP\"");
    assert_eq!(code, 0, "sourcing must succeed. stderr: {err}");
    assert_eq!(out.trim(), "10.77.9.182", "default must stay the incumbent");

    let (code, out, err) = run_sourced_env(
        "printf '%s\\n' \"$STATIC_IP\"",
        &[("IMAG_IP", "10.77.9.187")],
    );
    assert_eq!(code, 0, "sourcing with IMAG_IP must succeed. stderr: {err}");
    assert_eq!(
        out.trim(),
        "10.77.9.187",
        "IMAG_IP must override the provisioned address"
    );
}

/// The NDI runtime is copied from a fleet cam box — pinning cam1 alone means provisioning FAILS
/// whenever that one box is down (it was down on 2026-07-27). Any reachable cam will do.
#[test]
fn ndi_peer_falls_back_to_any_reachable_cam() {
    // "host status" lines, in preference order; the first OK one wins.
    let probe = "10.77.9.61 down\n10.77.9.62 down\n10.77.9.63 up\n10.77.9.65 up\n";
    let (code, out, err) = run_sourced(&format!(
        "printf '%s' {} | imag_pick_ndi_peer",
        shell_quote(probe)
    ));
    assert_eq!(code, 0, "a reachable peer must be picked. stderr: {err}");
    assert_eq!(out.trim(), "10.77.9.63", "first REACHABLE candidate wins");

    let none = "10.77.9.61 down\n10.77.9.62 down\n";
    let (code, _out, err) = run_sourced(&format!(
        "printf '%s' {} | imag_pick_ndi_peer",
        shell_quote(none)
    ));
    assert_ne!(code, 0, "no reachable cam must fail loud, not return empty");
    assert!(
        err.to_lowercase().contains("ndi"),
        "the error must name what could not be fetched: {err}"
    );
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}
