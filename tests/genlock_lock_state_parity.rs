//! #1298 — an EXECUTABLE C-vs-Rust parity gate for the genlock lock-state decision.
//!
//! `src/genlock_lock_state.rs` is the Tier-0 authority and
//! `vendor/obs-studio/frontend/widgets/GenlockLockState.hpp` is the production port the
//! OBS statusbar widget actually calls; the two are required to be numerically identical.
//! A static text anchor (see `tests/genlock_preload.rs`) proves the C still *says* the
//! right thing, but the frontend is compiled only by the Windows/Linux genlock workflows,
//! so nothing else executes it at all.
//!
//! This gate closes that hole: it lifts the `genlock_decide_lock_state` block VERBATIM out
//! of the header, compiles it standalone with `cc`, runs the C decision over an exhaustive
//! small grid of facets, and requires byte-identical `(state, reason)` from
//! [`camera_box::genlock_lock_state::decide`] on the same inputs. A divergence on either
//! side — a flipped precedence, a dropped branch, a renamed reason — fails here in seconds
//! instead of surviving to a live rig.
//!
//! Per the project's test-strictness rule this FAILS LOUDLY rather than skipping if the C
//! toolchain is missing — a parity test that silently passes without running is worse than
//! no test.

use camera_box::genlock_lock_state::{decide, GenlockFacets};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Lift the contiguous `genlock_lock_state` enum + reason enum + facets struct + the
/// `genlock_decide_lock_state` function VERBATIM out of the header.
fn lift_decision() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src.find("typedef enum genlock_lock_state {").unwrap_or_else(|| {
        panic!("#1298: {HEADER} no longer defines `typedef enum genlock_lock_state` — the pure decision block is gone, nothing to check parity against.")
    });
    let func = src
        .find("static inline genlock_lock_state_t genlock_decide_lock_state(")
        .unwrap_or_else(|| {
            panic!("#1298: {HEADER} no longer defines the genlock_decide_lock_state function")
        });
    assert!(
        func > start,
        "#1298: the enums and genlock_decide_lock_state are no longer contiguous in {HEADER} — keep the block together or the lift splices unrelated code."
    );
    let end = src[func..]
        .find("\n}\n")
        .map(|i| func + i + 3)
        .expect("#1298: genlock_decide_lock_state has no closing brace");
    src[start..end].to_string()
}

/// The facet grid both sides must agree on: an exhaustive sweep of all 2^8 boolean-flag
/// combinations (the #1303 `audio_unpaired` is the 8th flag, bit 128) crossed with a small set of
/// (n_inputs, n_locked) pairs — including the impossible `n_locked > n_inputs` (the decision is
/// total and both ports must treat it identically).
fn vectors() -> Vec<GenlockFacets> {
    let counts = [
        (0u32, 0u32),
        (1, 0),
        (1, 1),
        (3, 0),
        (3, 2),
        (3, 3),
        (7, 0),
        (7, 6),
        (7, 7),
        (2, 7),
    ];
    let mut v = Vec::new();
    for &(n_inputs, n_locked) in &counts {
        for bits in 0u32..(1 << 8) {
            v.push(GenlockFacets {
                n_inputs,
                n_locked,
                recent_event: bits & 1 != 0,
                qpc_drift_beyond_bound: bits & 2 != 0,
                clock_present: bits & 4 != 0,
                clock_locked: bits & 8 != 0,
                clock_ntp_failed: bits & 16 != 0,
                output_present: bits & 32 != 0,
                output_stamping: bits & 64 != 0,
                audio_unpaired: bits & 128 != 0,
            });
        }
    }
    v
}

#[test]
fn c_lock_state_decision_matches_the_rust_authority_1298() {
    let block = lift_decision();
    let vs = vectors();

    // --- build the C harness --------------------------------------------------------
    let mut c = String::from("#include <stdio.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n    genlock_lock_facets_t f; genlock_lock_reason_t r; genlock_lock_state_t s;\n");
    for g in &vs {
        c.push_str(&format!(
            "    f.n_inputs={}; f.n_locked={}; f.recent_event={}; f.qpc_drift_beyond_bound={}; \
             f.clock_present={}; f.clock_locked={}; f.clock_ntp_failed={}; f.output_present={}; f.output_stamping={}; f.audio_unpaired={};\n\
             \x20   s=genlock_decide_lock_state(&f,&r); printf(\"%d %d\\n\",(int)s,(int)r);\n",
            g.n_inputs,
            g.n_locked,
            g.recent_event as i32,
            g.qpc_drift_beyond_bound as i32,
            g.clock_present as i32,
            g.clock_locked as i32,
            g.clock_ntp_failed as i32,
            g.output_present as i32,
            g.output_stamping as i32,
            g.audio_unpaired as i32,
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_lock_state_parity_1298");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("parity.c");
    let bin = dir.join("parity.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    // --- compile (loudly, never skipped) --------------------------------------------
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1298: could not run the C compiler `{cc}` ({e}). This gate compiles the \
                 vendored genlock_decide_lock_state to prove the C and the Rust authority agree \
                 numerically; it must FAIL rather than skip when the toolchain is absent. \
                 Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1298: genlock_decide_lock_state lifted from {HEADER} does NOT COMPILE standalone under \
         -Wall -Wextra -Werror. The frontend is otherwise built only by the genlock workflows, so \
         this is very likely a real compile error heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    // --- run + compare ---------------------------------------------------------------
    let run = Command::new(&bin)
        .output()
        .expect("#1298: the compiled parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1298: the parity harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<(u8, u8)> = stdout
        .lines()
        .map(|l| {
            let mut it = l.split_whitespace();
            let s: u8 = it.next().unwrap().parse().expect("state int");
            let r: u8 = it.next().unwrap().parse().expect("reason int");
            (s, r)
        })
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "#1298: the harness printed {} lines for {} vectors",
        c_out.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (g, got_c) in vs.iter().zip(&c_out) {
        let (state, reason) = decide(g);
        let got_rs = (state.code(), reason.code());
        if got_rs != *got_c {
            diffs.push(format!(
                "  n_inputs={} n_locked={} recent={} qpc={} clk_present={} clk_locked={} ntp={} out_present={} out_stamp={} -> C {:?}, Rust {:?}",
                g.n_inputs, g.n_locked, g.recent_event as i32, g.qpc_drift_beyond_bound as i32,
                g.clock_present as i32, g.clock_locked as i32, g.clock_ntp_failed as i32,
                g.output_present as i32, g.output_stamping as i32, got_c, got_rs
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1298: the vendored C genlock_decide_lock_state DIVERGED from the Tier-0 Rust authority on \
         {} of {} vectors. These two are required to be numerically identical — the Rust one is \
         unit-tested and the C one is what ships to the rig, so a divergence means the deployed \
         indicator is not the behaviour any test covers:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
