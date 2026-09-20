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

// --- issue 1317 (this lane): the strih-obs-start.sh / strih-obs-stop.sh launcher pair -------------

/// Source an ARBITRARY launcher script (not the lib) and run `body`. The launchers BASH_SOURCE-guard
/// their live flow (like setup-strih.sh), so sourcing them defines only their pure functions -- no
/// OBS launch, no session, no side effects.
fn run_sourced_arb(rel: &str, env: &[(&str, &str)], body: &str) -> (i32, String, String) {
    let script = manifest_dir().join(rel);
    assert!(script.exists(), "{} not found", script.display());
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script);
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
fn launcher_pair_ok_passes_when_both_scripts_are_present_and_executable() {
    let (code, _o, _e) = run_sourced(
        &[],
        "d=$(mktemp -d)\n\
         : > \"$d/strih-obs-start.sh\"; chmod +x \"$d/strih-obs-start.sh\"\n\
         : > \"$d/strih-obs-stop.sh\";  chmod +x \"$d/strih-obs-stop.sh\"\n\
         strih_launcher_pair_ok \"$d\"; rc=$?\n\
         rm -rf \"$d\"; exit $rc",
    );
    assert_eq!(code, 0, "both launchers present + executable must pass");
}

#[test]
fn launcher_pair_ok_fails_and_names_a_missing_script() {
    let (code, out, _e) = run_sourced(
        &[],
        "d=$(mktemp -d)\n\
         : > \"$d/strih-obs-start.sh\"; chmod +x \"$d/strih-obs-start.sh\"\n\
         out=$(strih_launcher_pair_ok \"$d\"); rc=$?\n\
         printf '%s' \"$out\"\n\
         rm -rf \"$d\"; exit $rc",
    );
    assert_ne!(code, 0, "a missing launcher must fail");
    assert!(
        out.contains("strih-obs-stop.sh"),
        "the failure output must name the missing script: {out}"
    );
}

#[test]
fn launcher_pair_ok_fails_and_names_a_non_executable_script() {
    let (code, out, _e) = run_sourced(
        &[],
        "d=$(mktemp -d)\n\
         : > \"$d/strih-obs-start.sh\"; chmod +x \"$d/strih-obs-start.sh\"\n\
         : > \"$d/strih-obs-stop.sh\";  chmod -x \"$d/strih-obs-stop.sh\"\n\
         out=$(strih_launcher_pair_ok \"$d\"); rc=$?\n\
         printf '%s' \"$out\"\n\
         rm -rf \"$d\"; exit $rc",
    );
    assert_ne!(code, 0, "a non-executable launcher must fail");
    assert!(
        out.contains("strih-obs-stop.sh"),
        "the failure output must name the non-executable script: {out}"
    );
}

/// issue 1317: `setup-strih.sh` step 8 must install BOTH launcher scripts (mode 0755) BEFORE the
/// `systemctl --user enable strih-obs.service` line -- an enabled unit whose ExecStart target is
/// missing flaps 203/EXEC under Restart=on-failure.
#[test]
fn setup_strih_installs_both_launchers_before_enabling_the_unit() {
    let s = read_script("scripts/setup-strih.sh");
    let start_install = s
        .find("install -m 0755 \"${HERE}/strih-obs-start.sh\"")
        .expect("setup-strih must install strih-obs-start.sh mode 0755");
    let stop_install = s
        .find("install -m 0755 \"${HERE}/strih-obs-stop.sh\"")
        .expect("setup-strih must install strih-obs-stop.sh mode 0755");
    let enable = s
        .find("systemctl --user enable strih-obs.service")
        .expect("setup-strih step 8 must enable strih-obs.service");
    assert!(
        start_install < enable && stop_install < enable,
        "both launcher installs must precede the unit enable (starts {start_install} / stops {stop_install} vs enable {enable})"
    );
}

/// issue 1317 lock-step: the unit's ExecStart/ExecStop basenames must equal the launcher basenames
/// setup-strih installs into /usr/local/bin -- so a rename of either can never silently dangle the
/// ExecStart target again (the very gap this lane closes).
#[test]
fn unit_execstart_execstop_basenames_match_the_installed_launchers() {
    let unit = read_script("systemd/strih-obs.service");
    let base_of = |prefix: &str| -> String {
        let line = unit
            .lines()
            .find(|l| l.trim_start().starts_with(prefix))
            .unwrap_or_else(|| panic!("unit must have an {prefix} line"));
        let rhs = line.trim_start().split_once('=').unwrap().1.trim();
        let path = rhs.split_whitespace().next().unwrap();
        path.rsplit('/').next().unwrap().to_string()
    };
    let start = base_of("ExecStart=");
    let stop = base_of("ExecStop=");
    assert_eq!(start, "strih-obs-start.sh");
    assert_eq!(stop, "strih-obs-stop.sh");

    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains(&format!("/usr/local/bin/{start}")),
        "setup-strih must install the unit's ExecStart target {start} into /usr/local/bin"
    );
    assert!(
        setup.contains(&format!("/usr/local/bin/{stop}")),
        "setup-strih must install the unit's ExecStop target {stop} into /usr/local/bin"
    );
    assert!(
        manifest_dir().join(format!("scripts/{start}")).exists(),
        "scripts/{start} must exist"
    );
    assert!(
        manifest_dir().join(format!("scripts/{stop}")).exists(),
        "scripts/{stop} must exist"
    );
}

/// issue 1317: strih-obs-start.sh's session-display resolver (Wayland-first, X11 fallback, else fail
/// loud) is a pure sourced function -- a wayland-* socket under XDG_RUNTIME_DIR resolves to
/// WAYLAND_DISPLAY=<sock> (its sibling .lock is ignored); neither a wayland nor an X socket -> a
/// non-zero return (the unit is After=graphical-session.target, so no display is a hard fail).
#[test]
fn start_script_session_env_resolves_wayland_and_fails_loud_when_absent() {
    let (code, out, _e) = run_sourced_arb(
        "scripts/strih-obs-start.sh",
        &[],
        "rt=$(mktemp -d)\n\
         : > \"$rt/wayland-0\"; : > \"$rt/wayland-0.lock\"\n\
         rc=0\n\
         out=$(XDG_RUNTIME_DIR=\"$rt\" STRIH_X11_SOCKET_DIR=\"$rt/nox\" strih_resolve_session_env) || rc=$?\n\
         printf '%s' \"$out\"\n\
         rm -rf \"$rt\"; exit $rc",
    );
    assert_eq!(code, 0, "a wayland-0 socket must resolve");
    assert!(
        out.contains("WAYLAND_DISPLAY=wayland-0"),
        "a wayland socket must resolve to WAYLAND_DISPLAY=wayland-0: {out}"
    );

    let (code2, _o2, _e2) = run_sourced_arb(
        "scripts/strih-obs-start.sh",
        &[],
        "rt=$(mktemp -d)\n\
         rc=0\n\
         out=$(XDG_RUNTIME_DIR=\"$rt\" STRIH_X11_SOCKET_DIR=\"$rt/nox\" strih_resolve_session_env) || rc=$?\n\
         rm -rf \"$rt\"; exit $rc",
    );
    assert_ne!(code2, 0, "no wayland + no X socket must fail loud");
}

// --- issue 1317 (this lane): runtime packages + the /usr prefix install --------------------------

/// issue 1317: `strih_runtime_packages_from_file` parses RUNTIME_PACKAGES.txt into apt package names,
/// skipping blank lines and comment lines (first non-whitespace `#`) and trimming whitespace.
#[test]
fn runtime_packages_from_file_skips_comments_and_blanks() {
    let (code, out, _e) = run_sourced(
        &[],
        "f=$(mktemp)\n\
         printf '%s\\n' '# issue 1317 header' '' 'libavcodec62' '   libqt6core6t64  ' '# c2' 'libopengl0' > \"$f\"\n\
         strih_runtime_packages_from_file \"$f\"; rc=$?\n\
         rm -f \"$f\"; exit $rc",
    );
    assert_eq!(code, 0, "parser must succeed on a valid file");
    let pkgs: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        pkgs,
        vec!["libavcodec62", "libqt6core6t64", "libopengl0"],
        "must list only the package names, comments/blanks skipped + whitespace trimmed: {out}"
    );
    // A missing file must fail (fail-closed; setup-strih.sh pre-checks existence separately).
    let (code2, _o2, _e2) = run_sourced(&[], "strih_runtime_packages_from_file /no/such/file");
    assert_ne!(code2, 0, "a missing packages file must return non-zero");
}

/// issue 1317: `strih_ldd_unresolved` lists each `=> not found` soname (deduped) and prints NOTHING
/// when every dependency resolves — verify-strih.sh fails iff its output is non-empty.
#[test]
fn ldd_unresolved_lists_not_found_and_is_empty_when_resolved() {
    let (code, out, _e) = run_sourced(
        &[],
        "printf '%s\\n' \
           '\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)' \
           '\tlibavcodec.so.62 => not found' \
           '\tlibobs.so.30 => not found' \
           '\tlinux-vdso.so.1 (0x2)' | strih_ldd_unresolved",
    );
    assert_eq!(code, 0);
    let mut unresolved: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    unresolved.sort_unstable();
    assert_eq!(
        unresolved,
        vec!["libavcodec.so.62", "libobs.so.30"],
        "must list exactly the two unresolved sonames: {out}"
    );
    // All-resolved input -> empty output (the bundle can load).
    let (_c2, out2, _e2) = run_sourced(
        &[],
        "printf '%s\\n' '\tlibc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1)' | strih_ldd_unresolved",
    );
    assert!(
        out2.trim().is_empty(),
        "an all-resolved ldd output must produce no unresolved sonames: {out2}"
    );
}

/// issue 1317: `setup-strih.sh` step 4 must install the runtime packages (apt-get) BEFORE the /usr
/// prefix install (`strih_install_bundle_prefix`), and both BEFORE the step-8 unit enable — a fresh
/// box needs the Qt6/ffmpeg/GL runtime installed and the bundle on the loader path before OBS runs.
#[test]
fn setup_strih_installs_runtime_packages_before_the_prefix_install_and_enable() {
    let s = read_script("scripts/setup-strih.sh");
    let apt = s
        .find("apt-get install -y --no-install-recommends")
        .expect("setup-strih step 4 must apt-get install the runtime packages");
    let prefix = s
        .find("strih_install_bundle_prefix \"$GENLOCK_DIR\"")
        .expect("setup-strih step 4 must install the bundle into the /usr prefix");
    let enable = s
        .find("systemctl --user enable strih-obs.service")
        .expect("setup-strih step 8 must enable strih-obs.service");
    assert!(
        apt < prefix,
        "apt-get install must run BEFORE the /usr prefix install (apt {apt} vs prefix {prefix})"
    );
    assert!(
        prefix < enable,
        "the /usr prefix install must run BEFORE the unit enable (prefix {prefix} vs enable {enable})"
    );
    // Fail-closed: an absent RUNTIME_PACKAGES.txt must `fail` the step (same contract as TARGET-RELEASE).
    assert!(
        s.contains("RUNTIME_PACKAGES.txt missing"),
        "setup-strih must fail-closed when the staged bundle has no RUNTIME_PACKAGES.txt"
    );
}

/// issue 1317: `strih_install_bundle_prefix` copies the bundle libs into LIBDIR BEFORE `ldconfig`
/// (an ldconfig before the copy would not pick up the new libs) and installs the frontend to
/// BINDIR/obs — the imag on-box program's install shape.
#[test]
fn install_bundle_prefix_copies_libs_before_ldconfig() {
    let lib = read_script("scripts/lib/strih-provision.sh");
    let cp = lib
        .find("cp -a \"${bundle}/lib/x86_64-linux-gnu/.\"")
        .expect("strih_install_bundle_prefix must cp -a the bundle libs into LIBDIR");
    let ldconfig = lib
        .find("\n  ldconfig")
        .expect("strih_install_bundle_prefix must run ldconfig");
    assert!(
        cp < ldconfig,
        "the lib copy must precede ldconfig (cp {cp} vs ldconfig {ldconfig})"
    );
    assert!(
        lib.contains("install -m 0755 -o root -g root \"${bundle}/bin/obs\" \"${bindir}/obs\""),
        "strih_install_bundle_prefix must install the frontend to BINDIR/obs (0755 root)"
    );
}

/// issue 1317: `verify-strih.sh` must gate BOTH runtime-library resolution (via `strih_ldd_unresolved`)
/// AND the installed runtime packages (via `strih_runtime_packages_from_file` over RUNTIME_PACKAGES.txt),
/// as an item BEFORE the generic "OBS not running under the supervisor" item.
#[test]
fn verify_strih_checks_runtime_resolution_and_packages() {
    let v = read_script("scripts/verify-strih.sh");
    let ldd = v
        .find("strih_ldd_unresolved")
        .expect("verify-strih must run strih_ldd_unresolved over the installed OBS + libs");
    assert!(
        v.contains("RUNTIME_PACKAGES.txt"),
        "verify-strih must check the installed runtime packages via RUNTIME_PACKAGES.txt"
    );
    assert!(
        v.contains("strih_runtime_packages_from_file"),
        "verify-strih must parse RUNTIME_PACKAGES.txt via strih_runtime_packages_from_file"
    );
    let obs_running = v
        .find("OBS not running under the strih-obs.service supervisor")
        .expect("verify-strih must have the OBS-running item");
    assert!(
        ldd < obs_running,
        "the runtime-resolution item must run BEFORE the OBS-running item (ldd {ldd} vs {obs_running})"
    );
}

/// issue 1317: `strih-obs-start.sh` must launch the bundle from its /usr prefix
/// (`${STRIH_OBS_BIN:-/usr/bin/obs}`), not the /opt staged copy — the prefix is now on the loader
/// path. STRIH_OBS_BIN still overrides it.
#[test]
fn start_script_default_obs_bin_is_the_usr_prefix() {
    let s = read_script("scripts/strih-obs-start.sh");
    assert!(
        s.contains("OBS_BIN=\"${STRIH_OBS_BIN:-/usr/bin/obs}\""),
        "the launcher default OBS binary must be /usr/bin/obs"
    );
    assert!(
        !s.contains("${STRIH_OBS_BIN:-/opt/obs-genlock/bin/obs}"),
        "the launcher must no longer default to the /opt staged copy (not on the loader path)"
    );
    // The STRIH_OBS_BIN seam still works when sourced.
    let (code, out, _e) = run_sourced_arb(
        "scripts/strih-obs-start.sh",
        &[("STRIH_OBS_BIN", "/custom/obs")],
        "printf '%s' \"$OBS_BIN\"",
    );
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "/custom/obs",
        "STRIH_OBS_BIN must override the default: {out}"
    );
}

// --- issue 1317 (this lane): dantesync CLIENT unit + the verify sleep-mask predicate --------------

/// `strih_dantesync_unit_text` emits a systemd unit whose ExecStart is a dantesync CLIENT
/// invocation (`/usr/local/bin/dantesync --ntp-server …`) — never `--service`, never a master mode —
/// in the EXACT cambox unit shape (Type=simple, Restart=always, WantedBy=multi-user.target).
#[test]
fn dantesync_unit_text_is_a_client_ntp_server_invocation_never_service_or_master() {
    let (code, out, _e) = run_sourced(&[], "strih_dantesync_unit_text '--ntp-server strih.lan'");
    assert_eq!(code, 0, "a client invocation must emit a unit");
    assert!(
        out.contains("ExecStart=/usr/local/bin/dantesync --ntp-server strih.lan"),
        "ExecStart must be the dantesync CLIENT daemon at /usr/local/bin/dantesync: {out}"
    );
    assert!(
        out.contains("Type=simple"),
        "unit must be Type=simple: {out}"
    );
    assert!(
        out.contains("Restart=always"),
        "unit must Restart=always: {out}"
    );
    assert!(
        out.contains("RestartSec=5"),
        "unit must set RestartSec=5: {out}"
    );
    assert!(
        out.contains("WantedBy=multi-user.target"),
        "unit must be WantedBy=multi-user.target: {out}"
    );
    assert!(
        !out.contains("--service"),
        "`--service` is a run mode, not an installer flag — it must never appear: {out}"
    );
    assert!(
        !out.to_lowercase().contains("server_mode") && !out.to_lowercase().contains("master"),
        "the unit must never carry a server/master mode: {out}"
    );

    // With no arg it defaults to the client args helper (still a client unit).
    let (c2, out2, _e2) = run_sourced(&[], "strih_dantesync_unit_text");
    assert_eq!(c2, 0);
    assert!(
        out2.contains("ExecStart=/usr/local/bin/dantesync --ntp-server"),
        "the default ExecStart must be the client args: {out2}"
    );

    // Fail-closed: a master invocation emits NOTHING and returns non-zero. (`--master` matches the
    // classifier's `*master*` master pattern; note `--ntp-server-mode` would NOT — it matches the
    // client `*ntp-server*` pattern, so the master token here must be a genuine one.)
    let (c3, out3, _e3) = run_sourced(&[], "strih_dantesync_unit_text '--master'");
    assert_ne!(c3, 0, "a master invocation must be refused");
    assert!(
        out3.trim().is_empty(),
        "a refused (master) invocation must emit NOTHING: {out3}"
    );
}

/// `strih_verify_sleep_masked` grades the FIRST line only, so a correctly-masked box passes even
/// when `systemctl is-enabled`'s "masked"-to-stdout-AND-exit-1 makes a `|| echo masked` fallback
/// DOUBLE-append ("masked\nmasked") — the exact issue-1317 false-FAIL. "enabled"/"static" fail.
#[test]
fn verify_sleep_masked_grades_first_line_and_survives_the_double_masked_bug() {
    let (c1, _o, _e) = run_sourced(&[], "strih_verify_sleep_masked masked");
    assert_eq!(c1, 0, "a plain 'masked' must pass");

    let (c2, _o, _e) = run_sourced(
        &[],
        "s=\"$(printf 'masked\\nmasked')\"; strih_verify_sleep_masked \"$s\"",
    );
    assert_eq!(
        c2, 0,
        "the double-appended 'masked\\nmasked' must still pass (the false-FAIL bug)"
    );

    let (c3, _o, _e) = run_sourced(&[], "strih_verify_sleep_masked enabled");
    assert_ne!(c3, 0, "'enabled' must fail");
    let (c4, _o, _e) = run_sourced(&[], "strih_verify_sleep_masked static");
    assert_ne!(c4, 0, "'static' must fail");
    let (c5, _o, _e) = run_sourced(&[], "strih_verify_sleep_masked ''");
    assert_ne!(c5, 0, "an empty state must fail-closed");
}

/// issue 1317: `setup-strih.sh` step 2 must INSTALL the dantesync unit (write it via
/// `strih_dantesync_unit_text` into /etc/systemd/system/dantesync.service), not merely validate the
/// client args — and this must precede the OBS unit enable (step 8).
#[test]
fn setup_strih_installs_the_dantesync_unit_in_step_2() {
    let s = read_script("scripts/setup-strih.sh");
    let emit = s
        .find("strih_dantesync_unit_text \"$DS_ARGS\"")
        .expect("setup-strih step 2 must emit the dantesync unit via strih_dantesync_unit_text");
    assert!(
        s.contains("/etc/systemd/system/dantesync.service"),
        "setup-strih must write the dantesync unit to /etc/systemd/system/dantesync.service"
    );
    assert!(
        s.contains("rm -f /var/run/dantesync.lock"),
        "setup-strih must clear a stale /var/run/dantesync.lock before (re)start"
    );
    let enable = s
        .find("systemctl --user enable strih-obs.service")
        .expect("setup-strih step 8 must enable strih-obs.service");
    assert!(
        emit < enable,
        "the dantesync unit install (step 2) must precede the OBS enable (emit {emit} vs enable {enable})"
    );
}

/// issue 1317: `setup-strih.sh` must install the NDI runtime (via the shared `ndi_runtime_install_cmds`)
/// BEFORE the OBS launch/enable — DistroAV needs libndi on the loader path at OBS start, else it loads
/// UI-only (ERR-404). The shared recipe lives in scripts/lib/ndi-runtime.sh.
#[test]
fn setup_strih_installs_the_ndi_runtime_before_the_obs_enable() {
    let s = read_script("scripts/setup-strih.sh");
    let ndi = s
        .find("ndi_runtime_install_cmds")
        .expect("setup-strih must install the NDI runtime via the shared ndi_runtime_install_cmds");
    let enable = s
        .find("systemctl --user enable strih-obs.service")
        .expect("setup-strih step 8 must enable strih-obs.service");
    assert!(
        ndi < enable,
        "the NDI runtime install must precede the OBS enable (ndi {ndi} vs enable {enable})"
    );
    assert!(
        s.contains(". \"${HERE}/lib/ndi-runtime.sh\""),
        "setup-strih must source the shared scripts/lib/ndi-runtime.sh"
    );
    assert!(
        manifest_dir().join("scripts/lib/ndi-runtime.sh").exists(),
        "scripts/lib/ndi-runtime.sh must exist"
    );
}

/// issue 1317: `verify-strih.sh` item 6 must assert the dantesync UNIT is active + a FRESH offset
/// (the shared `dantesync_offset_verdict`), NOT read /etc/dantesync/config.json (a Windows/imag
/// artifact a flag-based Linux client never creates); item 11 must grade sleep via
/// `strih_verify_sleep_masked` (no `|| echo masked` double-append).
#[test]
fn verify_strih_dantesync_and_sleep_items_are_fixed() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("dantesync_offset_verdict"),
        "verify-strih item 6 must grade a fresh offset via dantesync_offset_verdict"
    );
    assert!(
        v.contains("systemctl is-active dantesync"),
        "verify-strih item 6 must assert the dantesync unit is active"
    );
    assert!(
        !v.contains("/etc/dantesync/config.json"),
        "verify-strih must NOT read /etc/dantesync/config.json (a flag-based Linux client has none)"
    );
    assert!(
        v.contains("strih_verify_sleep_masked"),
        "verify-strih item 11 must grade sleep via strih_verify_sleep_masked"
    );
    assert!(
        !v.contains("|| echo masked"),
        "verify-strih must drop the `|| echo masked` double-append (the false-FAIL bug)"
    );
}

/// issue 1346: `strih_projector_verdict SAVEPROJ EXT SAVED` grades the fixed HDMI projector
/// acceptance (report-only): SaveProjectors=true pre-seeded, and — when an external monitor is
/// connected — a saved type-3/4 projector entry exists. Fail-closed order: SaveProjectors first,
/// then external-monitor presence, then the saved entry. Returns 0 ONLY for the fully-`ok` state;
/// every other state prints its own token and returns non-zero (the caller renders 0->PASS else
/// NOTE, since the whole item is report-only). Fixtures: connected/not-connected + present/absent.
#[test]
fn projector_verdict_grades_saveprojectors_hdmi_and_saved_entry() {
    // fully healthy: SaveProjectors on, an external monitor, a saved projector -> ok, rc 0
    let (c, out, _e) = run_sourced(&[], "strih_projector_verdict 1 1 1");
    assert_eq!(c, 0, "the fully-configured state must be ok; token={out}");
    assert_eq!(out, "ok");

    // SaveProjectors not pre-seeded -> saveprojectors-missing (checked FIRST, even with no monitor)
    let (c2, out2, _e) = run_sourced(&[], "strih_projector_verdict 0 1 1");
    assert_ne!(c2, 0);
    assert_eq!(out2, "saveprojectors-missing");
    let (c2b, out2b, _e) = run_sourced(&[], "strih_projector_verdict 0 0 0");
    assert_ne!(c2b, 0);
    assert_eq!(
        out2b, "saveprojectors-missing",
        "SaveProjectors is graded before the monitor"
    );

    // SaveProjectors ok but no external monitor connected (today's box) -> hdmi-absent (report-only)
    let (c3, out3, _e) = run_sourced(&[], "strih_projector_verdict 1 0 0");
    assert_ne!(c3, 0);
    assert_eq!(out3, "hdmi-absent");

    // external monitor present but no saved projector yet -> projector-unseeded
    let (c4, out4, _e) = run_sourced(&[], "strih_projector_verdict 1 1 0");
    assert_ne!(c4, 0);
    assert_eq!(out4, "projector-unseeded");

    // fail-closed defaults: missing args behave as 0 (not configured)
    let (c5, out5, _e) = run_sourced(&[], "strih_projector_verdict");
    assert_ne!(c5, 0);
    assert_eq!(out5, "saveprojectors-missing");
}

/// issue 1345 M3a: `strih_janus_audiobridge_jcfg_text ROOM SECRET_PATH` renders the interkom room
/// jcfg (48 kHz, plain-RTP participants) with the secret as a PLACEHOLDER (never inlined), and names
/// the secret path only in a provenance comment.
#[test]
fn janus_audiobridge_jcfg_renders_interkom_room_without_inlining_a_secret() {
    let (code, out, err) = run_sourced(
        &[],
        "strih_janus_audiobridge_jcfg_text 1000 /etc/intercom-hub/janus-room.secret",
    );
    assert_eq!(code, 0, "renderer must succeed; stderr={err}");
    assert!(
        out.contains("room-1000:"),
        "declares room-1000; got:\n{out}"
    );
    assert!(
        out.contains("description = \"interkom\""),
        "names the interkom room"
    );
    assert!(out.contains("sampling_rate = 48000"), "48 kHz room");
    assert!(
        out.contains("allow_rtp_participants = true"),
        "plain-RTP allowed"
    );
    assert!(out.contains("record = false"), "recording off");
    // The secret is a PLACEHOLDER, never a value; the path is only in a comment.
    assert!(
        out.contains("@JANUS_ROOM_SECRET@"),
        "the secret is a placeholder"
    );
    assert!(
        out.contains("/etc/intercom-hub/janus-room.secret"),
        "the provenance comment names the secret path"
    );
    // No hex-looking secret leaked into the rendered text (only the placeholder line carries `secret`).
    for line in out.lines() {
        if line.trim_start().starts_with("secret") {
            assert!(
                line.contains("@JANUS_ROOM_SECRET@"),
                "the secret line must be the placeholder, not a value: {line}"
            );
        }
    }
}

/// issue 1345 M3a: `strih_janus_ws_jcfg_text LAN_IP` renders the WebSocket transport jcfg on :8188
/// with NO wss (TLS terminates on the dev1 front) and the admin API off; the LAN IP is documented.
#[test]
fn janus_ws_jcfg_renders_ws_no_wss() {
    let (code, out, err) = run_sourced(&[], "strih_janus_ws_jcfg_text 10.77.9.203");
    assert_eq!(code, 0, "renderer must succeed; stderr={err}");
    assert!(out.contains("ws = true"), "ws enabled; got:\n{out}");
    assert!(out.contains("ws_port = 8188"), "ws on :8188");
    assert!(out.contains("wss = false"), "no wss (TLS on the front)");
    assert!(out.contains("admin_ws = false"), "admin API off");
    assert!(out.contains("10.77.9.203"), "the LAN IP is documented");
}

/// issue 1345 M3a: `strih_janus_room_jcfg_ok ROOM` (stdin: jcfg) grades whether the interkom room is
/// declared at 48 kHz with plain-RTP participants — a pure grep, no janus binary. A rendered jcfg for
/// room 1000 passes for 1000 and fails for a different room id (fail-closed).
#[test]
fn janus_room_jcfg_ok_grades_the_rendered_room() {
    let (c_ok, out_ok, _e) = run_sourced(
        &[],
        "if strih_janus_audiobridge_jcfg_text 1000 /x | strih_janus_room_jcfg_ok 1000; then echo OK; else echo BAD; fi",
    );
    assert_eq!(c_ok, 0);
    assert!(
        out_ok.contains("OK"),
        "the rendered room 1000 jcfg must grade OK; got: {out_ok}"
    );

    let (_c, out_bad, _e) = run_sourced(
        &[],
        "if strih_janus_audiobridge_jcfg_text 1000 /x | strih_janus_room_jcfg_ok 9999; then echo OK; else echo BAD; fi",
    );
    assert!(
        out_bad.contains("BAD"),
        "a mismatched room id must fail-closed; got: {out_bad}"
    );

    // A jcfg missing the sampling_rate line fails.
    let (_c2, out_missing, _e) = run_sourced(
        &[],
        "if printf 'room-1000:\\n    description = \"interkom\"\\n    allow_rtp_participants = true\\n' | strih_janus_room_jcfg_ok 1000; then echo OK; else echo BAD; fi",
    );
    assert!(
        out_missing.contains("BAD"),
        "a jcfg without sampling_rate = 48000 must fail; got: {out_missing}"
    );
}

/// issue 1345 M3a: `setup-strih.sh` step 14 must apt-install janus + enable-only (never start) and
/// run BEFORE the final verify (step 15). Mirrors the dantesync/NDI ordering anchors.
#[test]
fn setup_strih_installs_janus_enable_only_before_final_verify() {
    let s = read_script("scripts/setup-strih.sh");
    let apt = s
        .find("apt-get install -y janus")
        .expect("setup-strih step 14 must apt-get install janus");
    let enable = s
        .find("systemctl enable janus")
        .expect("setup-strih step 14 must enable janus");
    let verify = s
        .find("verify-strih.sh acceptance gate")
        .expect("setup-strih step 15 must run the verify gate");
    assert!(
        apt < enable,
        "install before enable (apt {apt} vs enable {enable})"
    );
    assert!(
        enable < verify,
        "janus enable must precede the final verify (enable {enable} vs verify {verify})"
    );
    assert!(
        !s.contains("systemctl start janus") && !s.contains("systemctl restart janus"),
        "janus is enable-only (never start/restart) until the M4 cut-over"
    );
    assert!(
        s.contains("TOTAL_STEPS=17"),
        "TOTAL_STEPS must be bumped for the janus step (17 after the issue-1317 perf + companion steps)"
    );
}

// =====================================================================================
// issue 1317 (this lane): the two provisioning steps the owner caught missing live on the
// strih-lx notebook -- (1) CPU PERFORMANCE governor, (2) Bitfocus Companion Satellite.
// The pure emitters/predicates below live in scripts/lib/strih-provision.sh and are wired
// as two new numbered steps in setup-strih.sh + two acceptance items in verify-strih.sh.
// =====================================================================================

/// The performance-mode apply block PREFERS power-profiles-daemon (`powerprofilesctl set
/// performance`) and FALLS BACK to writing the `performance` scaling_governor -- the fleet order
/// (the setup-device STEP-13 governor precedent + the design). It also masks the sleep targets, and
/// the whole emitted block must be syntactically valid bash (the caller evals it under set -e).
#[test]
fn performance_mode_apply_prefers_powerprofiles_then_governor_and_masks_sleep() {
    let (code, out, err) = run_sourced(&[], "strih_performance_mode_apply");
    assert_eq!(code, 0, "emitter must succeed; stderr={err}");
    assert!(
        out.contains("powerprofilesctl set performance"),
        "must prefer power-profiles-daemon: {out}"
    );
    assert!(
        out.contains("scaling_governor"),
        "must fall back to the scaling_governor write: {out}"
    );
    assert!(out.contains("performance"), "must set performance: {out}");
    assert!(
        out.contains("sleep.target"),
        "must mask the sleep/suspend targets (setup-device STEP-13 shape): {out}"
    );
    let ppd = out
        .find("powerprofilesctl")
        .expect("powerprofilesctl must appear");
    let gov = out
        .find("scaling_governor")
        .expect("scaling_governor must appear");
    assert!(
        ppd < gov,
        "power-profiles-daemon must be PREFERRED (checked before) the governor fallback (ppd {ppd} vs gov {gov})"
    );
    // The emitted block is eval'd by setup-strih.sh under `set -euo pipefail` -- it must parse.
    let (code, _o, err) = run_sourced(&[], "strih_performance_mode_apply | bash -n");
    assert_eq!(
        code, 0,
        "emitted apply block must be valid bash; stderr={err}"
    );
}

/// The persistent CPU performance systemd oneshot mirrors setup-device.sh's cpu-performance.service.
#[test]
fn cpu_performance_unit_is_the_fleet_oneshot() {
    let (code, out, _e) = run_sourced(&[], "strih_cpu_performance_unit_text");
    assert_eq!(code, 0);
    assert!(out.contains("Type=oneshot"), "oneshot: {out}");
    assert!(
        out.contains("RemainAfterExit=yes"),
        "remain-after-exit: {out}"
    );
    assert!(
        out.contains("scaling_governor") && out.contains("performance"),
        "ExecStart must write performance to scaling_governor: {out}"
    );
    assert!(
        out.contains("WantedBy=multi-user.target"),
        "install target: {out}"
    );
}

/// The verify (perf) governor predicate: 0 iff EVERY online core reports `performance` and there is
/// at least one core (fail-closed on empty/unreadable input -- test-strictness).
#[test]
fn verify_governor_ok_requires_every_core_performance_failclosed_on_empty() {
    let (code, _o, _e) = run_sourced(
        &[],
        "printf 'performance\\nperformance\\nperformance\\n' | strih_verify_governor_ok",
    );
    assert_eq!(code, 0, "all-performance must pass");
    let (code, _o, _e) = run_sourced(
        &[],
        "printf 'performance\\npowersave\\nperformance\\n' | strih_verify_governor_ok",
    );
    assert_ne!(code, 0, "one non-performance core must FAIL");
    let (code, _o, _e) = run_sourced(&[], "printf '' | strih_verify_governor_ok");
    assert_ne!(code, 0, "empty (unreadable) governors must fail-closed");
}

/// The Companion Satellite version is PINNED to the real stable release (v3.4.0, never 'latest',
/// never the nonexistent 1.11.0 the bounced lane pinned) and env-overridable, like the dantesync/NDI
/// version pins.
#[test]
fn companion_satellite_version_is_pinned_never_latest() {
    let (code, out, _e) = run_sourced(&[], "strih_companion_satellite_version");
    assert_eq!(code, 0);
    assert!(!out.trim().is_empty(), "a version must be pinned");
    assert!(
        !out.to_lowercase().contains("latest"),
        "never 'latest' (reproducible pin): {out}"
    );
    assert_eq!(
        out.trim(),
        "3.4.0",
        "must pin the real Bitfocus stable v3.4.0, not the bounced nonexistent 1.11.0: {out}"
    );
    let (_c, out2, _e) = run_sourced(
        &[("COMPANION_SATELLITE_VERSION", "9.9.9")],
        "strih_companion_satellite_version",
    );
    assert_eq!(out2.trim(), "9.9.9", "COMPANION_SATELLITE_VERSION override");
}

/// The download URL is the Bitfocus CDN x64 TAR.GZ (never a GitHub-release .deb — the bounce cause:
/// GitHub releases carry NO .deb assets), env-overridable via COMPANION_SATELLITE_TARBALL_URL.
#[test]
fn companion_satellite_tarball_url_is_the_pinned_cdn_targz_never_a_deb() {
    let (code, url, _e) = run_sourced(&[], "strih_companion_satellite_tarball_url");
    assert_eq!(code, 0);
    assert!(
        url.contains("cf-pub.bitfocus.io"),
        "url must be the Bitfocus CDN: {url}"
    );
    assert!(
        url.contains("companion-satellite"),
        "url must be the companion-satellite asset: {url}"
    );
    assert!(
        url.trim().ends_with(".tar.gz"),
        "url must be a .tar.gz (never a .deb): {url}"
    );
    assert!(
        !url.contains(".deb"),
        "url must NOT be a .deb (the bounce cause): {url}"
    );
    assert!(
        !url.contains("github.com"),
        "url must NOT be a GitHub release (no .deb assets there): {url}"
    );
    let (_c, url2, _e) = run_sourced(
        &[(
            "COMPANION_SATELLITE_TARBALL_URL",
            "https://example.test/x.tar.gz",
        )],
        "strih_companion_satellite_tarball_url",
    );
    assert_eq!(
        url2.trim(),
        "https://example.test/x.tar.gz",
        "COMPANION_SATELLITE_TARBALL_URL override"
    );
}

/// The tarball sha256 is pinned (64 lowercase hex) + env-overridable via COMPANION_SATELLITE_SHA256.
#[test]
fn companion_satellite_sha256_is_pinned() {
    let (code, sha, _e) = run_sourced(&[], "strih_companion_satellite_sha256");
    assert_eq!(code, 0);
    let sha = sha.trim();
    assert_eq!(sha.len(), 64, "sha256 must be 64 hex chars: {sha}");
    assert!(
        sha.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "sha256 must be lowercase hex: {sha}"
    );
    let (_c, sha2, _e) = run_sourced(
        &[("COMPANION_SATELLITE_SHA256", "deadbeef")],
        "strih_companion_satellite_sha256",
    );
    assert_eq!(
        sha2.trim(),
        "deadbeef",
        "COMPANION_SATELLITE_SHA256 override"
    );
}

/// The controller host defaults to the venue Companion controller 10.77.9.205, env-overridable.
#[test]
fn companion_satellite_host_defaults_to_the_venue_controller() {
    let (_c, host, _e) = run_sourced(&[], "strih_companion_satellite_host");
    assert_eq!(host.trim(), "10.77.9.205");
    let (_c, host2, _e) = run_sourced(
        &[("COMPANION_SATELLITE_HOST", "10.0.0.9")],
        "strih_companion_satellite_host",
    );
    assert_eq!(
        host2.trim(),
        "10.0.0.9",
        "COMPANION_SATELLITE_HOST override"
    );
}

/// The controller port defaults to the Satellite TCP API port 16622, env-overridable.
#[test]
fn companion_satellite_port_defaults_to_16622() {
    let (_c, port, _e) = run_sourced(&[], "strih_companion_satellite_port");
    assert_eq!(port.trim(), "16622");
    let (_c, port2, _e) = run_sourced(
        &[("COMPANION_SATELLITE_PORT", "16623")],
        "strih_companion_satellite_port",
    );
    assert_eq!(port2.trim(), "16623", "COMPANION_SATELLITE_PORT override");
}

/// The durable host.conf record keeps the controller host as `COMPANION_SATELLITE_HOST` (the
/// (companion) gate's human-readable record; the FUNCTIONAL seed is the app config.json below).
#[test]
fn companion_satellite_config_text_records_the_controller_host() {
    let (_c, cfg, _e) = run_sourced(&[], "strih_companion_satellite_config_text 10.77.9.205");
    assert!(
        cfg.contains("COMPANION_SATELLITE_HOST=10.77.9.205"),
        "config must record the controller host: {cfg}"
    );
}

/// The app config.json pre-seed uses the electron-store keys the Satellite v3.4.0 source reads —
/// `remoteIp` (controller host) + `remotePort` — NOT `host`/`companionAddress`; and it is valid JSON.
#[test]
fn companion_satellite_appconfig_json_seeds_remoteip_remoteport() {
    let (code, cfg, _e) = run_sourced(
        &[],
        "strih_companion_satellite_appconfig_json 10.77.9.205 16622",
    );
    assert_eq!(code, 0);
    assert!(
        cfg.contains("\"remoteIp\""),
        "must set remoteIp (the source's controller-host key): {cfg}"
    );
    assert!(
        cfg.contains("10.77.9.205"),
        "must carry the controller host: {cfg}"
    );
    assert!(cfg.contains("\"remotePort\""), "must set remotePort: {cfg}");
    assert!(
        cfg.contains("16622"),
        "must carry the controller port: {cfg}"
    );
    assert!(
        !cfg.contains("companionAddress"),
        "must not use the nonexistent companionAddress key: {cfg}"
    );
    let (jcode, _o, jerr) = run_sourced(
        &[],
        "strih_companion_satellite_appconfig_json 10.77.9.205 16622 | python3 -c 'import json,sys; json.load(sys.stdin)'",
    );
    assert_eq!(jcode, 0, "app config must be valid JSON; stderr={jerr}");
}

/// The operator-login autostart entry is a Desktop Entry launching the installed /opt binary, armed
/// on (owner rule: a needed feature is always-ON, never a forgettable manual launch).
#[test]
fn companion_satellite_autostart_text_is_a_desktop_entry() {
    let (code, txt, _e) = run_sourced(&[], "strih_companion_satellite_autostart_text");
    assert_eq!(code, 0);
    assert!(
        txt.contains("[Desktop Entry]"),
        "must be a Desktop Entry: {txt}"
    );
    assert!(
        txt.contains("Type=Application"),
        "must be an Application entry: {txt}"
    );
    assert!(
        txt.contains("Exec=/opt/companion-satellite/companion-satellite"),
        "must launch the installed binary: {txt}"
    );
    assert!(
        txt.contains("X-GNOME-Autostart-enabled=true"),
        "must be autostart-enabled: {txt}"
    );
}

/// The Companion Satellite install emitter downloads the PINNED tar.gz, VERIFIES its sha256
/// (fail-loud on mismatch), and runs the tarball's own `install.sh --system --force` (idempotent,
/// desktop model) after installing the deps — NEVER a `.deb`, never a mid-provision start.
#[test]
fn companion_satellite_install_downloads_pinned_tarball_verifies_sha_runs_installsh() {
    let (code, out, err) = run_sourced(&[], "strih_companion_satellite_install");
    assert_eq!(code, 0, "install emitter must succeed; stderr={err}");
    assert!(
        out.contains("cf-pub.bitfocus.io"),
        "must fetch the pinned CDN tarball: {out}"
    );
    assert!(
        out.contains("sha256sum -c"),
        "must verify the pinned sha256 (fail-loud): {out}"
    );
    assert!(
        out.contains("install.sh --system --force"),
        "must run the tarball's own idempotent install.sh: {out}"
    );
    assert!(
        out.contains("libusb-1.0-0-dev"),
        "must install the documented Companion Satellite deps: {out}"
    );
    assert!(
        !out.contains(".deb"),
        "must NOT install a .deb (the bounce cause): {out}"
    );
    assert!(
        !out.contains("systemctl start") && !out.contains("enable --now"),
        "companion is never started mid-provision: {out}"
    );
    let (code, _o, err) = run_sourced(&[], "strih_companion_satellite_install | bash -n");
    assert_eq!(
        code, 0,
        "emitted install block must be valid bash; stderr={err}"
    );
}

/// The verify (companion) verdict is fail-closed over the desktop-model signals: `ok` only for
/// binary-installed + desktop-udev-rule + operator-autostart + controller-host-seeded.
#[test]
fn companion_verdict_is_failclosed() {
    let (code, out, _e) = run_sourced(&[], "strih_companion_verdict 1 1 1 1");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "ok");
    for (args, tok) in [
        ("0 1 1 1", "not-installed"),
        ("1 0 1 1", "no-udev-rule"),
        ("1 1 0 1", "no-autostart"),
        ("1 1 1 0", "wrong-host"),
    ] {
        let (code, out, _e) = run_sourced(&[], &format!("strih_companion_verdict {args}"));
        assert_ne!(code, 0, "{args} must be non-ok");
        assert_eq!(out.trim(), tok, "{args} must verdict {tok}");
    }
    // Missing args default to 0 (fail-closed).
    let (code, out, _e) = run_sourced(&[], "strih_companion_verdict");
    assert_ne!(code, 0, "no args must fail-closed");
    assert_eq!(out.trim(), "not-installed");
}

/// setup-strih.sh must wire the perf step (apply + persistence unit) BEFORE the final verify gate,
/// and TOTAL_STEPS must be bumped to 17 for the two new steps.
#[test]
fn setup_strih_wires_performance_mode_before_final_verify() {
    let s = read_script("scripts/setup-strih.sh");
    let apply = s
        .find("strih_performance_mode_apply")
        .expect("setup-strih must call strih_performance_mode_apply");
    let unit = s
        .find("strih_cpu_performance_unit_text")
        .expect("setup-strih must write the cpu-performance persistence unit");
    let verify = s
        .find("verify-strih.sh acceptance gate")
        .expect("final verify present");
    assert!(
        apply < verify && unit < verify,
        "the perf step must run before the final verify (apply {apply}, unit {unit}, verify {verify})"
    );
    assert!(
        s.contains("TOTAL_STEPS=17"),
        "TOTAL_STEPS must be bumped to 17 for the perf + companion steps"
    );
}

/// setup-strih.sh must install Companion Satellite (install emitter), seed the app config.json
/// (functional controller pin) AND write the operator-login autostart, all BEFORE the final verify.
#[test]
fn setup_strih_wires_companion_satellite_before_final_verify() {
    let s = read_script("scripts/setup-strih.sh");
    let install = s
        .find("strih_companion_satellite_install")
        .expect("setup-strih must call strih_companion_satellite_install");
    let appcfg = s
        .find("strih_companion_satellite_appconfig_json")
        .expect("setup-strih must seed the app config.json with the controller host");
    let autostart = s
        .find("strih_companion_satellite_autostart_text")
        .expect("setup-strih must write the operator-login autostart entry");
    let verify = s
        .find("verify-strih.sh acceptance gate")
        .expect("final verify present");
    assert!(
        install < verify && appcfg < verify && autostart < verify,
        "the companion step must run before the final verify (install {install}, appcfg {appcfg}, autostart {autostart}, verify {verify})"
    );
}

/// verify-strih.sh must carry the (perf) governor + (companion) acceptance items.
#[test]
fn verify_strih_carries_perf_and_companion_items() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_verify_governor_ok"),
        "verify-strih must run the governor predicate for the (perf) item"
    );
    assert!(
        v.contains("strih_companion_verdict"),
        "verify-strih must run the companion verdict for the (companion) item"
    );
}

// =====================================================================================
// issue 1317 (this lane): post-cut-over fixes -- the seeder targets the OPERATOR collection
// (strih role: explicit names + update-only) and three step-15/16 live-found defects.
// =====================================================================================

/// The performance-mode apply now runs BOTH power-profiles-daemon AND the scaling_governor write
/// (NOT either/or): on intel_pstate active `powerprofilesctl set performance` sets EPP only and
/// leaves the governor `powersave`, so the governor MUST be written unconditionally (the live defect).
#[test]
fn performance_mode_apply_runs_both_ppd_and_governor_not_either_or() {
    let (code, out, err) = run_sourced(&[], "strih_performance_mode_apply");
    assert_eq!(code, 0, "emitter must succeed; stderr={err}");
    assert!(
        out.contains("powerprofilesctl set performance"),
        "must still run powerprofilesctl set performance: {out}"
    );
    assert!(
        out.contains("scaling_governor"),
        "must ALWAYS write the scaling_governor: {out}"
    );
    // The bug was `if ppd; then ...; else <governor>; fi` -- the governor gated behind an else, so on
    // intel_pstate active (ppd present) it never ran. The fix removes the `else`: both run.
    assert!(
        !out.contains("else"),
        "the governor write must NOT be an `else` fallback of ppd (both run unconditionally): {out}"
    );
    let (code, _o, err) = run_sourced(&[], "strih_performance_mode_apply | bash -n");
    assert_eq!(
        code, 0,
        "emitted apply block must be valid bash; stderr={err}"
    );
}

/// The effective-perf log line reports the triple governor / EPP / ppd profile (never a
/// self-contradicting "set to performance (now: powersave)"). Defaults fill in when a facet is absent.
#[test]
fn perf_effective_line_reports_the_governor_epp_ppd_triple() {
    let (code, out, _e) = run_sourced(
        &[],
        "strih_perf_effective_line performance performance performance",
    );
    assert_eq!(code, 0);
    assert!(out.contains("governor=performance"), "governor term: {out}");
    assert!(out.contains("EPP=performance"), "EPP term: {out}");
    assert!(out.contains("ppd=performance"), "ppd term: {out}");
    // fail-soft defaults when a facet is unreadable/absent (never a bare empty triple)
    let (_c, out2, _e) = run_sourced(&[], "strih_perf_effective_line");
    assert!(
        out2.contains("governor=") && out2.contains("EPP=") && out2.contains("ppd="),
        "missing facets default rather than vanish: {out2}"
    );
}

/// The OPERATOR seed manifest is valid JSON carrying `"mode":"update-only"` + EXPLICIT-name object
/// entries (NDI camN / NDI 2ME PVW / NDI 2ME PGM (mv) / cg / CG-obs), with the outputs unchanged.
#[test]
fn seed_manifest_json_is_update_only_with_the_explicit_operator_names() {
    let (code, out, err) = run_sourced(&[], "strih_lx_seed_manifest_json");
    assert_eq!(code, 0, "manifest emitter must succeed; stderr={err}");
    assert!(
        out.contains("\"mode\": \"update-only\""),
        "mode update-only: {out}"
    );
    for name in [
        "\"NDI cam1\"",
        "\"NDI cam7\"",
        "\"NDI 2ME PVW\"",
        "\"NDI 2ME PGM (mv)\"",
        "\"cg\"",
        "\"CG-obs\"",
    ] {
        assert!(out.contains(name), "manifest must carry {name}: {out}");
    }
    // senders (the DATA name-map) + the unchanged namespaced outputs
    assert!(out.contains("\"CAM1 (usb)\""), "sender name-map: {out}");
    assert!(
        out.contains("STRIH-LX (2ME PGM)"),
        "outputs unchanged: {out}"
    );
    assert!(
        out.contains("\"camera_latency_ms\": 3"),
        "floor-3 latency: {out}"
    );
    // it MUST be valid JSON
    let (jcode, _o, jerr) = run_sourced(
        &[],
        "strih_lx_seed_manifest_json | python3 -c 'import json,sys; json.load(sys.stdin)'",
    );
    assert_eq!(jcode, 0, "manifest must be valid JSON; stderr={jerr}");
}

/// The emitted manifest, parsed by strih_scenes.py, classifies the 2ME pair as feedback (non-genlock)
/// and every camera + cg as the certified genlock class -- proving the bash DATA + python code agree.
#[test]
fn seed_manifest_json_classifies_via_strih_scenes() {
    let scn_dir = manifest_dir().join("scripts");
    let scn = scn_dir.to_string_lossy().into_owned();
    let (code, out, err) = run_sourced(
        &[("SCN_DIR", scn.as_str())],
        r#"strih_lx_seed_manifest_json | python3 -c '
import sys, os, json
sys.path.insert(0, os.environ["SCN_DIR"])
import strih_scenes as m
text = sys.stdin.read()
inputs, _o, latency = m.parse_seed_manifest(text)
assert m.parse_seed_mode(text) == "update-only", "mode"
by = {p["input"]: p for p in m.seed_inputs(inputs, latency)}
assert by["NDI 2ME PVW"]["settings"]["genlock_fifo"] is False, "2ME PVW must be feedback"
assert by["NDI 2ME PGM (mv)"]["settings"]["genlock_fifo"] is False, "2ME PGM must be feedback"
assert by["NDI cam1"]["settings"]["genlock_fifo"] is True, "cam must be genlock"
assert by["cg"]["settings"]["genlock_fifo"] is True, "cg is a genlocked sender"
print("OK")
'"#,
    );
    assert_eq!(code, 0, "cross-check must pass; stdout={out} stderr={err}");
    assert!(out.contains("OK"), "cross-check printed OK: {out}");
}

/// The Companion Satellite install emitter installs `curl` in the dep line (a fresh strih-lx has NO
/// curl -- the emitter's own download failed on the first live run without it).
#[test]
fn companion_satellite_install_deps_include_curl() {
    let (code, out, _e) = run_sourced(&[], "strih_companion_satellite_install");
    assert_eq!(code, 0);
    assert!(
        out.contains("apt-get install -y curl") || out.contains(" curl "),
        "the dep install line must include curl (a fresh box has none): {out}"
    );
}

/// The REST-apply emitter: when the Satellite local REST (:9999) answers, POST the controller to
/// /api/config so the running instance adopts it live (the file seed alone left effective host
/// 127.0.0.1 until this POST). Best-effort, valid bash, idempotent no-op when the REST is down.
#[test]
fn companion_satellite_rest_apply_posts_the_controller_to_9999() {
    let (code, out, err) = run_sourced(
        &[],
        "strih_companion_satellite_rest_apply_cmd 10.77.9.205 16622",
    );
    assert_eq!(code, 0, "emitter must succeed; stderr={err}");
    assert!(
        out.contains(":9999"),
        "targets the Satellite local REST :9999: {out}"
    );
    assert!(
        out.contains("/api/status"),
        "guards on /api/status (no-op when down): {out}"
    );
    assert!(out.contains("/api/config"), "POSTs to /api/config: {out}");
    assert!(
        out.contains("\"protocol\":\"tcp\"") || out.contains("\"protocol\": \"tcp\""),
        "the POST body carries protocol tcp: {out}"
    );
    assert!(
        out.contains("10.77.9.205"),
        "carries the controller host: {out}"
    );
    assert!(out.contains("16622"), "carries the controller port: {out}");
    let (jcode, _o, jerr) = run_sourced(
        &[],
        "strih_companion_satellite_rest_apply_cmd 10.77.9.205 16622 | bash -n",
    );
    assert_eq!(
        jcode, 0,
        "emitted REST-apply block must be valid bash; stderr={jerr}"
    );
}

/// The live-aware (companion) status verdict: when the Satellite REST is NOT up it is a FILE-ONLY
/// check (pass through the file verdict + rc); when it IS up the controller link must be connected.
#[test]
fn companion_status_verdict_is_live_aware() {
    // not running -> file-only: an `ok` file verdict stays ok (rc 0), a bad one stays bad (rc 1)
    let (c, out, _e) = run_sourced(&[], "strih_companion_status_verdict ok 0 0");
    assert_eq!(c, 0);
    assert_eq!(out.trim(), "ok");
    let (c, out, _e) = run_sourced(&[], "strih_companion_status_verdict not-installed 0 0");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "not-installed");
    // running + connected -> ok-connected (rc 0)
    let (c, out, _e) = run_sourced(&[], "strih_companion_status_verdict ok 1 1");
    assert_eq!(c, 0);
    assert_eq!(out.trim(), "ok-connected");
    // running but NOT connected -> not-connected (rc 1), even with an ok file verdict
    let (c, out, _e) = run_sourced(&[], "strih_companion_status_verdict ok 1 0");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "not-connected");
}

/// setup-strih.sh step 6 must write the OPERATOR seed manifest via strih_lx_seed_manifest_json
/// (update-only + explicit names), and step 15 must log the effective governor/EPP/ppd triple.
#[test]
fn setup_strih_uses_the_operator_manifest_and_effective_perf_line() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("strih_lx_seed_manifest_json"),
        "setup-strih step 6 must write the manifest via strih_lx_seed_manifest_json"
    );
    assert!(
        s.contains("strih_perf_effective_line"),
        "setup-strih step 15 must log the effective governor/EPP/ppd triple"
    );
}

/// setup-strih.sh step 16 must run the REST-apply AFTER seeding the app config.json, and
/// verify-strih.sh item 19 must grade the live `connected` state via strih_companion_status_verdict.
#[test]
fn setup_strih_rest_apply_and_verify_live_connected() {
    let s = read_script("scripts/setup-strih.sh");
    let appcfg = s
        .find("strih_companion_satellite_appconfig_json")
        .expect("setup-strih must seed the app config.json");
    let rest = s
        .find("strih_companion_satellite_rest_apply_cmd")
        .expect("setup-strih step 16 must run the REST apply");
    assert!(
        appcfg < rest,
        "the REST apply must run AFTER seeding config.json (appcfg {appcfg} vs rest {rest})"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_companion_status_verdict"),
        "verify-strih item 19 must grade the live connected state via strih_companion_status_verdict"
    );
    assert!(
        v.contains("/api/status"),
        "verify-strih item 19 must probe the Satellite REST /api/status for the connected read"
    );
}

/// issue 1317 (5th live root cause): OBS resolves its helper processes (obs-ffmpeg-mux the record
/// muxer, obs-nvenc-test the NVENC probe) NEXT TO ITS OWN EXECUTABLE, so the prefix install must
/// enumerate the WHOLE bin/ dir -- installing only `obs` left the notebook with `NVENC not supported`
/// plus a broken record muxer. strih_bundle_bin_files enumerates every regular file under bin/.
#[test]
fn bundle_bin_files_enumerates_every_helper_not_just_obs() {
    let tmp = std::env::temp_dir().join(format!("strih_bin_1317_{}", std::process::id()));
    let bindir = tmp.join("bin");
    std::fs::create_dir_all(&bindir).unwrap();
    for f in ["obs", "obs-ffmpeg-mux", "obs-nvenc-test"] {
        std::fs::write(bindir.join(f), b"#!/bin/sh\n").unwrap();
    }
    let bundle = tmp.to_string_lossy().into_owned();
    let (code, out, err) = run_sourced(
        &[("BUNDLE", bundle.as_str())],
        "strih_bundle_bin_files \"$BUNDLE\"",
    );
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(code, 0, "enumeration must succeed; stderr={err}");
    for name in ["obs", "obs-ffmpeg-mux", "obs-nvenc-test"] {
        assert!(
            out.lines().any(|l| l.ends_with(&format!("/bin/{name}"))),
            "must enumerate bin/{name} (not just obs): {out}"
        );
    }
    assert_eq!(
        out.lines().filter(|l| !l.trim().is_empty()).count(),
        3,
        "must enumerate all three bin files: {out}"
    );
}

/// strih_install_bundle_prefix must install the WHOLE bin/ dir via strih_bundle_bin_files, not a
/// single hardcoded `bin/obs` (the pre-fix shape). Guarded by the source text since the actual
/// install needs root (`install -o root`), which a CI test never has.
#[test]
fn install_bundle_prefix_installs_all_bin_helpers_via_enumeration() {
    let lib = read_script("scripts/lib/strih-provision.sh");
    let body = lib
        .split("strih_install_bundle_prefix()")
        .nth(1)
        .expect("strih_install_bundle_prefix must exist");
    assert!(
        body.contains("strih_bundle_bin_files"),
        "the install must enumerate every bin helper via strih_bundle_bin_files, not a single obs"
    );
}

/// The OBS-helper acceptance verdict: fail-closed -- `ok` only when BOTH obs-ffmpeg-mux (record
/// muxer) AND obs-nvenc-test (NVENC probe) are present+executable beside obs (recording-critical).
#[test]
fn obs_helpers_verdict_is_failclosed() {
    let (c, out, _e) = run_sourced(&[], "strih_obs_helpers_verdict 1 1");
    assert_eq!(c, 0);
    assert_eq!(out.trim(), "ok");
    let (c, out, _e) = run_sourced(&[], "strih_obs_helpers_verdict 0 1");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "no-obs-ffmpeg-mux");
    let (c, out, _e) = run_sourced(&[], "strih_obs_helpers_verdict 1 0");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "no-obs-nvenc-test");
    // fail-closed on missing args
    let (c, _o, _e) = run_sourced(&[], "strih_obs_helpers_verdict");
    assert_ne!(c, 0);
}

/// The NVENC-log verdict (report-only, needs a running OBS): a healthy log carries
/// `[obs-nvenc] NVENC version:`; the missing-helper signature is `NVENC not supported` with no
/// version line; anything else (OBS not up yet) is unknown.
#[test]
fn nvenc_log_verdict_grades_the_obs_log() {
    let (c, out, _e) = run_sourced(
        &[],
        "printf '[obs-nvenc] NVENC version: 12.1 (compiled) / 13.0 (driver)\\n' | strih_nvenc_log_verdict",
    );
    assert_eq!(c, 0);
    assert_eq!(out.trim(), "nvenc-ok");
    let (c, out, _e) = run_sourced(
        &[],
        "printf '[NVENC] Failed to launch the NVENC test process\\nNVENC not supported\\n' | strih_nvenc_log_verdict",
    );
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "nvenc-unsupported");
    let (_c, out, _e) = run_sourced(
        &[],
        "printf 'some other log line\\n' | strih_nvenc_log_verdict",
    );
    assert_eq!(out.trim(), "nvenc-unknown");
}

/// verify-strih.sh must gate the OBS helper binaries beside /usr/bin/obs (recording-critical) and
/// grade the NVENC log via the pure verdicts.
#[test]
fn verify_strih_gates_obs_helpers_and_nvenc() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_obs_helpers_verdict"),
        "verify-strih must gate the OBS helper binaries via strih_obs_helpers_verdict"
    );
    assert!(
        v.contains("/usr/bin/obs-ffmpeg-mux") && v.contains("/usr/bin/obs-nvenc-test"),
        "verify-strih must check obs-ffmpeg-mux + obs-nvenc-test beside /usr/bin/obs"
    );
    assert!(
        v.contains("strih_nvenc_log_verdict"),
        "verify-strih must grade the NVENC log via strih_nvenc_log_verdict"
    );
}
