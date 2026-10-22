//! Issue 1439: the C helpers the obs-vban receive tests share (std only): the repo path and
//! compile-and-run of a C driver under the strict warning set with no float contraction.
//! Included by `#[path]` from `tests/vban_receive_timeline_parity_1439.rs` and
//! `tests/vban_receive_wiring_1439.rs`; each test binary uses all of it.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// A scratch dir named by pid + a per-process counter (std only), removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

fn scratch(tag: &str) -> Scratch {
    let n = SCRATCH.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "vban_receive_timeline_1439-{tag}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create the scratch dir");
    Scratch(dir)
}

/// Compiles C source `c` (plus `extra` flags) and returns its stdout lines.
pub fn compile_and_run(tag: &str, c: &str, extra: &[&str]) -> Vec<String> {
    let dir = scratch(tag);
    let driver = dir.0.join("driver.c");
    let bin = dir.0.join("driver.bin");
    fs::write(&driver, c).expect("write the driver");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=c11",
            "-ffp-contract=off",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Wsign-conversion",
            "-Wformat=2",
            "-Werror",
            "-O1",
        ])
        .args(extra)
        .arg(&driver)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1439: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 shipped C; it must FAIL rather than skip. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1439: the {tag} driver does NOT COMPILE under -Wall -Wextra -Wconversion \
         -Wsign-conversion -Wformat=2 -Werror:\n--- cc stderr ---\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1439: the compiled driver failed to execute");
    assert!(
        run.status.success(),
        "issue 1439: the {tag} driver exited non-zero"
    );
    String::from_utf8(run.stdout)
        .expect("driver stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}
