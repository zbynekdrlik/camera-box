//! The A/V-sync dock output's C++ sources, read as ONE text (issue 1386). Include with
//! `#[allow(dead_code)] #[path = "support/av_sync_dock_output.rs"] mod av_sync_dock_output;`.
//!
//! The output (`sync-test-output`) is split into `sync-test-output.cpp` (the obs_output_info
//! callbacks), `sync-test-output-video.cpp`, `sync-test-output-audio.cpp` and the shared
//! `sync-test-output-internal.hpp`. Every text anchor on the output reads the union through
//! [`source`], so a function moving between those files never breaks an anchor. The pwsh twin that
//! both windows-genlock workflows dot-source is `vendor/av-sync-dock/test/dock-output-source.ps1`:
//! the same file rule and the same (ordinal) order, change both together. The public
//! `sync-test-output.hpp` (the dock UI's interface) is not part of the union.

use std::path::PathBuf;

/// Where the output's files live, relative to the repo root.
pub const DIR: &str = "vendor/av-sync-dock/src";

/// How an anchor message names the union.
pub const LABEL: &str = "vendor/av-sync-dock/src/sync-test-output{.cpp,-*.cpp,-*.hpp}";

/// Whether a file in [`DIR`] is one of the output's own sources.
pub fn is_output_file(name: &str) -> bool {
    name == "sync-test-output.cpp"
        || (name.starts_with("sync-test-output-")
            && (name.ends_with(".cpp") || name.ends_with(".hpp")))
}

/// The output's files, in byte order of the file name.
pub fn files() -> Vec<PathBuf> {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), DIR].iter().collect();
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|e| e.unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display())))
        .filter(|e| e.path().is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_output_file(n))
        .collect();
    names.sort();
    assert!(
        names.iter().any(|n| n == "sync-test-output.cpp"),
        "{LABEL}: sync-test-output.cpp is gone from {}",
        dir.display()
    );
    names.into_iter().map(|n| dir.join(n)).collect()
}

/// Every output file's text, in [`files`] order, joined with a newline.
pub fn source() -> String {
    files()
        .iter()
        .map(|p| {
            std::fs::read_to_string(p)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}
