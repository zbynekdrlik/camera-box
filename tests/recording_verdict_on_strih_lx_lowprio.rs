//! issue 1351 (item, 22.9.2026) — the in-place strih-lx recording decode (`[8/8a]`,
//! `scripts/recording-verdict-on-strih-lx.sh`) must run at IDLE priority pinned to the Intel
//! hybrid E-cores so it never starves the live OBS receivers (the post-run relock storms of issue
//! 1354: cam1 +763, cam4 +799, cam5 +832, cam7 +782 relocks during the `[7/8]`+`[8/8a]` window,
//! 0 during the recording). A multi-core QR/pixel sweep over a 1080p60 5-min recording at nice 0,
//! unpinned, competes with OBS's `ndir:video` decode threads on the P-cores.
//!
//! These tests pin the pure encoding `strih_lx_lowprio_prefix <sysfs_root>` (fixture-driven) and a
//! static anchor that STEP 2's ssh-run line invokes `$ONSTRIHLX_CMD` through that prefix. STEP 2
//! is the remote-shell replica of the same contract (the E-core range is read ON the box so a
//! replacement notebook resolves its own cores). Implementing the main design (comment 5781015909,
//! Prístup 1).

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn onstrihlx_script() -> PathBuf {
    let s = manifest_dir().join("scripts/recording-verdict-on-strih-lx.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

fn script_text() -> String {
    fs::read_to_string(onstrihlx_script()).expect("read recording-verdict-on-strih-lx.sh")
}

/// Source the launcher and call `strih_lx_lowprio_prefix <root>`. Returns trimmed stdout (the
/// helper uses `printf` with no trailing newline). Mirrors harness_strih_platform_1351.rs's
/// source-and-call shape.
fn lowprio_prefix(root: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(". \"$1\"; strih_lx_lowprio_prefix \"$2\"")
        .arg("bash")
        .arg(onstrihlx_script())
        .arg(root)
        .output()
        .expect("run strih_lx_lowprio_prefix");
    assert!(
        out.status.success(),
        "strih_lx_lowprio_prefix exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// Build a fake sysfs root; when `cpus` is `Some`, write `<root>/devices/cpu_atom/cpus` with that
/// content (a `None` leaves the file absent — a non-hybrid box).
fn fake_sysfs(cpus: Option<&str>) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    if let Some(c) = cpus {
        let atom = dir.path().join("devices/cpu_atom");
        fs::create_dir_all(&atom).expect("mkdir cpu_atom");
        fs::write(atom.join("cpus"), c).expect("write cpus");
    }
    dir
}

// ================================================================================================
// strih_lx_lowprio_prefix — the pure fallback contract.
// ================================================================================================

#[test]
fn hybrid_box_pins_the_e_cores_at_idle_priority() {
    // strih-lx (measured live 22.9.2026): 16 CPUs, cpu_atom = 12-15 (E), cpu_core = 0-11 (P).
    let root = fake_sysfs(Some("12-15\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19 taskset -c 12-15",
        "a hybrid box must run the decode at nice 19 pinned to its E-cores"
    );
}

#[test]
fn cpu_atom_file_absent_falls_back_to_nice_only() {
    let root = fake_sysfs(None);
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19",
        "a non-hybrid box (no cpu_atom) just runs deprioritised, no taskset"
    );
}

#[test]
fn empty_cpu_atom_file_never_emits_an_empty_taskset() {
    let root = fake_sysfs(Some(""));
    let got = lowprio_prefix(root.path().to_str().unwrap());
    assert_eq!(
        got, "nice -n 19",
        "an empty cpu_atom/cpus must fall back, never pin to nothing"
    );
    assert!(
        !got.contains("taskset"),
        "must never emit a bare `taskset -c \"\"`: {got:?}"
    );
}

#[test]
fn whitespace_only_cpu_atom_file_falls_back() {
    // A file that is present but holds only whitespace is not a usable range.
    let root = fake_sysfs(Some("   \n\t\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19",
        "whitespace-only cpu_atom/cpus must fall back to nice-only"
    );
}

#[test]
fn a_trailing_newline_in_the_range_is_stripped() {
    // The sysfs file always ends with a newline; the pin must be the clean range, no newline.
    let root = fake_sysfs(Some("8-15\n"));
    assert_eq!(
        lowprio_prefix(root.path().to_str().unwrap()),
        "nice -n 19 taskset -c 8-15",
        "the sysfs trailing newline must not leak into the taskset argument"
    );
}

// ================================================================================================
// STEP 2 static anchor — the on-box decode is launched through the low-priority prefix.
// ================================================================================================

#[test]
fn step2_ssh_run_launches_onstrihlx_cmd_through_the_lowprio_prefix() {
    let s = script_text();

    // The nice base + the E-core taskset arm, resolved on the box (single-quoted for the ssh side).
    assert!(
        s.contains(r#"LP="nice -n 19""#),
        "STEP 2 must set the idle-priority base `nice -n 19`"
    );
    assert!(
        s.contains("[ -s /sys/devices/cpu_atom/cpus ]"),
        "STEP 2 must guard the taskset arm on the Intel-hybrid E-core sysfs file"
    );
    assert!(
        s.contains("taskset -c $(cat /sys/devices/cpu_atom/cpus)"),
        "STEP 2 must pin to the E-cores read ON the box (so a replacement notebook uses its own map)"
    );

    // The on-box verdict command must be launched THROUGH the prefix: `$LP $ONSTRIHLX_CMD`.
    assert!(
        s.contains(r#"\$LP $ONSTRIHLX_CMD"#),
        "the ssh-run line must invoke $ONSTRIHLX_CMD through the low-priority prefix $LP"
    );
}

#[test]
fn the_lowprio_snippet_is_single_quoted_so_dev1_never_expands_the_cat() {
    // The `$(cat …)` and `$LP` inside the snippet must be evaluated by the strih-lx shell, not by
    // dev1 — so the snippet is assigned with SINGLE quotes on the dev1 side.
    let s = script_text();
    assert!(
        s.contains(r#"LOWPRIO_SNIPPET='LP="nice -n 19";"#),
        "the low-priority snippet must be single-quoted on the dev1 side (remote evaluation)"
    );
}

#[test]
fn the_probe_scp_and_partial_pull_are_unchanged() {
    // The change is confined to STEP 2's launch prefix — the binary deploy and the partial/pixels
    // pull-back stay byte-identical (small files, seconds).
    let s = script_text();
    assert!(
        s.contains(r#"sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "$VERDICT_BIN" "${TARGET}:${REMOTE_BIN}""#),
        "the binary scp must be unchanged"
    );
    assert!(
        s.contains(r#"sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "${TARGET}:${OUT_PARTIAL}" "$local_partial""#),
        "the partial pull-back must be unchanged"
    );
}
