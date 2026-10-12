//! The ONE `TempPath` of the test harnesses (issue 1416, from the issue-1409 tempdir conversion):
//! a path in its own `tempfile::tempdir()`, the dir itself or a file in it. Include with
//! `#[path = "support/temp_path.rs"] mod temp_path; use temp_path::TempPath;` -- files under
//! `tests/support/` are not test crates of their own.
//!
//! The dir lives as long as the `TempPath` and is removed when it drops, so no pid-named dir is
//! left in /tmp. Bind the value for as long as the code under test reads the path: a helper that
//! builds one and returns only a string derived from it deletes the file first.

// Not every including test file uses every impl.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A path plus the `TempDir` that owns it. The fields are public so a helper builds it with a
/// struct literal: `TempPath { path, _dir: dir }`.
pub struct TempPath {
    pub path: PathBuf,
    pub _dir: tempfile::TempDir,
}

impl std::ops::Deref for TempPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}
