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

use camera_box::genlock_forced_table_audit::is_camera_input;
use camera_box::genlock_lock_state::{
    decide, input_phase_events, GenlockFacets, InputEventCounts, MediaClock,
};
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
/// combinations (the #1303 `audio_unpaired` is the 8th flag bit 128, `audio_unexpected` the 9th bit 256)
/// crossed with the three issue-1372-part-D `media_clock` verdicts and a small set of
/// (n_inputs, n_locked, n_absent) triples — including the impossible `n_locked > n_inputs` and
/// `n_absent > n_inputs` (the decision is total and both ports must treat them identically). The
/// #1299 n_absent axis exercises: none absent (old behaviour), some absent with a real connected
/// unlocked (DEGRADED), some absent with all connected locked (LOCKED), and ALL absent (HEALTHY-idle).
fn vectors() -> Vec<GenlockFacets> {
    // (n_inputs, n_locked, n_absent, n_idle) — #1341 added the n_idle axis (a connected-but-idle
    // input excluded from n_connected AND n_locked). Includes some-idle-all-live-locked -> LOCKED,
    // some-idle-with-a-live-unlocked -> DEGRADED, all-idle -> HEALTHY-idle, and the impossible
    // n_absent + n_idle > n_inputs (both ports saturate n_connected to 0).
    let counts = [
        (0u32, 0u32, 0u32, 0u32),
        (1, 0, 0, 0),
        (1, 1, 0, 0),
        (3, 0, 0, 0),
        (3, 2, 0, 0),
        (3, 3, 0, 0),
        (7, 0, 0, 0),
        (7, 6, 0, 0),
        (7, 7, 0, 0),
        (2, 7, 0, 0),
        // #1299 — absent-sender axis
        (4, 3, 1, 0), // 3 connected+locked of 4, 1 absent -> LOCKED (the reopen scenario)
        (4, 2, 1, 0), // 3 connected, only 2 locked -> DEGRADED
        (4, 0, 4, 0), // all senders absent -> HEALTHY-idle LOCKED
        (3, 0, 1, 0), // 2 connected, none locked -> UNLOCKED no_input_locked
        (7, 5, 2, 0), // 5 connected+locked of 5 connected -> LOCKED
        (7, 4, 2, 0), // 5 connected, 4 locked -> DEGRADED
        (2, 3, 5, 0), // n_absent > n_inputs (impossible) -> both ports saturate n_connected to 0
        // #1341 — idle-input axis
        (12, 2, 0, 10), // 2 live+locked, 10 idle -> LOCKED (the cg-OBS SongPlayer scenario)
        (12, 1, 0, 10), // 2 live, 1 locked -> DEGRADED (a live input genuinely unlocked)
        (4, 0, 0, 4),   // all idle -> HEALTHY-idle LOCKED
        (7, 3, 1, 3),   // 3 live+locked of 3 connected (1 absent, 3 idle) -> LOCKED
        (7, 2, 1, 3),   // 3 connected-live, only 2 locked -> DEGRADED
        (3, 0, 2, 3),   // n_absent + n_idle > n_inputs (impossible) -> saturate n_connected to 0
    ];
    let mut v = Vec::new();
    let media = [MediaClock::Ok, MediaClock::Drift, MediaClock::Undisciplined];
    for &(n_inputs, n_locked, n_absent, n_idle) in &counts {
        for bits in 0u32..(1 << 9) {
            for &media_clock in &media {
                v.push(GenlockFacets {
                    n_inputs,
                    n_locked,
                    n_absent,
                    n_idle,
                    recent_event: bits & 1 != 0,
                    qpc_drift_beyond_bound: bits & 2 != 0,
                    clock_present: bits & 4 != 0,
                    clock_locked: bits & 8 != 0,
                    clock_ntp_failed: bits & 16 != 0,
                    output_present: bits & 32 != 0,
                    output_stamping: bits & 64 != 0,
                    audio_unpaired: bits & 128 != 0,
                    audio_unexpected: bits & 256 != 0, // #1303 the 9th flag
                    media_clock,                       // issue 1372 part D
                });
            }
        }
    }
    v
}

#[test]
fn c_lock_state_decision_matches_the_rust_authority_1298() {
    let block = lift_decision();
    let vs = vectors();

    // --- build the C harness --------------------------------------------------------
    // One table row per vector + one loop (issue 1372 part D tripled the grid with the media_clock
    // axis; ~35k straight-line assignments took ~1 min to compile at -O1, a table compiles in <1 s).
    let mut c = String::from("#include <stdio.h>\n");
    c.push_str(&block);
    c.push_str("static const int V[][14] = {\n");
    for g in &vs {
        c.push_str(&format!(
            "    {{{},{},{},{},{},{},{},{},{},{},{},{},{},{}}},\n",
            g.n_inputs,
            g.n_locked,
            g.n_absent,
            g.n_idle,
            g.recent_event as i32,
            g.qpc_drift_beyond_bound as i32,
            g.clock_present as i32,
            g.clock_locked as i32,
            g.clock_ntp_failed as i32,
            g.output_present as i32,
            g.output_stamping as i32,
            g.audio_unpaired as i32,
            g.audio_unexpected as i32,
            g.media_clock.code(),
        ));
    }
    c.push_str(
        "};\n\
         int main(void){\n\
         \x20   genlock_lock_facets_t f; genlock_lock_reason_t r; genlock_lock_state_t s; size_t i;\n\
         \x20   for (i = 0; i < sizeof(V) / sizeof(V[0]); i++) {\n\
         \x20       f.n_inputs=V[i][0]; f.n_locked=V[i][1]; f.n_absent=V[i][2]; f.n_idle=V[i][3];\n\
         \x20       f.recent_event=V[i][4]; f.qpc_drift_beyond_bound=V[i][5]; f.clock_present=V[i][6];\n\
         \x20       f.clock_locked=V[i][7]; f.clock_ntp_failed=V[i][8]; f.output_present=V[i][9];\n\
         \x20       f.output_stamping=V[i][10]; f.audio_unpaired=V[i][11]; f.audio_unexpected=V[i][12];\n\
         \x20       f.media_clock=V[i][13];\n\
         \x20       s=genlock_decide_lock_state(&f,&r); printf(\"%d %d\\n\",(int)s,(int)r);\n\
         \x20   }\n",
    );
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
                "  n_inputs={} n_locked={} n_absent={} n_idle={} recent={} qpc={} clk_present={} clk_locked={} ntp={} out_present={} out_stamp={} audio_unpaired={} audio_unexpected={} media_clock={} -> C {:?}, Rust {:?}",
                g.n_inputs, g.n_locked, g.n_absent, g.n_idle, g.recent_event as i32, g.qpc_drift_beyond_bound as i32,
                g.clock_present as i32, g.clock_locked as i32, g.clock_ntp_failed as i32,
                g.output_present as i32, g.output_stamping as i32, g.audio_unpaired as i32, g.audio_unexpected as i32,
                g.media_clock.code(), got_c, got_rs
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

/// #1299 Part 3 — lift the `genlock_input_phase_events` function VERBATIM out of the header (it sits
/// AFTER `genlock_decide_lock_state`, so [`lift_decision`] never captures it and this is a separate
/// lift). The parity gate compiles it standalone and sweeps it against the Rust authority.
fn lift_phase_events() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline uint64_t genlock_input_phase_events(")
        .unwrap_or_else(|| {
            panic!("#1299: {HEADER} no longer defines genlock_input_phase_events — the connected-phase recent-event rule's C mirror is gone, nothing to parity-check.")
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1299: genlock_input_phase_events has no closing brace");
    src[start..end].to_string()
}

#[test]
fn c_input_phase_events_matches_the_rust_authority_1299() {
    let block = lift_phase_events();

    // The grid both sides must agree on: the connected + #1341 idle flags crossed with a spread of
    // per-class counts including 0, small, and a saturating extreme (UINT64_MAX) to exercise the
    // clamp. An idle input (connected && idle) must contribute 0 exactly like a disconnected one.
    let big = u64::MAX;
    let counts = [0u64, 1, 2, 7, 25, 500, big];
    let mut vs: Vec<InputEventCounts> = Vec::new();
    for &connected in &[false, true] {
        for &idle in &[false, true] {
            for &r in &counts {
                for &(l, b) in &[(0u64, 0u64), (3, 0), (0, 4), (2, 5), (big, 0), (0, big)] {
                    vs.push(InputEventCounts {
                        connected,
                        idle,
                        relocks: r,
                        late_holds: l,
                        backward_steps: b,
                    });
                }
            }
        }
    }

    // --- build the C harness --------------------------------------------------------
    let mut c = String::from("#include <stdio.h>\n#include <stdint.h>\n#include <inttypes.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n");
    for v in &vs {
        c.push_str(&format!(
            "    printf(\"%\" PRIu64 \"\\n\", genlock_input_phase_events({},{},{}ULL,{}ULL,{}ULL));\n",
            v.connected as i32, v.idle as i32, v.relocks, v.late_holds, v.backward_steps
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_phase_events_parity_1299");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("phase.c");
    let bin = dir.join("phase.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "#1299: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 genlock_input_phase_events to prove the C and the Rust authority agree; it must \
                 FAIL rather than skip when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1299: genlock_input_phase_events lifted from {HEADER} does NOT COMPILE standalone under \
         -Wall -Wextra -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1299: the compiled phase-events parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1299: harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<u64> = stdout
        .lines()
        .map(|l| l.trim().parse().expect("phase-events u64"))
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "#1299: harness printed {} lines for {} vectors",
        c_out.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (v, &got_c) in vs.iter().zip(&c_out) {
        let got_rs = input_phase_events(v);
        if got_rs != got_c {
            diffs.push(format!(
                "  connected={} idle={} relocks={} late={} backward={} -> C {}, Rust {}",
                v.connected as i32,
                v.idle as i32,
                v.relocks,
                v.late_holds,
                v.backward_steps,
                got_c,
                got_rs
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1299: the vendored C genlock_input_phase_events DIVERGED from the Rust authority on {} of \
         {} vectors — the connected-phase recent-event rule must be numerically identical on both \
         ports:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}

/// #1303 — lift the `genlock_ci_contains` + `genlock_name_is_camera` helpers VERBATIM out of the
/// header (they sit AFTER `genlock_input_phase_events`, contiguous). The camera classifier the
/// statusbar widget uses for the audio_unexpected term must stay byte-for-byte identical to the
/// canonical Rust `genlock_forced_table_audit::is_camera_input`, or the fleet indicator would grade
/// audio on a different notion of "camera" than the deploy-time certified-table preflight.
fn lift_is_camera() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline int genlock_ci_contains(")
        .unwrap_or_else(|| {
            panic!("#1303: {HEADER} no longer defines genlock_ci_contains — the camera classifier's C mirror is gone, nothing to parity-check.")
        });
    let cam = src
        .find("static inline int genlock_name_is_camera(")
        .unwrap_or_else(|| panic!("#1303: {HEADER} no longer defines genlock_name_is_camera"));
    assert!(
        cam > start,
        "#1303: genlock_ci_contains + genlock_name_is_camera are no longer contiguous in {HEADER}"
    );
    let end = src[cam..]
        .find("\n}\n")
        .map(|i| cam + i + 3)
        .expect("#1303: genlock_name_is_camera has no closing brace");
    src[start..end].to_string()
}

#[test]
fn c_name_is_camera_matches_the_rust_authority_1303() {
    let block = lift_is_camera();
    // Real rig names + edge cases: usb capture cards, cam+digit, program/music sources, no-digit
    // "camera" words, empty. The C mirror must agree with the canonical Rust on every one.
    let names = [
        "CAM3 (usb)",
        "CAM1 (usb)",
        "cam 2",
        "camera1",
        "cam7",
        "sp-fast_video",
        "cg",
        "NDI 2ME PGM",
        "mbc",
        "NDI obs hudba",
        "NDIAr cg",
        "VBAN cg-resolume",
        "CAMERA",
        "cam",
        "",
    ];

    let mut c = String::from("#include <stdio.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n");
    for n in &names {
        // Names are ASCII; escape backslash + quote defensively for the C literal.
        let esc = n.replace('\\', "\\\\").replace('"', "\\\"");
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_name_is_camera(\"{esc}\"));\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_name_is_camera_parity_1303");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("cam.c");
    let bin = dir.join("cam.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("#1303: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored genlock_name_is_camera to prove it agrees with the Rust is_camera_input; it must FAIL rather than skip. Install a C compiler or set CC.")
        });
    assert!(
        out.status.success(),
        "#1303: genlock_name_is_camera lifted from {HEADER} does NOT COMPILE standalone under -Wall -Wextra -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("#1303: the compiled is-camera parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1303: harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<bool> = stdout.lines().map(|l| l.trim() == "1").collect();
    assert_eq!(
        c_out.len(),
        names.len(),
        "#1303: harness printed {} lines for {} names",
        c_out.len(),
        names.len()
    );

    let mut diffs = Vec::new();
    for (n, &got_c) in names.iter().zip(&c_out) {
        let got_rs = is_camera_input(n);
        if got_rs != got_c {
            diffs.push(format!("  {n:?} -> C {got_c}, Rust {got_rs}"));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1303: the vendored C genlock_name_is_camera DIVERGED from the canonical Rust is_camera_input on {} of {} names — the LOCK-indicator audio_unexpected term must classify a camera identically to the certified-table preflight:\n{}",
        diffs.len(), names.len(), diffs.join("\n")
    );
}

/// #1299 Part 4 — lift the `genlock_qpc_drift_beyond_bound` function VERBATIM out of the header (it sits
/// AFTER `genlock_name_is_camera`, contiguous, so this is a separate lift). It writes a double
/// out-param, so the harness compares BOTH the int verdict and the measured-rate telemetry
/// against the Rust authority `camera_box::genlock_lock_state::qpc_drift_beyond_bound`.
fn lift_qpc_drift() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("static inline int genlock_qpc_drift_beyond_bound(")
        .unwrap_or_else(|| {
            panic!("#1299 Part 4: {HEADER} no longer defines genlock_qpc_drift_beyond_bound — the windowed wall-vs-QPC drift decision's C mirror is gone, nothing to parity-check.")
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1299 Part 4: genlock_qpc_drift_beyond_bound has no closing brace");
    src[start..end].to_string()
}

#[test]
fn c_qpc_drift_beyond_bound_matches_the_rust_authority_1299_part4() {
    use camera_box::genlock_lock_state::qpc_drift_beyond_bound;
    let block = lift_qpc_drift();

    // (rate_ready, drift_delta_ms, elapsed_ms, max_step_ms, step_bound_ms) — #1357 scope C: the
    // verdict is the wall STEP only (one semantics on every box); the windowed rate is report-only
    // telemetry both ports must still compute identically. Reopen shapes, the live 24.9. shapes
    // (strih-lx 0 ppm, stream 23.4 ppm) + edge cases (negative drift/step, elapsed 0, at-bound).
    let vs: [(i32, i64, i64, i64, i64); 14] = [
        (1, 641, 45_000_000, 0, 33), // overnight strih slope ≈14.24 ppm -> not beyond
        (0, 0, 0, 40, 33),           // 40 ms step -> beyond (rate not ready)
        (1, 6, 45_000, 1, 33),       // ≈133 ppm rate, sub-frame step -> not beyond (#1357)
        (0, 999, 1000, 0, 33),       // not ready, big delta ignored -> not beyond, 0.0
        (0, 0, 0, -40, 33),          // negative step magnitude -> beyond
        (1, -641, 45_000_000, 0, 33), // wall stepping back slowly -> not beyond
        (1, 5, 0, 0, 33),            // rate_ready but elapsed 0 -> measured 0.0 guard
        (1, 0, 300_000, 0, 33),      // strih-lx live: disciplined monotonic, 0 ppm -> not beyond
        (1, 7, 299_000, 1, 33),      // stream live 05:09:00: 23.4 ppm -> not beyond
        (1, 0, 300_000, 33, 33),     // step exactly at bound (not > ) -> not beyond
        (1, 0, 300_000, 34, 33),     // step one over bound -> beyond
        (1, 186, 300_000, 0, 33),    // 620 ppm rate alone -> not beyond (#1357)
        (0, 0, 0, 0, 33),            // nothing happening -> not beyond
        (1, 47, 299_000, 41, 33),    // stream-shaped window carrying a 41 ms step -> beyond
    ];

    let mut c = String::from("#include <stdio.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n    double m; int r;\n");
    for &(ready, dd, el, ms, sb) in &vs {
        c.push_str(&format!(
            "    m=0; r=genlock_qpc_drift_beyond_bound({ready},{dd}LL,{el}LL,{ms}LL,{sb}LL,&m); printf(\"%d %.9g\\n\", r, m);\n"
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_qpc_drift_parity_1299p4");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("qpc.c");
    let bin = dir.join("qpc.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args(["-std=gnu99", "-Wall", "-Wextra", "-Werror", "-O1"])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("#1299 Part 4: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored genlock_qpc_drift_beyond_bound to prove the C and the Rust authority agree; it must FAIL rather than skip. Install a C compiler or set CC.")
        });
    assert!(
        out.status.success(),
        "#1299 Part 4: genlock_qpc_drift_beyond_bound lifted from {HEADER} does NOT COMPILE standalone under -Wall -Wextra -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("#1299 Part 4: the compiled qpc-drift parity harness failed to execute");
    assert!(
        run.status.success(),
        "#1299 Part 4: harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let c_out: Vec<(bool, f64)> = stdout
        .lines()
        .map(|l| {
            let mut it = l.split_whitespace();
            let r = it.next().unwrap() == "1";
            let m: f64 = it.next().unwrap().parse().expect("measured ppm f64");
            (r, m)
        })
        .collect();
    assert_eq!(
        c_out.len(),
        vs.len(),
        "#1299 Part 4: harness printed {} lines for {} vectors",
        c_out.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (&(ready, dd, el, ms, sb), &(cr, cm)) in vs.iter().zip(&c_out) {
        let v = qpc_drift_beyond_bound(ready != 0, dd, el, ms, sb);
        let ppm_ok = (v.measured_ppm - cm).abs() <= 1e-6 * v.measured_ppm.abs().max(1.0);
        if v.beyond_bound != cr || !ppm_ok {
            diffs.push(format!(
                "  ready={ready} dd={dd} el={el} step={ms} sb={sb} -> C ({cr},{cm}), Rust ({},{})",
                v.beyond_bound, v.measured_ppm
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1299 Part 4: the vendored C genlock_qpc_drift_beyond_bound DIVERGED from the Rust authority on {} of {} vectors — the windowed wall-vs-QPC drift decision must be numerically identical on both ports:\n{}",
        diffs.len(), vs.len(), diffs.join("\n")
    );
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
        199,
        200,
        201,
        -200,
        -201,
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
    assert!(r(-51, 0) == -51 && r(33, 0) == 0 && r(201, 0) == 0 && r(-51, 1) == 0);
}

/// Issue 1372 part D — lift the media-clock block VERBATIM out of the header: from the
/// `genlock_media_discipline` enum through `genlock_media_clock_verdict`'s closing brace (it sits AFTER
/// `genlock_qpc_drift_beyond_bound`, so no earlier lift is disturbed). The harness compares both the
/// window-drift reduction and the verdict against the Rust authority.
fn lift_media_clock() -> String {
    let path = repo(HEADER);
    let src = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("typedef enum genlock_media_discipline {")
        .unwrap_or_else(|| {
            panic!("issue 1372 part D: {HEADER} no longer defines the genlock_media_discipline enum — the media-clock term's C mirror is gone, nothing to parity-check.")
        });
    let func = src
        .find("static inline genlock_media_clock_t genlock_media_clock_verdict(")
        .unwrap_or_else(|| {
            panic!("issue 1372 part D: {HEADER} no longer defines genlock_media_clock_verdict")
        });
    assert!(
        func > start,
        "issue 1372 part D: the media-clock enums and genlock_media_clock_verdict are no longer contiguous in {HEADER}"
    );
    let end = src[func..]
        .find("\n}\n")
        .map(|i| func + i + 3)
        .expect("issue 1372 part D: genlock_media_clock_verdict has no closing brace");
    src[start..end].to_string()
}

#[test]
fn c_media_clock_matches_the_rust_authority_1372_part_d() {
    use camera_box::genlock_lock_state::{
        media_clock_verdict, media_clock_window, media_clock_window_ready, MediaDiscipline,
        GENLOCK_MEDIA_CLOCK_BAND_US, GENLOCK_MEDIA_CLOCK_MAX_GAP_MS, GENLOCK_MEDIA_CLOCK_WINDOW_S,
    };
    let block = lift_media_clock();
    let (win, gap) = (GENLOCK_MEDIA_CLOCK_WINDOW_S, GENLOCK_MEDIA_CLOCK_MAX_GAP_MS);

    // Window vectors `((t_ms, offset_us) samples, window_s, max_gap_ms)`: the pre-part-A stream rate at
    // 1 Hz, the real dantesync step shapes (a locked master's 1460 µs, a client's 600 µs, a not-locked
    // master's 250 µs, the −146 µs seen on win-resolume) alone and on the rate, a late tick carrying a
    // step, wobble, a gap, jittered ticks, odd/even counts, a non-increasing time, 0/1 samples, a
    // non-positive window, and the i64 extremes the saturating arithmetic must agree on.
    let at_1hz = |n: i64, f: &dyn Fn(i64) -> i64| -> Vec<(i64, i64)> {
        (0..=n).map(|i| (i * 1000, f(i))).collect()
    };
    let mut late = at_1hz(600, &|i| i * 27 / 2);
    for (k, s) in late.iter_mut().enumerate().skip(300) {
        s.0 += 500;
        s.1 += 520 + if k >= 450 { 520 } else { 0 };
    }
    let jittered: Vec<(i64, i64)> = (0..=600i64)
        .map(|i| (i * 1000 + (i * 7919) % 97 - 48, i * 20 + (i * 31) % 41 - 20))
        .collect();
    /// `((t_ms, offset_us) samples, window_s, max_gap_ms)`.
    type Window = (Vec<(i64, i64)>, i64, i64);
    let windows: Vec<Window> = vec![
        (at_1hz(600, &|i| i * 27 / 2), win, gap),
        (at_1hz(600, &|i| (i / 60) * 1460), win, gap),
        (at_1hz(600, &|i| (i / 30) * 600), win, gap),
        (at_1hz(600, &|i| (i / 25) * 250), win, gap),
        (at_1hz(600, &|i| (i / 20) * -146), win, gap),
        (at_1hz(600, &|i| i * 27 / 2 + (i / 60) * 1460), win, gap),
        (late, win, gap),
        (at_1hz(600, &|i| (i * 7919) % 81 - 40), win, gap),
        (
            vec![(0, 0), (1000, 20), (121_000, -14_000), (122_000, -13_980)],
            win,
            gap,
        ),
        (jittered, win, gap),
        (at_1hz(3, &|i| -20 * i), win, gap),
        (vec![(0, 0), (1000, 10), (2000, 30)], win, gap),
        (vec![(0, 0), (3, -1), (6, -1)], win, gap),
        (vec![(0, 0), (3, 1), (6, 3)], win, gap),
        (at_1hz(600, &|i| (i * 9 / 20) * 20), win, gap),
        (at_1hz(600, &|i| (i * 9 / 20) * 40), win, gap),
        (
            (0..=300i64)
                .map(|i| (i * 1000, 0))
                .chain((1..=60i64).map(|k| (300_000 + k * 4000, 60 * k)))
                .collect(),
            win,
            gap,
        ),
        (
            (0..=3000i64)
                .map(|i| (i * 16, i * 16 * 20 / 1000))
                .collect(),
            win,
            gap,
        ),
        (
            vec![
                (0, 0),
                (1000, 0),
                (2000, 0),
                (3000, 0),
                (4000, 0),
                (5000, 0),
                (6000, 100),
            ],
            win,
            gap,
        ),
        (vec![(1000, 0), (1000, 50), (500, 70)], win, gap),
        (vec![(0, 7)], win, gap),
        (vec![], win, gap),
        (vec![(0, 0), (1000, 5)], 0, gap),
        (vec![(0, 0), (1000, 5)], -3, gap),
        (vec![(0, i64::MIN), (1, i64::MAX)], i64::MAX, i64::MAX),
        (
            vec![(0, 0), (1, i64::MIN), (2, i64::MAX)],
            i64::MAX,
            i64::MAX,
        ),
        (
            vec![(i64::MAX - 1, 0), (i64::MAX, 3), (i64::MIN, 4)],
            win,
            i64::MAX,
        ),
    ];
    // Readiness vectors `(counted_ms, window_s)`.
    let readies: Vec<(i64, i64)> = vec![
        (539_999, 600),
        (540_000, 600),
        (600_000, 600),
        (0, 600),
        (0, 0),
        (i64::MAX, -1),
        (i64::MAX, i64::MAX),
        (i64::MIN, 1),
    ];

    let disciplines = [
        MediaDiscipline::Unknown,
        MediaDiscipline::Active,
        MediaDiscipline::Disabled,
        MediaDiscipline::ReadFailed,
        MediaDiscipline::ApiMissing,
        MediaDiscipline::NotApplicable,
    ];
    let drifts = [
        0i64,
        1,
        1999,
        2000,
        2001,
        -2000,
        -2001,
        8100,
        -67_000,
        i64::MIN,
        i64::MAX,
    ];
    let mut verdicts: Vec<(bool, i64, i64, MediaDiscipline, bool)> = Vec::new();
    for &ready in &[false, true] {
        for &d in &drifts {
            for &disc in &disciplines {
                for &present in &[false, true] {
                    verdicts.push((ready, d, 2000, disc, present));
                }
            }
        }
    }

    let lit = |v: i64| -> String {
        if v == i64::MIN {
            "INT64_MIN".to_string()
        } else {
            format!("INT64_C({v})")
        }
    };
    let arr = |v: Vec<String>| {
        if v.is_empty() {
            "0".to_string()
        } else {
            v.join(",")
        }
    };
    let mut c = String::from("#include <stdio.h>\n#include <stdint.h>\n#include <inttypes.h>\n");
    c.push_str(&block);
    c.push_str("int main(void){\n    int64_t counted;\n");
    // Every window runs against several step bands: the production one, exact (0), none kept (-1), a
    // wide one and all-inclusive.
    let bands = [GENLOCK_MEDIA_CLOCK_BAND_US, 0, -1, 1000, i64::MAX];
    for (i, (w, ws, g)) in windows.iter().enumerate() {
        let ts: Vec<String> = w.iter().map(|s| lit(s.0)).collect();
        let ds: Vec<String> = w.iter().map(|s| lit(s.1)).collect();
        c.push_str(&format!(
            "    {{ static const int64_t t{i}[] = {{{}}}; static const int64_t d{i}[] = {{{}}}; static int64_t s{i}[{}]; static const int64_t b{i}[] = {{{}}}; for (size_t k = 0; k < sizeof(b{i}) / sizeof(b{i}[0]); k++) {{ counted = -1; const int64_t r = genlock_media_clock_window_drift_us(t{i}, d{i}, {}, {}, {}, b{i}[k], s{i}, &counted); printf(\"W %\" PRId64 \" %\" PRId64 \"\\n\", r, counted); }} }}\n",
            arr(ts),
            arr(ds),
            w.len().max(1),
            arr(bands.iter().map(|b| lit(*b)).collect()),
            w.len(),
            lit(*ws),
            lit(*g)
        ));
    }
    for &(counted, ws) in &readies {
        c.push_str(&format!(
            "    printf(\"R %d\\n\", genlock_media_clock_window_ready({}, {}));\n",
            lit(counted),
            lit(ws)
        ));
    }
    for &(ready, d, bound, disc, present) in &verdicts {
        c.push_str(&format!(
            "    printf(\"V %d\\n\", (int)genlock_media_clock_verdict({}, {}, INT64_C({bound}), {}, {}));\n",
            i32::from(ready),
            lit(d),
            disc.code(),
            i32::from(present)
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("genlock_media_clock_parity_1372");
    fs::create_dir_all(&dir).expect("create the parity scratch dir");
    let cfile = dir.join("media.c");
    let bin = dir.join("media.bin");
    fs::write(&cfile, &c).expect("write the parity harness");

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(&cc)
        .args([
            "-std=gnu99",
            "-Wall",
            "-Wextra",
            "-Wconversion",
            "-Werror",
            "-O1",
        ])
        .arg(&cfile)
        .arg("-o")
        .arg(&bin)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1372 part D: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored media-clock term to prove the C and the Rust authority agree; it must FAIL rather than skip. Install a C compiler or set CC.")
        });
    assert!(
        out.status.success(),
        "issue 1372 part D: the media-clock block lifted from {HEADER} does NOT COMPILE standalone under -Wall -Wextra -Wconversion -Werror:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1372 part D: the compiled media-clock parity harness failed to execute");
    assert!(
        run.status.success(),
        "issue 1372 part D: harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let pick = |tag: &str| -> Vec<Vec<i64>> {
        stdout
            .lines()
            .filter_map(|l| l.strip_prefix(tag))
            .map(|v| {
                v.split_whitespace()
                    .map(|x| x.parse().expect("an i64"))
                    .collect()
            })
            .collect()
    };
    let (c_windows, c_ready, c_verdicts) = (pick("W "), pick("R "), pick("V "));
    assert_eq!(
        c_windows.len(),
        windows.len() * bands.len(),
        "issue 1372 part D: window line count"
    );
    assert_eq!(
        c_ready.len(),
        readies.len(),
        "issue 1372 part D: readiness line count"
    );
    assert_eq!(
        c_verdicts.len(),
        verdicts.len(),
        "issue 1372 part D: verdict line count"
    );

    let mut diffs = Vec::new();
    let runs: Vec<(&Window, i64)> = windows
        .iter()
        .flat_map(|w| bands.iter().map(move |b| (w, *b)))
        .collect();
    for (((w, ws, g), band), cv) in runs.iter().zip(&c_windows) {
        let rv = media_clock_window(w, *ws, *g, *band);
        if cv[..] != [rv.drift_us, rv.counted_ms] {
            diffs.push(format!(
                "  window len={} first={:?} window_s={ws} gap={g} band={band} -> C {cv:?}, Rust {rv:?}",
                w.len(),
                w.first()
            ));
        }
    }

    // The python mirror (scripts/genlock_lock_decision.py, the fleet watchdog's decision module) gets
    // the same real-range runs: python ints never saturate, so the i64-extreme vectors stay C/Rust only.
    let real = |v: i64| v.unsigned_abs() < 1_000_000_000_000;
    let py_runs: Vec<(&Window, i64)> = runs
        .iter()
        .copied()
        .filter(|((w, ws, g), band)| {
            w.iter().all(|(t, o)| real(*t) && real(*o))
                && real(*ws)
                && real(*g)
                && (real(*band) || *band == i64::MAX)
        })
        .collect();
    let py_in: Vec<String> = py_runs
        .iter()
        .map(|((w, ws, g), band)| {
            let pts: Vec<String> = w.iter().map(|(t, o)| format!("[{t},{o}]")).collect();
            format!("[[{}],{ws},{g},{band}]", pts.join(","))
        })
        .collect();
    let py_file = dir.join("media_py_runs.json");
    fs::write(&py_file, format!("[{}]", py_in.join(","))).expect("write the python runs");
    let py = Command::new("python3")
        .arg("-c")
        .arg(
            "import json, sys; sys.path.insert(0, sys.argv[1]); import genlock_lock_decision as d\n\
             for w, ws, g, b in json.load(open(sys.argv[2])):\n\
             \x20   print(*d.media_clock_window([tuple(x) for x in w], ws, g, b))",
        )
        .arg(repo("scripts"))
        .arg(&py_file)
        .output()
        .unwrap_or_else(|e| {
            panic!("issue 1372 part D: could not run python3 ({e}); the python mirror of the media-clock term must be checked, never skipped")
        });
    assert!(
        py.status.success(),
        "issue 1372 part D: the python mirror run failed: {}",
        String::from_utf8_lossy(&py.stderr)
    );
    let py_out: Vec<Vec<i64>> = String::from_utf8(py.stdout)
        .expect("python stdout is utf-8")
        .lines()
        .map(|l| {
            l.split_whitespace()
                .map(|x| x.parse().expect("an int"))
                .collect()
        })
        .collect();
    assert_eq!(
        py_out.len(),
        py_runs.len(),
        "issue 1372 part D: python line count"
    );
    assert!(
        py_runs.len() > 100,
        "issue 1372 part D: too few real-range runs reach the python mirror"
    );
    for (((w, ws, g), band), pv) in py_runs.iter().zip(&py_out) {
        let rv = media_clock_window(w, *ws, *g, *band);
        if pv[..] != [rv.drift_us, rv.counted_ms] {
            diffs.push(format!(
                "  PYTHON window len={} first={:?} window_s={ws} gap={g} band={band} -> python {pv:?}, Rust {rv:?}",
                w.len(),
                w.first()
            ));
        }
    }
    for (&(counted, ws), cv) in readies.iter().zip(&c_ready) {
        let rv = i64::from(media_clock_window_ready(counted, ws));
        if cv[..] != [rv] {
            diffs.push(format!(
                "  ready counted={counted} window_s={ws} -> C {cv:?}, Rust {rv}"
            ));
        }
    }
    for (&(ready, d, bound, disc, present), cv) in verdicts.iter().zip(&c_verdicts) {
        let rv = i64::from(media_clock_verdict(ready, d, bound, disc, present).code());
        if cv[..] != [rv] {
            diffs.push(format!(
                "  ready={ready} drift={d} bound={bound} discipline={disc:?} present={present} -> C {cv:?}, Rust {rv}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "issue 1372 part D: the vendored C media-clock term DIVERGED from the Rust authority — the LOCK indicator must decide the audio-clock term identically on both ports:\n{}",
        diffs.join("\n")
    );
}
