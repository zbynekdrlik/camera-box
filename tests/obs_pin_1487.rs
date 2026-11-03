//! Issue 1487: the contract with the pinned obs release (zbynekdrlik/obs).
//!
//! `scripts/obs-release.pin` says which release the Linux OBS boxes run. These tests read the
//! release over HTTPS with curl: a pin bump fails here until every pinned value holds at the new
//! tag. No network = a FAILED test, never a skip.

use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn pin(key: &str) -> String {
    read("scripts/obs-release.pin")
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in the pin file"))
        .to_string()
}

fn fetch(url: &str) -> Vec<u8> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "120", url])
        .output()
        .expect("curl");
    assert!(
        out.status.success(),
        "{url}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn asset(name: &str) -> Vec<u8> {
    fetch(&format!(
        "https://github.com/zbynekdrlik/obs/releases/download/{}/{name}",
        pin("OBS_RELEASE_TAG")
    ))
}

fn sha256(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("blob");
    std::fs::write(&f, bytes).unwrap();
    let out = Command::new("sha256sum")
        .arg(&f)
        .output()
        .expect("sha256sum");
    String::from_utf8(out.stdout).unwrap()[..64].to_string()
}

#[test]
fn the_pin_carries_a_tag_a_commit_and_both_sha256() {
    let tag = pin("OBS_RELEASE_TAG");
    let v = tag.strip_prefix('v').expect("a vX.Y.Z tag");
    assert_eq!(v.split('.').count(), 3, "{tag}");
    assert_eq!(pin("OBS_RELEASE_COMMIT").len(), 40);
    assert_eq!(pin("OBS_RELEASE_STRIH_SHA256").len(), 64);
    assert_eq!(pin("OBS_RELEASE_MANIFEST_SHA256").len(), 64);
}

#[test]
fn the_pinned_tarball_sha256_is_the_releases_own() {
    let tag = pin("OBS_RELEASE_TAG");
    let name = format!("obs-strih-linux-x86_64-{tag}.tar.zst");
    let side = String::from_utf8(asset(&format!("{name}.sha256"))).unwrap();
    assert_eq!(
        side.trim(),
        format!("{}  {name}", pin("OBS_RELEASE_STRIH_SHA256")),
        "refresh scripts/obs-release.pin"
    );
}

#[test]
fn the_release_names_the_pinned_tag_and_commit() {
    let text = String::from_utf8(asset("OBS_RELEASE.txt")).unwrap();
    assert!(
        text.lines()
            .any(|l| l == format!("tag={}", pin("OBS_RELEASE_TAG"))),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l == format!("commit={}", pin("OBS_RELEASE_COMMIT"))),
        "{text}"
    );
}

/// The manifest is pinned by sha256 (the release publishes no .sha256 for it); its release block names
/// the tag + commit, and the OBS + DistroAV versions drift-guard grades a Linux box against are its own.
#[test]
fn the_pinned_manifest_is_the_releases_and_carries_the_pinned_versions() {
    let bytes = asset("BUNDLE_MANIFEST.json");
    assert_eq!(
        sha256(&bytes),
        pin("OBS_RELEASE_MANIFEST_SHA256"),
        "refresh OBS_RELEASE_MANIFEST_SHA256"
    );
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    let rel = &v["release"];
    assert_eq!(rel["obs_tag"], pin("OBS_RELEASE_TAG").as_str());
    assert_eq!(rel["obs_commit"], pin("OBS_RELEASE_COMMIT").as_str());
    assert_eq!(
        rel["versions"]["obs-studio"],
        pin("OBS_RELEASE_OBS_STUDIO_VERSION").as_str()
    );
    assert_eq!(
        rel["versions"]["distroav"],
        pin("OBS_RELEASE_DISTROAV_VERSION").as_str()
    );
    let files = v["files"].as_array().expect("files");
    for want in [
        "bin/obs",
        "lib/x86_64-linux-gnu/libobs.so.30",
        "lib/x86_64-linux-gnu/obs-plugins/dante-input.so",
        "lib/x86_64-linux-gnu/obs-plugins/obs-vban.so",
        "RUNTIME_PACKAGES.txt",
        "STRIH_BUILD_FLAGS.txt",
    ] {
        assert!(
            files.iter().any(|f| f["path"] == want),
            "the release has no {want}"
        );
    }
    for k in ["genlock_stats_version", "genlock_output_stats_version"] {
        assert!(rel[k].as_u64().unwrap_or(0) > 0, "{k}");
    }
}

/// Issue 1485: the dstack version the pinned release's dante_input runs (the dtray `--print` threshold
/// reads it from the pin, `scripts/lib/dtray.sh`) is the one the commit's FFI Cargo.lock locks.
#[test]
fn the_pinned_dstack_version_is_the_releases_lock() {
    let url = format!(
        "https://raw.githubusercontent.com/zbynekdrlik/obs/{}/plugins/dante-input/ffi/Cargo.lock",
        pin("OBS_RELEASE_COMMIT")
    );
    let lock = String::from_utf8(fetch(&url)).unwrap();
    let have = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"dstack\"\n"))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("version = \"")))
        .map(|v| format!("v{}", v.trim_end_matches('"')))
        .expect("the release's Cargo.lock locks dstack");
    assert_eq!(
        have,
        pin("OBS_RELEASE_DSTACK_VERSION"),
        "OBS_RELEASE_DSTACK_VERSION must be the release's locked dstack"
    );
}
