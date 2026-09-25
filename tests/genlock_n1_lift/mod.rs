//! Shared by the genlock parity gates (`tests/genlock_relock_selection_parity.rs` and
//! `tests/genlock_shallow_depth_parity_1367.rs`): lift the contiguous #1003 relock-selection
//! block, or the issue-1367 N==1 + #1049 convergence block and its `#define`s, VERBATIM from the
//! vendored `obs-source.c`, compile it standalone under `-Werror` with a test `main`, and return
//! the printed lines. A directory
//! module (`tests/genlock_n1_lift/mod.rs`), so cargo does not build it as a test target of its own.
//! Each including test file uses a different subset, hence the module-level `dead_code` allow.
#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The stub the lifted helpers need: only the fields they actually touch.
pub const RELOCK_PRELUDE: &str = r#"#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdio.h>
struct obs_source_frame { uint64_t timestamp; };
typedef struct obs_source {
    struct { struct obs_source_frame **array; size_t num; } async_frames;
    uint64_t genlock_phase_anchor_ns;
} obs_source_t;
"#;

/// Lift the `#1003` helper block verbatim from the vendored C.
pub fn lift_relock_helpers() -> String {
    let path = repo(OBS_SOURCE);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_abs_diff_ns(")
        .unwrap_or_else(|| {
            panic!(
                "#1003: {OBS_SOURCE} no longer defines genlock_abs_diff_ns — the phase-continuity \
             helpers are gone, so there is nothing to check parity against."
            )
        });
    let last = src
        .find("static inline uint64_t genlock_phase_anchor_from_present(")
        .unwrap_or_else(|| {
            panic!("#1003: {OBS_SOURCE} no longer defines genlock_phase_anchor_from_present")
        });
    let end = src[last..]
        .find("\n}\n")
        .map(|i| last + i + 3)
        .expect("#1003: genlock_phase_anchor_from_present has no closing brace");
    assert!(
        end > start,
        "#1003: the helper block in {OBS_SOURCE} is not contiguous — the lift would splice \
         unrelated code. Keep the four #1003 helpers adjacent."
    );
    src[start..end].to_string()
}

/// #1049 — the SAME executable-parity discipline for the phase-convergence decision. Lifts the
/// self-contained `genlock_phase_converge_due` helper VERBATIM from obs-source.c, compiles it
/// standalone under `-Werror`, and requires byte-identical booleans from
/// [`camera_box::genlock_backlog::should_converge_phase`] over a spread of vectors — a flipped
/// comparison, a lost saturation guard, or a wrong `interval/n` quantum fails here in seconds
/// rather than surviving to the rig.
///
/// issue 1367: the `genlock_n1_*` pure helpers (the N==1 pin-derived depth) sit CONTIGUOUSLY right
/// before `genlock_phase_converge_due`, so the lift takes the whole block from
/// `genlock_n1_tick_wall_ns` to the end of `genlock_phase_converge_due` — verbatim bytes, one
/// contiguous run — and the issue-1367 gates below compile the very same block.
/// `genlock_phase_converge_due` itself is unchanged by issue 1367 (its `n < 2` early return stays;
/// the SOURCE wrapper routes an N==1 tick to `genlock_n1_shed_due`).
pub fn lift_converge_helper() -> String {
    let path = repo(OBS_SOURCE);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_n1_tick_wall_ns(")
        .unwrap_or_else(|| {
            panic!(
                "issue 1367: {OBS_SOURCE} no longer defines genlock_n1_tick_wall_ns — the N==1 \
                 pin-derived depth helpers are gone, so there is nothing to check parity against."
            )
        });
    let converge = src[start..]
        .find("static inline bool genlock_phase_converge_due(")
        .map(|i| start + i)
        .unwrap_or_else(|| {
            panic!(
                "#1049: {OBS_SOURCE} no longer defines genlock_phase_converge_due right after the \
                 issue-1367 N==1 helpers — the phase convergence helper is gone or moved, so \
                 there is nothing to check parity against."
            )
        });
    let end = src[converge..]
        .find("\n}\n")
        .map(|i| converge + i + 3)
        .expect("#1049: genlock_phase_converge_due has no closing brace");
    src[start..end].to_string()
}

/// The `#define`s the lifted convergence + issue-1367 N==1 block references, lifted from the
/// SHIPPED C (never hard-coded — review 🟡2 of #1049).
pub fn converge_defines() -> String {
    [
        "GENLOCK_PHASE_PIN_HYSTERESIS_NS",
        "GENLOCK_DRAIN_MIN_TICK_INTERVAL",
        "GENLOCK_N2_JITTER_BUDGET_NS",
        "GENLOCK_N1_PIN_FRAME_TOLERANCE_NS",
        "GENLOCK_N1_DEEP_MARGIN_FRAMES",
        "GENLOCK_N1_ON_GRID_NS",
        "GENLOCK_N1_SHALLOW_SETTLE_TICKS",
        "GENLOCK_N1_SHALLOW_RELOCK_GAP_NS",
        // design 5830750134: the latch that never latches an outlier.
        "GENLOCK_N1_SHALLOW_MAX_EXTRA_FRAMES",
        "GENLOCK_N1_SHALLOW_HIST_BINS",
        "GENLOCK_N1_SHALLOW_LATCH_PERCENTILE",
        "GENLOCK_N1_SHALLOW_MAX_SPREAD_FRAMES",
        "GENLOCK_N1_SHALLOW_MAX_REJECTS",
        "GENLOCK_N1_SHALLOW_UNDER_TICKS",
        "GENLOCK_N1_SHALLOW_CHURN_RELOCKS",
        "GENLOCK_N1_SHALLOW_CHURN_QUIET_TICKS",
    ]
    .iter()
    .map(|name| lift_define(name))
    .collect::<Vec<_>>()
    .join("\n")
}

/// Compile the lifted block + `main_body` standalone under `-Werror` and return the printed
/// lines. FAILS LOUDLY (never skips) when no compiler is present.
pub fn compile_and_run_n1_block(dirname: &str, main_body: &str) -> Vec<String> {
    compile_and_run_n1_block_with(dirname, "", "", main_body)
}

/// [`compile_and_run_n1_block`] with an extra C `prelude` (before the lifted block, e.g. an
/// `#include` of the REAL `obs-genlock-grid.h`) and `tail` (after it, e.g. a lifted source-side
/// helper that calls into the block).
pub fn compile_and_run_n1_block_with(
    dirname: &str,
    prelude: &str,
    tail: &str,
    main_body: &str,
) -> Vec<String> {
    let mut c = format!(
        "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n#include <stdio.h>\n{prelude}{}\n",
        converge_defines()
    );
    c.push_str(&lift_converge_helper());
    c.push_str(tail);
    c.push_str("int main(void){\n");
    c.push_str(main_body);
    c.push_str("    return 0;\n}\n");
    compile_and_run_c(dirname, &c)
}

/// Write the C harness `c` to a scratch dir, compile it under `-Wall -Wextra -Werror` and return
/// its printed lines (trimmed). FAILS LOUDLY (never skips) when no compiler is present or the
/// lifted code does not compile.
pub fn compile_and_run_c(dirname: &str, c: &str) -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(dirname);
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("harness.c");
    let bin = dir.join("harness.bin");
    fs::write(&cfile, c).expect("write the parity harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 genlock helpers to prove the C and the Rust authority agree; it must FAIL rather \
                 than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "the lifted genlock block from {OBS_SOURCE} does NOT COMPILE standalone under -Wall \
         -Wextra -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("the compiled parity harness failed to execute");
    String::from_utf8(run.stdout)
        .expect("harness stdout is utf-8")
        .lines()
        .map(|l| l.trim().to_string())
        .collect()
}

/// Lift a `#define NAME <value>` line VERBATIM from the vendored C so the parity harness compiles
/// against the SHIPPED constant, never a hard-coded copy that could silently drift (issue-1049
/// review finding 🟡2). Returns the whole `#define …` line. Shared by the #1049 and #1161 gates,
/// so the not-found panic is issue-AGNOSTIC (naming which constant is missing, not a fixed ticket).
pub fn lift_define(name: &str) -> String {
    let src = fs::read_to_string(repo(OBS_SOURCE)).expect("read obs-source.c");
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with(&format!("#define {name} ")) {
            return t.to_string();
        }
    }
    panic!("{OBS_SOURCE} no longer defines {name} — the parity harness cannot lift it");
}
