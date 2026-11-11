//! Issue 1378 — the genlocked OBS rig build's Windows crash handler NEVER shows a dialog.
//!
//! Upstream `main_crash_handler` (`vendor/obs-studio/frontend/obs-main.cpp`, the `#ifdef _WIN32`
//! block, reached from `libobs/obs-win-crash-handler.c` `exception_handler` -> `bcrash()`) writes
//! `obs-studio/crashes/Crash <date>.txt`, then shows a task-modal
//! `MessageBoxA(..., MB_YESNO | MB_ICONERROR | MB_TASKMODAL)` asking whether to copy the crash log
//! to the clipboard, and only calls `exit(-1)` once somebody answers. Every managed Windows OBS box
//! (stream, resolume) runs unattended, so nobody is there to answer it. The crashed obs64 would stay
//! alive behind the dialog, and every OBS launcher first checks for a live obs64, so the box's
//! respawner (the `camera-box-obs-self-heal-stream` task on stream where it is enabled, the owner's
//! AHK safe-loop on resolume) could not start a fresh OBS. WER `DontShowUI` does not cover the
//! dialog, because OBS installs its own handler.
//!
//! The rig build's handler writes the crash file exactly as upstream, logs ONE line through
//! `blog(LOG_ERROR, ...)` that names the file (and says so when the write failed), and exits at
//! once. The path is canonicalised with the non-throwing `std::error_code` overload, because the
//! one-argument `canonical()` throws when the crash file could not be written, and an exception
//! out of a crash handler ends in `abort()` and a WER report.
//!
//! Two facets, the same shape as the other vendored-frontend guards (the vendored C++ compiles only
//! on CI, per the project's Tier-0 policy):
//!
//! - **Facet A**: std-only source anchors on the comment-stripped handler body (no dialog, no
//!   clipboard, the crash file still written, the log line, `exit(-1)` last) plus the pwsh
//!   lock-step in BOTH windows-genlock workflows. This is revert protection against a future
//!   `git subtree pull` re-importing the upstream dialog while the build still compiles.
//! - **Facet B**: the handler is lifted VERBATIM, compiled with the C++ toolchain against small
//!   stand-ins for the OBS helpers AND for the Windows dialog/clipboard API the upstream handler
//!   used, and run in a child process. A re-imported dialog is then caught at RUNTIME (the
//!   stand-in `MessageBoxA` prints a marker), not only as a compile error. It fails loudly when no
//!   C++ compiler is present (a gate that skips is worse than none). The lift compiles the
//!   non-`_WIN32` branches of the handler's two inner `#ifdef _WIN32` blocks; the wide-path open
//!   and the separator replace stay CI-only.
//!
//! FRONTEND change: it lands in `obs64.exe`, so it ships with a FULL-bundle deploy, never the fast
//! obs.dll path.

#[path = "support/cpp_source.rs"]
mod cpp_source;
use cpp_source::{body_of, squish, strip_cpp_comments};

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OBS_MAIN: &str = "vendor/obs-studio/frontend/obs-main.cpp";
const WINDOWS_GENLOCK_WF: &str = ".github/workflows/windows-genlock.yml";
const WINDOWS_GENLOCK_FAST_WF: &str = ".github/workflows/windows-genlock-fast.yml";

/// The exact signature of the Windows crash handler (upstream's, kept verbatim).
const HANDLER_SIG: &str =
    "static void main_crash_handler(const char *format, va_list args, void * /* param */)";

/// The unique ASCII marker of the one log line the handler writes instead of the dialog. Kept
/// ASCII so the C++ narrow literal, this anchor and the pwsh mirrors are byte-identical.
const NO_DIALOG_MARKER: &str =
    "exiting without the crash dialog (rig build: never block on a modal)";

/// The upstream dialog's own title literal: crash-only in this file (the other `MessageBoxA`
/// calls in obs-main.cpp are the VC-runtime check and `--help`), so the pwsh negative check keys
/// on it instead of a bare `MessageBoxA`.
const UPSTREAM_DIALOG_TITLE: &str = "\"OBS has crashed!\"";

/// The fixed crash-file stem the Facet B stand-in `GenerateTimeDateFilename` produces.
const CRASH_FILE_NAME: &str = "Crash 2026-09-27 12-00-00.txt";

/// The crash text the Facet B harness feeds the handler.
const CRASH_TEXT: &str = "Unhandled exception: c0000005 (issue 1378 harness)";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn repo_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// The handler body from its opening to its closing brace, straight from the vendored file.
fn raw_handler_body(src: &str) -> &str {
    assert!(
        src.contains(HANDLER_SIG),
        "{OBS_MAIN}: `{HANDLER_SIG}` not found -- the Windows crash handler was renamed or \
         removed; re-check the issue-1378 no-dialog patch."
    );
    body_of(src, HANDLER_SIG)
}

/// The handler body with comments dropped and whitespace collapsed, for the source anchors: prose
/// in a comment can then neither satisfy nor break an anchor.
fn handler_body() -> String {
    squish(&strip_cpp_comments(raw_handler_body(&repo_file(OBS_MAIN))))
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection).
// ----------------------------------------------------------------------------------------------

#[test]
fn windows_crash_handler_never_shows_a_dialog_1378() {
    let body = handler_body();

    for banned in [
        "MessageBox",
        "MB_TASKMODAL",
        "OpenClipboard",
        "SetClipboardData",
        "GlobalAlloc",
        "IDYES",
    ] {
        assert!(
            !body.contains(banned),
            "{OBS_MAIN}: the Windows crash handler contains `{banned}` -- the task-modal crash \
             dialog (or its clipboard copy) is BACK. On an unattended OBS box nobody answers it, \
             the crashed obs64 stays alive and no respawner can start a fresh OBS. Re-apply the \
             issue-1378 patch (write the crash file, log one line, exit(-1))."
        );
    }

    // The upstream dialog text lives in a file-level macro, so check the whole file too (the same
    // raw text the pwsh mirrors read).
    let file = squish(&repo_file(OBS_MAIN));
    for banned in [
        UPSTREAM_DIALOG_TITLE,
        "Would you like to copy the crash log",
    ] {
        assert!(
            !file.contains(banned),
            "{OBS_MAIN}: the upstream crash-dialog text `{banned}` is back in the file -- a \
             subtree pull likely re-imported the dialog; re-apply the issue-1378 patch."
        );
    }
}

#[test]
fn windows_crash_handler_still_writes_the_crash_file_1378() {
    let body = handler_body();
    for kept in [
        "string crashFilePath = \"obs-studio/crashes\";",
        "delete_oldest_file(true, crashFilePath.c_str());",
        "name += \"Crash \" + GenerateTimeDateFilename(\"txt\");",
        "BPtr<char> path(GetAppConfigPathPtr(name.c_str()));",
        "file << text;",
        "file.close();",
    ] {
        assert!(
            body.contains(kept),
            "{OBS_MAIN}: the Windows crash handler no longer contains `{kept}` -- the crash file \
             in %APPDATA%\\obs-studio\\crashes is the only evidence of a crash on an unattended \
             box; it must be written exactly as upstream."
        );
    }
}

#[test]
fn windows_crash_handler_logs_the_path_then_exits_1378() {
    let body = handler_body();

    let log_call = format!("blog(LOG_ERROR, \"Crash report %s %s -- {NO_DIALOG_MARKER}\"");
    let log_at = body.find(&log_call).unwrap_or_else(|| {
        panic!(
            "{OBS_MAIN}: the Windows crash handler does not log the one issue-1378 line \
             (`{log_call}, ...)`)."
        )
    });
    assert!(
        body.contains("crashFileWritten ? \"written to\" : \"could NOT be written to\""),
        "{OBS_MAIN}: the log line must say whether the crash file was written."
    );
    let write_at = body.find("file << text;").expect("crash-file write");
    let written_at = body
        .find("const bool crashFileWritten = !file.fail();")
        .unwrap_or_else(|| {
            panic!("{OBS_MAIN}: the handler no longer records whether the crash-file write worked")
        });
    let exit_at = body.find("exit(-1);").unwrap_or_else(|| {
        panic!("{OBS_MAIN}: the Windows crash handler no longer calls exit(-1)")
    });

    assert!(
        write_at < written_at && written_at < log_at && log_at < exit_at,
        "{OBS_MAIN}: the order must be: write the crash file, record the result, log its path, \
         exit(-1)."
    );
    assert!(
        body.trim_end().ends_with("exit(-1); }"),
        "{OBS_MAIN}: exit(-1) must be the handler's last statement -- nothing may run (or wait) \
         after the log line."
    );
    assert_eq!(
        body.matches("blog(").count(),
        1,
        "{OBS_MAIN}: the crash handler logs exactly ONE line."
    );
}

#[test]
fn crash_file_canonicalisation_cannot_throw_1378() {
    let body = handler_body();
    assert!(
        body.contains("canonical(filesystem::path(pathString), canonicalError)"),
        "{OBS_MAIN}: the crash-file path must be canonicalised with the non-throwing \
         std::error_code overload (`canonical(filesystem::path(pathString), canonicalError)`)."
    );
    assert!(
        !body.contains("canonical(filesystem::path(pathString)).u8string()"),
        "{OBS_MAIN}: the throwing one-argument canonical() is back -- it throws when the crash \
         file could not be written, and an exception out of the crash handler ends in abort() \
         and a WER report instead of a clean exit."
    );
}

#[test]
fn windows_genlock_workflows_mirror_the_no_dialog_anchor_1378() {
    // A rig-critical behavioural divergence in the vendored frontend that compiles only on CI:
    // like the unclean-shutdown auto-normal gate beside it, both windows-genlock builds re-assert
    // the source text in pwsh. Drop the check from either workflow and CI fails HERE.
    for wf in [WINDOWS_GENLOCK_WF, WINDOWS_GENLOCK_FAST_WF] {
        let squished = squish(&repo_file(wf));
        assert!(
            squished.contains(NO_DIALOG_MARKER),
            "{wf}: the issue-1378 pwsh anchor ('{NO_DIALOG_MARKER}') is missing -- the build no \
             longer asserts the crash handler exits without a dialog. Re-add the pwsh gate."
        );
        assert!(
            squished.contains(&format!("'{UPSTREAM_DIALOG_TITLE}'")),
            "{wf}: the issue-1378 pwsh gate no longer asserts the upstream dialog title \
             ({UPSTREAM_DIALOG_TITLE}) is ABSENT from obs-main.cpp -- re-add the negative check."
        );
    }
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the handler, compile it standalone, run it in a child process.
// ----------------------------------------------------------------------------------------------

/// Stand-ins for the OBS helpers the handler calls, plus the Windows dialog/clipboard API the
/// UPSTREAM handler used. The dialog stand-in prints a marker, so a re-imported dialog is seen at
/// runtime; after the fix these are simply unused (extern, so no unused-function warning).
const HARNESS_PRELUDE: &str = r#"#include <algorithm>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <fstream>
#include <memory>
#include <string>
#include <system_error>
using namespace std;

enum { LOG_ERROR = 100, LOG_WARNING = 200, LOG_INFO = 300 };
#define MAX_CRASH_REPORT_SIZE (200 * 1024)

static const char *g_config_root = nullptr;

template<typename T> class BPtr {
	T *ptr;

public:
	BPtr(T *p = nullptr) : ptr(p) {}
	BPtr(const BPtr &) = delete;
	BPtr &operator=(const BPtr &) = delete;
	~BPtr() { free(ptr); }
	operator T *() { return ptr; }
	T **operator&()
	{
		free(ptr);
		ptr = nullptr;
		return &ptr;
	}
	T *Get() const { return ptr; }
};

__attribute__((format(printf, 2, 3))) void blog(int log_level, const char *format, ...)
{
	va_list args;
	va_start(args, format);
	printf("BLOG %d: ", log_level);
	vprintf(format, args);
	printf("\n");
	fflush(stdout);
	va_end(args);
}

void delete_oldest_file(bool has_prefix, const char *location)
{
	printf("DELETE_OLDEST %d %s\n", has_prefix ? 1 : 0, location);
}

string GenerateTimeDateFilename(const char *extension, bool = false)
{
	return string("2026-09-27 12-00-00.") + extension;
}

char *GetAppConfigPathPtr(const char *name)
{
	string p = string(g_config_root) + "/" + name;
	return strdup(p.c_str());
}

/* The Windows dialog + clipboard API the upstream handler used. */
typedef void *HWND;
typedef void *HGLOBAL;
#define MB_YESNO 0x4u
#define MB_ICONERROR 0x10u
#define MB_TASKMODAL 0x2000u
#define IDYES 6
#define IDNO 7
#define GMEM_MOVEABLE 0x2u
#define CF_TEXT 1u
#ifndef CRASH_MESSAGE
#define CRASH_MESSAGE "crash log saved to: %s"
#endif
int MessageBoxA(HWND, const char *, const char *, unsigned)
{
	printf("MODAL-DIALOG-SHOWN\n");
	fflush(stdout);
	return IDNO;
}
HGLOBAL GlobalAlloc(unsigned, size_t n) { return malloc(n); }
void *GlobalLock(HGLOBAL h) { return h; }
int GlobalUnlock(HGLOBAL) { return 0; }
int OpenClipboard(HWND)
{
	printf("CLIPBOARD\n");
	return 1;
}
int EmptyClipboard() { return 1; }
void *SetClipboardData(unsigned, void *h) { return h; }
int CloseClipboard() { return 1; }

"#;

const HARNESS_MAIN: &str = r#"

static void crash(const char *format, ...)
{
	va_list args;
	va_start(args, format);
	main_crash_handler(format, args, nullptr);
	va_end(args);
}

int main(int argc, char **argv)
{
	if (argc < 3)
		return 2;
	g_config_root = argv[1];
	crash("%s", argv[2]);
	printf("HANDLER-RETURNED\n");
	return 0;
}
"#;

/// A pid-keyed scratch dir that is removed when the test ends, pass or fail.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "obs_crash_handler_1378_{tag}_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the scratch dir");
        Scratch(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Lift the handler VERBATIM (signature + body) and build it into the scratch dir.
fn build_harness(dir: &Path) -> PathBuf {
    let raw = repo_file(OBS_MAIN);
    let handler = format!("{HANDLER_SIG}\n{}", raw_handler_body(&raw));
    let src = format!("{HARNESS_PRELUDE}{handler}{HARNESS_MAIN}");
    let cpp = dir.join("crash_handler.cpp");
    let bin = dir.join("crash_handler.bin");
    fs::write(&cpp, &src).expect("write the harness");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "c++".to_string());
    let out = Command::new(&cxx)
        .args(["-std=c++17", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cpp)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1378: could not run the C++ compiler `{cxx}` ({e}). This gate compiles the \
                 vendored crash handler to prove it writes the crash file and exits without a \
                 dialog; it must FAIL rather than skip without a toolchain. Install one or set CXX."
            )
        });
    assert!(
        out.status.success(),
        "issue 1378: the crash handler lifted from {OBS_MAIN} does NOT COMPILE standalone under \
         -Werror (the vendored frontend otherwise compiles only on CI):\n--- c++ stderr ---\n{}\n\
         --- harness ---\n{src}",
        String::from_utf8_lossy(&out.stderr)
    );
    bin
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_handler(bin: &Path, config_root: &Path) -> Run {
    let out = Command::new(bin)
        .arg(config_root)
        .arg(CRASH_TEXT)
        .output()
        .expect("issue 1378: the compiled harness failed to execute");
    Run {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Everything a crash-handler run must show whatever the crash directory looks like.
fn assert_exits_without_dialog(run: &Run, case: &str) -> String {
    assert!(
        !run.stdout.contains("MODAL-DIALOG-SHOWN"),
        "issue 1378 ({case}): the lifted crash handler showed the crash dialog -- on an \
         unattended box it never returns.\nstdout:\n{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("CLIPBOARD"),
        "issue 1378 ({case}): the crash handler still copies to the clipboard.\nstdout:\n{}",
        run.stdout
    );
    assert!(
        !run.stdout.contains("HANDLER-RETURNED"),
        "issue 1378 ({case}): the crash handler returned instead of exiting."
    );
    assert_eq!(
        run.code,
        Some(255),
        "issue 1378 ({case}): the crash handler must end in exit(-1) (status 255 on Linux); got \
         {:?} (None = killed by a signal, e.g. abort() after an exception).\nstdout:\n{}\n\
         stderr:\n{}",
        run.code,
        run.stdout,
        run.stderr
    );
    let blog_lines: Vec<&str> = run
        .stdout
        .lines()
        .filter(|l| l.starts_with("BLOG "))
        .collect();
    assert_eq!(
        blog_lines.len(),
        1,
        "issue 1378 ({case}): exactly one log line expected.\nstdout:\n{}",
        run.stdout
    );
    let line = blog_lines[0];
    assert!(
        line.starts_with("BLOG 100: Crash report ") && line.ends_with(NO_DIALOG_MARKER),
        "issue 1378 ({case}): the log line is not the LOG_ERROR crash-path line: {line}"
    );
    assert!(
        run.stdout.contains("DELETE_OLDEST 1 obs-studio/crashes"),
        "issue 1378 ({case}): the crash handler no longer rotates obs-studio/crashes."
    );
    line.to_string()
}

#[test]
fn lifted_crash_handler_writes_the_file_logs_it_and_exits_1378() {
    let dir = Scratch::new("written");
    let bin = build_harness(dir.path());
    let root = dir.path().join("config");
    let crashes = root.join("obs-studio").join("crashes");
    fs::create_dir_all(&crashes).expect("create obs-studio/crashes");

    let run = run_handler(&bin, &root);
    let line = assert_exits_without_dialog(&run, "crash directory present");

    let crash_file = crashes.join(CRASH_FILE_NAME);
    let written = fs::read_to_string(&crash_file).unwrap_or_else(|e| {
        panic!(
            "issue 1378: the crash file {} was not written ({e})",
            crash_file.display()
        )
    });
    assert_eq!(
        written, CRASH_TEXT,
        "issue 1378: the crash file content differs"
    );

    let canonical = fs::canonicalize(&crash_file).expect("canonicalise the crash file");
    assert!(
        line.contains(&format!(
            "Crash report written to {} -- ",
            canonical.display()
        )),
        "issue 1378: the log line must name the canonical crash-file path {}: {line}",
        canonical.display()
    );
}

#[test]
fn lifted_crash_handler_exits_cleanly_when_the_crash_file_cannot_be_written_1378() {
    // No obs-studio/crashes directory: the crash file cannot be written and canonical() has
    // nothing to resolve. The handler must say so, name the requested path, and exit(-1) --
    // never throw into abort().
    let dir = Scratch::new("unwritable");
    let bin = build_harness(dir.path());
    let root = dir.path().join("config");
    fs::create_dir_all(&root).expect("create the config root");

    let run = run_handler(&bin, &root);
    let line = assert_exits_without_dialog(&run, "crash directory missing");

    let expected = format!("{}/obs-studio/crashes/{CRASH_FILE_NAME}", root.display());
    assert!(
        line.contains(&format!(
            "Crash report could NOT be written to {expected} -- "
        )),
        "issue 1378: with no crash directory the log line must say the file could NOT be written \
         and name the requested path {expected}: {line}"
    );
}
