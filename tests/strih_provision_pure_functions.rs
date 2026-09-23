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
fn audio_input_name_is_asio_zvuk_and_the_fail_loud_flag_is_gone() {
    // issue 1344: the program audio is now WIRED (the PipeWire graph), so the old fail-loud TODO
    // predicate + its STRIH_LX_AUDIO_WIRED flag are REMOVED — a derived verdict replaces them.
    let (code, _o, _e) = run_sourced(&[], "type strih_lx_audio_route_wired 2>/dev/null");
    assert_ne!(
        code, 0,
        "strih_lx_audio_route_wired must no longer be defined"
    );
    // The OBS program-audio input keeps a human name reported by setup/verify — now `ASIO zvuk`
    // (the OBS input name on strih-program.monitor, kept for the scene/mixer/E2E selectors).
    let (_c, name, _e) = run_sourced(&[], "strih_lx_audio_input_name");
    assert_eq!(name.trim(), "ASIO zvuk");
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
    // issue 1352: strih-obs-start.sh is no longer a verbatim `install` -- setup-strih substitutes the
    // @STRIH_LX_OBS_GPU_ENV@ marker with strih_lx_obs_gpu_env, WRITES the result to /usr/local/bin,
    // then chmod 0755. Assert the deployed write + the strih-obs-stop.sh install both precede enable.
    let start_write = s.find("> /usr/local/bin/strih-obs-start.sh").expect(
        "setup-strih must write the (GPU-env-substituted) strih-obs-start.sh to /usr/local/bin",
    );
    let start_chmod = s
        .find("chmod 0755 /usr/local/bin/strih-obs-start.sh")
        .expect("the substituted strih-obs-start.sh must be chmod 0755");
    let stop_install = s
        .find("install -m 0755 \"${HERE}/strih-obs-stop.sh\"")
        .expect("setup-strih must install strih-obs-stop.sh mode 0755");
    let enable = s
        .find("systemctl --user enable strih-obs.service")
        .expect("setup-strih step 8 must enable strih-obs.service");
    assert!(
        start_write < enable && start_chmod < enable && stop_install < enable,
        "both launcher installs must precede the unit enable (start_write {start_write} / chmod {start_chmod} / stop {stop_install} vs enable {enable})"
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
        lib.contains("install -m 0755 -o root -g root \"$binf\" \"${bindir}/${base}\""),
        "strih_install_bundle_prefix must install each bundle bin/* file (incl. obs) to BINDIR/<base> (0755 root) — issue 1317 bundle-helpers: obs + obs-ffmpeg-mux + obs-nvenc-test, a loop over bin/*, not a hardcoded obs"
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

// --- issue 1317 (this lane): dantesync ROLE-aware unit + the verify sleep-mask predicate -----------

/// issue 1317: `strih_dantesync_unit_text ROLE [ARGS]` renders the systemd unit for the given ROLE.
/// Post-M4 the strih notebook IS the fleet's NTP master, so `server` (the default) renders the BARE
/// NTP-master ExecStart (folding the live 10-ntp-master.conf drop-in into the unit); `client` renders
/// `--ntp-server <host>`. Fail-closed via strih_lx_dantesync_role_ok on an ambiguous shape (server
/// WITH args, client with a master/empty arg, an unknown role) -- but a plain server role is CORRECT
/// and NEVER refused (the reversal of the pre-M4 "always fail-closed on server mode" behaviour).
#[test]
fn dantesync_unit_text_renders_the_role_and_fail_closes_on_ambiguous_shapes() {
    // server role: the BARE NTP-master ExecStart, correct cambox unit shape, no client args.
    let (code, out, _e) = run_sourced(&[], "strih_dantesync_unit_text server ''");
    assert_eq!(
        code, 0,
        "a server role must emit a unit (the post-M4 NTP master)"
    );
    assert!(
        out.contains("ExecStart=/usr/local/bin/dantesync\n"),
        "server ExecStart must be the BARE NTP-master daemon: {out}"
    );
    assert!(
        !out.contains("--ntp-server"),
        "the server role must NOT carry the client --ntp-server args: {out}"
    );
    assert!(
        out.contains("Type=simple")
            && out.contains("Restart=always")
            && out.contains("RestartSec=5")
            && out.contains("WantedBy=multi-user.target"),
        "server unit must keep the cambox shape: {out}"
    );
    assert!(
        !out.contains("--service"),
        "`--service` is a run mode, not an installer flag — it must never appear: {out}"
    );

    // client role: the --ntp-server ExecStart.
    let (c2, out2, _e2) = run_sourced(
        &[],
        "strih_dantesync_unit_text client '--ntp-server strih.lan'",
    );
    assert_eq!(c2, 0, "a client role with client args must emit a unit");
    assert!(
        out2.contains("ExecStart=/usr/local/bin/dantesync --ntp-server strih.lan"),
        "client ExecStart must be the CLIENT daemon: {out2}"
    );
    // client with no ARGS defaults to the client args helper.
    let (c2b, out2b, _e) = run_sourced(&[], "strih_dantesync_unit_text client ''");
    assert_eq!(c2b, 0);
    assert!(
        out2b.contains("ExecStart=/usr/local/bin/dantesync --ntp-server"),
        "the default client ExecStart must carry the client args: {out2b}"
    );

    // Ambiguous shapes emit NOTHING and return non-zero. (`client ''` is NOT ambiguous: the
    // printer defaults an empty client args to the client helper -- asserted above.)
    for (role, args) in [
        ("server", "--ntp-server strih.lan"),
        ("client", "ntp_server_mode"),
        ("bogus", ""),
    ] {
        let (c3, out3, _e3) =
            run_sourced(&[], &format!("strih_dantesync_unit_text '{role}' '{args}'"));
        assert_ne!(
            c3, 0,
            "ambiguous role='{role}' args='{args}' must be refused"
        );
        assert!(
            out3.trim().is_empty(),
            "a refused invocation must emit NOTHING (role='{role}' args='{args}'): {out3}"
        );
    }
}

/// issue 1317: `strih_lx_dantesync_role_ok ROLE ARGS` is the ROLE-AWARE public gate that REPLACES
/// strih_lx_dantesync_is_client_not_master as the caller-facing predicate. server + empty -> ok (the
/// NTP master; never fail-closed on server mode itself), server + args -> ambiguous -> fail, client +
/// a real client arg -> ok, client + master/empty -> fail, unknown/empty role -> fail.
#[test]
fn dantesync_role_ok_is_role_aware_and_fail_closed_on_ambiguity() {
    let (c, _o, _e) = run_sourced(&[], "strih_lx_dantesync_role_ok server ''");
    assert_eq!(
        c, 0,
        "server role with no args must pass (the fleet NTP master)"
    );
    let (c2, _o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_role_ok server '--ntp-server strih.lan'",
    );
    assert_ne!(c2, 0, "server + args is an ambiguous shape -> fail-closed");
    let (c3, _o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_role_ok client '--ntp-server strih.lan'",
    );
    assert_eq!(c3, 0, "client + a real client arg must pass");
    for args in ["ntp_server_mode", "master", ""] {
        let (cx, _o, _e) = run_sourced(&[], &format!("strih_lx_dantesync_role_ok client '{args}'"));
        assert_ne!(cx, 0, "client + '{args}' must fail-closed");
    }
    for role in ["", "bogus"] {
        let (cx, _o, _e) = run_sourced(&[], &format!("strih_lx_dantesync_role_ok '{role}' ''"));
        assert_ne!(cx, 0, "role '{role}' must fail-closed");
    }
}

/// issue 1317: `strih_lx_dantesync_status_role_verdict ROLE REACHABLE MODE UDP123` grades the live
/// :8898/status + :123 read. ok ONLY for reachable + a locked mode (+ for server, a :123 listener);
/// every other state prints its own token + returns non-zero (verify-strih renders each token).
#[test]
fn dantesync_status_role_verdict_grades_reachable_locked_and_server_listener() {
    let (c, o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_status_role_verdict server 1 LOCK 1",
    );
    assert_eq!(c, 0, "server reachable+LOCK+listener -> ok");
    assert_eq!(o.trim(), "ok");
    let (_c, o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_status_role_verdict server 1 LOCK 0",
    );
    assert_eq!(
        o.trim(),
        "no-ntp-listener",
        "server without :123 -> no-ntp-listener"
    );
    let (c, o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_status_role_verdict client 1 NANO 0",
    );
    assert_eq!(c, 0, "client does not require a :123 listener");
    assert_eq!(o.trim(), "ok");
    let (_c, o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_status_role_verdict server 0 absent 0",
    );
    assert_eq!(o.trim(), "unreachable");
    let (_c, o, _e) = run_sourced(
        &[],
        "strih_lx_dantesync_status_role_verdict server 1 FREE 1",
    );
    assert_eq!(o.trim(), "mode:FREE", "a non-locked mode is reported");
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

/// issue 1317: `setup-strih.sh` step 2 must INSTALL the dantesync unit with the ROLE folded in (write
/// it via `strih_dantesync_unit_text "$DS_ROLE" "$DS_ARGS"` into /etc/systemd/system/dantesync.service),
/// default the role to `server` (the post-M4 NTP master), remove any stale dantesync.service.d/*.conf
/// drop-in, and this must precede the OBS unit enable (step 8).
#[test]
fn setup_strih_installs_the_dantesync_unit_in_step_2() {
    let s = read_script("scripts/setup-strih.sh");
    let emit = s
        .find("strih_dantesync_unit_text \"$DS_ROLE\" \"$DS_ARGS\"")
        .expect("setup-strih step 2 must emit the dantesync unit via strih_dantesync_unit_text ROLE ARGS");
    assert!(
        s.contains("/etc/systemd/system/dantesync.service"),
        "setup-strih must write the dantesync unit to /etc/systemd/system/dantesync.service"
    );
    assert!(
        s.contains("STRIH_LX_DANTESYNC_ROLE:-server"),
        "setup-strih step 2 must default the dantesync role to `server` (the post-M4 NTP master)"
    );
    assert!(
        s.contains("strih_lx_dantesync_role_ok \"$DS_ROLE\" \"$DS_ARGS\""),
        "setup-strih step 2 must guard the role+args via strih_lx_dantesync_role_ok"
    );
    assert!(
        s.contains("rm -f /etc/systemd/system/dantesync.service.d/*.conf"),
        "setup-strih step 2 must remove any stale dantesync.service.d/*.conf drop-in (role now in the unit)"
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
    // NOTE (issue 1345 M3 re-integration): the original M3 assertion here — "the janus step runs
    // BEFORE the audio TODO gate" — is SUPERSEDED by the issue-1344 restructure. The program-audio
    // step (12) is now the real PipeWire strih-program wiring, no longer a fail-loud `TODO(audio)`
    // gate that could block provisioning, so there is nothing for the janus step to precede. The
    // janus step keeps its own place (14) in the 17-step flow (audio 12 -> hub 13 -> janus 14).
    assert!(
        !s.contains("TODO(audio):"),
        "the audio step is now the real PipeWire wiring (issue 1344), never a fail-loud TODO gate"
    );
    assert!(
        s.contains("TOTAL_STEPS=17"),
        "TOTAL_STEPS must be bumped for the janus step (17 after the issue-1317 perf + companion steps)"
    );
}

// ==============================================================================
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
import sys, os, json, types
# The Rust Test/Coverage jobs do NOT install websocket-client, but strih_scenes.py imports it at
# module load (a DELIBERATE launcher-preflight contract). This cross-check exercises only the PURE
# helpers, so stub the websocket module so `import strih_scenes` succeeds without the runtime dep.
_ws = types.ModuleType("websocket")
_ws.create_connection = lambda *a, **k: None
sys.modules.setdefault("websocket", _ws)
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

/// issue 1317 review (non-blocker): a REAL OBS log is large (100s of KB). A `printf | grep -q` inside
/// an `if` SIGPIPEs printf on an early match under pipefail -> the pipeline goes non-zero -> the `if`
/// reads false -> a HEALTHY large log misgrades to nvenc-unknown. Feed a large log with an EARLY
/// version line and require nvenc-ok (the SIGPIPE-under-pipefail class, drift-guard-log-parsers.md).
#[test]
fn nvenc_log_verdict_survives_a_large_log_with_an_early_match() {
    let (c, out, err) = run_sourced(
        &[],
        "awk 'BEGIN{print \"[obs-nvenc] NVENC version: 12.1 (compiled) / 13.0 (driver)\"; for(i=0;i<20000;i++) print \"padding line to make the OBS log large (100s of KB) with an early match\"}' | strih_nvenc_log_verdict",
    );
    assert_eq!(
        c, 0,
        "an early match in a large log must still grade nvenc-ok; stderr={err}"
    );
    assert_eq!(out.trim(), "nvenc-ok");
    // the same for the unsupported signature buried in a large log (must not flip to unknown)
    let (c, out, _e) = run_sourced(
        &[],
        "awk 'BEGIN{print \"NVENC not supported\"; for(i=0;i<20000;i++) print \"padding line padding line padding line padding\"}' | strih_nvenc_log_verdict",
    );
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "nvenc-unsupported");
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

// --- issue 1344: the local PipeWire program-audio graph (VB-Matrix replacement) -----------------

/// The operator-session PipeWire drop-in must declare a null sink named `strih-program` (the node
/// OBS captures as `strih-program.monitor` for its `ASIO zvuk` program input).
#[test]
fn program_sink_conf_declares_the_strih_program_null_sink() {
    let (code, out, err) = run_sourced(&[], "strih_pipewire_program_sink_conf");
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("support.null-audio-sink"),
        "must create a null sink; got: {out}"
    );
    assert!(
        out.contains("strih-program"),
        "the sink node name must be strih-program; got: {out}"
    );
    assert!(
        out.contains("Audio/Sink"),
        "the null sink must be an Audio/Sink; got: {out}"
    );
}

/// The loopback republish node must NOT itself be present on the null-sink emitter (it lives in a
/// sibling conf), and must republish `strih-program` as a real Audio/Source (issue 1344 follow-up,
/// 20.9.2026 live diagnosis: strih-program.monitor is NOT pulse-visible to OBS on this box, so OBS
/// must capture this loopback's republished node `strih-program-source` instead).
#[test]
fn program_loopback_conf_republishes_a_real_audio_source() {
    let (code, out, err) = run_sourced(&[], "strih_pipewire_program_loopback_conf");
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("libpipewire-module-loopback"),
        "must load the loopback module; got: {out}"
    );
    assert!(
        out.contains("node.target"),
        "must declare a capture target; got: {out}"
    );
    assert!(
        out.contains("\"strih-program\""),
        "must capture the strih-program sink; got: {out}"
    );
    assert!(
        out.contains("stream.capture.sink") && out.contains("true"),
        "must capture the SINK output (stream.capture.sink=true), not a monitor; got: {out}"
    );
    assert!(
        out.contains("strih-program-source"),
        "the republished node must be named strih-program-source; got: {out}"
    );
    assert!(
        out.contains("\"Audio/Source\""),
        "the republished node must be a plain Audio/Source; got: {out}"
    );
    assert!(
        !out.contains("Audio/Source/Virtual"),
        "must NEVER be Audio/Source/Virtual (OBS enumerates it but never links the capture          stream -- proven live silent); got: {out}"
    );
}

/// The WirePlumber rule must pin the MiniFuse 4 to its pro-audio profile at 48 kHz (the talkback
/// capture path OBS never touches — the hub reads it).
#[test]
fn wireplumber_rule_pins_the_minifuse_pro_audio_at_48k() {
    let (code, out, err) = run_sourced(&[], "strih_wireplumber_minifuse_rule");
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("MiniFuse"),
        "the rule must match the MiniFuse; got: {out}"
    );
    assert!(
        out.contains("pro-audio"),
        "the rule must select the pro-audio profile; got: {out}"
    );
    assert!(
        out.contains("48000"),
        "the rule must pin 48 kHz; got: {out}"
    );
}

/// The systemd drop-in must run the hub as the operator (NOT DynamicUser) with the operator's
/// PipeWire runtime dir, so the pw-cat children reach the operator's audio session.
#[test]
fn intercom_audio_dropin_runs_as_the_operator_with_the_pipewire_runtime() {
    let (code, out, err) = run_sourced(&[], "strih_intercom_audio_dropin newlevel 1000");
    assert_eq!(code, 0, "stderr={err}");
    assert!(
        out.contains("[Service]"),
        "a systemd [Service] override; got: {out}"
    );
    assert!(
        out.contains("DynamicUser=no"),
        "must disable DynamicUser; got: {out}"
    );
    assert!(
        out.contains("User=newlevel"),
        "must run as the operator; got: {out}"
    );
    assert!(
        out.contains("XDG_RUNTIME_DIR=/run/user/1000"),
        "must set the operator runtime dir; got: {out}"
    );
    assert!(
        out.contains("pipewire"),
        "must join the pipewire group; got: {out}"
    );
}

/// The derived (audio) verdict: FAIL when the sink / input / hub-rx is absent; NOTE when wired but
/// FOH idle; PASS only when wired AND FOH-live audio clears the -60 dBFS bar.
#[test]
fn program_audio_verdict_matrix() {
    // sink absent -> FAIL
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 0 1 pulse_input_capture 1 1",
    );
    assert_ne!(c, 0);
    assert!(out.starts_with("FAIL"), "sink absent must FAIL; got: {out}");
    // wrong OBS input kind -> FAIL
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 1 1 asio_input_capture 1 1",
    );
    assert_ne!(c, 0);
    assert!(
        out.starts_with("FAIL"),
        "asio input kind must FAIL; got: {out}"
    );
    // hub not receiving fohabl-strih -> FAIL
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 1 0 pulse_input_capture 1 1",
    );
    assert_ne!(c, 0);
    assert!(out.starts_with("FAIL"), "no hub rx must FAIL; got: {out}");
    // wired, FOH idle -> NOTE (level unchecked), exit 0
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 1 1 pulse_input_capture 0 na",
    );
    assert_eq!(c, 0);
    assert!(out.starts_with("NOTE"), "FOH idle must NOTE; got: {out}");
    // wired, FOH live, level below bar -> FAIL
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 1 1 pulse_input_capture 1 0",
    );
    assert_ne!(c, 0);
    assert!(
        out.starts_with("FAIL"),
        "FOH live but silent must FAIL; got: {out}"
    );
    // wired, FOH live, level ok -> PASS
    let (c, out, _e) = run_sourced(
        &[],
        "strih_lx_program_audio_verdict 1 1 pulse_input_capture 1 1",
    );
    assert_eq!(c, 0);
    assert!(
        out.starts_with("PASS"),
        "wired + live + level must PASS; got: {out}"
    );
}

/// setup-strih step 12 must INSTALL the pipewire graph (no more fail-loud TODO) and wire the hub
/// audio drop-in; verify-strih must grade the audio via the pure verdict.
#[test]
fn setup_and_verify_wire_the_pipewire_program_audio() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("strih_pipewire_program_sink_conf"),
        "setup-strih step 12 must install the strih-program sink conf"
    );
    assert!(
        s.contains("strih_pipewire_program_loopback_conf"),
        "setup-strih step 12 must install the strih-program loopback conf (issue 1344 follow-up)"
    );
    assert!(
        s.contains("strih_wireplumber_minifuse_rule"),
        "setup-strih step 12 must install the WirePlumber MiniFuse rule"
    );
    assert!(
        s.contains("strih_intercom_audio_dropin"),
        "setup-strih step 12 must install the intercom-hub audio drop-in"
    );
    assert!(
        !s.contains("STRIH_LX_AUDIO_WIRED"),
        "the manual STRIH_LX_AUDIO_WIRED flag must be removed (derived verify replaces it)"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_program_audio_verdict"),
        "verify-strih must grade program audio via strih_lx_program_audio_verdict"
    );
}

// --- issue 1317 slice: bake qt6-svg-plugins + /usr-prefix chrome-sandbox setuid ------------------
// The strih-lx OBS UI needs (1) the Qt6 SVG icon-engine/imageformat plugins (Ubuntu 26.04 ships them
// in the SEPARATE `qt6-svg-plugins` package, NOT libqt6svg6) so OBS 32's SVG Yami theme renders its
// icons, and (2) the CEF chrome-sandbox setuid at EVERY path the running OBS could load it from --
// including the /usr-prefix obs-plugins copy the /usr-install OBS actually loads (the /opt bundle
// copy alone is not enough). These pure helpers are the ONE source of truth for the setuid target
// set + the verify assertions.

#[test]
fn chrome_sandbox_setuid_roots_include_both_bundle_and_usr_obs_plugins() {
    // ONE source of truth for the setuid target set: the /opt bundle root AND the resolved
    // /usr-prefix obs-plugins dir the /usr-install OBS loads chrome-sandbox from.
    let (code, out, _e) = run_sourced(
        &[],
        "strih_lx_chrome_sandbox_setuid_roots /opt/obs-genlock /usr/lib/x86_64-linux-gnu",
    );
    assert_eq!(code, 0, "setuid-roots helper must succeed");
    let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.iter().any(|l| l.trim() == "/opt/obs-genlock"),
        "must include the bundle root: {out}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.trim() == "/usr/lib/x86_64-linux-gnu/obs-plugins"),
        "must include the resolved /usr-prefix obs-plugins dir: {out}"
    );
    assert_eq!(lines.len(), 2, "exactly the two setuid roots: {out}");
}

#[test]
fn chrome_sandbox_fix_cmd_covers_multiple_roots_including_usr() {
    // Variadic over the resolved roots: every emitted statement setuids chrome-sandbox by NAME under
    // its root, chown root:root + chmod 4755, ;-terminated, never --no-sandbox, and the /usr obs-plugins
    // root the running OBS loads is covered (the gap this slice closes).
    let (code, out, _e) = run_sourced(
        &[],
        "strih_lx_chrome_sandbox_fix_cmd /opt/obs-genlock /usr/lib/x86_64-linux-gnu/obs-plugins",
    );
    assert_eq!(code, 0, "variadic builder must succeed");
    assert!(
        out.contains("/opt/obs-genlock"),
        "covers the bundle root: {out}"
    );
    assert!(
        out.contains("/usr/lib/x86_64-linux-gnu/obs-plugins"),
        "covers the /usr-prefix obs-plugins root (the live-broken 0755 path): {out}"
    );
    assert_eq!(
        out.matches("-name chrome-sandbox").count(),
        2,
        "one find per root: {out}"
    );
    assert_eq!(
        out.matches("chmod 4755").count(),
        2,
        "chmod 4755 per root: {out}"
    );
    assert!(out.contains("chown root:root"), "chown root:root: {out}");
    assert!(
        !out.contains("--no-sandbox"),
        "never weakens the sandbox: {out}"
    );
    // Every emitted statement is ;-terminated (the v4l2-neutral.sh mid-string embedding gotcha).
    for l in out.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            l.trim_end().ends_with(';'),
            "each emitted statement must end with ';': {l}"
        );
    }
}

#[test]
fn chrome_sandbox_fix_cmd_still_single_root_backward_compatible() {
    // The existing single-root call site (setup-strih.sh) keeps working: one find/chown/chmod.
    let (code, out, _e) = run_sourced(&[], "strih_lx_chrome_sandbox_fix_cmd /opt/obs-genlock");
    assert_eq!(code, 0);
    assert_eq!(out.matches("-name chrome-sandbox").count(), 1, "{out}");
    assert_eq!(out.matches("chmod 4755").count(), 1, "{out}");
}

#[test]
fn chrome_sandbox_usr_path_is_the_obs_plugins_copy() {
    let (code, out, _e) = run_sourced(
        &[],
        "strih_lx_chrome_sandbox_usr_path /usr/lib/x86_64-linux-gnu",
    );
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "/usr/lib/x86_64-linux-gnu/obs-plugins/chrome-sandbox"
    );
}

#[test]
fn qt6_svg_iconengine_path_is_the_libqsvgicon_plugin() {
    let (code, out, _e) = run_sourced(
        &[],
        "strih_lx_qt6_svg_iconengine_path /usr/lib/x86_64-linux-gnu",
    );
    assert_eq!(code, 0);
    assert_eq!(
        out.trim(),
        "/usr/lib/x86_64-linux-gnu/qt6/plugins/iconengines/libqsvgicon.so"
    );
}

#[test]
fn obs_ui_fix_verdict_tokens_ok_svg_missing_wrong_owner_wrong_mode() {
    // Combined verdict for the verify item: SVG iconengine present (1) AND the /usr chrome-sandbox is
    // root:root 4755. Fail-closed order: svg first, then owner, then mode.
    let (c, out, _e) = run_sourced(&[], "strih_lx_obs_ui_fix_verdict 1 root:root 4755");
    assert_eq!(c, 0, "svg present + root:root/4755 must be ok");
    assert_eq!(out.trim(), "ok");
    let (c, out, _e) = run_sourced(&[], "strih_lx_obs_ui_fix_verdict 0 root:root 4755");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "svg-missing");
    let (c, out, _e) = run_sourced(&[], "strih_lx_obs_ui_fix_verdict 1 newlevel:newlevel 4755");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "sandbox-wrong-owner");
    let (c, out, _e) = run_sourced(&[], "strih_lx_obs_ui_fix_verdict 1 root:root 0755");
    assert_ne!(c, 0);
    assert_eq!(out.trim(), "sandbox-wrong-mode");
}

/// Wiring: `setup-strih.sh` installs `qt6-svg-plugins` (idempotent apt-get) so the SVG theme icons
/// render, and the F6 chrome-sandbox step resolves its setuid target set via the ONE-source-of-truth
/// helper (covering the /usr-prefix obs-plugins copy, not just $GENLOCK_DIR).
#[test]
fn setup_strih_installs_qt6_svg_and_setuids_both_chrome_sandbox_roots() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("apt-get install -y qt6-svg-plugins"),
        "setup-strih must idempotently install qt6-svg-plugins (OBS 32 SVG theme icons)"
    );
    assert!(
        s.contains("strih_lx_chrome_sandbox_setuid_roots"),
        "setup-strih F6 must resolve the setuid target roots via the ONE-source-of-truth helper"
    );
}

/// Wiring: `verify-strih.sh` gains ONE report item asserting BOTH the qt6-svg iconengine plugin is
/// present AND chrome-sandbox is setuid 4755 at the /usr path the running OBS loads.
#[test]
fn verify_strih_asserts_qt6_svg_and_usr_chrome_sandbox() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_qt6_svg_iconengine_path"),
        "verify-strih must assert the qt6-svg iconengine plugin path"
    );
    assert!(
        v.contains("strih_lx_obs_ui_fix_verdict"),
        "verify-strih must grade the combined OBS-UI fix via the pure verdict"
    );
    assert!(
        v.contains("strih_lx_chrome_sandbox_usr_path"),
        "verify-strih must read chrome-sandbox at the resolved /usr path"
    );
}

// =====================================================================================
// issue 1352: strih-lx OBS on the RTX 5050 via XWayland PRIME -- provisioning bake
// (helper + unit + GPU env printer + Janus local_ip + DistroAV output names + self-loop seed)
// =====================================================================================

/// issue 1352 (b): `strih_lx_obs_gpu_env` prints the 4 XWayland-PRIME exports on ONE line (no
/// trailing newline) so a `$(...)` embedding never glues the following statement.
#[test]
fn gpu_env_prints_the_four_xwayland_prime_exports_on_one_line() {
    let (code, out, err) = run_sourced(&[], "strih_lx_obs_gpu_env");
    assert_eq!(code, 0, "printer must succeed; stderr={err}");
    for ex in [
        "export QT_QPA_PLATFORM=xcb",
        "export __NV_PRIME_RENDER_OFFLOAD=1",
        "export __GLX_VENDOR_LIBRARY_NAME=nvidia",
        "export __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json",
    ] {
        assert!(out.contains(ex), "gpu env must carry {ex}: {out}");
    }
    assert!(
        !out.contains('\n'),
        "the exports must be a single line: {out:?}"
    );
    assert_eq!(
        out.matches("export ").count(),
        4,
        "exactly 4 exports: {out}"
    );
}

/// issue 1352 (e): `strih_lx_ndi_output_ini_cmds USER_INI` carries the BARE DistroAV output names +
/// enabled flags, and functionally upserts them into [NDIPlugin] idempotently, preserving other
/// sections and never clobbering an unparseable ini.
#[test]
fn ndi_output_ini_cmds_upserts_the_bare_distroav_output_names_idempotently() {
    let (code, out, err) =
        run_sourced(&[], "strih_lx_ndi_output_ini_cmds /tmp/does-not-matter.ini");
    assert_eq!(code, 0, "printer must succeed; stderr={err}");
    for kv in [
        "MainOutputName=2ME PGM",
        "PreviewOutputName=2ME PVW",
        "MainOutputEnabled=true",
        "PreviewOutputEnabled=true",
    ] {
        assert!(out.contains(kv), "printer must carry {kv}: {out}");
    }
    let body = r#"
tmp="$(mktemp)"
printf '[General]\nFoo=bar\n' > "$tmp"
eval "$(strih_lx_ndi_output_ini_cmds "$tmp")"
eval "$(strih_lx_ndi_output_ini_cmds "$tmp")"
echo "MAIN=$(grep -c 'MainOutputName=2ME PGM' "$tmp")"
echo "PREVN=$(grep -c 'PreviewOutputName=2ME PVW' "$tmp")"
echo "MAINEN=$(grep -c 'MainOutputEnabled=true' "$tmp")"
echo "PREVEN=$(grep -c 'PreviewOutputEnabled=true' "$tmp")"
echo "KEPT=$(grep -c 'Foo=bar' "$tmp")"
rm -f "$tmp"
"#;
    let (c, o, e) = run_sourced(&[], body);
    assert_eq!(c, 0, "functional upsert must succeed; stderr={e}");
    assert!(
        o.contains("MAIN=1"),
        "one MainOutputName after idempotent upsert: {o}"
    );
    assert!(o.contains("PREVN=1"), "one PreviewOutputName: {o}");
    assert!(o.contains("MAINEN=1"), "MainOutputEnabled=true: {o}");
    assert!(o.contains("PREVEN=1"), "PreviewOutputEnabled=true: {o}");
    assert!(
        o.contains("KEPT=1"),
        "other sections preserved (never clobbered): {o}"
    );
}

/// issue 1352 (d): `strih_janus_audiobridge_jcfg_text ROOM SECRET LOCAL_IP` pins `local_ip` inside
/// `general` (before the room block); a blank/omitted LOCAL_IP omits the assignment (the ws_ip idiom).
#[test]
fn janus_audiobridge_jcfg_pins_local_ip_in_general_when_given() {
    let (code, out, err) =
        run_sourced(&[], "strih_janus_audiobridge_jcfg_text 1000 /x 10.77.9.202");
    assert_eq!(code, 0, "renderer must succeed; stderr={err}");
    let ip_at = out
        .find("local_ip = \"10.77.9.202\"")
        .expect("3-arg must pin local_ip in general");
    let room_at = out.find("room-1000:").expect("must still render the room");
    assert!(
        ip_at < room_at,
        "local_ip must be inside general (before the room block): {out}"
    );
    // 2-arg (blank): no local_ip ASSIGNMENT line (the doc comment mentions local_ip, the block does not).
    let (c2, out2, _e) = run_sourced(&[], "strih_janus_audiobridge_jcfg_text 1000 /x");
    assert_eq!(c2, 0);
    assert!(
        !out2.contains("local_ip = \""),
        "a blank local_ip must omit the assignment: {out2}"
    );
}

/// issue 1352 (f): the 2ME feedback pair receives strih-lx's OWN `STRIH-LX (2ME …)` outputs (the M4
/// self-loop); the dead parallel-phase `STRIH-SNV (2ME …)` senders are gone.
#[test]
fn seed_manifest_2me_feedback_senders_are_the_strih_lx_self_loop() {
    let (code, out, err) = run_sourced(&[], "strih_lx_seed_manifest_json");
    assert_eq!(code, 0, "manifest must succeed; stderr={err}");
    assert!(
        out.contains(r#"{"sender": "STRIH-LX (2ME PVW)", "input": "NDI 2ME PVW""#),
        "2ME PVW must receive the STRIH-LX self-loop sender: {out}"
    );
    assert!(
        out.contains(r#"{"sender": "STRIH-LX (2ME PGM)", "input": "NDI 2ME PGM (mv)""#),
        "2ME PGM must receive the STRIH-LX self-loop sender: {out}"
    );
    assert!(
        !out.contains("STRIH-SNV (2ME PVW)"),
        "no dead STRIH-SNV (2ME PVW) sender: {out}"
    );
    assert!(
        !out.contains("STRIH-SNV (2ME PGM)"),
        "no dead STRIH-SNV (2ME PGM) sender: {out}"
    );
}

/// issue 1352 (b): setup-strih substitutes the `@STRIH_LX_OBS_GPU_ENV@` marker with the printer output
/// (the `@JANUS_ROOM_SECRET@` idiom), so the printer is the ONE source of truth and the wrapper does
/// not hardcode the exports.
#[test]
fn setup_strih_substitutes_the_gpu_env_marker_via_the_printer() {
    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains("strih_lx_obs_gpu_env"),
        "setup must consume the gpu-env printer"
    );
    assert!(
        setup.contains("//#@STRIH_LX_OBS_GPU_ENV@/"),
        "setup must substitute the @STRIH_LX_OBS_GPU_ENV@ marker (the @JANUS_ROOM_SECRET@ idiom)"
    );
    let wrapper = read_script("scripts/strih-obs-start.sh");
    assert!(
        wrapper.contains("#@STRIH_LX_OBS_GPU_ENV@"),
        "the repo wrapper must carry the marker line (a bash comment)"
    );
    assert!(
        !wrapper.contains("__NV_PRIME_RENDER_OFFLOAD=1"),
        "the wrapper must NOT hardcode the PRIME exports (single source of truth = the printer)"
    );
}

/// issue 1352 (e): setup-strih step 7 seeds [NDIPlugin] via the pure printer (eval-consumed).
#[test]
fn setup_strih_seeds_ndi_output_names_via_the_printer() {
    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains("eval \"$(strih_lx_ndi_output_ini_cmds"),
        "setup step 7 must seed [NDIPlugin] via strih_lx_ndi_output_ini_cmds (eval-consumed)"
    );
}

/// issue 1352 (d): the audiobridge jcfg call passes STATIC_IP (= strih_lx_ip, the ONE IP source of
/// truth) as the LOCAL_IP arg so a renumber cannot strand the plain-RTP bind.
#[test]
fn setup_strih_pins_janus_local_ip_from_the_static_ip_source_of_truth() {
    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains("strih_janus_audiobridge_jcfg_text \"$JANUS_ROOM\" \"$JANUS_SECRET_FILE\" \"$STATIC_IP\""),
        "setup must pass STATIC_IP to the audiobridge jcfg (the renumber-proof local_ip pin)"
    );
    assert!(
        setup.contains("STATIC_IP=\"$(strih_lx_ip)\""),
        "STATIC_IP must be the single IP source of truth (strih_lx_ip)"
    );
}

/// issue 1352 (a): the mv-host helper + `--user` unit exist in the repo; setup installs python3-xlib,
/// the helper (0755) + the unit, and ENABLE-ONLY registers it (never a live start). The unit's
/// ExecStart references the installed helper.
#[test]
fn setup_strih_installs_and_enables_the_mv_host_helper() {
    assert!(
        manifest_dir().join("scripts/strih-mv-host.py").exists(),
        "scripts/strih-mv-host.py must exist"
    );
    assert!(
        manifest_dir()
            .join("systemd/strih-mv-host.service")
            .exists(),
        "systemd/strih-mv-host.service must exist"
    );
    let unit = read_script("systemd/strih-mv-host.service");
    assert!(
        unit.contains("/usr/local/bin/strih-mv-host.py"),
        "the unit ExecStart must reference the installed helper"
    );
    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains("apt-get install -y python3-xlib"),
        "setup must install python3-xlib (the helper's only dep)"
    );
    assert!(
        setup.contains("install -m 0755 \"${HERE}/strih-mv-host.py\""),
        "setup must install the helper mode 0755"
    );
    assert!(
        setup.contains("install -m 0644 \"${HERE}/../systemd/strih-mv-host.service\""),
        "setup must install the --user unit"
    );
    // issue 1352 acceptance (22.9.2026): the vendored child-host projector is LIVE, and the runtime
    // helper CONFLICTS with it (both hosting mechanisms active = MV 0.6 fps / 505 ms presents;
    // helper stopped = 30 fps / 5 ms). Provisioning therefore installs the helper but leaves it
    // DISABLED unless STRIH_MV_HOST_ENABLED=1 (a bundle without the vendored fix).
    assert!(
        setup.contains("STRIH_MV_HOST_ENABLED:-0"),
        "setup must gate the mv-host enablement on STRIH_MV_HOST_ENABLED (default 0)"
    );
    assert!(
        setup.contains("systemctl --user enable strih-mv-host.service"),
        "setup must still be able to enable the mv-host unit (the STRIH_MV_HOST_ENABLED=1 fallback)"
    );
    assert!(
        setup.contains("systemctl --user disable --now strih-mv-host.service"),
        "setup must disable (and stop) the conflicting helper by default"
    );
    assert!(
        !setup.contains("systemctl --user start strih-mv-host"),
        "mv-host must never be live-started by provisioning (the provisioning convention)"
    );
    let verify = read_script("scripts/verify-strih.sh");
    assert!(
        verify.contains("STRIH_MV_HOST_ENABLED:-0") && verify.contains("enablement mismatch"),
        "verify-strih must grade the mv-host enablement against STRIH_MV_HOST_ENABLED (default DISABLED)"
    );
}

/// issue 1352 (c): setup installs avahi-utils (avahi-browse) and verify greps for it.
#[test]
fn setup_strih_installs_avahi_utils_and_verify_greps_avahi_browse() {
    let setup = read_script("scripts/setup-strih.sh");
    assert!(
        setup.contains("apt-get install -y avahi-utils"),
        "setup must install avahi-utils (avahi-browse for NDI/mDNS discovery)"
    );
    let verify = read_script("scripts/verify-strih.sh");
    assert!(
        verify.contains("command -v avahi-browse"),
        "verify must assert avahi-browse is present"
    );
}

/// issue 1352: verify-strih.sh gates every provisioned item (mv-host, gpu-env, avahi, DistroAV output
/// names, janus local_ip).
#[test]
fn verify_strih_asserts_the_1352_provisioning_items() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih-mv-host.service"),
        "verify must gate the mv-host unit"
    );
    assert!(
        v.contains("import Xlib"),
        "verify must assert python3-xlib importable"
    );
    assert!(
        v.contains("QT_QPA_PLATFORM=xcb"),
        "verify must grep the deployed wrapper for the RTX exports"
    );
    assert!(
        v.contains("#@STRIH_LX_OBS_GPU_ENV@"),
        "verify must detect an un-substituted gpu-env marker"
    );
    assert!(
        v.contains("MainOutputName=2ME PGM"),
        "verify must assert the DistroAV output names in user.ini"
    );
    assert!(
        v.contains("local_ip = \""),
        "verify must grep the janus local_ip pin"
    );
}

/// issue 1352 acceptance run (22.9.2026): `verify-strih.sh` aborted SILENTLY (exit 1, no line) right
/// after the remoteos item on strih-lx, because `strih_lx_program_audio_verdict` returns non-zero on a
/// FAIL verdict and its `$(...)` assignment ran WITHOUT the `|| true` every sibling verdict assignment
/// carries -- under `set -e` a failing command substitution in an assignment terminates the script, so
/// the ~10 items after it (mv-host, gpu-env, avahi, ndi-outputs, ...) never ran. The verdict text is
/// printed on stdout regardless of the return code; the `case` below it grades it. Pin the guard.
/// 22.9.2026: setup-strih step 17 runs verify-strih.sh as ROOT, whose pw-cli cannot see the
/// operator's PipeWire session, so item 9 read "strih-program sink missing" on a healthy box (the
/// sink was there for the user). The probe must target the operator session when EUID is 0.
#[test]
fn verify_strih_program_audio_probe_targets_the_operator_session_when_root() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("sudo -u \"${STRIH_LX_USER:-newlevel}\" XDG_RUNTIME_DIR=\"/run/user/$(id -u \"${STRIH_LX_USER:-newlevel}\")\" pw-cli ls Node"),
        "verify item 9 must run pw-cli as the operator with the operator's XDG_RUNTIME_DIR when root"
    );
    assert!(
        v.contains("if [ \"$(id -u)\" = 0 ]; then") && v.contains("PW_LS=\"$(pw-cli ls Node 2>/dev/null || true)\""),
        "verify item 9 must branch on EUID: operator-session probe as root, inline pw-cli otherwise"
    );
}

#[test]
fn verify_strih_audio_verdict_assignment_survives_set_e() {
    let v = read_script("scripts/verify-strih.sh");
    let line = v
        .lines()
        .find(|l| l.contains("AUDIO_VERDICT=\"$(strih_lx_program_audio_verdict"))
        .expect("verify-strih must assign AUDIO_VERDICT from strih_lx_program_audio_verdict");
    assert!(
        line.contains("|| true)\""),
        "the AUDIO_VERDICT command substitution must carry `|| true` so a FAIL verdict (non-zero \
         return) cannot abort the whole gate under set -e: {line}"
    );
}

/// issue 1352 acceptance run (22.9.2026): the NVENC gate line piped `ffmpeg -encoders || cat "$LOG"`
/// into `strih_lx_nvenc_available_ok` -- on strih-lx the distro ffmpeg exits 0 WITHOUT any nvenc
/// encoder, so the `||` fallback never reached the OBS log even though the very next report-only line
/// proved `[obs-nvenc] NVENC version:` live in OBS. Both evidence sources must be concatenated (`;`),
/// so either the ffmpeg encoder list or the OBS log satisfies the predicate.
#[test]
fn verify_strih_nvenc_gate_feeds_both_ffmpeg_and_the_obs_log() {
    let v = read_script("scripts/verify-strih.sh");
    let line = v
        .lines()
        .find(|l| l.contains("strih_lx_nvenc_available_ok && ok"))
        .expect("verify-strih must grade NVENC via strih_lx_nvenc_available_ok");
    assert!(
        line.contains("2>/dev/null; cat \"$LOG\"") && !line.contains("|| cat \"$LOG\""),
        "the NVENC gate must feed BOTH `ffmpeg -encoders` and the OBS log (`;`, not `||`): {line}"
    );
}

/// issue 1352 acceptance run (22.9.2026): the janus audiobridge jcfg is root:root 0640 on the box, so
/// verify-strih.sh run as the operator user could not read it (`Permission denied`) and reported
/// `general.local_ip NOT pinned (or jcfg unreadable)` right after the pin was installed. The reads
/// must fall back to `sudo -n cat` (non-interactive; a box without a cached sudo ticket still degrades
/// to the honest "unreadable" note, never a false PASS) so a provisioned pin grades as pinned.
#[test]
fn verify_strih_reads_the_janus_jcfg_via_sudo_n_fallback() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("sudo -n cat \"$JANUS_AB\""),
        "verify-strih must read the root-only janus audiobridge jcfg via a `sudo -n cat` fallback"
    );
}

/// issue 1352 acceptance run (22.9.2026), the second half of the NVENC gate bug: once the gate fed
/// the (large) OBS log into `strih_lx_nvenc_available_ok`, the predicate's `grep -q` exited on the
/// first match, the producer took SIGPIPE, and under `pipefail` the pipeline returned 141 -> the gate
/// still FAILED with NVENC provably live. The predicate must read its input to EOF (exit 0 iff a
/// match) so a large early-match input passes under `set -o pipefail` -- the drain-safe parser class
/// of `.claude/rules/drift-guard-log-parsers.md`.
#[test]
fn nvenc_predicate_is_drain_safe_under_pipefail_with_a_large_early_match() {
    let body = r#"{ printf 'x [obs-nvenc] NVENC version: 12\n'; head -c 3000000 /dev/zero | tr '\0' 'a'; } | strih_lx_nvenc_available_ok; echo "rc=$?""#;
    let (_, out, err) = run_sourced(&[], body);
    assert!(
        out.contains("rc=0"),
        "a large early-match input must pass under pipefail (rc=0), got stdout={out:?} stderr={err:?}"
    );
    let (_, out2, _) = run_sourced(
        &[],
        r#"printf 'no hardware encoder here\n' | strih_lx_nvenc_available_ok; echo "rc=$?""#,
    );
    assert!(
        out2.contains("rc=1"),
        "no nvenc evidence must still fail: {out2:?}"
    );
}

// --- issue 1317 (this lane): the 5 hand-patch bake-ins (A BrowserHWAccel seed, B plugin prune, C
// collection hygiene, D RustDesk) + their setup/verify wiring --------------------------------------

/// (A) `strih_lx_obs_global_ini_cmds OBS_CFG_DIR` seeds [General] BrowserHWAccel=false into
/// global.ini via an idempotent RawConfigParser upsert, preserving other keys/sections; exactly one
/// BrowserHWAccel key results.
#[test]
fn obs_global_ini_cmds_seeds_browserhwaccel_false_idempotently() {
    let body = r#"
d="$(mktemp -d)"; mkdir -p "$d/obs"
eval "$(strih_lx_obs_global_ini_cmds "$d/obs")"
grep -q '^\[General\]' "$d/obs/global.ini" && grep -q '^BrowserHWAccel=false' "$d/obs/global.ini" && echo FIRST_OK
printf '[General]\nMaxLogs=10\nBrowserHWAccel=true\n[Video]\nFPSCommon=30\n' > "$d/obs/global.ini"
eval "$(strih_lx_obs_global_ini_cmds "$d/obs")"
grep -q '^BrowserHWAccel=false' "$d/obs/global.ini" && grep -q '^MaxLogs=10' "$d/obs/global.ini" && grep -q '^FPSCommon=30' "$d/obs/global.ini" && echo UPSERT_OK
n="$(grep -c '^BrowserHWAccel' "$d/obs/global.ini")"; echo "count=$n"
rm -rf "$d"
"#;
    let (code, out, err) = run_sourced(&[], body);
    assert_eq!(code, 0, "harness failed: {err}");
    assert!(
        out.contains("FIRST_OK"),
        "fresh global.ini must gain [General] BrowserHWAccel=false: {out}"
    );
    assert!(
        out.contains("UPSERT_OK"),
        "upsert must flip true->false + preserve other keys/sections: {out}"
    );
    assert!(
        out.contains("count=1"),
        "exactly one BrowserHWAccel key: {out}"
    );
}

/// (A wiring) setup-strih step 7 seeds global.ini via the printer (OBS stopped); verify-strih asserts
/// BrowserHWAccel=false.
#[test]
fn setup_strih_seeds_browserhwaccel_in_step_7_and_verify_checks_it() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("strih_lx_obs_global_ini_cmds \"$OBS_CFG\""),
        "step 7 must seed global.ini via strih_lx_obs_global_ini_cmds"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("BrowserHWAccel=false"),
        "verify-strih must assert BrowserHWAccel=false"
    );
}

/// (B) the prune LIST (3 basenames, never distroav/browser) + the two obs-plugins DIRS are the ONE
/// source of truth used by BOTH the setup prune loop and the verify absence check.
#[test]
fn plugin_prune_list_and_dirs_are_the_one_source_of_truth() {
    let (_c, out, _e) = run_sourced(&[], "strih_lx_obs_plugin_prune_list");
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 3, "exactly 3 prune basenames: {out}");
    for want in ["decklink*.so", "obs-qsv11.so", "obs-vst.so"] {
        assert!(
            lines.contains(&want),
            "prune list must contain {want}: {out}"
        );
    }
    assert!(
        !out.to_lowercase().contains("distroav") && !out.to_lowercase().contains("browser"),
        "the prune list must NEVER carry distroav/browser: {out}"
    );
    let (_c, dirs, _e) = run_sourced(
        &[],
        "strih_lx_obs_plugin_dirs /opt/obs-genlock /usr/lib/x86_64-linux-gnu",
    );
    assert!(
        dirs.contains("/opt/obs-genlock/lib/x86_64-linux-gnu/obs-plugins"),
        "bundle plugin dir: {dirs}"
    );
    assert!(
        dirs.contains("/usr/lib/x86_64-linux-gnu/obs-plugins"),
        "usr plugin dir: {dirs}"
    );
}

/// (B functional) the prune expands globs against a fake plugin tree, removes ONLY the 3 kinds from
/// BOTH dirs, keeps distroav/browser, and is idempotent (a 2nd pass removes nothing).
#[test]
fn plugin_prune_removes_only_dead_plugins_from_both_dirs() {
    let body = r#"
root="$(mktemp -d)"
B="$root/opt/lib/x86_64-linux-gnu/obs-plugins"; U="$root/usr/obs-plugins"
mkdir -p "$B" "$U"
for d in "$B" "$U"; do
  : > "$d/decklink-output-ui.so"; : > "$d/decklink-captions.so"; : > "$d/obs-qsv11.so"; : > "$d/obs-vst.so"
  : > "$d/distroav.so"; : > "$d/obs-browser.so"; : > "$d/libcef.so"
done
prune() {
  while IFS= read -r pd; do
    [ -d "$pd" ] || continue
    while IFS= read -r pat; do
      [ -n "$pat" ] || continue
      for pf in "$pd"/$pat; do [ -e "$pf" ] && rm -f "$pf" && echo "rm $(basename "$pf")"; done
    done < <(strih_lx_obs_plugin_prune_list)
  done < <(printf '%s\n' "$B" "$U")
}
prune
echo "SECOND:"; prune
for d in "$B" "$U"; do
  for g in decklink-output-ui.so decklink-captions.so obs-qsv11.so obs-vst.so; do [ -e "$d/$g" ] && echo "SURVIVED $g"; done
  for k in distroav.so obs-browser.so libcef.so; do [ -e "$d/$k" ] || echo "MISSING $k"; done
done
rm -rf "$root"
"#;
    let (code, out, err) = run_sourced(&[], body);
    assert_eq!(code, 0, "harness failed: {err}");
    assert!(
        !out.contains("SURVIVED"),
        "a dead plugin survived the prune: {out}"
    );
    assert!(
        !out.contains("MISSING"),
        "a keeper was wrongly pruned: {out}"
    );
    // 4 kinds x 2 dirs = 8 removals in the first pass; nothing after SECOND:.
    let (_pre, post) = out
        .split_once("SECOND:")
        .expect("harness must print SECOND:");
    assert!(
        !post.contains("rm "),
        "the 2nd prune pass must remove nothing (idempotent): {post}"
    );
}

/// (B wiring) setup-strih step 4 prunes via the shared list+dirs; verify-strih asserts absence via
/// the SAME source of truth.
#[test]
fn setup_and_verify_prune_via_the_same_source_of_truth() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("strih_lx_obs_plugin_prune_list"),
        "step 4 must prune via strih_lx_obs_plugin_prune_list"
    );
    assert!(
        s.contains("strih_lx_obs_plugin_dirs"),
        "step 4 must iterate the shared plugin dirs"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_obs_plugin_prune_list") && v.contains("strih_lx_obs_plugin_dirs"),
        "verify-strih must assert prune absence via the SAME source of truth"
    );
}

/// (C) collection hygiene verdict is REPORT-ONLY: ok iff both counts 0, else the token; verify-strih
/// renders it as a NOTE (never a hard FAIL), and setup-strih NEVER touches the collection.
#[test]
fn collection_hygiene_verdict_is_report_only_and_counts_both() {
    let (c, o, _e) = run_sourced(&[], "strih_collection_hygiene_verdict 0 0");
    assert_eq!(c, 0);
    assert_eq!(o.trim(), "ok");
    let (_c, o, _e) = run_sourced(&[], "strih_collection_hygiene_verdict 10 0");
    assert_eq!(o.trim(), "shader_filter:10");
    let (_c, o, _e) = run_sourced(&[], "strih_collection_hygiene_verdict 0 1");
    assert_eq!(o.trim(), "lua:1");
    let (_c, o, _e) = run_sourced(&[], "strih_collection_hygiene_verdict 10 1");
    assert_eq!(o.trim(), "shader_filter:10,lua:1");
    let (c, o, _e) = run_sourced(&[], "strih_collection_hygiene_verdict");
    assert_eq!(c, 0);
    assert_eq!(o.trim(), "ok", "omitted args default to clean");
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_collection_hygiene_verdict"),
        "verify-strih must grade collection hygiene"
    );
    assert!(
        v.contains("note \"(collection-hygiene)"),
        "the hygiene item must be a NOTE (report-only)"
    );
    assert!(
        !v.contains("bad \"(collection-hygiene)"),
        "the hygiene item must NEVER be a hard FAIL"
    );
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        !s.contains("strih_collection_hygiene_verdict"),
        "setup-strih must NEVER rewrite the collection (hygiene is report-only, in verify-strih)"
    );
}

/// (D) RustDesk pinned facts + the install emitter: pinned version/url/sha, a fail-loud sha256 verify,
/// enable --now, the permanent password read from a 0600 FILE inside the block (never a literal), and
/// the emitted block bash-parses.
#[test]
fn rustdesk_facts_are_pinned_and_the_emitter_verifies_sha_and_hides_the_password() {
    assert_eq!(run_sourced(&[], "strih_rustdesk_version").1.trim(), "1.4.9");
    assert_eq!(
        run_sourced(&[], "strih_rustdesk_deb_sha256").1.trim(),
        "7244ba47c40e804172044bfbe659467c54ce46554c98e78c8c0406f1d612fda3"
    );
    let url = run_sourced(&[], "strih_rustdesk_deb_url").1;
    assert!(
        url.contains("rustdesk-1.4.9-x86_64.deb"),
        "url must name the pinned deb: {url}"
    );
    let body =
        r#"strih_rustdesk_install_cmds 1.4.9 http://example/rd.deb DEADBEEFCAFE /etc/rd/pw.secret"#;
    let (code, blk, err) = run_sourced(&[], body);
    assert_eq!(code, 0, "emitter failed: {err}");
    assert!(blk.contains("sha256sum"), "must verify the sha256: {blk}");
    assert!(
        blk.contains("DEADBEEFCAFE"),
        "must compare against the pinned sha: {blk}"
    );
    assert!(
        blk.contains("systemctl enable --now rustdesk"),
        "must enable --now: {blk}"
    );
    assert!(
        blk.contains("rustdesk --password"),
        "must set the permanent password: {blk}"
    );
    assert!(
        blk.contains("/etc/rd/pw.secret"),
        "must read the pw FILE path: {blk}"
    );
    // never any literal password (a fleet-like literal must not appear in the emitted text).
    assert!(
        !blk.to_lowercase().contains("newlevel"),
        "the password value must never be in the emitted block: {blk}"
    );
    // the emitted block must bash-parse (bash -n reads it from stdin).
    let (nc, _o, ne) = run_sourced(
        &[],
        "strih_rustdesk_install_cmds 1.4.9 http://example/rd.deb DEADBEEFCAFE /etc/rd/pw.secret | bash -n",
    );
    assert_eq!(nc, 0, "the emitted rustdesk block must bash-parse: {ne}");
}

/// (D wiring) setup-strih installs RustDesk in a lettered sub-step 16b (TOTAL_STEPS stays 17), reads
/// the pw file path from a provisioning-input env var, and verify-strih checks `rustdesk --get-id`.
#[test]
fn setup_strih_installs_rustdesk_in_step_16b_and_keeps_total_steps_17() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("TOTAL_STEPS=17"),
        "TOTAL_STEPS must stay 17 (lettered sub-steps)"
    );
    assert!(
        s.contains("step \"16b\""),
        "RustDesk must be a lettered sub-step 16b so TOTAL_STEPS stays 17"
    );
    assert!(
        s.contains("strih_rustdesk_install_cmds"),
        "step 16b must install via strih_rustdesk_install_cmds"
    );
    assert!(
        s.contains("STRIH_LX_RUSTDESK_PW_FILE"),
        "step 16b must read the pw file path from a provisioning-input env var"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("rustdesk --get-id"),
        "verify-strih must check rustdesk --get-id"
    );
    assert!(
        v.contains("(rustdesk)"),
        "the rustdesk item must be labelled"
    );
}

/// (G wiring) verify-strih has the dantesync-role live item: it reads :8898/status, grades via the
/// pure verdict, and checks the ntp :123 listener for the server role.
#[test]
fn verify_strih_has_the_dantesync_role_live_item() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_dantesync_status_role_verdict"),
        "verify-strih must grade the role via the pure verdict"
    );
    assert!(
        v.contains("(dantesync-role)"),
        "the role item must be labelled"
    );
    assert!(
        v.contains("8898/status"),
        "verify-strih must read :8898/status"
    );
    assert!(
        v.contains(":123"),
        "verify-strih must check the ntp :123 listener for the server role"
    );
    assert!(
        v.contains("STRIH_LX_DANTESYNC_ROLE:-server"),
        "verify-strih must default the role to `server` (matching setup-strih)"
    );
}

/// (H) setup-strih installs ffmpeg (which provides ffprobe) via the same idempotent apt family as
/// avahi-utils; verify-strih asserts ffprobe + ffmpeg present (the on-box recording-verdict E2E
/// spawns ffprobe). A TOOL dependency of the E2E verdict, NOT a bundle soname (never in
/// RUNTIME_PACKAGES.txt).
#[test]
fn setup_strih_installs_ffmpeg_and_verify_checks_ffprobe() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("apt-get install -y ffmpeg"),
        "setup-strih must apt-install ffmpeg (provides ffprobe for the on-box E2E)"
    );
    // it must NOT be smuggled into the bundle runtime-packages contract.
    assert!(
        !s.contains("RUNTIME_PACKAGES.txt") || !s.contains("ffmpeg RUNTIME_PACKAGES"),
        "ffmpeg is a tool dep, not a bundle soname -- never in RUNTIME_PACKAGES.txt"
    );
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("(ffmpeg)"),
        "verify-strih must have an ffmpeg item"
    );
    assert!(
        v.contains("command -v \"$_t\"") && v.contains("ffprobe") && v.contains("ffmpeg"),
        "verify-strih must assert ffprobe + ffmpeg present"
    );
}

// ===========================================================================================
// issue 1353: strih-lx bkshading SERVICE provisioning -- the shading panel backend moved off the
// Windows PC to the notebook as a systemd unit. Three pure printers (artifact name / system unit /
// config seed) + setup-strih step 16c (enable-only) + verify-strih item + the ci.yml service
// artifact. The panel web assets are EMBEDDED in the binary (bkshading/service/src/http.rs), so the
// unit needs only the self-contained binary; web/ ships beside it per the design as a panel-source
// copy. Development = one camera on cam1.
// ===========================================================================================

/// The Linux service artifact NAME is single-sourced here (the ci.yml upload + the setup-strih fetch
/// both key on it) -- the strih variant of the Windows `bkshading-windows-amd64` canon.
#[test]
fn bkshading_artifact_name_is_the_linux_service_variant() {
    let (code, out, _e) = run_sourced(&[], "strih_bkshading_artifact_name");
    assert_eq!(code, 0);
    assert_eq!(out.trim(), "bkshading-service-linux-amd64");
}

/// The systemd unit is a SYSTEM unit run as the operator (User=newlevel for libndi/PipeWire parity),
/// ExecStart the installed binary with --config, Restart=on-failure, multi-user target, and it never
/// self-starts (enable-only is the installer's job).
#[test]
fn bkshading_unit_text_is_the_system_service_unit() {
    let (code, out, _e) = run_sourced(&[], "strih_bkshading_unit_text");
    assert_eq!(code, 0, "the unit printer must succeed");
    assert!(
        out.contains("User=newlevel"),
        "must run as the operator: {out}"
    );
    assert!(
        out.contains("ExecStart=/opt/bkshading/bkshading --config /etc/bkshading/bkshading.toml"),
        "ExecStart must be the installed binary + --config: {out}"
    );
    assert!(
        out.contains("Restart=on-failure"),
        "must restart on-failure: {out}"
    );
    assert!(
        out.contains("WantedBy=multi-user.target"),
        "system-unit install target: {out}"
    );
    assert!(
        out.contains("Type=simple"),
        "simple long-running service: {out}"
    );
    assert!(
        !out.contains("ExecStartPre") && !out.contains("systemctl start"),
        "the unit must not self-start (enable-only): {out}"
    );
}

/// The committed systemd/bkshading-service.service is byte-identical to the printer output (ONE
/// source of truth, no drift) -- setup-strih writes the live unit FROM the printer.
#[test]
fn committed_bkshading_unit_matches_the_printer() {
    let (code, printed, _e) = run_sourced(&[], "strih_bkshading_unit_text");
    assert_eq!(code, 0);
    let committed = read_script("systemd/bkshading-service.service");
    assert_eq!(
        printed.trim_end(),
        committed.trim_end(),
        "systemd/bkshading-service.service must equal strih_bkshading_unit_text output (source of truth)"
    );
}

/// The config seed carries the operator bind + the cam1 cambox-relay camera (development = one camera
/// on cam1) and is valid TOML; the bind defaults to :8770 and an arg overrides it.
#[test]
fn bkshading_config_text_seeds_cam1_and_is_valid_toml() {
    let (code, cfg, _e) = run_sourced(&[], "strih_bkshading_config_text");
    assert_eq!(code, 0);
    assert!(
        cfg.contains("bind = \"0.0.0.0:8770\""),
        "default operator bind :8770: {cfg}"
    );
    assert!(
        cfg.contains("id = \"cam1\""),
        "must carry the cam1 camera: {cfg}"
    );
    assert!(
        cfg.contains("transport = \"cambox-relay\""),
        "cam1 is reached through the cambox relay: {cfg}"
    );
    assert!(
        cfg.contains("address = \"cam1.lan:8771\""),
        "cam1 relay address (host-independent of the service): {cfg}"
    );
    assert!(
        cfg.contains("ndi_preview = \"CAM1 (usb)\""),
        "cam1 NDI preview name: {cfg}"
    );
    assert!(cfg.contains("grab_fps = 60"), "cam1 grab fps: {cfg}");
    let (jc, _o, je) = run_sourced(
        &[],
        "strih_bkshading_config_text | python3 -c 'import tomllib,sys; tomllib.load(sys.stdin.buffer)'",
    );
    assert_eq!(jc, 0, "config seed must be valid TOML; stderr={je}");
    let (c2, cfg2, _e) = run_sourced(&[], "strih_bkshading_config_text 127.0.0.1:9999");
    assert_eq!(c2, 0);
    assert!(
        cfg2.contains("bind = \"127.0.0.1:9999\""),
        "an explicit bind arg overrides the default: {cfg2}"
    );
}

/// setup-strih.sh wires the bkshading service as lettered sub-step 16c (TOTAL_STEPS stays 17): fetch
/// the artifact, install to /opt/bkshading, seed the config only-if-absent, write the unit from the
/// printer, and ENABLE it -- never START it (the supervisor deploys the running service; the Windows
/// service is the fallback until the owner accepts).
#[test]
fn setup_strih_installs_bkshading_service_in_step_16c_enable_only() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(s.contains("TOTAL_STEPS=17"), "TOTAL_STEPS must stay 17");
    assert!(
        s.contains("step \"16c\""),
        "bkshading service must be lettered sub-step 16c so TOTAL_STEPS stays 17"
    );
    assert!(
        s.contains("strih_bkshading_artifact_name"),
        "the fetch keys on the single-sourced artifact name"
    );
    assert!(
        s.contains("/opt/bkshading"),
        "installs the binary to /opt/bkshading"
    );
    assert!(
        s.contains("[ ! -f /etc/bkshading/bkshading.toml ]"),
        "the config is seeded ONLY if absent (never clobbered)"
    );
    assert!(
        s.contains("strih_bkshading_config_text"),
        "the config is seeded from the pure printer"
    );
    assert!(
        s.contains("strih_bkshading_unit_text"),
        "the unit is written from the pure printer"
    );
    assert!(
        s.contains("systemctl enable bkshading-service"),
        "the unit is enabled"
    );
    assert!(
        !s.contains("systemctl start bkshading-service"),
        "the installer must NEVER start the service (enable-only)"
    );
    assert!(
        !s.contains("enable --now bkshading-service"),
        "the installer must NEVER enable --now the service (enable-only)"
    );
}

/// verify-strih.sh has the bkshading-service acceptance item: an enabled-but-inactive unit is the
/// CORRECT enable-only state (report-only, like intercom-hub/janus); an ACTIVE unit is asserted HARD
/// -- the :8770 listener OWNER must be the bkshading binary (the rule's "confirm the listener's
/// owner" check via ss -tlnp) and /api/state must answer.
#[test]
fn verify_strih_has_the_bkshading_service_item() {
    let v = read_script("scripts/verify-strih.sh");
    assert!(
        v.contains("(bkshading-service)"),
        "the item must be labelled"
    );
    assert!(
        v.contains("bkshading-service.service"),
        "verify-strih must check the bkshading-service unit"
    );
    assert!(
        v.contains("ss -tlnp"),
        "verify-strih must confirm the listener's OWNER via ss -tlnp"
    );
    // The service's health probe is `/api/version` -- `/api/state` is the RELAY's (cambox :8771)
    // and the intercom hub's endpoint, never the service's (live 23.9.2026: :8770/api/state = 404,
    // so a healthy running service graded FAIL "silent").
    assert!(
        v.contains("/api/version"),
        "verify-strih must curl the service's /api/version"
    );
    assert!(
        v.contains("8770"),
        "verify-strih must reference the service port :8770"
    );
}

/// The ci.yml Linux bkshading job uploads the service artifact under the SAME name the printer
/// single-sources, staging the service binary + the panel web assets.
#[test]
fn ci_uploads_the_bkshading_service_linux_artifact() {
    let (code, name, _e) = run_sourced(&[], "strih_bkshading_artifact_name");
    assert_eq!(code, 0);
    let ci = read_script(".github/workflows/ci.yml");
    assert!(
        ci.contains(&format!("name: {}", name.trim())),
        "ci.yml must upload the artifact named by strih_bkshading_artifact_name ({})",
        name.trim()
    );
    assert!(
        ci.contains("bkshading/service/web"),
        "the service artifact must stage the panel web assets"
    );
    assert!(
        ci.contains("target/release/bkshading"),
        "the service artifact must carry the service binary"
    );
}

/// The :8770 listener-owner extraction (verify-strih's hard active-health check, deliverable #4) must
/// return the BARE process name from real `ss -tlnp` output (`users:(("bkshading",pid=...,fd=...))`),
/// NEVER with a trailing quote -- a trailing-quote parse bug graded a HEALTHY active service as
/// unhealthy (owner="bkshading\"" never equals "bkshading"), making the go-live acceptance signal
/// permanently unpassable. A no-match line prints nothing and returns 0 (the report path never aborts).
#[test]
fn bkshading_listener_owner_extracts_the_bare_process_name() {
    let ss = "LISTEN 0 4096 0.0.0.0:8770 0.0.0.0:* users:((\\\"bkshading\\\",pid=1234,fd=8))";
    let (code, out, _e) = run_sourced(
        &[],
        &format!("printf '%s\\n' \"{ss}\" | strih_bkshading_listener_owner"),
    );
    assert_eq!(code, 0, "the owner extraction must not abort (report-only)");
    assert_eq!(
        out.trim(),
        "bkshading",
        "must extract the bare owner name with NO trailing quote: {out:?}"
    );
    // A listener with no `users:((...))` field (or a non-matching line) -> empty, exit 0.
    let (c2, out2, _e) = run_sourced(
        &[],
        "printf '%s\\n' 'LISTEN 0 128 1.2.3.4:22 *:*' | strih_bkshading_listener_owner",
    );
    assert_eq!(c2, 0);
    assert!(
        out2.trim().is_empty(),
        "a non-matching listener line must yield an empty owner: {out2:?}"
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
    // the USB NIC's driver is r8152 (RTL815x USB-ethernet); add sibling NICs with DIFFERENT drivers
    // (onboard enp7s0/r8169, wifi wlp0s20f3/iwlwifi) + a bare `lo`, so a driver scan must discriminate.
    let r8152 = root.join("sys/bus/usb/drivers/r8152");
    std::fs::create_dir_all(&r8152).unwrap();
    symlink(&r8152, devdir.join("driver")).unwrap();
    for (nic, drv, bus) in [("enp7s0", "r8169", "pci"), ("wlp0s20f3", "iwlwifi", "pci")] {
        let sib = root.join("sys/devices").join(format!("{nic}-dev"));
        let drvdir = root.join("sys/bus").join(bus).join("drivers").join(drv);
        std::fs::create_dir_all(&sib).unwrap();
        std::fs::create_dir_all(&drvdir).unwrap();
        let nd = root.join("sys/class/net").join(nic);
        std::fs::create_dir_all(&nd).unwrap();
        symlink(&sib, nd.join("device")).unwrap();
        symlink(&drvdir, sib.join("driver")).unwrap();
    }
    std::fs::create_dir_all(root.join("sys/class/net/lo")).unwrap();
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
fn nic_iface_by_driver_resolves_r8152_and_fails_loud_on_multiple() {
    // The USB NIC is r8152; the onboard NIC is r8169; wifi is iwlwifi. Driver-first resolution must
    // pick ONLY the r8152 iface, fall back (rc 1, empty) when none matches, and fail loud
    // (rc 2, MULTI:) when more than one r8152 exists.
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let (c1, o1, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_iface_by_driver \"$FX/sys\" r8152",
    );
    assert_eq!(
        (c1, o1.trim()),
        (0, "enx6c1ff766154b"),
        "r8152 -> the USB NIC"
    );
    let (c2, o2, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_iface_by_driver \"$FX/sys\" r8169",
    );
    assert_eq!((c2, o2.trim()), (0, "enp7s0"), "r8169 -> the onboard NIC");
    let (c3, o3, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_iface_by_driver \"$FX/sys\" no_such_driver",
    );
    assert_ne!(c3, 0, "no match must return non-zero (caller falls back)");
    assert!(o3.trim().is_empty(), "no match prints nothing");
    // add a SECOND r8152 iface -> MULTI: + rc 2 (fail loud).
    let dev2 = dir
        .path()
        .join("sys/devices/pci0000:00/0000:00:14.0/usb2/2-3/2-3:1.0");
    std::fs::create_dir_all(&dev2).unwrap();
    let nd2 = dir.path().join("sys/class/net/enx000000000002");
    std::fs::create_dir_all(&nd2).unwrap();
    std::os::unix::fs::symlink(&dev2, nd2.join("device")).unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("sys/bus/usb/drivers/r8152"),
        dev2.join("driver"),
    )
    .unwrap();
    let (c4, o4, _e) = run_sourced(
        &[("FX", root.as_str())],
        "strih_nic_iface_by_driver \"$FX/sys\" r8152",
    );
    assert_eq!(c4, 2, "two r8152 NICs must fail loud (rc 2)");
    assert!(
        o4.starts_with("MULTI:")
            && o4.contains("enx6c1ff766154b")
            && o4.contains("enx000000000002"),
        "MULTI must name every colliding iface; got {o4}"
    );
}

#[test]
fn emitted_irq_script_resolves_iface_by_driver_without_the_address() {
    // With NO STRIH_NIC_IFACE and NO matching address in the fixture, the boot script must STILL
    // resolve the iface via the r8152 driver (the whole point of the boot-safe path) and pin cpu 15.
    let dir = irq_fixture("enx6c1ff766154b");
    let root = dir.path().to_str().unwrap().to_string();
    let body = "strih_nic_irq_affinity_script_text > \"$FX/affinity.sh\"; \
                SYS_ROOT=\"$FX/sys\" PROC_INTERRUPTS=\"$FX/interrupts\" CPU_ATOM_FILE=\"$FX/cpu_atom\" \
                IRQ_DIR=\"$FX/irq\" bash \"$FX/affinity.sh\"";
    let (code, out, err) = run_sourced(&[("FX", root.as_str())], body);
    assert_eq!(
        code, 0,
        "boot script must resolve by driver with no address; stderr={err} stdout={out}"
    );
    assert!(
        out.contains("iface enx6c1ff766154b"),
        "must log the driver-resolved USB NIC iface; got {out}"
    );
    let got = std::fs::read_to_string(dir.path().join("irq/125/smp_affinity_list")).unwrap();
    assert_eq!(
        got.trim(),
        "15",
        "driver-resolved run must pin cpu 15; wrote {got}"
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
        unit.contains("After=network-online.target")
            && unit.contains("Wants=network-online.target"),
        "must order + want network-online.target (the NIC is up when it fires)"
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
    assert!(
        v.contains("strih_nic_iface_by_driver") && v.contains("r8152"),
        "verify must resolve the NIC iface driver-first (r8152), same as the boot script"
    );
}

/// The bkshading-service item's health probe must hit a route the SERVICE actually serves. Live
/// 23.9.2026: the item curled `:8770/api/state` (the relay's / intercom hub's endpoint), the service
/// answers 404 there, so a healthy running service (listener owned by `bkshading`, panel 200,
/// `/api/version` 200) graded FAIL "silent". Parse the routes from the service's own router and
/// require the probed path to be one of them.
#[test]
fn verify_strih_bkshading_probe_hits_a_real_service_route() {
    let v = read_script("scripts/verify-strih.sh");
    let start = v
        .find("BKSH_PORT=\"${BKSHADING_SERVICE_PORT:-8770}\"")
        .expect("the bkshading-service item's port line");
    let end = start
        + v[start..]
            .find("(bkshading-service) installed (enabled=")
            .expect("the item's report-only branch");
    let item = &v[start..end];
    let marker = ":${BKSH_PORT}/";
    let at = item
        .find(marker)
        .expect("the item must curl the service on :${BKSH_PORT}");
    let path: String = item[at + marker.len() - 1..]
        .chars()
        .take_while(|c| *c != '"' && !c.is_whitespace())
        .collect();
    let router = read_script("bkshading/service/src/http.rs");
    let routes: Vec<String> = router
        .split(".route(\"")
        .skip(1)
        .filter_map(|r| r.split('"').next().map(str::to_string))
        .collect();
    assert!(
        routes.iter().any(|r| r == &path),
        "verify-strih probes the service at `{path}`, which is not a route the service serves \
         (routes: {routes:?})"
    );
}

// ---- issue 1317 remainder: the imag-parity genlock rtprio grant + no crash popups ----------------
//
// Owner report 23.9.2026: a crash popup (apport / update-notifier-crash) the operator had to close
// in the morning, and every strih-lx OBS session logging `genlock: could NOT set render-tick thread
// SCHED_FIFO prio 10 (errno 1 — missing rtprio ulimit grant?)`. imag provisions both (setup-imag.sh:
// the issue-484 limits.d grant, the apport/whoopsie mask, systemd-coredump); strih-lx now does too.

/// The render-tick SCHED_FIFO priority the vendored OBS requests (obs-video.c
/// `#define GENLOCK_RT_PRIORITY`). A grant below it would still EPERM.
fn vendored_genlock_rt_priority() -> u32 {
    let src = read_script("vendor/obs-studio/libobs/obs-video.c");
    let line = src
        .lines()
        .find(|l| l.trim_start().starts_with("#define GENLOCK_RT_PRIORITY"))
        .expect("obs-video.c must define GENLOCK_RT_PRIORITY");
    line.split_whitespace()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .expect("GENLOCK_RT_PRIORITY value")
}

/// The rtprio value imag grants (setup-imag.sh `${DESKTOP_USER}   -   rtprio   N`).
fn imag_rtprio_value() -> u32 {
    let s = read_script("scripts/setup-imag.sh");
    let line = s
        .lines()
        .find(|l| l.starts_with("${DESKTOP_USER}") && l.contains("rtprio"))
        .expect("setup-imag.sh must carry the issue-484 rtprio grant line");
    line.split_whitespace()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .expect("imag rtprio value")
}

#[test]
fn rtprio_limits_text_grants_the_desktop_user_the_imag_rtprio_value() {
    let (code, out, err) = run_sourced(&[], "strih_rtprio_limits_text alice");
    assert_eq!(code, 0, "stderr={err}");
    let want = format!("alice   -   rtprio   {}", imag_rtprio_value());
    assert!(
        out.lines().any(|l| l == want),
        "the limits.d body must carry `{want}` (imag parity), got:\n{out}"
    );
    for l in out.lines().filter(|l| !l.trim().is_empty() && *l != want) {
        assert!(
            l.starts_with('#'),
            "every other line must be a comment, got: {l}"
        );
    }
    assert!(
        imag_rtprio_value() >= vendored_genlock_rt_priority(),
        "the granted rtprio must cover the vendored render-tick priority"
    );
}

#[test]
fn rtprio_limits_text_refuses_an_empty_user() {
    let (code, out, _e) = run_sourced(&[], "strih_rtprio_limits_text ''");
    assert_ne!(
        code, 0,
        "an empty user must be refused (never a grant for nobody)"
    );
    assert!(out.trim().is_empty(), "no body on refusal, got: {out}");
}

#[test]
fn rtprio_limits_path_is_the_strih_limits_d_file_with_an_env_seam() {
    let (_c, out, _e) = run_sourced(&[], "strih_rtprio_limits_path");
    assert_eq!(out, "/etc/security/limits.d/95-strih-genlock-rtprio.conf");
    let (_c, out, _e) = run_sourced(
        &[("STRIH_RTPRIO_LIMITS_FILE", "/tmp/x/95.conf")],
        "strih_rtprio_limits_path",
    );
    assert_eq!(out, "/tmp/x/95.conf");
}

#[test]
fn rtprio_grant_ok_requires_the_user_line_at_or_above_the_vendored_priority() {
    let prio = vendored_genlock_rt_priority();
    // Round trip: the rendered body satisfies the grader for the same user only.
    let (c, _o, e) = run_sourced(
        &[],
        "strih_rtprio_limits_text alice | strih_rtprio_grant_ok alice",
    );
    assert_eq!(c, 0, "own body must grade as a grant; stderr={e}");
    let (c, _o, _e) = run_sourced(
        &[],
        "strih_rtprio_limits_text alice | strih_rtprio_grant_ok bob",
    );
    assert_ne!(c, 0, "a grant for another user is not a grant");
    let cases = [
        (format!("alice - rtprio {prio}"), true),
        (format!("alice\t-\trtprio\t{}", prio + 10), true),
        // an explicit soft+hard pair is the same grant as `-`
        (
            format!("alice soft rtprio {prio}\nalice hard rtprio {prio}"),
            true,
        ),
        ("alice - rtprio unlimited".to_string(), true),
        // a hard-only grant leaves the SOFT limit at 0 -> sched_setscheduler still EPERMs
        (format!("alice hard rtprio {prio}"), false),
        (format!("alice soft rtprio {prio}"), false),
        (format!("alice - rtprio {}", prio - 1), false),
        (format!("# alice - rtprio {prio}"), false),
        (format!("alice - nice {prio}"), false),
        (String::new(), false),
    ];
    for (body, want) in cases {
        let (c, _o, _e) = run_sourced(
            &[("BODY", &body)],
            "printf '%s\\n' \"$BODY\" | strih_rtprio_grant_ok alice",
        );
        assert_eq!(c == 0, want, "grant_ok on `{body}` must be {want}");
    }
}

const FIFO_FAIL_LINE: &str = "info: genlock: could NOT set render-tick thread SCHED_FIFO prio 10 \
     (errno 1 — missing rtprio ulimit grant?) — continuing SCHED_OTHER (#484)";
const FIFO_OK_LINE: &str =
    "info: genlock: render-tick thread set SCHED_FIFO prio 10 on the isolated core (#484)";

/// Feed `log` from a FILE (a >128 KB env var would hit the kernel's per-argument E2BIG limit).
fn session_verdict(grant: &str, running: &str, log: &str) -> (i32, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("obs.txt");
    std::fs::write(&path, log).unwrap();
    let (c, o, _e) = run_sourced(
        &[("LOGFILE", path.to_str().unwrap())],
        &format!("strih_rtprio_session_verdict {grant} {running} < \"$LOGFILE\""),
    );
    (c, o)
}

#[test]
fn rtprio_session_verdict_grades_grant_and_the_live_obs_log() {
    assert_eq!(
        session_verdict("0", "1", FIFO_OK_LINE),
        (1, "no-grant".into())
    );
    assert_eq!(session_verdict("0", "0", ""), (1, "no-grant".into()));
    // grant present, running OBS still logs the EPERM line -> its lingering user manager predates
    // the grant; it applies at the next reboot.
    assert_eq!(
        session_verdict("1", "1", FIFO_FAIL_LINE),
        (2, "grant-pending-reboot".into())
    );
    assert_eq!(
        session_verdict("1", "1", FIFO_OK_LINE),
        (0, "ok-sched-fifo".into())
    );
    assert_eq!(
        session_verdict("1", "1", "no genlock line"),
        (0, "ok".into())
    );
    // OBS not running: the newest log is a PAST session -- never graded.
    assert_eq!(session_verdict("1", "0", FIFO_FAIL_LINE), (0, "ok".into()));
}

#[test]
fn rtprio_session_verdict_survives_a_large_log_under_pipefail() {
    // The drift-guard-log-parsers SIGPIPE class: a real OBS log is 100s of KB and the matching line
    // is EARLY -- a `printf | grep -q` shape misgrades under pipefail. >64 KB fixture.
    let mut log = String::from(FIFO_FAIL_LINE);
    log.push('\n');
    for i in 0..4000 {
        log.push_str(&format!(
            "info: filler line {i} ..........................................\n"
        ));
    }
    assert!(log.len() > 64 * 1024);
    assert_eq!(
        session_verdict("1", "1", &log),
        (2, "grant-pending-reboot".into())
    );
}

#[test]
fn crash_popup_units_cover_imags_mask_plus_the_apport_coredump_hook() {
    let (c, out, _e) = run_sourced(&[], "strih_crash_popup_units");
    assert_eq!(c, 0);
    let units: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    // Superset of imag's mask (setup-imag.sh `systemctl mask apport.service whoopsie.service`).
    let imag = read_script("scripts/setup-imag.sh");
    let imag_line = imag
        .lines()
        .find(|l| l.trim_start().starts_with("systemctl mask apport.service"))
        .expect("setup-imag.sh apport/whoopsie mask line");
    for u in imag_line
        .split_whitespace()
        .filter(|w| w.ends_with(".service"))
    {
        assert!(
            units.contains(&u),
            "imag masks {u}; strih must too: {units:?}"
        );
    }
    // 26.04: apport's systemd-coredump OnSuccess hook writes /var/crash (-> the popup) even with
    // apport.service masked; masking the TEMPLATE blocks every instance.
    assert!(
        units.contains(&"apport-coredump-hook@.service"),
        "the apport coredump hook template must be masked too: {units:?}"
    );
}

#[test]
fn crash_popup_member_ok_grades_a_template_by_is_enabled_only() {
    let hook = "apport-coredump-hook@.service";
    let cases = [
        (hook, "masked", "", true),
        (hook, "masked-runtime", "", true),
        (hook, "not-found", "inactive", true),
        (hook, "", "", true),                // apport not installed at all
        (hook, "static", "inactive", false), // the 26.04 default: pulled by OnSuccess
        (hook, "enabled", "", false),
        (hook, "disabled", "", false), // disable does not stop an OnSuccess= pull
        ("apport.service", "masked", "inactive", true),
        ("apport.service", "masked", "active", false),
        ("whoopsie.service", "enabled", "inactive", false),
    ];
    for (unit, en, act, want) in cases {
        let (c, _o, _e) = run_sourced(
            &[("U", unit), ("EN", en), ("ACT", act)],
            "strih_crash_popup_member_ok \"$U\" \"$EN\" \"$ACT\"",
        );
        assert_eq!(
            c == 0,
            want,
            "member_ok({unit}, enabled={en:?}, active={act:?}) must be {want}"
        );
    }
}

#[test]
fn crash_reports_count_counts_only_crash_files_and_tolerates_a_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    for f in ["_usr_bin_obs.1000.crash", "_usr_bin_x.0.crash", "notes.txt"] {
        std::fs::write(dir.path().join(f), "x").unwrap();
    }
    std::fs::create_dir(dir.path().join("sub.crash")).unwrap(); // a DIR is not a report
    let (c, out, _e) = run_sourced(
        &[("D", dir.path().to_str().unwrap())],
        "strih_crash_reports_count \"$D\"",
    );
    assert_eq!((c, out.as_str()), (0, "2"));
    let (c, out, _e) = run_sourced(&[], "strih_crash_reports_count /nonexistent/crash-dir");
    assert_eq!((c, out.as_str()), (0, "0"));
}

#[test]
fn rtprio_wording_names_the_reboot_not_a_relogin() {
    // strih-obs.service inherits its limits from the LINGERING user@UID manager (setup-strih
    // step 13 enables linger), which applies pam_limits only when it starts -- i.e. at boot. A
    // "next login" claim sends the supervisor down a false diagnosis.
    let (_c, body, _e) = run_sourced(&[], "strih_rtprio_limits_text alice");
    let setup = setup_11c_block();
    let v = read_script("scripts/verify-strih.sh");
    let item32 = block_between(&v, "# 32)", "# 33)");
    for (what, text) in [
        ("limits body", body.as_str()),
        ("setup 11c", setup.as_str()),
        ("verify 32", item32),
    ] {
        assert!(
            !text.contains("next login"),
            "{what} must not claim the grant applies at the next login"
        );
        assert!(text.contains("reboot"), "{what} must name the reboot");
    }
}

#[test]
fn crash_popup_unit_ok_passes_only_masked_or_absent_and_not_running() {
    let cases = [
        ("masked", "inactive", true),
        ("masked\nmasked", "inactive", true), // the is-enabled || echo double-append shape
        ("masked-runtime", "inactive", true),
        ("disabled", "inactive", true),
        ("not-found", "inactive", true),
        ("", "inactive", true), // unit file absent (older systemd prints nothing)
        ("masked", "failed", true),
        ("enabled", "inactive", false),
        ("static", "inactive", false),
        ("masked", "active", false), // masked but still running (oneshot RemainAfterExit)
        ("disabled", "activating", false),
        ("masked", "", false), // unreadable active state -> fail closed
        ("weird", "inactive", false),
    ];
    for (en, act, want) in cases {
        let (c, _o, _e) = run_sourced(
            &[("EN", en), ("ACT", act)],
            "strih_crash_popup_unit_ok \"$EN\" \"$ACT\"",
        );
        assert_eq!(
            c == 0,
            want,
            "unit_ok(enabled={en:?}, active={act:?}) must be {want}"
        );
    }
}

#[test]
fn crash_popup_verdict_orders_units_then_coredump() {
    let v = |bad: &str, core: &str| {
        let (c, o, _e) = run_sourced(
            &[("BAD", bad), ("CORE", core)],
            "strih_crash_popup_verdict \"$BAD\" \"$CORE\"",
        );
        (c, o)
    };
    assert_eq!(v("", "1"), (0, "ok".into()));
    assert_eq!(
        v("apport.service", "1"),
        (1, "units-live: apport.service".into())
    );
    assert_eq!(
        v("apport.service whoopsie.service", "0"),
        (1, "units-live: apport.service whoopsie.service".into())
    );
    assert_eq!(v("", "0"), (1, "no-systemd-coredump".into()));
    assert_eq!(v("", ""), (1, "no-systemd-coredump".into()));
}

#[test]
fn setup_strih_step_11c_is_a_lettered_substep_between_11b_and_12() {
    let s = read_script("scripts/setup-strih.sh");
    assert!(
        s.contains("TOTAL_STEPS=17"),
        "a lettered sub-step keeps TOTAL_STEPS at 17"
    );
    let b = s.find("step \"11b\"").expect("step 11b");
    let c = s
        .find("step \"11c\"")
        .expect("setup-strih must carry step \"11c\"");
    let twelve = s.find("step 12 ").expect("step 12");
    assert!(b < c && c < twelve, "step 11c must sit between 11b and 12");
}

/// Extract `[start, end)` from `text` by literal anchors (end searched after start).
fn block_between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let s = text
        .find(start)
        .unwrap_or_else(|| panic!("anchor `{start}` not found"));
    let e = s + text[s..]
        .find(end)
        .unwrap_or_else(|| panic!("end anchor `{end}` not found after `{start}`"));
    &text[s..e]
}

/// Write an executable fake tool into `bin`.
fn fake_tool(bin: &std::path::Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let p = bin.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Run an extracted script block under the callers' real `set -euo pipefail` with the lib
/// sourced, a fake-tool dir first on PATH, and the caller's prelude. Returns (exit, out, err).
fn run_block(
    bin: &std::path::Path,
    env: &[(&str, &str)],
    prelude: &str,
    block: &str,
) -> (i32, String, String) {
    let harness = format!("set -euo pipefail\n. \"$SCRIPT\"\n{prelude}\n{block}\n");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib())
        .env("PATH", path);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run block");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const SETUP_PRELUDE: &str = "step() { echo \"STEP $1\"; }\nwarn() { echo \"WARN $1\"; }\n\
     fail() { echo \"FAIL: $1\" >&2; exit 1; }\nDESKTOP_USER=alice";

fn setup_11c_block() -> String {
    let s = read_script("scripts/setup-strih.sh");
    block_between(&s, "step \"11c\"", "\n# -----------").to_string()
}

#[test]
fn setup_strih_step_11c_writes_grant_masks_units_and_installs_coredump() {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = dir.path().join("calls.log");
    fake_tool(&bin, "systemctl", "echo \"systemctl $*\" >> \"$CALLS\"");
    fake_tool(&bin, "apt-get", "echo \"apt-get $*\" >> \"$CALLS\"");
    let limits = dir.path().join("limits.d/95-strih-genlock-rtprio.conf");
    let (code, out, err) = run_block(
        &bin,
        &[
            ("CALLS", calls.to_str().unwrap()),
            ("STRIH_RTPRIO_LIMITS_FILE", limits.to_str().unwrap()),
        ],
        SETUP_PRELUDE,
        &setup_11c_block(),
    );
    assert_eq!(code, 0, "step 11c must succeed; stdout={out} stderr={err}");
    let body = std::fs::read_to_string(&limits).expect("the limits.d grant must be written");
    let want = format!("alice   -   rtprio   {}", imag_rtprio_value());
    assert!(body.lines().any(|l| l == want), "grant body: {body}");
    let log = std::fs::read_to_string(&calls).unwrap();
    for u in ["apport.service", "whoopsie.service"] {
        assert!(
            log.contains(&format!("systemctl disable --now {u}")),
            "{u} must be disabled+stopped; calls:\n{log}"
        );
    }
    for u in [
        "apport.service",
        "apport-coredump-hook@.service",
        "whoopsie.service",
    ] {
        assert!(
            log.contains(&format!("systemctl mask {u}")),
            "{u} must be masked; calls:\n{log}"
        );
    }
    assert!(
        !out.contains("next login"),
        "the grant applies at the next reboot (lingering user manager), not a login: {out}"
    );
    assert!(
        log.lines()
            .any(|l| l.starts_with("apt-get install") && l.contains("systemd-coredump")),
        "systemd-coredump must be installed; calls:\n{log}"
    );
}

#[test]
fn setup_strih_step_11c_fails_loud_when_coredump_install_or_mask_fails() {
    for (tool, body) in [
        ("apt-get", "exit 100"),
        ("systemctl", "[ \"$1\" = mask ] && exit 1; exit 0"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        fake_tool(&bin, "systemctl", "exit 0");
        fake_tool(&bin, "apt-get", "exit 0");
        fake_tool(&bin, tool, body);
        let limits = dir.path().join("limits.d/95.conf");
        let (code, _out, err) = run_block(
            &bin,
            &[("STRIH_RTPRIO_LIMITS_FILE", limits.to_str().unwrap())],
            SETUP_PRELUDE,
            &setup_11c_block(),
        );
        assert_ne!(code, 0, "a failing {tool} must fail step 11c loudly");
        assert!(
            err.contains("FAIL:"),
            "the failure must go through fail(); stderr={err}"
        );
    }
}

const VERIFY_PRELUDE: &str = "FAILS=0\nok() { echo \"PASS $1\"; }\n\
     bad() { echo \"FAIL $1\"; FAILS=$((FAILS+1)); }\nnote() { echo \"NOTE $1\"; }\n\
     newest_log() { ls -1t \"${OBS_LOG_DIR}\"/*.txt 2>/dev/null | head -1; }\nSTRIH_LX_USER=alice";

/// Run verify-strih items 32 + 33 against fake live state. `units` rows are
/// `(unit, is-enabled output, is-active output)`; `obs_running` drives the supervisor check;
/// `crash_reports` stale `*.crash` files are placed in the (seamed) crash dir.
fn run_verify_items(
    grant: Option<&str>,
    obs_running: bool,
    log: Option<&str>,
    units: &[(&str, &str, &str)],
    coredump_status: &str,
    crash_reports: usize,
) -> (i32, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let state = dir.path().join("units.state");
    let rows: String = units
        .iter()
        .map(|(u, en, act)| format!("{u}|{en}|{act}\n"))
        .collect();
    std::fs::write(&state, rows).unwrap();
    fake_tool(
        &bin,
        "systemctl",
        r#"if [ "$1" = --user ]; then [ "$FAKE_OBS" = 1 ]; exit $?; fi
row="$(grep -F -- "$2|" "$FAKE_STATE" | head -1 || true)"
if [ -z "$row" ]; then [ "$1" = is-active ] && echo inactive; exit 3; fi
if [ "$1" = is-enabled ]; then out="$(echo "$row" | cut -d'|' -f2)"; else out="$(echo "$row" | cut -d'|' -f3)"; fi
[ -n "$out" ] && echo "$out"
case "$out" in enabled|active|static) exit 0;; *) exit 1;; esac"#,
    );
    fake_tool(&bin, "pgrep", "exit 1");
    fake_tool(
        &bin,
        "dpkg-query",
        r#"[ -n "$FAKE_CORE" ] || exit 1; printf '%s' "$FAKE_CORE""#,
    );
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    if let Some(text) = log {
        std::fs::write(logs.join("2026-09-23 08-00-00.txt"), text).unwrap();
    }
    let limits = dir.path().join("95-strih-genlock-rtprio.conf");
    if let Some(g) = grant {
        std::fs::write(&limits, g).unwrap();
    }
    let crash_dir = dir.path().join("crash");
    std::fs::create_dir_all(&crash_dir).unwrap();
    for i in 0..crash_reports {
        std::fs::write(crash_dir.join(format!("_usr_bin_obs.{i}.crash")), "x").unwrap();
    }
    let v = read_script("scripts/verify-strih.sh");
    let block = block_between(&v, "# 32)", "\necho \"\"\nif [ \"$FAILS\" -eq 0 ]");
    // the REAL shared OBS-running predicate verify-strih defines at its top (items 1 and 32)
    let obs_running_fn = block_between(&v, "obs_running() {", "\n}\n");
    let prelude = format!("{VERIFY_PRELUDE}\n{obs_running_fn}\n}}");
    run_block(
        &bin,
        &[
            ("FAKE_STATE", state.to_str().unwrap()),
            ("FAKE_OBS", if obs_running { "1" } else { "0" }),
            ("FAKE_CORE", coredump_status),
            ("OBS_LOG_DIR", logs.to_str().unwrap()),
            ("STRIH_RTPRIO_LIMITS_FILE", limits.to_str().unwrap()),
            ("STRIH_CRASH_DIR", crash_dir.to_str().unwrap()),
        ],
        &prelude,
        &format!("{block}\necho \"FAILS=$FAILS\""),
    )
}

const MASKED_UNITS: [(&str, &str, &str); 3] = [
    ("apport.service", "masked", "inactive"),
    ("apport-coredump-hook@.service", "masked", ""),
    ("whoopsie.service", "masked", "inactive"),
];
const CORE_OK: &str = "install ok installed";

#[test]
fn verify_strih_rtprio_and_crash_popup_items_pass_on_a_provisioned_box() {
    let grant = "# c\nalice   -   rtprio   20\n";
    let (c, out, err) = run_verify_items(
        Some(grant),
        true,
        Some(FIFO_OK_LINE),
        &MASKED_UNITS,
        CORE_OK,
        0,
    );
    assert_eq!(
        c, 0,
        "the items must not abort the gate; stderr={err}\n{out}"
    );
    assert!(out.contains("FAILS=0"), "{out}");
    assert!(out.contains("PASS (rtprio)"), "{out}");
    assert!(out.contains("PASS (crash-popup)"), "{out}");
    assert!(!out.contains("NOTE"), "a clean box prints no NOTE: {out}");
    // No OBS log at all + OBS down: still a pass on the grant (never aborts on a missing log).
    let (c, out, err) = run_verify_items(Some(grant), false, None, &MASKED_UNITS, CORE_OK, 0);
    assert_eq!(c, 0, "stderr={err}\n{out}");
    assert!(
        out.contains("FAILS=0") && out.contains("PASS (rtprio)"),
        "{out}"
    );
}

#[test]
fn verify_strih_rtprio_item_fails_without_grant_and_notes_a_pending_reboot() {
    let (_c, out, _e) =
        run_verify_items(None, true, Some(FIFO_FAIL_LINE), &MASKED_UNITS, CORE_OK, 0);
    assert!(
        out.contains("FAIL (rtprio)") && out.contains("FAILS=1"),
        "{out}"
    );
    let grant = "alice - rtprio 20\n";
    let (_c, out, _e) = run_verify_items(
        Some(grant),
        true,
        Some(FIFO_FAIL_LINE),
        &MASKED_UNITS,
        CORE_OK,
        0,
    );
    assert!(
        out.contains("NOTE (rtprio)") && out.contains("reboot") && out.contains("FAILS=0"),
        "{out}"
    );
}

#[test]
fn verify_strih_crash_popup_item_fails_on_a_live_unit_or_missing_coredump() {
    let grant = "alice - rtprio 20\n";
    let live = [
        ("apport.service", "enabled", "active"),
        ("apport-coredump-hook@.service", "masked", ""),
        ("whoopsie.service", "masked", "inactive"),
    ];
    let (_c, out, _e) = run_verify_items(Some(grant), false, None, &live, CORE_OK, 0);
    assert!(
        out.contains("FAIL (crash-popup)")
            && out.contains("apport.service")
            && out.contains("FAILS=1"),
        "{out}"
    );
    // The 26.04 default: apport.service + whoopsie masked, but the coredump hook template is still
    // `static` (pulled by systemd-coredump's OnSuccess=) -> the popup returns -> FAIL.
    let hook_live = [
        ("apport.service", "masked", "inactive"),
        ("apport-coredump-hook@.service", "static", ""),
        ("whoopsie.service", "masked", "inactive"),
    ];
    let (_c, out, _e) = run_verify_items(Some(grant), false, None, &hook_live, CORE_OK, 0);
    assert!(
        out.contains("FAIL (crash-popup)")
            && out.contains("apport-coredump-hook@.service")
            && out.contains("FAILS=1"),
        "{out}"
    );
    let (_c, out, _e) = run_verify_items(Some(grant), false, None, &MASKED_UNITS, "", 0);
    assert!(
        out.contains("FAIL (crash-popup)") && out.contains("no-systemd-coredump"),
        "{out}"
    );
    // Units absent entirely (a box without apport/whoopsie): not-found passes.
    let absent: [(&str, &str, &str); 0] = [];
    let (_c, out, _e) = run_verify_items(Some(grant), false, None, &absent, CORE_OK, 0);
    assert!(
        out.contains("PASS (crash-popup)") && out.contains("FAILS=0"),
        "{out}"
    );
}

#[test]
fn verify_strih_crash_popup_item_notes_stale_crash_reports_without_failing() {
    // update-notifier re-raises the popup at login for reports ALREADY in /var/crash; deleting
    // them is a supervisor data action, so provisioning REPORTS them (NOTE) and never fails on it.
    let grant = "alice - rtprio 20\n";
    let (c, out, err) = run_verify_items(Some(grant), false, None, &MASKED_UNITS, CORE_OK, 2);
    assert_eq!(c, 0, "stderr={err}\n{out}");
    assert!(
        out.contains("PASS (crash-popup)")
            && out.contains("NOTE (crash-popup) 2 stale crash report")
            && out.contains("FAILS=0"),
        "{out}"
    );
}
