//! issue 1317 (follow-up F6) — the CEF `chrome-sandbox` SUID sandbox helper must be owned
//! root:root mode 4755 (setuid root) on the strih-lx box or the 4 browser sources cannot launch
//! (Chromium aborts the sandbox helper without `--no-sandbox`, which is the rejected approach —
//! it weakens every browser source's isolation session-wide).
//!
//! Two guards here, both Tier-0 (bash + static text, zero cargo):
//!
//! (a) FUNCTIONAL embedding test — reproduces the `tests/harness_v4l2_neutral_744.rs` pattern:
//! `strih_lx_chrome_sandbox_fix_cmd`'s emitted text is embedded mid-string via `$(...)` with a
//! trailing command after it, over a fake bundle dir with stand-in `chown`/`chmod` on `PATH`. It
//! proves the `;` termination (the following command runs as its OWN statement, never glued) AND
//! that chown/chmod actually hit the real `chrome-sandbox` path.
//! (b) STATIC-anchor tests — `setup-strih.sh` calls the builder AFTER the bundle install, and
//! `verify-strih.sh` carries the new chrome-sandbox verdict check.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn lib_script() -> PathBuf {
    let p = manifest_dir().join("scripts/lib/strih-provision.sh");
    assert!(p.exists(), "{} not found", p.display());
    p
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "strih-chrome-sandbox-1317-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Install a fake `chown`/`chmod` on PATH that appends its argv (space-joined) as one line to
/// `$ARGV_LOG` and mutates nothing — so the test observes exactly what the emitted text invoked.
fn install_fake(bin_dir: &std::path::Path, name: &str) {
    let script = "#!/usr/bin/env bash\necho \"$@\" >> \"$ARGV_LOG\"\n";
    let p = bin_dir.join(name);
    fs::write(&p, script).unwrap_or_else(|e| panic!("write fake {name}: {e}"));
    let mut perms = fs::metadata(&p).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    fs::set_permissions(&p, perms).unwrap();
}

#[test]
fn chrome_sandbox_fix_cmd_embedding_never_glues_the_following_command() {
    let dir = scratch("embed");
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_fake(&bin, "chown");
    install_fake(&bin, "chmod");

    // A fake bundle root carrying chrome-sandbox at the real multiarch obs-plugins location.
    let root = dir.join("obs-genlock");
    let plug = root.join("lib/x86_64-linux-gnu/obs-plugins");
    fs::create_dir_all(&plug).unwrap();
    let cs = plug.join("chrome-sandbox");
    fs::write(&cs, "").expect("create fake chrome-sandbox");

    let marker = dir.join("marker");
    fs::write(&marker, "").expect("create marker file");
    let argv_log = dir.join("argvlog");
    fs::write(&argv_log, "").expect("create argv log");

    // Embed `$(strih_lx_chrome_sandbox_fix_cmd <root>)` mid-string, followed by a `rm -f <marker>;`
    // on the "next line" via a backslash-newline continuation inside an outer double-quoted string
    // — the exact shape a caller that splices the builder into a larger command uses. If the `;`
    // termination were missing, the trailing `rm` would glue onto the last chmod's argv (the #746
    // failure mode) and the marker would survive.
    let harness = format!(
        r#"set -uo pipefail
. "$SCRIPT"
export PATH="{bin}:$PATH"
export ARGV_LOG="{argv_log}"
CMD="echo start; \
   $(strih_lx_chrome_sandbox_fix_cmd {root}) \
   rm -f {marker}; \
   echo done"
eval "$CMD"
"#,
        bin = bin.display(),
        argv_log = argv_log.display(),
        root = root.display(),
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
        "the `rm -f <marker>` following the builder's embedding must run as its OWN command — if it \
         got glued onto chmod's argv (the #746 bug), the marker file would still exist"
    );
    let argv = fs::read_to_string(&argv_log).expect("read argv log");
    let cs_disp = cs.display().to_string();
    assert!(
        argv.lines().any(|l| l == format!("root:root {cs_disp}")),
        "chown must have targeted root:root on the real chrome-sandbox path: {argv}"
    );
    assert!(
        argv.lines().any(|l| l == format!("4755 {cs_disp}")),
        "chmod must have set 4755 (setuid root) on the real chrome-sandbox path: {argv}"
    );
    assert!(
        !argv.split_whitespace().any(|t| t == "rm"),
        "the fake chown/chmod must NEVER receive \"rm\" as an argument (the #746 glue failure): {argv}"
    );
}

#[test]
fn setup_strih_calls_the_builder_after_the_bundle_install() {
    let s = read("scripts/setup-strih.sh");
    // The bundle install writes the genlock markers (`genlock_write_markers`); the chrome-sandbox
    // fix must be wired AFTER it (it operates on the just-installed bundle).
    let install = s
        .find("genlock_write_markers")
        .expect("setup-strih must install the bundle via genlock_write_markers");
    let fix = s
        .find("strih_lx_chrome_sandbox_fix_cmd")
        .expect("setup-strih must call the chrome-sandbox setuid builder");
    assert!(
        fix > install,
        "the chrome-sandbox fix must run AFTER the bundle install (genlock_write_markers @ {install}, builder @ {fix})"
    );
    // It must gate on the BROWSER-ON marker (fail-loud on a BROWSER-ON bundle whose helper is
    // absent; loud SKIP on BROWSER-OFF), and must never fall back to --no-sandbox.
    assert!(
        s.contains("strih_lx_browser_bundle_required"),
        "the fix must gate on the STRIH_BUILD_FLAGS.txt BROWSER-ON marker"
    );
    assert!(
        !s.contains("--no-sandbox"),
        "setup-strih must not weaken the sandbox with --no-sandbox"
    );
}

#[test]
fn verify_strih_carries_the_chrome_sandbox_check() {
    let v = read("scripts/verify-strih.sh");
    assert!(
        v.contains("strih_lx_chrome_sandbox_verdict"),
        "verify-strih must run the chrome-sandbox setuid verdict"
    );
    assert!(
        v.contains("chrome-sandbox"),
        "verify-strih must reference the chrome-sandbox helper by name"
    );
}
