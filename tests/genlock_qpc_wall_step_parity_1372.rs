//! Issue 1372 — an EXECUTABLE C-vs-Rust parity gate for the LOCK indicator's date-step booking.
//!
//! `camera_box::genlock_lock_state::qpc_wall_step_rebase_ms` is the Tier-0 authority and
//! `genlock_qpc_wall_step_rebase_ms` in `vendor/obs-studio/frontend/widgets/GenlockLockState.hpp`
//! is the production port the statusbar widget calls when the wall clock steps against the media
//! clock (a coordinated dantesync fleet date step). The frontend is compiled only by the genlock
//! workflows, so this gate lifts the function VERBATIM, compiles it standalone with `cc` under
//! `-Wall -Wextra -Wconversion -Werror` and requires identical results from both ports.
//!
//! Kept in its own file (the review of the first cut: `genlock_lock_state_parity.rs` was already
//! past the file budget). FAILS LOUDLY rather than skipping if the C toolchain is missing.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Issue 1372 — lift the `genlock_qpc_wall_step_rebase_ms` function VERBATIM out of the header (it
/// sits right after `genlock_qpc_drift_beyond_bound`, before the media-clock block, so this is a
/// separate lift). The widget books a coordinated dantesync date step with it and stays LOCKED; a
/// clock SET and a step STORM must still stay in the qpc history and DEGRADE.
fn lift_qpc_wall_step_rebase() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline int64_t genlock_qpc_wall_step_rebase_ms(")
        .unwrap_or_else(|| {
            panic!("issue 1372: {HEADER} no longer defines genlock_qpc_wall_step_rebase_ms — the date-step booking's C mirror is gone, nothing to parity-check.")
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("issue 1372: genlock_qpc_wall_step_rebase_ms has no closing brace");
    src[start..end].to_string()
}

#[test]
fn c_qpc_wall_step_rebase_matches_the_rust_authority_1372() {
    use camera_box::genlock_lock_state::{
        qpc_wall_step_rebase_ms, GENLOCK_QPC_STEP_BOUND_MS, GENLOCK_QPC_WALL_STEPS_PER_WINDOW,
        GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS,
    };
    let block = lift_qpc_wall_step_rebase();
    let (sb, bm, spw) = (
        GENLOCK_QPC_STEP_BOUND_MS,
        GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS,
        GENLOCK_QPC_WALL_STEPS_PER_WINDOW,
    );
    // (jump_ms, booked_in_window) at the production bounds, plus the i64 extremes: the logged −51 ms
    // date step, both bound edges, the book-limit edges, a clock set and a second step in the window.
    let mut vs: Vec<(i64, i64)> = Vec::new();
    for j in [
        -51,
        51,
        0,
        1,
        -1,
        33,
        -33,
        34,
        -34,
        65,
        66,
        67,
        -66,
        -67,
        200,
        -200,
        3_600_000,
        -3_600_000,
        i64::MAX,
        i64::MIN,
        i64::MIN + 1,
    ] {
        for booked in [0, 1, 2] {
            vs.push((j, booked));
        }
    }

    let mut c = String::from("#include <stdio.h>\n#include <stdint.h>\n#include <inttypes.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n");
    for &(j, booked) in &vs {
        // i64::MIN is not a valid C integer literal; spell it as (-MAX - 1).
        let jl = if j == i64::MIN {
            "(-9223372036854775807LL - 1)".to_string()
        } else {
            format!("{j}LL")
        };
        c.push_str(&format!(
            "    printf(\"%\" PRId64 \"\\n\", genlock_qpc_wall_step_rebase_ms({jl}, {sb}LL, {bm}LL, {booked}LL, {spw}LL));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_qpc_wall_step_parity_1372");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("rebase.c");
    let bin = dir.join("rebase.bin");
    fs::write(&cfile, &c).expect("write the parity harness");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Wconversion", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1372: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored genlock_qpc_wall_step_rebase_ms to prove the C and the Rust authority agree; it must FAIL rather than skip. Install a C compiler or set CC.")
        });
    assert!(
        out.status.success(),
        "issue 1372: genlock_qpc_wall_step_rebase_ms lifted from {HEADER} does NOT COMPILE standalone under -Wall -Wextra -Wconversion -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1372: the compiled rebase parity harness failed to execute");
    assert!(run.status.success(), "issue 1372: harness exited non-zero");
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<i64> = stdout
        .lines()
        .map(|l| l.trim().parse().expect("an i64 per line"))
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "issue 1372: harness printed {} lines",
        c_out.len()
    );
    let mut diffs = Vec::new();
    for (&(j, booked), &cv) in vs.iter().zip(&c_out) {
        let rv = qpc_wall_step_rebase_ms(j, sb, bm, booked, spw);
        if rv != cv {
            diffs.push(format!("  jump={j} booked={booked} -> C {cv}, Rust {rv}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1372: the vendored C genlock_qpc_wall_step_rebase_ms DIVERGED from the Rust authority on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
    // The vectors must reach every branch: a booked step, a sub-bound jump, a clock set and a storm.
    let r = |j, b| qpc_wall_step_rebase_ms(j, sb, bm, b, spw);
    assert!(r(-51, 0) == -51 && r(33, 0) == 0 && r(67, 0) == 0 && r(-51, 1) == 0);
}
