//! Behavioral guard for the `/update-av-stack` engine `scripts/update-av-stack.sh` (#44).
//!
//! The slash command automates the documented `git subtree pull --squash` release-bump of
//! the vendored OBS/DistroAV stack (vendor/README.md). Its deterministic core — parse the
//! pinned-version manifest, decide whether each component is behind upstream, and emit the
//! exact catch-up command — must be correct regardless of network state, because that is what
//! decides whether production boxes get a re-patched new release or silently linger on an old
//! one. These tests source the REAL script (its `BASH_SOURCE != $0` guard skips the
//! network/mutating flow) and exercise those pure functions directly, the same convention as
//! tests/harness_remote_env_quoting.rs.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script() -> PathBuf {
    let s = manifest_dir().join("scripts/update-av-stack.sh");
    assert!(s.exists(), "{} not found", s.display());
    s
}

/// Source the script and run `body` (which may call the pure functions). Returns stdout.
fn run_sourced(body: &str, extra_env: &[(&str, &str)]) -> String {
    let harness = format!("set -uo pipefail\n. \"$SCRIPT\"\n{body}", body = body);
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(&harness).env("SCRIPT", script());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "sourced harness exited non-zero.\nstdout={:?}\nstderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn parse_manifest_extracts_subtree_components_and_skips_ndi() {
    // A representative vendor table: header, separator, two subtree rows, and the NDI row
    // that rides inside DistroAV (no standalone subtree — must be skipped).
    let readme = std::env::temp_dir().join(format!("av_manifest_{}.md", std::process::id()));
    let table = "\
# vendor

| dir | upstream | version | imported as |
|---|---|---|---|
| `vendor/obs-studio` | github.com/obsproject/obs-studio | **32.1.2** (commit `fb4d98bf8`) | git subtree --squash |
| `vendor/distroav` | github.com/DistroAV/DistroAV | **6.2.1** (commit `038d9d6`) | git subtree --squash |
| NDI SDK headers | shipped inside DistroAV (`vendor/distroav/lib/ndi/`) | SDK v6 (plugin requires **NDI ≥ 6.3.0**) | part of the DistroAV tree |

Some prose afterwards.
";
    std::fs::write(&readme, table).expect("write temp readme");

    let out = run_sourced(
        "parse_manifest \"$README\"",
        &[("README", readme.to_str().unwrap())],
    );
    let _ = std::fs::remove_file(&readme);

    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "expected exactly 2 subtree components (NDI row must be skipped), got: {out:?}"
    );
    assert_eq!(
        lines[0],
        "vendor/obs-studio\thttps://github.com/obsproject/obs-studio.git\t32.1.2\tfb4d98bf8",
        "obs row mis-parsed"
    );
    assert_eq!(
        lines[1], "vendor/distroav\thttps://github.com/DistroAV/DistroAV.git\t6.2.1\t038d9d6",
        "distroav row mis-parsed"
    );
    assert!(
        !out.contains("ndi") && !out.contains("NDI"),
        "NDI-headers row must not be emitted as a subtree component: {out:?}"
    );
}

#[test]
fn version_status_compares_numerically_and_flags_unknown() {
    // (current, latest, expected)
    let cases = [
        ("32.1.2", "32.1.2", "UP-TO-DATE"),
        ("32.1.2", "32.2.0", "BEHIND"),
        ("6.2.1", "6.10.0", "BEHIND"), // numeric, not lexical (10 > 2)
        ("6.2.1", "6.1.0", "AHEAD"),
        ("v6.2.1", "6.2.1", "UP-TO-DATE"), // leading v tolerated
        ("6.2.1", "", "UNKNOWN"),          // upstream lookup failed -> never a false UP-TO-DATE
    ];
    for (cur, lat, want) in cases {
        let out = run_sourced(
            "version_status \"$CUR\" \"$LAT\"",
            &[("CUR", cur), ("LAT", lat)],
        );
        assert_eq!(
            out.trim(),
            want,
            "version_status({cur:?}, {lat:?}) should be {want}"
        );
    }
}

#[test]
fn subtree_pull_cmd_emits_exact_catch_up_command() {
    let out = run_sourced(
        "subtree_pull_cmd vendor/obs-studio https://github.com/obsproject/obs-studio.git 32.2.0",
        &[],
    );
    assert_eq!(
        out.trim(),
        "git subtree pull --prefix=vendor/obs-studio https://github.com/obsproject/obs-studio.git 32.2.0 --squash"
    );
}

#[test]
fn normalize_url_canonicalizes_all_input_shapes() {
    let cases = [
        (
            "github.com/obsproject/obs-studio",
            "https://github.com/obsproject/obs-studio.git",
        ),
        (
            "https://github.com/DistroAV/DistroAV",
            "https://github.com/DistroAV/DistroAV.git",
        ),
        (
            "https://github.com/DistroAV/DistroAV.git",
            "https://github.com/DistroAV/DistroAV.git",
        ),
        ("github.com/o/r/", "https://github.com/o/r.git"),
    ];
    for (raw, want) in cases {
        let out = run_sourced("normalize_url \"$RAW\"", &[("RAW", raw)]);
        assert_eq!(out.trim(), want, "normalize_url({raw:?})");
    }
}
