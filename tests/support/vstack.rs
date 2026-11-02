//! Issue 1487: the pinned vstack source, read from cargo's git checkout of the tag (root Cargo.toml
//! depends on its crates, so the whole tag is on disk). `tests/python/vstack_src.py` is the same
//! lookup for pytest. Use with `#[path = "support/vstack.rs"] mod vstack;`.

#![allow(dead_code)] // each test binary uses part of this file

use std::path::{Path, PathBuf};

/// The repo root of this checkout.
fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The pin file's value of KEY.
pub fn pin(key: &str) -> String {
    let p = repo().join("scripts/vstack-release.pin");
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or_else(|| panic!("no {key} in {}", p.display()))
        .to_string()
}

/// `<cargo home>/git/checkouts/vstack-<hash>/<short commit>/`, the checkout of the pinned commit.
pub fn root() -> PathBuf {
    let commit = pin("VSTACK_RELEASE_COMMIT");
    let home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap()).join(".cargo"));
    let checkouts = home.join("git/checkouts");
    for db in std::fs::read_dir(&checkouts).expect("the cargo git checkouts") {
        let db = db.unwrap().path();
        if !db
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("vstack-")
        {
            continue;
        }
        for co in std::fs::read_dir(&db).unwrap() {
            let dir = co.unwrap().path();
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            if commit.starts_with(&name) && dir.join("rig/core/Cargo.toml").exists() {
                return dir;
            }
        }
    }
    panic!(
        "no checkout of vstack {commit} under {}",
        checkouts.display()
    );
}

/// A path inside the pinned tag.
pub fn path(rel: &str) -> PathBuf {
    root().join(rel)
}

/// The text of a file inside the pinned tag.
pub fn read(rel: &str) -> String {
    let p: PathBuf = path(rel);
    read_path(&p)
}

fn read_path(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}
