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
    // issue 1345 M3 follow-up (d): WS is BOUND to the LAN IP (not 0.0.0.0 / all interfaces). The
    // dev1 front reaches it over the LAN; the loopback probe/hub session use the HTTP transport.
    assert!(
        out.contains("ws_ip = \"10.77.9.203\""),
        "WS must bind the LAN IP (ws_ip), never all interfaces; got:\n{out}"
    );
}

/// issue 1345 M3 follow-up (d): `strih_janus_http_jcfg_text` renders the Janus HTTP transport jcfg
/// bound to LOOPBACK 127.0.0.1 only (`ip = "127.0.0.1"`) — the hub's plain-RTP session + local
/// probes use it; the phone never touches HTTP. NO https, admin API off, never a 0.0.0.0 bind.
#[test]
fn janus_http_jcfg_binds_loopback_only() {
    let (code, out, err) = run_sourced(&[], "strih_janus_http_jcfg_text");
    assert_eq!(code, 0, "renderer must succeed; stderr={err}");
    assert!(out.contains("http = true"), "http enabled; got:\n{out}");
    assert!(out.contains("port = 8088"), "http on :8088");
    assert!(
        out.contains("ip = \"127.0.0.1\""),
        "HTTP API MUST bind loopback 127.0.0.1 only; got:\n{out}"
    );
    assert!(out.contains("https = false"), "no https (no TLS here)");
    assert!(out.contains("admin_http = false"), "admin HTTP API off");
    assert!(
        !out.contains("0.0.0.0"),
        "the HTTP API must never bind all interfaces; got:\n{out}"
    );
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

/// issue 1345 M3a: `setup-strih.sh` must apt-install janus + enable-only + write all three jcfg files
/// (audiobridge room, WS transport, HTTP transport) and run BEFORE the final verify. The Janus step
/// must run BEFORE the audio TODO gate so the audio `fail` (issue 1344) no longer blocks it (issue
/// 1345 M3 follow-ups b/c/d). Mirrors the dantesync/NDI ordering anchors.
#[test]
fn setup_strih_installs_janus_enable_only_before_final_verify() {
    let s = read_script("scripts/setup-strih.sh");
    let apt = s
        .find("apt-get install -y janus")
        .expect("setup-strih must apt-get install janus");
    let enable = s
        .find("systemctl enable janus")
        .expect("setup-strih must enable janus");
    let verify = s
        .find("verify-strih.sh acceptance gate")
        .expect("setup-strih must run the verify gate");
    assert!(
        apt < enable,
        "install before enable (apt {apt} vs enable {enable})"
    );
    assert!(
        enable < verify,
        "janus enable must precede the final verify (enable {enable} vs verify {verify})"
    );
    // Never an UNCONDITIONAL start; a restart is allowed ONLY guarded by is-active (Ubuntu auto-
    // starts janus on install before the jcfg exists — restart to load the fresh jcfg if running).
    assert!(
        !s.contains("systemctl start janus"),
        "janus is never unconditionally started until the M4 cut-over"
    );
    if s.contains("systemctl restart janus") {
        let restart = s.find("systemctl restart janus").unwrap();
        let guard = s.find("systemctl is-active --quiet janus");
        assert!(
            guard.is_some_and(|g| g < restart && restart - g < 300),
            "any `systemctl restart janus` must be guarded by a nearby `systemctl is-active --quiet janus`"
        );
    }
    // The HTTP transport jcfg is written (loopback bind) alongside the audiobridge + WS jcfg.
    assert!(
        s.contains("strih_janus_http_jcfg_text")
            && s.contains("/etc/janus/janus.transport.http.jcfg"),
        "setup-strih must render + write the HTTP transport jcfg (loopback bind)"
    );
    // The Janus step runs BEFORE the audio TODO gate (which `fail`s until the MiniFuse graph is
    // wired, issue 1344) — the live box stopped at the audio step and never reached Janus.
    let audio_gate = s
        .find("TODO(audio):")
        .expect("setup-strih must carry the audio TODO gate");
    assert!(
        apt < audio_gate,
        "the janus step must run BEFORE the audio TODO gate (janus {apt} vs audio {audio_gate})"
    );
    assert!(
        s.contains("TOTAL_STEPS=15"),
        "TOTAL_STEPS must be bumped for the janus step"
    );
}
