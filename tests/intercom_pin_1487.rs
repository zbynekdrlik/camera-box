//! Issue 1487: the contract with the pinned intercom release, read from cargo's git checkout of the
//! tag (the workspace depends on `intercom-vban` from that repo, so the whole tag is on disk; the hub
//! crate is never built here).
//!
//! A pin bump fails here until `tests/fixtures/intercom_pinned/` is refreshed from the new release and
//! every contract below holds at it. `src/drm_output_audio_match.rs` is deliberately two copies.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The pin file's value of KEY.
fn pin(key: &str) -> String {
    let text = read(&root().join("scripts/intercom-release.pin"));
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in the pin file"))
        .to_string()
}

/// The directory of cargo's checkout of the intercom tag: building this crate put it at
/// `<cargo home>/git/checkouts/intercom-<hash>/<first 7 of the locked commit>/`.
fn checkout() -> PathBuf {
    let lock = read(&root().join("Cargo.lock"));
    let tag = pin("INTERCOM_RELEASE_TAG");
    let needle = format!("git+https://github.com/zbynekdrlik/intercom?tag={tag}#");
    let at = lock.find(&needle).expect("the locked intercom source") + needle.len();
    let rev = &lock[at..at + 7];
    let home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".cargo"));
    let checkouts = home.join("git/checkouts");
    for e in std::fs::read_dir(&checkouts).expect("the cargo git checkouts") {
        let dir = e.unwrap().path().join(rev);
        let name = dir.parent().unwrap().file_name().unwrap().to_string_lossy();
        if name.starts_with("intercom-") && dir.join("hub/src/matrix.rs").exists() {
            return dir;
        }
    }
    panic!(
        "no checkout of intercom {rev} under {}",
        checkouts.display()
    );
}

#[test]
fn the_git_dependency_and_the_lock_carry_the_pinned_tag() {
    let tag = pin("INTERCOM_RELEASE_TAG");
    let cargo = read(&root().join("Cargo.toml"));
    let want = format!(
        "intercom-vban = {{ git = \"https://github.com/zbynekdrlik/intercom\", tag = \"{tag}\" }}"
    );
    assert!(cargo.contains(&want), "root Cargo.toml must carry `{want}`");
    let lock = read(&root().join("Cargo.lock"));
    assert!(
        lock.contains(&format!(
            "git+https://github.com/zbynekdrlik/intercom?tag={tag}#"
        )),
        "Cargo.lock must carry the tag"
    );
    assert_eq!(pin("INTERCOM_RELEASE_SHA256").len(), 64);
}

#[test]
fn the_intercom_tree_left_this_repo() {
    assert!(!root().join("intercom").exists());
    assert!(!root().join("systemd/intercom-hub.service").exists());
    assert!(!read(&root().join("Cargo.toml")).contains("intercom/"));
}

#[test]
fn the_pinned_copy_is_the_tags_routing_files_and_unit() {
    let co = checkout();
    let fix = root().join("tests/fixtures/intercom_pinned");
    for f in [
        "intercom.strih-snv.toml",
        "intercom.strih-pp.toml",
        "intercom.stream-snv.toml",
        "intercom.stream-pp.toml",
    ] {
        assert_eq!(read(&co.join(f)), read(&fix.join(f)), "{f}");
    }
    assert_eq!(
        read(&co.join("systemd/intercom-hub.service")),
        read(&fix.join("intercom-hub.service")),
        "the unit"
    );
}

/// The hub's copy adds a 3-line doc note saying it is a copy; everything else is the same rule. The C
/// twin, its parity test and the fixtures stay here, so a rule change lands in both repositories.
#[test]
fn the_hubs_copy_of_the_audio_match_rule_equals_ours() {
    let hub = read(&checkout().join("hub/src/drm_output_audio_match.rs"));
    let ours = read(&root().join("src/drm_output_audio_match.rs"));
    let note = "//! The hub's copy (intercom#1).";
    let start = hub.find(note).expect("the hub's copy note");
    let end = start
        + hub[start..]
            .find("//!\n")
            .expect("the note's blank doc line")
        + 4;
    let stripped = format!("{}{}", &hub[..start], &hub[end..]);
    assert_eq!(stripped, ours);
}

/// The other half of `test_our_side_of_the_turn_credential_contract` (the pytest): the file name, path
/// and route `strih-interkom-public.sh` hands the hub.
#[test]
fn the_hub_reads_the_same_turn_credential_name_path_and_route() {
    let co = checkout();
    let turn = read(&co.join("hub/src/turn.rs"));
    assert!(turn.contains("pub const TURN_CONFIG_CRED: &str = \"turn.toml\";"));
    assert!(turn.contains("pub const TURN_CONFIG_DEFAULT: &str = \"/etc/intercom-hub/turn.toml\";"));
    let http = read(&co.join("hub/src/http.rs"));
    assert!(http.contains(".route(\"/api/turn-rest\", get(api_turn_rest))"));
}

#[test]
fn the_hub_default_receiver_name_is_strih_snv() {
    let m = read(&checkout().join("hub/src/matrix.rs"));
    assert!(m.contains("\"STRIH-SNV (interkom)\".to_string()"));
}

/// Issue 1485: the Dante device the hub declares at the tag is the one the leg's filter admits.
#[test]
fn the_hubs_dante_ports_are_the_ones_the_leg_declares() {
    let toml = read(&checkout().join("intercom.strih-snv.toml"));
    for want in [
        "name = \"strih-snv-intercom\"",
        "iface = \"enp7s0.70\"",
        "arc_port = 4441",
        "cmc_port = 8801",
        "flows_port = 4456",
        "rx_port_base = 47100",
    ] {
        assert!(toml.contains(want), "the hub TOML must carry `{want}`");
    }
    let env = read(&root().join("scripts/strih-boxes/strih-snv.env"));
    assert!(env.contains("strih-snv-intercom=4441:8801:4456:47100-47131"));
}

/// Issue 1485: the dstack version the pinned hub runs (the dtray `--print` threshold reads it from the pin,
/// `scripts/lib/dtray.sh`) is the one the tag's own Cargo.lock locks: the RELEASE TAG of its git source
/// (`?tag=vX.Y.Z#`), the same kind of value as DTRAY_DEVICE_FIRST_DSTACK, never the crate version.
#[test]
fn the_pinned_dstack_version_is_the_tags_lock() {
    let lock = read(&checkout().join("Cargo.lock"));
    let want = pin("INTERCOM_RELEASE_DSTACK_VERSION");
    let have = lock
        .split("[[package]]")
        .find(|p| p.contains("\nname = \"dstack\"\n"))
        .and_then(|p| p.lines().find_map(|l| l.strip_prefix("source = \"")))
        .and_then(|src| src.split("?tag=").nth(1))
        .and_then(|t| t.split('#').next())
        .map(str::to_string)
        .expect("the tag's Cargo.lock locks dstack");
    assert_eq!(
        have, want,
        "INTERCOM_RELEASE_DSTACK_VERSION must be the tag's locked dstack"
    );
}
