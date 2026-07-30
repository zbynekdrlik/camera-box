//! #744 — `scripts/recording-e2e.sh` used to hardcode `v4l2-ctl --set-ctrl=saturation=50,contrast=50`
//! at three sites ([0/8] preflight, [2/8] cam1 deploy, [2b/8] ALL_CAMBOX per-box loop, #338/#312).
//! That literal `50` IS the ShadowCast 2's own 0-100 factory default — but on the Elgato 4K S
//! cards (0-255 range, default 128) the SAME literal is ~39% of default: dark, chroma-muted
//! picture (live 2026-07-13: both Elgato boxes sitting at contrast=50/saturation=50 on a 0-255
//! range; resetting to 128/128 instantly fixed the dark multiview tiles, mean luma 29/30 -> 101/
//! 100). Very likely the root cause of #740's colour gate reading red on every node identically.
//!
//! Also #744 item 2: those same sites hardcoded `/dev/video0`, which RENUMBERS on USB
//! re-enumeration (#728 — CAM1's Elgato was `/dev/video1` at swap time, `/dev/video0` days later).
//!
//! These tests (a) pin that the foreign literal is GONE from recording-e2e.sh and that the new
//! `scripts/lib/v4l2-neutral.sh` helper is actually wired into all three sites, and (b) source the
//! REAL `scripts/lib/v4l2-neutral.sh` (never re-implement the parser) against captured
//! `v4l2-ctl --list-ctrls` fixtures shaped after the ShadowCast 2 (0-100, default 50) and Elgato
//! 4K S (0-255, default 128) cards, plus the NZXT Signal HD60 (no picture controls at all) and a
//! partial-exposure shape.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/v4l2-neutral.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn recording_e2e_text() -> String {
    let p = manifest_dir().join("scripts/recording-e2e.sh");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Source the shared lib and run `body`, returning stdout. Asserts the harness itself exited 0.
fn run_sourced(body: &str) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}");
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Call `v4l2_neutral_default_ctrl_arg` with a fixture `--list-ctrls` transcript passed via an
/// env var (never interpolated into the bash -c script text — a fixture with embedded quotes or
/// `$` must never need bash-escaping by the test itself).
fn ctrl_arg_for(fixture: &str) -> String {
    let harness =
        "set -uo pipefail\n. \"$SCRIPT\"\nv4l2_neutral_default_ctrl_arg \"$FIXTURE\"".to_string();
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("SCRIPT", lib_script())
        .env("FIXTURE", fixture)
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "v4l2_neutral_default_ctrl_arg exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// ShadowCast 2 shape: 0-100 range, default 50 on both contrast+saturation (the certified COLOUR
/// set's reference_pct=50 already lands here — this is the harmless case #338/#312 relied on).
const LIST_CTRLS_SHADOWCAST: &str = "\
                     brightness 0x00980900 (int)    : min=0 max=255 step=1 default=128 value=128
                       contrast 0x00980901 (int)    : min=0 max=100 step=1 default=50 value=50
                     saturation 0x00980902 (int)    : min=0 max=100 step=1 default=50 value=50
                            hue 0x00980903 (int)    : min=0 max=100 step=1 default=50 value=50
";

/// Elgato 4K S shape (#744's live evidence): 0-255 range, default 128 — the SAME literal 50
/// applied by the old hardcode is ~39% of this card's own neutral, producing the dark/muted
/// picture. `value=50` here is the SMEARED live state this helper must reset away from.
const LIST_CTRLS_ELGATO_SMEARED: &str = "\
                       contrast 0x00980901 (int)    : min=0 max=255 step=1 default=128 value=50
                     saturation 0x00980902 (int)    : min=0 max=255 step=1 default=128 value=50
                            hue 0x00980903 (int)    : min=-180 max=180 step=1 default=0 value=0
";

/// NZXT Signal HD60 shape: no picture controls exposed at all (targets.md: "no V4L2 picture
/// controls exposed"). The helper must apply NOTHING — never force a foreign value onto a card
/// that has neither control.
const LIST_CTRLS_NZXT_NO_CONTROLS: &str = "\
             pixel_rate 0x009f0902 (int64)  : value=148500000 flags=read-only, volatile
";

/// A card exposing only saturation (no contrast control) — the helper must emit just that one
/// clause, no dangling comma.
const LIST_CTRLS_SATURATION_ONLY: &str = "\
                     saturation 0x00980902 (int)    : min=0 max=255 step=1 default=128 value=50
";

#[test]
fn shadowcast_2_resolves_to_its_own_0_100_default() {
    assert_eq!(
        ctrl_arg_for(LIST_CTRLS_SHADOWCAST),
        "saturation=50,contrast=50"
    );
}

#[test]
fn elgato_4k_s_resolves_to_its_own_0_255_default_not_the_shadowcast_literal() {
    // The core #744 regression: on the 0-255 card the answer must be the device's OWN default
    // (128), never the ShadowCast literal 50 that smeared the picture.
    assert_eq!(
        ctrl_arg_for(LIST_CTRLS_ELGATO_SMEARED),
        "saturation=128,contrast=128"
    );
}

#[test]
fn card_with_no_saturation_or_contrast_control_gets_nothing_applied() {
    assert_eq!(ctrl_arg_for(LIST_CTRLS_NZXT_NO_CONTROLS), "");
}

#[test]
fn card_exposing_only_saturation_emits_that_clause_alone() {
    assert_eq!(ctrl_arg_for(LIST_CTRLS_SATURATION_ONLY), "saturation=128");
}

#[test]
fn hue_is_never_read_or_emitted_338() {
    // A control list where hue's own default differs sharply from contrast/saturation — if the
    // parser ever picked up hue by accident, this would show up in the output.
    let out = ctrl_arg_for(LIST_CTRLS_SHADOWCAST);
    assert!(
        !out.contains("hue"),
        "hue must never be read or emitted (#338): {out}"
    );
}

#[test]
fn resolve_node_cmd_and_set_default_cmd_are_present_and_distinct() {
    let resolve = run_sourced("v4l2_neutral_resolve_node_cmd");
    let apply = run_sourced("v4l2_neutral_set_default_cmd");
    assert!(
        resolve.contains("V4L2_NEUTRAL_NODE"),
        "resolve cmd must set V4L2_NEUTRAL_NODE: {resolve}"
    );
    assert!(
        resolve.contains("/sys/class/video4linux/"),
        "resolve cmd must consult the kernel's own sysfs index attribute: {resolve}"
    );
    assert!(
        apply.contains("--set-ctrl="),
        "apply cmd must call v4l2-ctl --set-ctrl: {apply}"
    );
    assert!(
        !apply.contains("saturation=50,contrast=50"),
        "apply cmd must never hardcode the foreign ShadowCast literal: {apply}"
    );
}

#[test]
fn regression_e2e_script_no_longer_hardcodes_the_foreign_literal_744() {
    let text = recording_e2e_text();
    assert!(
        !text.contains("saturation=50,contrast=50"),
        "recording-e2e.sh must no longer hardcode saturation=50,contrast=50 — #744 (this literal \
         darkens 0-255 Elgato cards; each card must get its OWN --list-ctrls default instead)"
    );
}

#[test]
fn regression_e2e_script_sources_the_v4l2_neutral_lib_744() {
    let text = recording_e2e_text();
    assert!(
        text.contains("lib/v4l2-neutral.sh"),
        "recording-e2e.sh must source scripts/lib/v4l2-neutral.sh (#744)"
    );
}

#[test]
fn regression_e2e_script_wires_the_helper_into_all_three_sites_744() {
    let text = recording_e2e_text();
    let apply_calls = text.matches("v4l2_neutral_apply_cmds").count();
    let resolve_calls = text.matches("v4l2_neutral_resolve_node_cmd").count();
    let set_default_calls = text.matches("v4l2_neutral_set_default_cmd").count();
    // [0/8] preflight uses the combined v4l2_neutral_apply_cmds; [2/8] cam1 deploy and [2b/8]
    // ALL_CAMBOX loop each embed resolve_node_cmd + set_default_cmd separately (so the fuser
    // busy-wait sitting between them can reuse the SAME resolved $V4L2_NEUTRAL_NODE).
    assert!(
        apply_calls >= 1,
        "expected [0/8] to call v4l2_neutral_apply_cmds at least once, found {apply_calls}"
    );
    assert!(
        resolve_calls >= 2,
        "expected [2/8] + [2b/8] to each call v4l2_neutral_resolve_node_cmd, found {resolve_calls}"
    );
    assert!(
        set_default_calls >= 2,
        "expected [2/8] + [2b/8] to each call v4l2_neutral_set_default_cmd, found {set_default_calls}"
    );
}

#[test]
fn regression_e2e_script_no_longer_hardcodes_dev_video0_in_the_three_sites_744() {
    // The fuser busy-wait + v4l2-ctl calls at [2/8]/[2b/8] must reference the RESOLVED node
    // ($V4L2_NEUTRAL_NODE), never the bare literal /dev/video0 -- USB nodes renumber (#728).
    // /dev/video0 may still appear elsewhere in the file (e.g. as v4l2-neutral.sh's own
    // last-resort fallback default, or in unrelated comments) -- this pins only the specific
    // fuser busy-wait line that used to hardcode it.
    let text = recording_e2e_text();
    assert!(
        !text.contains("fuser -s /dev/video0"),
        "the fuser busy-wait must target the resolved $V4L2_NEUTRAL_NODE, not a hardcoded \
         /dev/video0 (#744, #728 -- USB grabber nodes renumber)"
    );
}

// --- live-caught regression (gate run 29265311504, the #746 push's E2E): command substitution
// UNCONDITIONALLY STRIPS trailing newlines from a function's captured output, so embedding
// `$(v4l2_neutral_set_default_cmd)` mid-string (as [2/8]/[2b/8] do, followed by more literal
// remote text) glued its LAST line onto whatever text followed, with NO separator at all --
// `v4l2-ctl ... --get-ctrl=... 2>/dev/null` + `rm -f /tmp/....log` became ONE command line,
// v4l2-ctl errored "unknown arguments: rm", and the `rm` never ran. Fixed by ending each `_cmd`
// function's last statement with an explicit `;` that survives the newline-strip. These tests
// reproduce the EXACT embedding shape recording-e2e.sh uses (a fake `v4l2-ctl` capturing argv +
// a marker file the trailing `rm -f` must remove) against the REAL library -- a structural
// "ends with `;`" check alone would not have caught the actual live failure mode.

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("v4l2-neutral-746-{}-{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Install a fake `v4l2-ctl` on PATH that appends its argv (space-joined) as one line to
/// `$ARGV_LOG`, and prints nothing to stdout (so `--list-ctrls` yields an empty capture, the
/// `if [ -n "$_v4l2_ctrlarg" ]` branch is skipped, and the ONLY real v4l2-ctl invocation that
/// runs is the final `--get-ctrl=...` line -- the exact one the live bug glued text onto).
fn install_fake_v4l2_ctl(bin_dir: &std::path::Path) {
    let script = "#!/usr/bin/env bash\necho \"$@\" >> \"$ARGV_LOG\"\n";
    fs::write(bin_dir.join("v4l2-ctl"), script).expect("write fake v4l2-ctl");
    let mut perms = fs::metadata(bin_dir.join("v4l2-ctl"))
        .unwrap()
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(bin_dir.join("v4l2-ctl"), perms).unwrap();
}

#[test]
fn set_default_cmd_embedding_never_glues_the_following_command_746() {
    let dir = scratch("set-default");
    install_fake_v4l2_ctl(&dir);
    let marker = dir.join("marker");
    fs::write(&marker, "").expect("create marker file");
    let argv_log = dir.join("argvlog");
    fs::write(&argv_log, "").expect("create argv log");

    // Reproduces recording-e2e.sh's EXACT [2/8]/[2b/8] embedding shape: a resolved
    // $V4L2_NEUTRAL_NODE, then `$(v4l2_neutral_set_default_cmd)` embedded mid-string, followed
    // by a `rm -f <marker>;` on the "next line" via a backslash-newline continuation inside an
    // outer double-quoted string -- exactly what the harness's own sshpass ssh command strings
    // look like.
    let harness = format!(
        r#"set -uo pipefail
. "$SCRIPT"
export PATH="{bin}:$PATH"
export ARGV_LOG="{argv_log}"
V4L2_NEUTRAL_NODE=/fake/video0
CMD="echo start; \
   $(v4l2_neutral_set_default_cmd) \
   rm -f {marker}; \
   echo done"
eval "$CMD"
"#,
        bin = dir.display(),
        argv_log = argv_log.display(),
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
        "the `rm -f <marker>` that follows v4l2_neutral_set_default_cmd's embedding must run as \
         its OWN command -- if it got glued onto v4l2-ctl's argv instead (the live #746 bug), \
         the marker file would still exist"
    );
    let argv_log_text = fs::read_to_string(&argv_log).expect("read argv log");
    assert!(
        !argv_log_text.contains("rm"),
        "the fake v4l2-ctl must NEVER receive \"rm\" as one of its arguments -- that is the \
         exact live failure (\"unknown arguments: rm\"): {argv_log_text}"
    );
    assert!(
        argv_log_text.contains("--get-ctrl=saturation,contrast"),
        "the final get-ctrl readback must still have run cleanly: {argv_log_text}"
    );
}

#[test]
fn resolve_node_cmd_embedding_never_glues_the_following_command_746() {
    // Same shape, for v4l2_neutral_resolve_node_cmd. Without the trailing `;` fix, its last
    // statement (a bare `V4L2_NEUTRAL_NODE=...` assignment, no command name) glued directly onto
    // whatever command follows at the embedding site becomes a bash "VAR=value command" PREFIX
    // assignment -- which sets the variable ONLY in that one command's temporary environment, not
    // in the calling shell -- so a LATER reference to $V4L2_NEUTRAL_NODE reads as unset. The
    // explicit trailing `;` prevents that: it closes the assignment as its own statement before
    // anything else can attach to it as a prefix.
    let dir = scratch("resolve-node");
    let marker = dir.join("marker");
    fs::write(&marker, "").expect("create marker file");
    let harness = format!(
        r#"set -uo pipefail
. "$SCRIPT"
CMD="echo start; \
   $(v4l2_neutral_resolve_node_cmd) \
   rm -f {marker}; \
   echo \"node=\$V4L2_NEUTRAL_NODE\"; \
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
        "both echo markers must have run: {stdout}"
    );
    assert!(
        !marker.exists(),
        "the `rm -f <marker>` following v4l2_neutral_resolve_node_cmd's embedding must run as \
         its own command: {stdout}"
    );
    assert!(
        stdout.contains("node=/dev/video0") || stdout.contains("node=/dev/video"),
        "V4L2_NEUTRAL_NODE must have been resolved/assigned before the following commands ran: \
         {stdout}"
    );
}
