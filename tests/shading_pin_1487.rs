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
    pin_in("scripts/shading-release.pin", key)
}

/// Issue 1452: the relay pin is chosen per box, so a tree carries several pin files of one format.
fn pin_in(file: &str, key: &str) -> String {
    read(file)
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {file}"))
        .to_string()
}

/// Every pin file of the tree with the fixture dir holding its tag's contract copy.
const PINS: [(&str, &str); 2] = [
    (
        "scripts/shading-release.pin",
        "tests/fixtures/shading_pinned",
    ),
    (
        "scripts/shading-release-rest.pin",
        "tests/fixtures/shading_pinned_rest",
    ),
];

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
    raw_at(&pin("SHADING_RELEASE_COMMIT"), path)
}

fn raw_at(commit: &str, path: &str) -> String {
    fetch(&format!(
        "https://raw.githubusercontent.com/zbynekdrlik/shading/{commit}/{path}"
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
    for (file, _) in PINS {
        let tag = pin_in(file, "SHADING_RELEASE_TAG");
        let v = tag.strip_prefix('v').expect("a vX.Y.Z tag");
        assert_eq!(v.split('.').count(), 3, "{file}: {tag}");
        assert_eq!(pin_in(file, "SHADING_RELEASE_COMMIT").len(), 40, "{file}");
        for (key, _) in ASSETS {
            assert_eq!(pin_in(file, key).len(), 64, "{file}: {key}");
        }
    }
}

#[test]
fn every_pinned_sha256_is_the_releases_own() {
    for (file, _) in PINS {
        let tag = pin_in(file, "SHADING_RELEASE_TAG");
        for (key, asset) in ASSETS {
            let side = fetch(&format!(
                "https://github.com/zbynekdrlik/shading/releases/download/{tag}/{asset}.sha256"
            ));
            assert_eq!(
                side.trim(),
                format!("{}  {asset}", pin_in(file, key)),
                "{asset}: refresh {file}"
            );
        }
    }
}

/// Issue 1452: the per-box map names only pin files of this tree, and the default pin is never a
/// REST relay (a v0.2.x relay has no gphoto2 fallback: a box goes onto it only by name).
#[test]
fn the_box_map_names_only_existing_pin_files_and_the_default_stays_the_gphoto2_relay() {
    let map = read("scripts/shading-release-boxes.txt");
    let mut listed = 0;
    for line in map.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut f = line.split_whitespace();
        let (box_, file) = (f.next().unwrap(), f.next().expect("a pin file"));
        assert!(
            PINS.iter().any(|(p, _)| *p == format!("scripts/{file}")),
            "{box_}: {file} is no pin file of this tree"
        );
        listed += 1;
    }
    assert!(listed >= 1, "the map lists the REST boxes");
    assert!(
        pin("SHADING_RELEASE_TAG").starts_with("v0.1."),
        "the fleet default stays the gphoto2 relay until every camera serves REST"
    );
}

/// The contract and the one table a camera-box lib keeps in sync with shading's source.
#[test]
fn the_pinned_copy_is_the_tags_contract_and_ndi_search_paths() {
    for (file, fixtures) in PINS {
        assert_eq!(
            raw_at(&pin_in(file, "SHADING_RELEASE_COMMIT"), "docs/CONTRACT.md"),
            read(&format!("{fixtures}/CONTRACT.md")),
            "{file}: the contract moved: read what changed, adapt, refresh the fixture"
        );
    }
    assert_eq!(
        raw("bkshading/service/src/preview/ndi_paths.rs"),
        read("tests/fixtures/shading_pinned/ndi_paths.rs"),
        "the NDI search paths moved: adapt scripts/lib/bkshading-ndi-runtime.sh, refresh the fixture"
    );
}

/// Issue 1452: the REST endpoints the E2E uses directly while the relay is stopped are contract rows
/// of the REST relay's contract, and so is the camera link file camera-box installs.
#[test]
fn the_e2e_rest_endpoints_and_the_camera_link_stand_in_the_rest_contract() {
    let contract = read("tests/fixtures/shading_pinned_rest/CONTRACT.md");
    for ep in [
        "video/iso",
        "video/shutter",
        "video/whiteBalance",
        "video/whiteBalanceTint",
        "lens/iris",
        "system/format",
    ] {
        assert!(contract.contains(&format!("| `{ep}` |")), "{ep}");
    }
    assert!(contract.contains("/etc/systemd/network/05-bkshading-camera.network"));
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
    // issue 1452: each box's relay version is the version of ITS pin (scripts/shading-release-boxes.txt)
    assert!(read("scripts/deploy-fleet.sh")
        .contains("_rver=\"$(SHADING_RELEASE_PIN_FILE=\"$_rpin\" shading_pin_version"));
    assert!(!read("scripts/deploy-fleet.sh").contains("not from the same build"));
}
