//! Regression guard for remote env interpolation in `scripts/loopback-e2e.sh` (#39).
//!
//! PR #38's review flagged that the loopback harness built the remote env prefix by
//! interpolating values inside single quotes:
//!
//! ```sh
//! ssh_cam "SOURCE='${SOURCE}' ... bash -s"
//! ```
//!
//! `SOURCE` carries the free-text `source` workflow input. A single quote in any value
//! closes the quote early and everything after it is parsed by the REMOTE root shell on
//! the camera device — a quote-injection that can run arbitrary commands. The fix builds
//! the prefix with `printf %q`, which emits a shell-safe quoting of every value.
//!
//! This test sources the REAL script (its `BASH_SOURCE != $0` guard skips the
//! build/deploy/ssh flow), calls `build_remote_env` with a malicious `SOURCE`, then
//! parses the resulting prefix with a shell exactly as the remote side would. It asserts
//! the payload (a) never executes and (b) round-trips verbatim. RED before the `printf %q`
//! fix (the single-quote interpolation injects); GREEN after.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn loopback_remote_env_is_injection_safe() {
    let script = manifest_dir().join("scripts/loopback-e2e.sh");
    assert!(script.exists(), "{} not found", script.display());

    // Per-process marker the injection payload tries to create. If it exists after the
    // run, the malicious value escaped the quoting and executed a command remotely.
    // System temp dir (always writable; PID keeps it unique across parallel test procs).
    let marker =
        std::env::temp_dir().join(format!("loopback_inject_marker_{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);

    // Malicious SOURCE: close the single quote, run `touch <marker>`, reopen a quote.
    // A safe builder must neutralise all of this and preserve the value verbatim.
    let evil = format!("evil'; touch {}; echo 'pwned", marker.display());

    // Source the harness, build the prefix from the malicious SOURCE, then simulate the
    // remote shell: apply the assignment prefix to `bash -c` and read SOURCE back.
    let harness = r#"
set -uo pipefail
export SOURCE="$EVIL"
. "$SCRIPT"
prefix="$(build_remote_env)"
set +e
roundtrip="$(eval "$prefix bash -c 'printf %s \"\$SOURCE\"'" 2>/dev/null)"
printf 'ROUNDTRIP<<%s>>' "$roundtrip"
"#;

    let out = Command::new("bash")
        .arg("-c")
        .arg(harness)
        .env("SCRIPT", &script)
        .env("EVIL", &evil)
        .output()
        .expect("failed to run bash harness");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // (a) No injection: the payload's `touch <marker>` must NOT have run.
    let injected = marker.exists();
    let _ = std::fs::remove_file(&marker);
    assert!(
        !injected,
        "quote-injection executed: a single quote in SOURCE escaped the remote env \
         quoting and ran `touch {}`. build_remote_env must quote with `printf %q` (or \
         equivalent). stdout={stdout:?} stderr={stderr:?}",
        marker.display()
    );

    // (b) Value integrity: SOURCE must round-trip through the remote shell verbatim.
    let roundtrip = stdout
        .strip_prefix("ROUNDTRIP<<")
        .and_then(|s| s.strip_suffix(">>"))
        .unwrap_or_else(|| {
            panic!("harness produced no ROUNDTRIP marker. stdout={stdout:?} stderr={stderr:?}")
        });
    assert_eq!(
        roundtrip, evil,
        "SOURCE was mangled crossing the remote env boundary — quoting is not safe. \
         stderr={stderr:?}"
    );
}
