//! Shared helpers of the issue-1367 stale-duplicate gate (`tests/distroav_stale_duplicate_retarget_1367.rs`
//! and `tests/distroav_stale_duplicate_replay_1367.rs`): read `vendor/distroav/src/ndi-source.cpp`, lift
//! its helper blocks VERBATIM, and compile + run a C harness built from them. Include with
//! `#[allow(dead_code)] #[path = "support/ndi_source_lift_1367.rs"] mod lift;` -- files under
//! `tests/support/` are not test crates of their own.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

pub fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

pub fn repo_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
pub fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn require(src: &str, needle: &str, why: &str) {
    assert!(
        src.contains(needle),
        "{NDI_SOURCE}: issue 1367 patch missing — `{needle}` not found. {why}"
    );
}

/// Byte index of `needle` in `src`, panicking with `why` when absent.
pub fn index_of(src: &str, needle: &str, why: &str) -> usize {
    src.find(needle).unwrap_or_else(|| {
        panic!("{NDI_SOURCE}: issue 1367 patch missing — `{needle}` not found. {why}")
    })
}

/// The body of the function whose definition starts with `signature`, to its first `\n}\n`.
pub fn body_of(src: &str, signature: &str) -> String {
    let start = index_of(src, signature, "function definition");
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1367: `{signature}` has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

/// Lift a `static inline` helper VERBATIM, from its signature to the first `\n}\n`.
pub fn lift_fn(signature: &str) -> String {
    let src = repo_file(NDI_SOURCE);
    let start = src.find(signature).unwrap_or_else(|| {
        panic!(
            "issue 1367: {NDI_SOURCE} no longer defines `{signature}` — nothing to compile. \
             Re-apply the fix."
        )
    });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .unwrap_or_else(|| panic!("issue 1367: `{signature}` has no closing brace `\\n}}\\n`"));
    src[start..end].to_string()
}

/// Lift a `static const` constant line VERBATIM, from its declaration to the terminating `;`.
pub fn lift_const(prefix: &str) -> String {
    let src = repo_file(NDI_SOURCE);
    let start = src.find(prefix).unwrap_or_else(|| {
        panic!("issue 1367: {NDI_SOURCE} no longer declares `{prefix}` — re-apply the fix.")
    });
    let end = src[start..]
        .find(";\n")
        .map(|i| start + i + 2)
        .unwrap_or_else(|| panic!("issue 1367: `{prefix}` has no terminating `;`"));
    src[start..end].to_string()
}

/// The contiguous verdict block VERBATIM: the verdict enum, the contested check, the picker, the
/// #1180 equality helper and the duplicate-aware verdict.
pub fn lift_verdict_block() -> String {
    let src = repo_file(NDI_SOURCE);
    let start = index_of(&src, "enum ndi_verify_verdict_1367 {", "the verdict enum");
    let sig = index_of(
        &src,
        "static inline int ndi_identity_verdict_1367(",
        "the verdict helper",
    );
    assert!(
        start < sig,
        "issue 1367: the verdict enum must precede the verdict helper"
    );
    let end = src[sig..]
        .find("\n}\n")
        .map(|i| sig + i + 3)
        .expect("issue 1367: the verdict helper has no closing brace");
    src[start..end].to_string()
}

/// The state block VERBATIM, between its BEGIN and END markers.
pub fn lift_state_block() -> String {
    let src = repo_file(NDI_SOURCE);
    let start = index_of(
        &src,
        "/* camera-box #1367 stale-duplicate state: BEGIN",
        "the state block BEGIN marker",
    );
    let end_marker = "/* camera-box #1367 stale-duplicate state: END */";
    let end = index_of(&src, end_marker, "the state block END marker") + end_marker.len();
    src[start..end].to_string()
}

pub const PRELUDE: &str = "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n\
                       #include <string.h>\n#include <stdio.h>\n\
                       typedef struct { const char *p_ndi_name; const char *p_url_address; } NDIlib_source_t;\n";

/// Each compile gets its own file names: several tests build the SAME tag (the six replays all
/// build "sequence"), and a parallel runner (CI's nextest, `cargo test` threads) would otherwise run
/// one test's binary while another rewrites it (ETXTBSY, "Text file busy"). The pid separates
/// processes, the counter separates threads of one process.
pub fn compile_and_run(c: &str, tag: &str) -> Vec<String> {
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join("distroav_stale_duplicate_retarget_1367");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let stem = format!("{tag}-{}-{n}", std::process::id());
    let cfile = dir.join(format!("{stem}.c"));
    let bin = dir.join(format!("{stem}.bin"));
    fs::write(&cfile, c).expect("write the harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wformat=2",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "issue 1367: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored helpers to prove they COMPILE and compute the spec; it must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1367: the {tag} harness built from {NDI_SOURCE} does NOT COMPILE under -Wall -Wextra \
         -Wformat=2 -Wconversion -Werror — very likely a real compile error heading for CI:\n\
         --- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1367: the compiled harness failed to execute");
    // This call's own files only; a failed removal leaves a uniquely named scratch file behind.
    fs::remove_file(&cfile).ok();
    fs::remove_file(&bin).ok();
    assert!(
        run.status.success(),
        "issue 1367: the {tag} harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|l| l.to_string())
        .collect()
}
