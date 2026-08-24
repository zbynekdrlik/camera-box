//! #1197 — bounded COLD-finder discovery-wait + re-enforce, wired into the mv-reverify path.
//!
//! Right after a strih OBS boot OR the #1093 escalation force-kill restart, the fresh DistroAV finder
//! is COLD: a genuinely-live sender is not-yet-discovered, the #1114 reattach empties a correct
//! ndi_source_name (mangle-protection refuses to re-apply an absent name) and nothing WAITS for the
//! finder to warm up + re-enforce the #399 baseline (#1158's permanent-empty wedge). This adds a
//! shared runner `mv_reverify_finder_heal_wait` (→ `set-ndi-mapping.py --heal-wait`) wired at two
//! sites: (A) `mv_reverify_or_escalate` after the restart across ALL active inputs, and (B)
//! `mv_reverify_resolve_wait` scoped to the one camera, before the pixel poll.
//!
//! All Tier-0 (no rig, no ssh, no OBS): the runner's env seam (`MV_REVERIFY_HEAL_WAIT_CMD`) drives a
//! fake offline; the WIRING is a static read of the lib text (the sibling-harness model).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_path() -> PathBuf {
    manifest_dir().join("scripts/lib/mv-reverify-escalate.sh")
}

fn lib_text() -> String {
    fs::read_to_string(lib_path()).expect("read mv-reverify-escalate.sh")
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

fn make_exec(p: &PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(p).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(p, perms).unwrap();
}

/// Source the lib under `set -uo pipefail` (never `-e`, so a WARN-only `|| ...` fallback cannot abort
/// the harness), then run `snippet`. Returns (stdout+stderr, exit_ok).
fn run(env: &str, snippet: &str) -> (String, bool) {
    let script = format!(
        "set -uo pipefail\n. \"{}\" 2>/dev/null\n{env}\n{snippet}",
        lib_path().display()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("run bash");
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    (s, out.status.success())
}

// ---- the runner exists + honors the seam -----------------------------------------------------

#[test]
fn lib_defines_the_finder_heal_wait_runner() {
    assert!(
        lib_text().contains("mv_reverify_finder_heal_wait()"),
        "#1197: scripts/lib/mv-reverify-escalate.sh must define mv_reverify_finder_heal_wait"
    );
}

#[test]
fn finder_heal_wait_runs_the_seam_with_host_active_deadline() {
    // The env seam receives <host> <active_spec> <deadline_s> so a fake can assert what it was called
    // with (mirrors MV_REVERIFY_OBS_RESTART_CMD's arg convention).
    let log = std::env::temp_dir().join(format!("mv_hw_{}.log", nanos()));
    let stub = std::env::temp_dir().join(format!("mv_hw_{}.sh", nanos()));
    fs::write(
        &stub,
        format!(
            "#!/usr/bin/env bash\necho \"HW $1 $2 $3\" >> '{}'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    make_exec(&stub);
    let (out, ok) = run(
        &format!("MV_REVERIFY_HEAL_WAIT_CMD='{}'\nSTRIH=x", stub.display()),
        "mv_reverify_finder_heal_wait 10.0.0.9 'cam3' 90; echo RC=$?",
    );
    let logged = fs::read_to_string(&log).unwrap_or_default();
    let _ = fs::remove_file(&stub);
    let _ = fs::remove_file(&log);
    assert!(ok, "runner should exit 0; out=\n{out}");
    assert!(
        logged.contains("HW 10.0.0.9 cam3 90"),
        "#1197: the seam must be called with host/active/deadline; logged={logged:?} out=\n{out}"
    );
    assert!(
        out.contains("RC=0"),
        "#1197: WARN-only runner returns 0; out=\n{out}"
    );
}

#[test]
fn finder_heal_wait_is_warn_only_even_when_the_seam_fails() {
    // A failed heal-wait (e.g. some inputs still absent -> exit 3, or a WS error) must NEVER fail the
    // run: the pixel re-verify / the next camera's own reverify is the real gate.
    let (out, ok) = run(
        "MV_REVERIFY_HEAL_WAIT_CMD='/bin/false'\nSTRIH=x",
        "mv_reverify_finder_heal_wait 10.0.0.9 'cam3' 90; echo RC=$?",
    );
    assert!(ok, "harness ran; out=\n{out}");
    assert!(
        out.contains("RC=0"),
        "#1197: mv_reverify_finder_heal_wait must be WARN-only (return 0) even when the heal-wait \
         command fails; out=\n{out}"
    );
}

// ---- the default (non-seam) path invokes the #399 authority ----------------------------------

#[test]
fn default_path_invokes_set_ndi_mapping_heal_wait() {
    // Anchor on the INVOCATION form (the quoted script path + the flag), never a bare token a comment
    // could also contain — so this proves the real call site, not prose.
    assert!(
        lib_text().contains("set-ndi-mapping.py\" --host") && lib_text().contains("--heal-wait"),
        "#1197: the default runner path must invoke set-ndi-mapping.py --heal-wait"
    );
}

// ---- WIRING: (A) or_escalate warms the finder after the restart, before the re-check ----------

fn body_of(fn_header: &str) -> String {
    // Slice from a function header to the next top-level `\n<name>() {` header (or EOF).
    let s = lib_text();
    let start = s
        .find(fn_header)
        .unwrap_or_else(|| panic!("no {fn_header} in the lib"));
    let after = &s[start + fn_header.len()..];
    // next function definition header at column 0
    let end = after.find("\n}\n").map(|e| e + 3).unwrap_or(after.len());
    after[..end].to_string()
}

#[test]
fn or_escalate_warms_all_active_inputs_after_the_restart_before_the_recheck() {
    let body = body_of("mv_reverify_or_escalate() {");
    let warm = body.find("mv_reverify_finder_heal_wait").expect(
        "#1197: mv_reverify_or_escalate must warm the fresh finder (mv_reverify_finder_heal_wait) \
         after the escalation restart",
    );
    let sweep = body
        .find("MV_REVERIFY_SWEEP_CMD")
        .expect("#1093: the sweep-off must exist in mv_reverify_or_escalate");
    // the LAST preflight_mv_reverify in the body is the single post-restart re-check
    let recheck = body
        .rfind("preflight_mv_reverify")
        .expect("#1093: the post-restart re-check must exist");
    assert!(
        sweep < warm && warm < recheck,
        "#1197: the finder-warm must run AFTER the burn sweep-off and BEFORE the single re-check \
         (sweep={sweep} warm={warm} recheck={recheck})"
    );
    assert!(
        body.contains("CAMERA_ACTIVE_SET"),
        "#1197: the post-restart warm must cover ALL active inputs (--active \"${{CAMERA_ACTIVE_SET:-}}\")"
    );
}

// ---- WIRING: (B) resolve_wait warms the one camera's finder before the pixel poll -------------

#[test]
fn resolve_wait_warms_the_finder_before_the_pixel_poll() {
    let body = body_of("mv_reverify_resolve_wait() {");
    let warm = body.find("mv_reverify_finder_heal_wait").expect(
        "#1197: mv_reverify_resolve_wait must warm this camera's finder + re-enforce its baseline \
         (mv_reverify_finder_heal_wait) before the pixel poll",
    );
    // the pixel poll is the `while [ "$SECONDS" -lt "$deadline" ]` loop
    let poll = body
        .find("while [ \"$SECONDS\" -lt \"$deadline\" ]")
        .expect("#1114: the resolve-wait pixel poll loop must exist");
    assert!(
        warm < poll,
        "#1197: the per-camera finder-warm must run BEFORE the pixel poll (warm={warm} poll={poll})"
    );
}
