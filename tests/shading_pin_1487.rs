//! Issue 1487: the contract with the pinned shading release.
//!
//! `scripts/shading-release.pin` says which release the boxes run. The shading repo is no dependency
//! of this workspace, so these tests read the release and the tag over HTTPS with curl: a pin bump
//! fails here until the pin and `tests/fixtures/shading_pinned/` hold at the new tag. No network = a
//! FAILED test, never a skip.

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
    read("scripts/shading-release.pin")
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in the pin file"))
        .to_string()
}

fn fetch(url: &str) -> String {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "60", url])
        .output()
        .expect("curl");
    assert!(
        out.status.success(),
        "{url}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

fn raw(path: &str) -> String {
    fetch(&format!(
        "https://raw.githubusercontent.com/zbynekdrlik/shading/{}/{path}",
        pin("SHADING_RELEASE_COMMIT")
    ))
}

const ASSETS: [(&str, &str); 4] = [
    (
        "SHADING_RELAY_LINUX_AMD64_SHA256",
        "bkshading-relay-linux-amd64",
    ),
    (
        "SHADING_RELAY_LINUX_ARM64_SHA256",
        "bkshading-relay-linux-arm64",
    ),
    (
        "SHADING_SERVICE_LINUX_AMD64_SHA256",
        "bkshading-service-linux-amd64.tar.gz",
    ),
    (
        "SHADING_SERVICE_WINDOWS_AMD64_SHA256",
        "bkshading-service-windows-amd64.exe",
    ),
];

#[test]
fn the_pin_carries_a_tag_a_commit_and_every_asset() {
    let tag = pin("SHADING_RELEASE_TAG");
    let v = tag.strip_prefix('v').expect("a vX.Y.Z tag");
    assert_eq!(v.split('.').count(), 3, "{tag}");
    assert_eq!(pin("SHADING_RELEASE_COMMIT").len(), 40);
    for (key, _) in ASSETS {
        assert_eq!(pin(key).len(), 64, "{key}");
    }
}

#[test]
fn every_pinned_sha256_is_the_releases_own() {
    let tag = pin("SHADING_RELEASE_TAG");
    for (key, asset) in ASSETS {
        let side = fetch(&format!(
            "https://github.com/zbynekdrlik/shading/releases/download/{tag}/{asset}.sha256"
        ));
        assert_eq!(
            side.trim(),
            format!("{}  {asset}", pin(key)),
            "{asset}: refresh scripts/shading-release.pin"
        );
    }
}

/// The contract and the one table a camera-box lib keeps in sync with shading's source.
#[test]
fn the_pinned_copy_is_the_tags_contract_and_ndi_search_paths() {
    assert_eq!(
        raw("docs/CONTRACT.md"),
        read("tests/fixtures/shading_pinned/CONTRACT.md"),
        "the contract moved: read what changed, adapt, refresh the fixture"
    );
    assert_eq!(
        raw("bkshading/service/src/preview/ndi_paths.rs"),
        read("tests/fixtures/shading_pinned/ndi_paths.rs"),
        "the NDI search paths moved: adapt scripts/lib/bkshading-ndi-runtime.sh, refresh the fixture"
    );
}

/// The gphoto2 keys the E2E uses directly while the relay is stopped are a contract row.
#[test]
fn the_e2e_gphoto2_keys_stand_in_the_contract() {
    let contract = read("tests/fixtures/shading_pinned/CONTRACT.md");
    for key in ["iso", "d002", "d004", "f-number", "d005", "d006"] {
        assert!(contract.contains(&format!("| `{key}` |")), "{key}");
    }
}

#[test]
fn camera_box_no_longer_builds_bkshading() {
    let cargo = read("Cargo.toml");
    assert!(!cargo.contains("bkshading/"), "no workspace member");
    assert!(
        !cargo.contains("members"),
        "the appliance is the only member"
    );
    let ci = read(".github/workflows/ci.yml");
    assert!(!ci.contains("  bkshading:"), "no bkshading job");
    assert!(!ci.contains("bkshading-windows"), "no bkshading job");
    assert!(!ci.contains("dist-relay"), "no relay artifact");
}

#[test]
fn every_deploy_path_takes_the_pinned_release() {
    for f in [
        "scripts/bkshading-deploy-relay.sh",
        "scripts/bkshading-deploy-service.sh",
        "scripts/deploy-fleet.sh",
        "scripts/lib/bkshading-relay-provision.sh",
        "scripts/lib/strih-snv-deploy.sh",
        "scripts/lib/strih-provision.sh",
    ] {
        let s = read(f);
        assert!(s.contains("shading-release.sh"), "{f} sources the lib");
        assert!(!s.contains("bkshading-linux-amd64"), "{f}: a CI artifact");
        assert!(!s.contains("bkshading-windows-amd64"), "{f}: a CI artifact");
    }
    let setup = read("scripts/setup-strih.sh");
    assert!(setup.contains("shading_release_service \"${HERE}/..\""));
    assert!(read("scripts/lib/strih-snv-deploy.sh").contains("shading_release_service \"$w/repo\""));
    assert!(read("scripts/deploy-fleet.sh").contains("shading_release_relay \"$HERE/..\" amd64"));
}

#[test]
fn the_relay_version_is_graded_against_the_pin_never_camera_box() {
    let h = read("scripts/rig-dev-handover-check.sh");
    assert!(h.contains("bkshading_relay_version_verdict \"$pin\" \"$rlv\" \"$main\""));
    assert!(read("scripts/deploy-fleet.sh").contains("RELAY_VER=\"$(shading_pin_version"));
    assert!(!read("scripts/deploy-fleet.sh").contains("not from the same build"));
}
