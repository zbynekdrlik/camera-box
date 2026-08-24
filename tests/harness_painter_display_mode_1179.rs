//! #1179 — the painter-side `--display-mode WxH@RR` override is merged on frame-probe
//! (`src/bin/frame-probe.rs` + `src/painter_mode.rs`, the 2560x1080@100 experiment from issue 881),
//! but the E2E harness (`scripts/recording-e2e.sh`) launches every painter it manages with a
//! hard-coded `nohup /tmp/frame-probe --paint-only …` invocation that carries NO `--display-mode`,
//! so `PAINTER_DISPLAY_MODE=… bash scripts/recording-e2e.sh …` cannot run a full sweep at the
//! override. This wires an OPT-IN passthrough: when `PAINTER_DISPLAY_MODE` is set (and valid), every
//! painter recording-e2e.sh launches gets `--display-mode <mode>`; when it is unset the launch is
//! BYTE-IDENTICAL to today (no flag ⇒ frame-probe's CLI defaults).
//!
//! Three layers locked here (all Tier-0 — no rig, no ssh, no cargo-run of the appliance; the launch
//! is exercised via a fake `frame-probe` stand-in on PATH):
//!  1. the pure lib `scripts/lib/painter-display-mode.sh` — `painter_display_mode_args` resolves the
//!     mode from arg/`$PAINTER_DISPLAY_MODE`, prints nothing (rc 0) when unset/empty, validates the
//!     `WxH@RR` shape (also blocking shell-metacharacter injection) and prints `--display-mode
//!     <mode>` when set, and FAILS LOUD (stderr + rc 1) on a malformed value;
//!  2. the launch EMBEDDING — reproducing recording-e2e.sh's variable-embedding shape, the fake
//!     `frame-probe`'s argv carries no `--display-mode` when the env is unset (the byte-identity
//!     guarantee) and exactly `--display-mode 2560x1080@100` when it is set;
//!  3. recording-e2e.sh actually WIRES it — sources the lib and embeds the computed flag at BOTH
//!     painter launch sites (the `[3/8]` measurement painter and the `AV_RESTART_GATE` painter),
//!     via a static read of the shell script (the same model as tests/harness_cbox_burn_log_persist.rs).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    manifest_dir().join("scripts/lib/painter-display-mode.sh")
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the lib and run `snippet` (with `env` prepended verbatim). Uses `set -uo pipefail`
/// (never `-e`) so a helper's own non-zero return doesn't abort the harness — the caller inspects
/// the returned exit-ok / stdout / stderr directly.
fn run(env: &str, snippet: &str) -> (bool, String, String) {
    let script = format!(
        "set -uo pipefail\n{env}\n. \"{}\"\n{snippet}",
        lib_script().display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

// --- layer 1: the pure lib ---

#[test]
fn unset_prints_nothing_and_succeeds() {
    let (ok, out, _err) = run(
        "unset PAINTER_DISPLAY_MODE",
        "painter_display_mode_args; echo \"[$?]\"",
    );
    assert!(
        ok,
        "#1179: an unset override must succeed (byte-identical path)"
    );
    assert_eq!(
        out, "[0]",
        "#1179: unset PAINTER_DISPLAY_MODE ⇒ ZERO bytes of flag args + rc 0, so the launch is \
         byte-identical to today: got {out:?}"
    );
}

#[test]
fn empty_string_prints_nothing() {
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE=''",
        "painter_display_mode_args; echo \"[$?]\"",
    );
    assert!(ok);
    assert_eq!(
        out, "[0]",
        "#1179: a set-but-empty override is treated as unset (empty, rc 0): got {out:?}"
    );
}

#[test]
fn set_integer_refresh_prints_the_flag() {
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE=2560x1080@100",
        "painter_display_mode_args",
    );
    assert!(ok, "#1179: a valid override must succeed");
    assert_eq!(
        out, "--display-mode 2560x1080@100",
        "#1179: a set override ⇒ the frame-probe `--display-mode <mode>` flag args: got {out:?}"
    );
}

#[test]
fn set_fractional_refresh_prints_the_flag() {
    // parse_display_mode accepts a fractional RR (1920x1080@59.94); the passthrough must too.
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE=1920x1080@59.94",
        "painter_display_mode_args",
    );
    assert!(ok);
    assert_eq!(out, "--display-mode 1920x1080@59.94");
}

#[test]
fn outer_whitespace_is_trimmed() {
    // parse_display_mode trims each component; the passthrough trims OUTER whitespace too, so a
    // trailing/leading space accident works instead of aborting the whole E2E run.
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE=' 2560x1080@100 '",
        "painter_display_mode_args",
    );
    assert!(
        ok,
        "#1179: a space-padded but well-shaped value must be trimmed + accepted, not rejected"
    );
    assert_eq!(out, "--display-mode 2560x1080@100");
}

#[test]
fn explicit_arg_overrides_env() {
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE=2560x1080@100",
        "painter_display_mode_args 3840x2160@60",
    );
    assert!(ok);
    assert_eq!(
        out, "--display-mode 3840x2160@60",
        "#1179: an explicit arg takes precedence over the env default"
    );
}

#[test]
fn malformed_value_fails_loud() {
    let (ok, _out, err) = run(
        "export PAINTER_DISPLAY_MODE=notamode",
        "painter_display_mode_args",
    );
    assert!(
        !ok,
        "#1179: a malformed override must FAIL (rc != 0), never silently pass"
    );
    assert!(
        err.contains("ERROR") && err.contains("PAINTER_DISPLAY_MODE"),
        "#1179: a malformed override must name the bad value on stderr: {err:?}"
    );
}

#[test]
fn injection_attempt_is_rejected() {
    // A shell-metacharacter payload must be rejected by the WxH@RR shape check, never emitted into
    // the remote ssh command string.
    let (ok, out, _err) = run(
        "export PAINTER_DISPLAY_MODE='2560x1080@100; rm -rf /'",
        "painter_display_mode_args || true",
    );
    assert!(ok, "|| true swallows the rc so we can inspect stdout");
    assert_eq!(
        out, "",
        "#1179: an injection payload must produce NO flag output (rejected, not passed through): {out:?}"
    );
}

#[test]
fn malformed_value_aborts_the_caller_under_set_e() {
    // The real embedding is `VAR="$(painter_display_mode_args)"` under recording-e2e.sh's own
    // `set -euo pipefail` — a malformed value must abort the run BEFORE any ssh/deploy, not be
    // silently dropped (the Approach-3 hazard the design comment rejected).
    let script = format!(
        "set -euo pipefail\nexport PAINTER_DISPLAY_MODE=bad@value\n. \"{}\"\n\
         flag=\"$(painter_display_mode_args)\"\necho \"REACHED flag=[$flag]\"",
        lib_script().display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert!(
        !out.status.success(),
        "#1179: a malformed value must abort the caller under set -e"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("REACHED"),
        "#1179: the caller must NOT continue past a malformed override"
    );
}

#[test]
fn malformed_value_aborts_the_split_local_form_in_a_function_under_set_e() {
    // The AV_RESTART site uses `local X` then `X="$(painter_display_mode_args)"` INSIDE a function
    // (split decl/assign — deliberately, because a collapsed `local X="$(...)"` MASKS the command
    // substitution's exit code (SC2155) and returns 0, silently dropping a malformed value). This
    // guards that regression: the split form must still abort under set -e.
    let script = format!(
        "set -euo pipefail\n. \"{}\"\n\
         f() {{ local x; x=\"$(PAINTER_DISPLAY_MODE=bad@value painter_display_mode_args)\"; \
         echo \"REACHED x=[$x]\"; }}\nf",
        lib_script().display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    assert!(
        !out.status.success(),
        "#1179: a malformed value must abort even the AV split-local-in-a-function form under set -e"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("REACHED"),
        "#1179: the function must NOT continue past a malformed override"
    );
}

// --- layer 2: the launch embedding (fake frame-probe on PATH) ---

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "painter-display-mode-1179-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Install a fake `frame-probe` on PATH that appends its argv (space-joined) to `$ARGV_LOG`.
fn install_fake_frame_probe(bin_dir: &Path) {
    let script = "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >> \"$ARGV_LOG\"\n";
    let p = bin_dir.join("frame-probe");
    fs::write(&p, script).expect("write fake frame-probe");
    let mut perms = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&p, perms).unwrap();
}

/// Reproduce recording-e2e.sh's variable-embedding shape (compute the flag var, embed it in a
/// `nohup frame-probe --paint-only … $_cam2_marker_flags $_cam2_display_mode_flag …` line) and
/// return the fake frame-probe's captured argv. `env` is prepended verbatim.
fn launch_argv(env: &str) -> String {
    let dir = scratch("launch");
    install_fake_frame_probe(&dir);
    let argv_log = dir.join("argv.log");
    fs::write(&argv_log, "").expect("create argv log");
    let script = format!(
        "set -euo pipefail\nexport PATH=\"{bin}:$PATH\"\nexport ARGV_LOG=\"{log}\"\n{env}\n\
         . \"{lib}\"\n_cam2_marker_flags=\"\"\n_cam2_display_mode_flag=\"$(painter_display_mode_args)\"\n\
         ( nohup frame-probe --paint-only --dual-qr --wall-clock --paint-log /tmp/painter.csv \\\n\
           --paint-fps 60 --qr-size 700 --run-id 12345 --duration-secs 330 \\\n\
           $_cam2_marker_flags \\\n\
           $_cam2_display_mode_flag \\\n\
           >/dev/null 2>&1 & )\nwait 2>/dev/null || true\n\
         _i=0; while [ ! -s \"{log}\" ] && [ $_i -lt 100 ]; do sleep 0.05; _i=$((_i+1)); done\n\
         cat \"{log}\"",
        bin = dir.display(),
        log = argv_log.display(),
        lib = lib_script().display(),
        env = env,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run launch harness");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn launch_argv_unset_carries_no_display_mode() {
    let argv = launch_argv("unset PAINTER_DISPLAY_MODE");
    assert!(
        argv.contains("--paint-only --dual-qr"),
        "#1179: sanity — the base painter flags must be present: {argv:?}"
    );
    assert!(
        !argv.contains("--display-mode"),
        "#1179: with PAINTER_DISPLAY_MODE unset the launched frame-probe argv must carry NO \
         --display-mode (byte-identical to today): {argv:?}"
    );
}

#[test]
fn launch_argv_set_carries_exactly_the_flag() {
    let argv = launch_argv("export PAINTER_DISPLAY_MODE=2560x1080@100");
    assert!(
        argv.contains("--display-mode 2560x1080@100"),
        "#1179: with PAINTER_DISPLAY_MODE set the launched frame-probe argv must carry \
         --display-mode 2560x1080@100: {argv:?}"
    );
    assert_eq!(
        argv.matches("--display-mode").count(),
        1,
        "#1179: exactly ONE --display-mode must be threaded through, never duplicated: {argv:?}"
    );
}

// --- layer 3: static wiring guards on recording-e2e.sh ---

#[test]
fn recording_e2e_sources_the_painter_display_mode_lib() {
    let s = recording_e2e_text();
    assert!(
        s.contains("lib/painter-display-mode.sh"),
        "#1179: recording-e2e.sh must source scripts/lib/painter-display-mode.sh"
    );
}

#[test]
fn recording_e2e_computes_the_flag_at_both_launch_sites() {
    let s = recording_e2e_text();
    // The [3/8] measurement painter and the AV_RESTART_GATE painter each compute the flag via
    // `VAR="$(painter_display_mode_args)"` — so at least two calls to the helper.
    let calls = s.matches("painter_display_mode_args").count();
    assert!(
        calls >= 2,
        "#1179: expected the flag computed at BOTH the [3/8] and AV_RESTART painter launch sites \
         (>=2 painter_display_mode_args calls), found {calls}"
    );
}

/// Byte offset of the `[3/8]` painter step, bounded by the next step banner (`[4/8 pre-check]`).
fn step_3_of_8_block(s: &str) -> &str {
    let start = s
        .find("echo \"[3/8] cam2")
        .expect("#1179: recording-e2e.sh must have the [3/8] cam2 painter step");
    let end = s[start..]
        .find("echo \"[4/8 pre-check]")
        .map(|i| start + i)
        .expect("#1179: expected the [4/8 pre-check] banner to bound the [3/8] block");
    &s[start..end]
}

#[test]
fn step_3_of_8_launch_embeds_the_display_mode_flag() {
    let s = recording_e2e_text();
    let block = step_3_of_8_block(&s);
    // the measurement painter is the unique `--wall-clock` launch
    let launch = block
        .find("nohup /tmp/frame-probe --paint-only --dual-qr --wall-clock")
        .map(|i| &block[i..])
        .expect("#1179: the [3/8] measurement painter launch must still be present");
    let window = &launch[..launch.len().min(400)];
    assert!(
        window.contains("$_cam2_display_mode_flag"),
        "#1179: the [3/8] painter launch must embed the #1179 display-mode flag var \
         ($_cam2_display_mode_flag), like the adjacent $_cam2_marker_flags: got:\n{window}"
    );
}

/// The AV_RESTART_GATE painter launch — the unique `--paint-fps`-immediately-after-`--dual-qr`
/// nohup (no `--wall-clock`), inside `av_restart_record_and_emit_plan()`.
#[test]
fn av_restart_launch_embeds_the_display_mode_flag() {
    let s = recording_e2e_text();
    let launch = s
        .find("nohup /tmp/frame-probe --paint-only --dual-qr --paint-fps")
        .map(|i| &s[i..])
        .expect("#1179: the AV_RESTART_GATE painter launch must still be present");
    let window = &launch[..launch.len().min(500)];
    assert!(
        window.contains("$_av_display_mode_flag"),
        "#1179: the AV_RESTART_GATE painter launch must embed the #1179 display-mode flag var \
         ($_av_display_mode_flag): got:\n{window}"
    );
}
