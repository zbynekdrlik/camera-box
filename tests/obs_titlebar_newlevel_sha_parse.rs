//! #1018 — behavioral test for the OBS title bar's deployed-build identifier.
//!
//! Background: the production boxes (strih/stream) run a custom vendored OBS whose window
//! title stamps the newlevel.media build identity so the operator can tell at a glance
//! WHICH build is running (version-integrity epic #125). The identifier USED to be the
//! compiler `__DATE__` reformatted to ISO — but OBS's own reproducible build (`/Brepro`,
//! cmake/windows/compilerconfig.cmake) blanks `__DATE__` to a short placeholder, so
//! `newlevel_iso_date(__DATE__)` returned the #313 "unknown" fallback on EVERY production
//! build (#1018: the title read "newlevel.media build unknown" on both boxes). The compile
//! date is also the WRONG signal: a FAST obs.dll hot-swap advances the deployed build but
//! never reswaps obs64.exe, so its compile date goes stale vs what is actually running.
//!
//! The fix: the title now shows the short commit SHA read from `GENLOCK_BUILD_SHA.txt` (the
//! marker every deploy writes at the install root). The pure formatting — trim the file
//! contents to the first token, validate it is a hex SHA, return the short (9-char) form,
//! and fall back to "unknown" on anything malformed — lives in the OBS/Qt-free
//! vendor/obs-studio/frontend/widgets/NewlevelBuildSha.hpp so it is unit-testable off-rig
//! (the OBS frontend only builds on the 150-min windows-genlock.yml). A title-bar helper
//! must NEVER throw (it runs inside OBSBasic construction, #313), so every input path here
//! returns a string.
//!
//! RED (before the fix): NewlevelBuildSha.hpp does not exist yet -> the C++ harness fails
//! to compile -> this test FAILS. GREEN (after the fix): the pure header formats a real SHA
//! to its short form and falls back to "unknown" on malformed/empty input -> the harness
//! exits 0.

use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The C++ harness: includes the REAL production header and drives `newlevel_short_sha()`
/// with real-shaped file contents plus the malformed inputs a title helper must survive.
/// Exits 0 on success; a wrong result returns a distinct non-zero code.
const HARNESS_CPP: &str = r#"
#include "NewlevelBuildSha.hpp"
#include <cstdio>
#include <string>

static int check(int code, const char *label, const std::string &got, const std::string &want)
{
    if (got != want) {
        std::fprintf(stderr, "FAIL(%s): got '%s' (expected '%s')\n",
                     label, got.c_str(), want.c_str());
        return code;
    }
    return 0;
}

int main()
{
    int r;
    /* (1) A full 40-char SHA with a trailing newline -> the 9-char short form. */
    if ((r = check(1, "full+lf", newlevel_short_sha("6e679ad8790f191f039c04f615c020959d038183\n"), "6e679ad87"))) return r;

    /* (2) CRLF line ending (Copy-Item / echo on Windows) -> same short form. */
    if ((r = check(2, "full+crlf", newlevel_short_sha("6e679ad8790f191f039c04f615c020959d038183\r\n"), "6e679ad87"))) return r;

    /* (3) Leading + trailing whitespace is trimmed. */
    if ((r = check(3, "spaces", newlevel_short_sha("   6e679ad8790f191f039c04f615c020959d038183   "), "6e679ad87"))) return r;

    /* (4) Uppercase hex is normalized to lowercase (git SHAs are lowercase). */
    if ((r = check(4, "upper", newlevel_short_sha("6E679AD8790F191F039C04F615C020959D038183\n"), "6e679ad87"))) return r;

    /* (5) A trailing note after the SHA -> only the first token is used. */
    if ((r = check(5, "note", newlevel_short_sha("6e679ad8790f191f039c04f615c020959d038183 built on box\n"), "6e679ad87"))) return r;

    /* (6) A short (7-char) git short-sha shorter than 9 -> returned whole, never padded. */
    if ((r = check(6, "short7", newlevel_short_sha("abc1234\n"), "abc1234"))) return r;

    /* (7) Empty contents (stock/absent marker) -> safe "unknown", never a throw. */
    if ((r = check(7, "empty", newlevel_short_sha(""), "unknown"))) return r;

    /* (8) Whitespace-only contents -> "unknown". */
    if ((r = check(8, "ws-only", newlevel_short_sha("   \r\n  "), "unknown"))) return r;

    /* (9) Non-hex garbage -> "unknown" (never show a bogus id as if it were a build). */
    if ((r = check(9, "nonhex", newlevel_short_sha("not-a-sha-value\n"), "unknown"))) return r;

    /* (10) A too-short token (< 7 chars) -> "unknown". */
    if ((r = check(10, "tooshort", newlevel_short_sha("abc\n"), "unknown"))) return r;

    /* (11) A leading UTF-8 BOM is skipped (string-concat so the \xBF escape does not
     *      greedily swallow the following hex digit). */
    if ((r = check(11, "bom", newlevel_short_sha("\xEF\xBB\xBF" "6e679ad8790f191f039c04f615c020959d038183\n"), "6e679ad87"))) return r;

    std::printf("OK newlevel_short_sha: valid -> 9-char lowercase, malformed -> unknown\n");
    return 0;
}
"#;

#[test]
fn newlevel_short_sha_formats_the_deployed_sha_and_never_throws() {
    let widgets = manifest_dir().join("vendor/obs-studio/frontend/widgets");
    let header = widgets.join("NewlevelBuildSha.hpp");
    assert!(
        header.exists(),
        "#1018: missing pure formatter header {} — the title bar's deployed-build SHA \
         formatting must be extracted into an OBS/Qt-free header so it is unit-testable \
         off-rig (the frontend only builds on the 150-min windows-genlock.yml).",
        header.display()
    );

    // Unique temp workspace so parallel test runs don't collide.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let work = std::env::temp_dir().join(format!(
        "obs_titlebar_newlevel_sha_parse_{}_{}",
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
        "#1018: the pure SHA-formatter header failed to compile with {cxx}:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin).output().expect("run compiled harness");
    let _ = std::fs::remove_dir_all(&work); // best-effort cleanup
    assert!(
        run.status.success(),
        "#1018: newlevel_short_sha() harness FAILED (exit {:?}). A valid SHA must format to \
         its 9-char lowercase short form and a malformed/empty marker must return the \
         'unknown' fallback (never throw — the helper runs during OBSBasic construction).\n\
         stdout: {}\nstderr: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr),
    );
}
