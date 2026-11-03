//! Issue 1487: camera-box builds on one pinned vstack tag: the pin file, every git dependency, the
//! lock and the OBS strih build agree, and no copy of the moved code stays here.

#[path = "support/vstack.rs"]
mod vstack;

use std::path::PathBuf;

const CRATES: [&str; 4] = [
    "camera-box-jxs",
    "rig-core",
    "qpsk-marker",
    "svt-jpegxs-sys",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = root().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn every_vstack_dependency_carries_the_pinned_tag() {
    let tag = vstack::pin("VSTACK_RELEASE_TAG");
    let cargo = read("Cargo.toml");
    let want = format!("{{ git = \"https://github.com/zbynekdrlik/vstack\", tag = \"{tag}\"");
    for c in CRATES {
        assert!(
            cargo.contains(&format!("{c} = {want}")),
            "{c} must depend on {want}"
        );
    }
    assert_eq!(
        cargo.matches("zbynekdrlik/vstack").count(),
        CRATES.len(),
        "a vstack dependency outside the pinned list"
    );
}

#[test]
fn the_lock_resolves_every_crate_to_the_pinned_commit() {
    let tag = vstack::pin("VSTACK_RELEASE_TAG");
    let commit = vstack::pin("VSTACK_RELEASE_COMMIT");
    assert_eq!(commit.len(), 40);
    let lock = read("Cargo.lock");
    let source = format!("git+https://github.com/zbynekdrlik/vstack?tag={tag}#{commit}");
    assert_eq!(lock.matches(&source).count(), CRATES.len(), "{source}");
    assert_eq!(
        lock.matches("zbynekdrlik/vstack").count(),
        CRATES.len(),
        "the lock carries a second vstack source"
    );
}

/// `jxs-recv-ffi` depends on the codec unconditionally: as a root dependency it would make every
/// default build need cmake and nasm.
#[test]
fn a_default_build_needs_no_codec() {
    let cargo = read("Cargo.toml");
    for c in ["jxs-recv-ffi", "jxs-send-ffi"] {
        assert!(!cargo.contains(&format!("{c} =")), "{c}");
    }
    assert!(cargo.contains("jxs = [\"dep:svt-jpegxs-sys\", \"camera-box-jxs/jxs\"]"));
    let tag = vstack::pin("VSTACK_RELEASE_TAG");
    assert!(cargo.contains(&format!(
        "svt-jpegxs-sys = {{ git = \"https://github.com/zbynekdrlik/vstack\", tag = \"{tag}\", optional = true }}"
    )));
}

#[test]
fn the_moved_code_left_this_repo() {
    for gone in [
        "jxs",
        "rig",
        "vendor/svt-jpeg-xs",
        "src/genlock_backlog.rs",
        "src/genlock_n1_depth.rs",
        "src/genlock_wall_step.rs",
        "src/qpsk_marker.rs",
        "src/qpsk_marker_scan.rs",
    ] {
        assert!(!root().join(gone).exists(), "{gone} must be gone");
    }
    let cargo = read("Cargo.toml");
    assert!(!cargo.contains("path = \"jxs/") && !cargo.contains("path = \"rig/"));
}

#[test]
fn the_old_paths_stay_as_reexports() {
    let lib = read("src/lib.rs");
    for line in [
        "pub use ::qpsk_marker::qpsk_marker;",
        "pub use ::qpsk_marker::qpsk_marker_scan;",
        "pub use rig_core::genlock_backlog;",
        "pub use rig_core::genlock_grid;",
        "pub use rig_core::genlock_n1_depth;",
        "pub use rig_core::genlock_wall_step;",
    ] {
        assert!(lib.contains(line), "{line}");
    }
}

/// vstack's root declares no release profile, so without these the archive OBS links would lose the
/// LTO and strip this repo's own release build gives it.
#[test]
fn the_strih_build_links_the_pinned_tag_with_this_repos_release_profile() {
    let wf = read(".github/workflows/linux-genlock.yml");
    let at = wf
        .find("- name: Fetch the pinned vstack source")
        .expect("the fetch step");
    let fetch = &wf[at..at + wf[at..].find("- name: Build the jxs-recv-ffi").unwrap()];
    for needle in [
        "VSTACK_RELEASE_TAG=//p' scripts/vstack-release.pin",
        "VSTACK_RELEASE_COMMIT=//p' scripts/vstack-release.pin",
        "--branch \"$tag\" https://github.com/zbynekdrlik/vstack",
        "if [ \"$have\" != \"$commit\" ]; then",
        "; exit 1; fi",
    ] {
        assert!(fetch.contains(needle), "the fetch step lacks `{needle}`");
    }
    assert!(wf.contains("working-directory: ${{ env.VSTACK_SRC }}"));
    // vstack's root carries the release profile since v0.1.4; a repeat here could only drift.
    assert!(!wf.contains("CARGO_PROFILE_RELEASE_"));
    assert!(wf.contains("cargo rustc --locked --release -p jxs-recv-ffi"));
    assert!(
        wf.contains("-DCAMERA_BOX_JXS_FFI_LIB=\"$VSTACK_SRC/target/release/libjxs_recv_ffi.a\"")
    );
}

/// pytest reads constants from the pinned source, and that job compiles nothing.
#[test]
fn the_python_job_fetches_the_pinned_source() {
    let ci = read(".github/workflows/ci.yml");
    let job = &ci[ci.find("  python-tests:").unwrap()..ci.find("  build:").unwrap()];
    assert!(job.contains("run: cargo fetch"));
}
