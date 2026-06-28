//! #313 — behavioral regression test for NewlevelBuildDate()'s compile-date parse.
//!
//! #152 stamps the newlevel.media build identity + compile date into the OBS window title.
//! The date is produced by reformatting the compiler `__DATE__` ("Mmm DD YYYY") to ISO.
//! The original parse indexed/`substr`-ed the string with NO size guard: `d.substr(7, 4)`
//! throws `std::out_of_range` (MSVC `std::_Xran`, "invalid string position") when the date
//! is shorter than 11 chars. In the windows-genlock build the compile-date string was not
//! the expected 11-char form, so the throw propagated out of `NewlevelBuildDate()` ->
//! `UpdateTitleBar()` -> the `OBSBasic` constructor -> `OBSApp::OBSInit`, and OBS ABORTED at
//! startup on EVERY launch (#313). A title-bar helper must never be able to crash OBS.
//!
//! The parse is now the pure, OBS/Qt-free `newlevel_iso_date()` in
//! vendor/obs-studio/frontend/widgets/NewlevelBuildDate.hpp — the EXACT function the
//! frontend's `NewlevelBuildDate()` calls with `__DATE__`. The OBS frontend only builds on
//! the 150-min `windows-genlock.yml`, so this crash class is invisible to normal PR CI;
//! but the header compiles standalone, so this test compiles + runs it in a tiny C++ harness
//! (default features, plain `c++`, no probe, no OBS core) and drives the malformed inputs the
//! frontend cannot.
//!
//! RED (before the fix): the parse has no size guard -> `newlevel_iso_date("Jun 28")` throws
//! -> the uncaught exception aborts the harness -> non-zero exit -> this test FAILS. GREEN
//! (after the fix): a too-short date returns the safe "unknown" fallback, a well-formed date
//! still reformats to ISO -> the harness exits 0.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C++ harness: includes the REAL production header and drives `newlevel_iso_date()`
/// with the exact malformed inputs that crashed OBS plus well-formed dates. Exits 0 on
/// success; a wrong result returns a distinct non-zero code, and (pre-fix) an uncaught
/// `std::out_of_range` aborts the process — both make the test FAIL.
const HARNESS_CPP: &str = r#"
#include "NewlevelBuildDate.hpp"
#include <cstdio>
#include <string>

int main()
{
    /* (1) The #313 crash input: a truncated "Mmm DD" (6 chars) — substr(7,4) is out of
     *     range. Must NOT throw; must fall back to "unknown". (Pre-fix: throws -> abort.) */
    if (newlevel_iso_date("Jun 28") != "unknown") {
        std::fprintf(stderr, "FAIL: short date -> '%s' (expected 'unknown')\n",
                     newlevel_iso_date("Jun 28").c_str());
        return 1;
    }

    /* (2) An empty date (no __DATE__ at all) must also fall back, never throw. */
    if (newlevel_iso_date("") != "unknown") {
        std::fprintf(stderr, "FAIL: empty date -> '%s' (expected 'unknown')\n",
                     newlevel_iso_date("").c_str());
        return 2;
    }

    /* (3) A well-formed two-digit-day __DATE__ reformats to ISO YYYY-MM-DD. */
    if (newlevel_iso_date("Jun 28 2026") != "2026-06-28") {
        std::fprintf(stderr, "FAIL: 'Jun 28 2026' -> '%s' (expected '2026-06-28')\n",
                     newlevel_iso_date("Jun 28 2026").c_str());
        return 3;
    }

    /* (4) __DATE__ space-pads a single-digit day ("Jan  1 2020") — still ISO with a 0. */
    if (newlevel_iso_date("Jan  1 2020") != "2020-01-01") {
        std::fprintf(stderr, "FAIL: 'Jan  1 2020' -> '%s' (expected '2020-01-01')\n",
                     newlevel_iso_date("Jan  1 2020").c_str());
        return 4;
    }

    std::printf("OK newlevel_iso_date: short/empty -> unknown, valid -> ISO\n");
    return 0;
}
"#;

#[test]
fn newlevel_iso_date_never_throws_on_short_or_empty_date() {
    let widgets = manifest_dir().join("vendor/obs-studio/frontend/widgets");
    let header = widgets.join("NewlevelBuildDate.hpp");
    assert!(
        header.exists(),
        "#313: missing pure parse header {} — NewlevelBuildDate()'s compile-date parse must \
         be extracted into an OBS/Qt-free header so it is unit-testable off-rig.",
        header.display()
    );

    // Unique temp workspace so parallel test runs don't collide.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "obs_titlebar_newlevel_parse_{}_{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&work).expect("create temp workdir");
    let src = work.join("harness.cpp");
    let bin = work.join("harness");
    std::fs::write(&src, HARNESS_CPP).expect("write C++ harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let compile = Command::new(&cxx)
        .arg("-std=c++17")
        .arg("-Wall")
        .arg("-I")
        .arg(&widgets)
        .arg(&src)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| panic!("failed to invoke C++ compiler '{cxx}': {e}"));
    assert!(
        compile.status.success(),
        "#313: the parse header failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#313: newlevel_iso_date() regression harness FAILED (exit {:?}). A short/empty/\
         malformed compile date must return the 'unknown' fallback and NEVER throw (an \
         out-of-range substr aborted OBS at startup), while a well-formed __DATE__ still \
         reformats to ISO.\nstdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
