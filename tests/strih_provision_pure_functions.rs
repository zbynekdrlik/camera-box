//! Functional (execution) guard for `scripts/lib/strih-provision.sh`'s pure #1317 decision
//! helpers — the Linux strih notebook (`strih-lx`) role FACTS + decisions.
//!
//! Same convention as `tests/setup_imag_pure_functions.rs::run_sourced` /
//! `tests/deploy_genlock_fleet.rs`: the lib is source-only (no top-level statements, its own
//! `# airuleset:script-ok` header), so sourcing it defines only the pure functions in the
//! harness shell — no root, no network, no side effects. This closes the gap a purely textual
//! guard cannot: it catches a silent LOGIC inversion (e.g. the client-not-master check flipped,
//! or a STRIH-SNV collision guard that stops firing).
//!
//! These run on CI (Tier-0 bans local `cargo test`); a green run here proves the pure decisions
//! the strih provisioning + acceptance gate rely on.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib() -> PathBuf {
    let s = manifest_dir().join("scripts/lib/strih-provision.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Read one of the orchestrator scripts as text (for the static-anchor wiring tests below).
fn read_script(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the real lib and run `body`. Returns (exit_code, stdout, stderr). `env` is passed as
/// KEY=VALUE pairs so a test can drive the STRIH_LX_* seams without leaking into other tests.
fn run_sourced(env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", lib());
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

#[test]
fn lib_is_source_only_and_defines_the_pure_functions() {
    // Sourcing must succeed with no top-level side effects (exit 0, no stderr noise).
    let (code, _o, err) = run_sourced(&[], "type strih_lx_ndi_inputs >/dev/null");
    assert_eq!(code, 0, "sourcing the lib must succeed; stderr={err}");
}

#[test]
fn ndi_inputs_are_the_ten_role_inputs() {
    let (code, out, _e) = run_sourced(&[], "strih_lx_ndi_inputs");
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 10, "exactly 10 NDI inputs, got: {out}");
    for want in [
        "CAM1 (usb)",
        "CAM7 (usb)",
        "STRIH-SNV (2ME PGM)",
        "STRIH-SNV (2ME PVW)",
        "RESOLUME-SNV (cg-obs)",
    ] {
        assert!(lines.contains(&want), "missing input {want} in: {out}");
    }
}

#[test]
fn ndi_outputs_and_republishes_are_namespaced_strih_lx_never_strih_snv() {
    let (_c, outs, _e) = run_sourced(&[], "strih_lx_ndi_outputs; strih_lx_ndi_republishes");
    for l in outs.lines().filter(|l| !l.is_empty()) {
        assert!(
            l.starts_with("STRIH-LX ("),
            "every output/republish must be STRIH-LX-namespaced, got: {l}"
        );
        assert!(
            !l.starts_with("STRIH-SNV "),
            "a STRIH-SNV sender leaked: {l}"
        );
    }
    assert!(outs.contains("STRIH-LX (2ME PGM)"));
    assert!(outs.contains("STRIH-LX (2ME PVW)"));
    assert!(outs.contains("STRIH-LX (MULTIVIEW)"));
}

#[test]
fn camera_latency_is_the_rig_floor_three() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_camera_latency_ms");
    assert_eq!(out.trim(), "3");
}

#[test]
fn bundle_artifact_is_the_strih_variant() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_bundle_artifact");
    assert_eq!(out.trim(), "obs-genlock-linux-x86_64-strih");
}

#[test]
fn dantesync_client_args_point_at_the_ntp_server_and_never_server_mode() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_dantesync_client_args");
    assert!(out.contains("--ntp-server strih.lan"), "got: {out}");
    assert!(
        !out.contains("server_mode") && !out.contains("--master"),
        "must not be master: {out}"
    );
    // Overridable NTP server seam.
    let (_c2, out2, _e2) = run_sourced(
        &[("STRIH_LX_NTP_SERVER", "strih2.lan")],
        "strih_lx_dantesync_client_args",
    );
    assert!(
        out2.contains("--ntp-server strih2.lan"),
        "override ignored: {out2}"
    );
}

#[test]
fn dantesync_client_check_is_fail_closed_and_rejects_master_modes() {
    // Client modes pass.
    for mode in ["client", "ntp-server=strih.lan", "slave"] {
        let (code, _o, _e) = run_sourced(
            &[],
            &format!("strih_lx_dantesync_is_client_not_master '{mode}'"),
        );
        assert_eq!(code, 0, "client mode '{mode}' should pass");
    }
    // Master/server/empty must FAIL (fail-closed).
    for mode in ["ntp_server_mode", "server", "master", "grandmaster", ""] {
        let (code, _o, _e) = run_sourced(
            &[],
            &format!("strih_lx_dantesync_is_client_not_master '{mode}'"),
        );
        assert_ne!(code, 0, "master/empty mode '{mode}' must fail-closed");
    }
}

#[test]
fn profile_facts_carry_the_windows_light_profile_shape() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_profile_facts");
    for want in [
        "base_res=1920x1080",
        "fps=30",
        "color_format=NV12",
        "out_mode=Advanced",
        "rec_encoder=obs_nvenc_hevc_tex",
        "rec_path=/srv/_REC",
        "rec_format=mkv",
        "rec_split_min=15",
    ] {
        assert!(out.contains(want), "profile fact missing {want} in: {out}");
    }
}

#[test]
fn audio_route_is_a_fail_loud_todo_until_wired() {
    // Unwired (default) -> the predicate fails, so setup-strih's audio step FAILS loud.
    let (code, _o, _e) = run_sourced(&[], "strih_lx_audio_route_wired");
    assert_ne!(
        code, 0,
        "audio route must read UNWIRED by default (fail-loud TODO)"
    );
    // Explicitly wired -> passes.
    let (code2, _o2, _e2) = run_sourced(
        &[("STRIH_LX_AUDIO_WIRED", "1")],
        "strih_lx_audio_route_wired",
    );
    assert_eq!(
        code2, 0,
        "audio route must read wired when STRIH_LX_AUDIO_WIRED=1"
    );
    let (_c, name, _e) = run_sourced(&[], "strih_lx_audio_input_name");
    assert_eq!(name.trim(), "MiniFuse 4");
}

#[test]
fn output_name_ok_accepts_strih_lx_and_rejects_strih_snv() {
    let (c1, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'STRIH-LX (2ME PGM)'");
    assert_eq!(c1, 0);
    let (c2, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'STRIH-SNV (2ME PGM)'");
    assert_ne!(c2, 0, "a STRIH-SNV output name must be rejected");
    let (c3, _o, _e) = run_sourced(&[], "strih_lx_output_name_ok 'random'");
    assert_ne!(c3, 0);
}

#[test]
fn no_second_strih_snv_sender_guard_fires_on_a_collision() {
    // A clean strih-lx-only output set passes.
    let (code, _o, _e) = run_sourced(
        &[],
        "printf '%s\\n' 'STRIH-LX (2ME PGM)' 'STRIH-LX (MULTIVIEW)' | strih_lx_no_second_strihsnv_sender",
    );
    assert_eq!(code, 0, "a clean STRIH-LX-only output set must pass");
    // A STRIH-SNV name in the live set must fail (never a 2nd STRIH-SNV sender).
    let (code2, _o, _e) = run_sourced(
        &[],
        "printf '%s\\n' 'STRIH-LX (2ME PGM)' 'STRIH-SNV (2ME PGM)' | strih_lx_no_second_strihsnv_sender",
    );
    assert_ne!(code2, 0, "a second STRIH-SNV sender must be rejected");
}

// --- issue 1317 (F6): chrome-sandbox setuid-root builder + verdict ------------------------------

#[test]
fn chrome_sandbox_fix_cmd_emits_find_chown_root_and_chmod_4755() {
    // The builder must locate chrome-sandbox by NAME under the bundle root and chown root:root +
    // chmod 4755 it (Chromium's SUID sandbox contract). --no-sandbox (the rejected approach) must
    // never appear.
    let (code, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_fix_cmd /opt/obs-genlock");
    assert_eq!(code, 0, "builder must succeed");
    assert!(
        out.contains("-name chrome-sandbox"),
        "must locate chrome-sandbox by name: {out}"
    );
    assert!(
        out.contains("chown root:root"),
        "must chown root:root: {out}"
    );
    assert!(
        out.contains("chmod 4755"),
        "must chmod 4755 (setuid root): {out}"
    );
    assert!(
        !out.contains("--no-sandbox"),
        "must not weaken the sandbox with --no-sandbox: {out}"
    );
    // Every emitted statement is ;-terminated so a mid-string $(...) embedding never glues the
    // following command (the v4l2-neutral.sh _cmd-helper gotcha): the last non-empty line ends `;`.
    let last = out
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    assert!(
        last.trim_end().ends_with(';'),
        "the last emitted statement must end with ';': {out}"
    );
}

#[test]
fn chrome_sandbox_fix_cmd_shell_quotes_a_root_with_spaces() {
    // %q-style quoting keeps a space in the bundle root from splitting into two find arguments.
    let (_c, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_fix_cmd '/opt/obs genlock'");
    assert!(
        out.contains("/opt/obs\\ genlock") || out.contains("'/opt/obs genlock'"),
        "a bundle root with spaces must be shell-quoted: {out}"
    );
}

#[test]
fn chrome_sandbox_verdict_tokens_ok_missing_wrong_owner_wrong_mode() {
    // root:root + 4755 + present -> ok (exit 0).
    let (c, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_verdict root:root 4755 1");
    assert_eq!(c, 0, "root:root/4755/present must be ok");
    assert_eq!(out.trim(), "ok");
    // absent -> missing (fail-closed, checked first).
    let (c, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_verdict '?' '?' 0");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "missing");
    // wrong owner -> wrong-owner.
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_chrome_sandbox_verdict newlevel:newlevel 4755 1",
    );
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "wrong-owner");
    // not setuid (mode 700, the artifact's own state) -> wrong-mode.
    let (c, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_verdict root:root 700 1");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "wrong-mode");
}

// --- issue 1317 (owner ROZHODNUTÉ 18.9.): bundle-vs-box release parity -----------------------------

/// `strih_lx_release_parity_ok BUNDLE_FLAGS_TEXT BOX_VERSION_ID` -> 0 iff the flags text carries the
/// marker line `TARGET-RELEASE: ubuntu-<BOX_VERSION_ID>`. Fail-closed: a mismatch (a 24.04-built
/// bundle on a 26.04 box — the ffmpeg/Qt soname crash) OR an absent marker (a pre-marker bundle that
/// must be rebuilt, never trusted) OR an empty box VERSION_ID all return non-zero.
#[test]
fn release_parity_ok_matches_the_marker_and_is_fail_closed() {
    let flags_2604 = "variant=strih\\nENABLE_BROWSER=ON\\nBROWSER-ON: obs-browser + CEF 6533\\nTARGET-RELEASE: ubuntu-26.04";
    let (code, _o, _e) = run_sourced(
        &[],
        &format!("strih_lx_release_parity_ok $'{flags_2604}' 26.04"),
    );
    assert_eq!(
        code, 0,
        "a bundle TARGET-RELEASE == box VERSION_ID must pass"
    );

    // 24.04-built bundle on a 26.04 box -> FAIL (the soname mismatch this gate exists to catch).
    let flags_2404 = "variant=strih\\nENABLE_BROWSER=ON\\nTARGET-RELEASE: ubuntu-24.04";
    let (code, _o, _e) = run_sourced(
        &[],
        &format!("strih_lx_release_parity_ok $'{flags_2404}' 26.04"),
    );
    assert_ne!(code, 0, "a 24.04 bundle on a 26.04 box must FAIL");

    // no TARGET-RELEASE marker at all -> fail-closed (a pre-marker bundle must be rebuilt).
    let flags_none = "variant=strih\\nENABLE_BROWSER=ON\\nBROWSER-ON: obs-browser + CEF 6533";
    let (code, _o, _e) = run_sourced(
        &[],
        &format!("strih_lx_release_parity_ok $'{flags_none}' 26.04"),
    );
    assert_ne!(code, 0, "an absent TARGET-RELEASE marker must fail closed");

    // empty box VERSION_ID -> fail-closed (the box release must be known).
    let (code, _o, _e) = run_sourced(
        &[],
        &format!("strih_lx_release_parity_ok $'{flags_2604}' ''"),
    );
    assert_ne!(code, 0, "an empty box VERSION_ID must fail closed");
}

/// issue 1317: `setup-strih.sh` step 4 must gate on release parity BEFORE the `cp -a` bundle install
/// — a bundle built for another Ubuntu release must never be copied onto the box.
#[test]
fn setup_strih_gates_release_parity_before_installing_the_bundle() {
    let s = read_script("scripts/setup-strih.sh");
    let gate = s
        .find("strih_lx_release_parity_ok")
        .expect("setup-strih must call strih_lx_release_parity_ok in step 4");
    let install = s
        .find("cp -a \"${STRIH_LX_BUNDLE_SRC%/}/.\"")
        .expect("setup-strih must install the bundle via cp -a");
    assert!(
        gate < install,
        "the release-parity gate must run BEFORE the bundle cp -a (never install a wrong-release bundle)"
    );
}

/// issue 1317: `verify-strih.sh` must assert bundle-vs-box release parity as an acceptance item,
/// reading the installed TARGET-RELEASE marker vs the box's os-release VERSION_ID.
#[test]
fn verify_strih_carries_the_release_parity_check() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_release_parity_ok"),
        "verify-strih must run the release-parity predicate"
    );
    assert!(
        v.contains("TARGET-RELEASE") || v.contains("VERSION_ID"),
        "verify-strih must reference the release marker / os-release it compares"
    );
}

// =============================================================================
// issue 1317 item H: strih-lx USB-NIC xhci IRQ placement (NET_RX softirq off the OBS cores).
// The pure resolvers + the emitted boot script + the systemd unit are driven over /proc-shaped
// fixtures matching the real box (issue 1354: iface enx6c1ff766154b -> xhci PCI function
// 0000:00:14.0 -> IRQ 125, cpu_atom 12-15 -> target E-core 15).
// =============================================================================

/// Build a /sys + /proc/interrupts + cpu_atom + /proc/irq fixture shaped like the real strih-lx box
/// and return the TempDir (keep it alive; $FX = its path in the harness bodies below).
fn irq_fixture(iface: &str) -> tempfile::TempDir {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // the USB NIC device path: .../pci0000:00/0000:00:14.0/usb2/2-2/2-2:1.0
    let devdir = root.join("sys/devices/pci0000:00/0000:00:14.0/usb2/2-2/2-2:1.0");
    std::fs::create_dir_all(&devdir).unwrap();
    let netdir = root.join("sys/class/net").join(iface);
    std::fs::create_dir_all(&netdir).unwrap();
    symlink(&devdir, netdir.join("device")).unwrap();
    // the real xhci row (125, with the PCI function in its IR-PCI-MSI chip column), a DECOY second
    // xhci controller (0000:00:0d.0 / IRQ 200) that must NOT be selected, and a non-xhci row.
    let interrupts = "            CPU0       CPU1\n \
        1:  9  0  IO-APIC  1-edge  i8042\n \
        125:  0 3946905142  IR-PCI-MSI-0000:00:14.0  0-edge  xhci_hcd\n \
        200:  12  3  IR-PCI-MSI-0000:00:0d.0  0-edge  xhci_hcd\n \
        300:  5  0  IR-PCI-MSI-0000:00:1f.6  0-edge  eno1\n";
    std::fs::write(root.join("interrupts"), interrupts).unwrap();
    std::fs::write(root.join("cpu_atom"), "12-15\n").unwrap();
    std::fs::create_dir_all(root.join("irq/125")).unwrap();
    std::fs::write(root.join("irq/125/smp_affinity_list"), "6\n").unwrap();
    dir
}

#[test]
fn nic_xhci_resolves_iface_to_pci_function_irq_and_target_cpu() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (c1, pci, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_pci_function \"$FX/sys\" enx6c1ff766154b",
    );
    assert_eq!(c1, 0);
    assert_eq!(pci.trim(), "0000:00:14.0", "pci function; got {pci}");
    let (c2, irqs, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_irqs \"$FX/interrupts\" 0000:00:14.0",
    );
    assert_eq!(c2, 0);
    assert_eq!(irqs.trim(), "125", "xhci irqs; got {irqs}");
    let (c3, cpu, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_irq_target_cpu \"$FX/cpu_atom\"",
    );
    assert_eq!(c3, 0);
    assert_eq!(
        cpu.trim(),
        "15",
        "target cpu (last cpu_atom E-core); got {cpu}"
    );
}

#[test]
fn nic_xhci_irqs_selects_only_the_matching_controller() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (_c, irqs, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_irqs \"$FX/interrupts\" 0000:00:14.0",
    );
    let lines: Vec<&str> = irqs.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines,
        vec!["125"],
        "must select ONLY 0000:00:14.0's IRQ (never the decoy 0000:00:0d.0 / 200); got {irqs}"
    );
    // a controller with no matching row fails-closed (no IRQ, non-zero).
    let (c2, out2, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_irqs \"$FX/interrupts\" 0000:00:99.9",
    );
    assert_ne!(c2, 0, "a non-present controller must fail-closed");
    assert!(out2.trim().is_empty());
}

#[test]
fn nic_xhci_pci_function_fails_loud_on_missing_iface() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (code, out, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_pci_function \"$FX/sys\" ghost0",
    );
    assert_ne!(code, 0, "a missing iface must fail-closed");
    assert!(
        out.trim().is_empty(),
        "no PCI function printed for a missing iface"
    );
}

#[test]
fn nic_irq_target_cpu_last_atom_and_online_fallback() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (_c, cpu, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_irq_target_cpu \"$FX/cpu_atom\"",
    );
    assert_eq!(cpu.trim(), "15");
    // non-hybrid: cpu_atom absent -> highest online cpu.
    std::fs::write(dir.path().join("online"), "0-7\n").unwrap();
    let (_c2, cpu2, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_irq_target_cpu \"$FX/no-such-atom\" \"$FX/online\"",
    );
    assert_eq!(
        cpu2.trim(),
        "7",
        "fallback = highest online cpu; got {cpu2}"
    );
}

#[test]
fn nic_irq_affinity_verdict_ok_multi_below() {
    let cases: &[(&str, &str, &str, i32)] = &[
        ("15", "12", "ok", 0),
        ("12-15", "12", "multi", 1),
        ("6,15", "12", "multi", 1),
        ("6", "12", "below-atom", 1),
        ("3", "", "ok", 0), // non-hybrid: no floor check
    ];
    for (aff, atom, want_tok, want_code) in cases {
        let (code, out, _e) = run_sourced(
            &[],
            &format!("strih_nic_irq_affinity_verdict '{aff}' '{atom}'"),
        );
        assert_eq!(code, *want_code, "verdict code for aff={aff} atom={atom}");
        assert_eq!(
            out.trim(),
            *want_tok,
            "verdict token for aff={aff} atom={atom}"
        );
    }
}

#[test]
fn irq_total_count_sums_the_row_and_counter_advanced_is_liveness() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (_c, tot, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_irq_total_count \"$FX/interrupts\" 125",
    );
    assert_eq!(
        tot.trim(),
        "3946905142",
        "sum of the IRQ row's per-cpu counts; got {tot}"
    );
    // advancing predicate: > is advancing, == is a frozen IRQ, garbage fail-closed.
    let (ca, _o, _e) = run_sourced(&[], "strih_counter_advanced 100 200");
    assert_eq!(ca, 0, "200 > 100 must read as advancing");
    let (cb, _o, _e) = run_sourced(&[], "strih_counter_advanced 200 200");
    assert_ne!(
        cb, 0,
        "equal counters must NOT read as advancing (a frozen IRQ)"
    );
    let (cc, _o, _e) = run_sourced(&[], "strih_counter_advanced x 200");
    assert_ne!(cc, 0, "garbage must fail-closed");
}

#[test]
fn cpulist_min_and_max() {
    for (list, minv, maxv) in [("12-15", "12", "15"), ("3,1,2", "1", "3"), ("5", "5", "5")] {
        let (_c, mn, _e) = run_sourced(&[], &format!("strih_cpulist_min '{list}'"));
        assert_eq!(mn.trim(), minv, "min of {list}");
        let (_c, mx, _e) = run_sourced(&[], &format!("strih_cpulist_max '{list}'"));
        assert_eq!(mx.trim(), maxv, "max of {list}");
    }
}

#[test]
fn emitted_irq_script_resolves_and_writes_the_target_cpu() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let body = "strih_nic_irq_affinity_script_text > \"$FX/affinity.sh\"; \
                SYS_ROOT=\"$FX/sys\" PROC_INTERRUPTS=\"$FX/interrupts\" CPU_ATOM_FILE=\"$FX/cpu_atom\" \
                IRQ_DIR=\"$FX/irq\" STRIH_NIC_IFACE=enx6c1ff766154b bash \"$FX/affinity.sh\"";
    let (code, out, err) = run_sourced(&[("FX", root.as_str())], body);
    assert_eq!(
        code, 0,
        "emitted script must succeed over the fixture; stderr={err} stdout={out}"
    );
    assert!(
        out.contains("IRQ 125"),
        "emitted script must log IRQ 125; got {out}"
    );
    let got = std::fs::read_to_string(dir.path().join("irq/125/smp_affinity_list")).unwrap();
    assert_eq!(
        got.trim(),
        "15",
        "emitted script must pin IRQ 125 to cpu 15; wrote {got}"
    );
}

#[test]
fn emitted_irq_script_fails_loud_when_iface_unresolvable() {
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let body = "strih_nic_irq_affinity_script_text > \"$FX/affinity.sh\"; \
                SYS_ROOT=\"$FX/sys\" PROC_INTERRUPTS=\"$FX/interrupts\" CPU_ATOM_FILE=\"$FX/cpu_atom\" \
                IRQ_DIR=\"$FX/irq\" STRIH_NIC_IFACE=ghost0 bash \"$FX/affinity.sh\"";
    let (code, out, err) = run_sourced(&[("FX", root.as_str())], body);
    assert_ne!(
        code, 0,
        "a ghost iface must make the emitted script exit non-zero"
    );
    assert!(
        format!("{out}{err}").contains("FATAL"),
        "must print a FATAL journal line; out={out} err={err}"
    );
    // a failed resolution must NOT have written any affinity (the fixture's original "6" stands).
    let got = std::fs::read_to_string(dir.path().join("irq/125/smp_affinity_list")).unwrap();
    assert_eq!(
        got.trim(),
        "6",
        "a failed resolution must not write an affinity"
    );
}

#[test]
fn emitted_irq_script_resolution_matches_the_lib_helpers() {
    // The emitted boot script and the verify-side lib helpers MUST resolve the same IRQ + cpu over
    // the same fixture (they are two consumers of one behaviour -- a drift would fail here).
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (_c, pci, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_xhci_pci_function \"$FX/sys\" enx6c1ff766154b",
    );
    let (_c, irqs, _e) = run_sourced(
        &[("FX", root.as_str())],
        &format!("strih_nic_xhci_irqs \"$FX/interrupts\" {}", pci.trim()),
    );
    let (_c, cpu, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_irq_target_cpu \"$FX/cpu_atom\"",
    );
    assert_eq!(
        (pci.trim(), irqs.trim(), cpu.trim()),
        ("0000:00:14.0", "125", "15"),
        "lib helpers must resolve the same IRQ 125 -> cpu 15 the emitted script writes"
    );
}

#[test]
fn emitted_irq_script_and_lib_agree_on_a_multi_cluster_cpu_atom() {
    // The emitted boot script resolves target-cpu with tr/sed/sort/tail; the lib uses
    // strih_cpulist_max. Prove they agree on a MULTI-CLUSTER cpu_atom ("8-11,12-15" -> 15), not
    // just the single-range real-box fixture, so a future divergence between the two would fail.
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    std::fs::write(dir.path().join("cpu_atom"), "8-11,12-15\n").unwrap();
    let (_c, lib_cpu, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_irq_target_cpu \"$FX/cpu_atom\"",
    );
    assert_eq!(
        lib_cpu.trim(),
        "15",
        "lib target cpu on a multi-cluster cpu_atom; got {lib_cpu}"
    );
    let body = "strih_nic_irq_affinity_script_text > \"$FX/affinity.sh\"; \
                SYS_ROOT=\"$FX/sys\" PROC_INTERRUPTS=\"$FX/interrupts\" CPU_ATOM_FILE=\"$FX/cpu_atom\" \
                IRQ_DIR=\"$FX/irq\" STRIH_NIC_IFACE=enx6c1ff766154b bash \"$FX/affinity.sh\"";
    let (code, _out, err) = run_sourced(&[("FX", root.as_str())], body);
    assert_eq!(
        code, 0,
        "emitted script must succeed on a multi-cluster cpu_atom; stderr={err}"
    );
    let got = std::fs::read_to_string(dir.path().join("irq/125/smp_affinity_list")).unwrap();
    assert_eq!(
        got.trim(),
        "15",
        "emitted script must also pick the last E-core (15); wrote {got}"
    );
}

#[test]
fn nic_irq_affinity_unit_is_enable_only_and_byte_parity_with_committed_file() {
    let (_c, unit, _e) = run_sourced(&[], "strih_nic_irq_affinity_unit_text");
    let committed = read_script("systemd/strih-nic-irq-affinity.service");
    assert_eq!(
        unit, committed,
        "the printer must equal the committed unit byte-for-byte"
    );
    assert!(unit.contains("Type=oneshot"), "must be a oneshot");
    assert!(unit.contains("RemainAfterExit=yes"), "must RemainAfterExit");
    assert!(
        unit.contains("After=network-pre.target"),
        "must order after network-pre.target"
    );
    assert!(
        unit.contains("WantedBy=multi-user.target"),
        "must be a system unit (multi-user)"
    );
    assert!(
        unit.contains("ExecStart=/usr/local/bin/strih-nic-irq-affinity.sh"),
        "must run the emitted script"
    );
    assert!(
        !unit.to_lowercase().contains("--now") && !unit.contains("ExecStartPre"),
        "the unit is enable-only: no live start / no ExecStartPre"
    );
}

#[test]
fn setup_strih_installs_the_irq_affinity_script_and_enables_the_unit_enable_only() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("strih_nic_irq_affinity_script_text > /usr/local/bin/strih-nic-irq-affinity.sh"),
        "setup-strih must emit the affinity script to /usr/local/bin"
    );
    assert!(
        s.contains("systemctl enable strih-nic-irq-affinity.service"),
        "setup-strih must enable the unit"
    );
    assert!(
        !s.contains("systemctl start strih-nic-irq-affinity")
            && !s.contains("--now strih-nic-irq-affinity"),
        "the affinity unit must be enable-only (never a live start)"
    );
    assert!(
        !s.contains("/proc/irq/") && !s.contains("> \"/proc/irq"),
        "setup-strih must not hard-code an IRQ number nor write affinity itself (the emitted script does)"
    );
}

#[test]
fn verify_strih_checks_nic_irq_affinity_with_a_live_advancing_read() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_nic_xhci_irqs"),
        "verify must resolve the xhci IRQ via the lib"
    );
    assert!(
        v.contains("smp_affinity_list"),
        "verify must read the IRQ smp_affinity_list"
    );
    assert!(
        v.contains("strih_counter_advanced") && v.contains("sleep 2"),
        "verify must assert the IRQ counter ADVANCES over a live 2-s window (never a static file check)"
    );
    assert!(
        v.contains("strih_nic_irq_affinity_verdict"),
        "verify must use the single-E-core placement verdict"
    );
}
