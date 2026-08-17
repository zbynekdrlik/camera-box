//! #767 (event-critical, 2026-08-13) reconnect-on-sender-restart guard for the vendored DistroAV
//! NDI source receiver thread (`vendor/distroav/src/ndi-source.cpp`).
//!
//! Background: the #764 keep-alive fix (a genlocked source's receiver never sleeps/tears-down on
//! HIDE) is done + deployed. This ticket's REMAINING scope, reported live 2026-08-13, is a
//! DIFFERENT failure: when a genlocked source's SENDER instance restarts (a new NDI instance, e.g.
//! after a box reboot), the receiver stays bound to the dead/half-open connection and NEVER rebinds
//! — 41 minutes of silent black. Root cause: `ndi_source_thread` recreates the receiver ONLY inside
//! the `if (s->config.reset_ndi_receiver)` block (set only by `ndi_source_update`); the steady loop
//! has no self-rebind path, and NDI's own name-based reconnect only re-resolves once
//! `recv_get_no_connections()` drops to 0 — which a hard-reboot half-open connection never does. The
//! only recovery observed live was forcing a full receiver reset (SetInputSettings → reset).
//!
//! The fix: a stale-frame watchdog in the steady loop that, for a genlocked + CONNECTED source with
//! no new frame for `GENLOCK_RECONNECT_STALE_NS`, forces `reset_ndi_receiver` so the existing reset
//! block rebinds. The decision is the pure `static inline genlock_reconnect_decision(...)` helper.
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored C compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md`
//! (#1026 / the parity-gate section) this file (a) SOURCE-ANCHORS the C tokens with a std-only
//! `fs::read_to_string` guard runnable via `rustc --test` (revert protection against a future
//! `git subtree pull`), and (b) LIFTS the pure `genlock_reconnect_decision` helper VERBATIM, compiles
//! it with the C toolchain against a tiny stub, and runs it over a hand-written truth table that
//! encodes the exact intended behaviour at every guard boundary — proving the SHIPPED bytes COMPUTE,
//! not just SAY, the right thing (mirrors `tests/genlock_relock_selection_parity.rs`; the helper is
//! the sole authority here — nothing in the Rust appliance consumes it — so the truth table IS the
//! spec). Per the project's test-strictness rule the lift-compile FAILS LOUDLY if no C compiler is
//! present, never skips.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn vendor_file(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so the anchors survive reformatting
/// (an upstream merge re-indenting a line, a clang-format wrap move).
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection: a `git subtree pull` re-importing stock DistroAV
// silently drops the whole watchdog; CI then fails loudly here).
// ----------------------------------------------------------------------------------------------

#[test]
fn reconnect_decision_helper_and_const_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline bool genlock_reconnect_decision("),
        "{NDI_SOURCE}: #767 patch missing — the pure `genlock_reconnect_decision(...)` decision \
         helper is gone. Without it a genlocked source whose sender instance restarts never rebinds \
         (41-min silent-black incident, 2026-08-13). A `git subtree pull` likely reverted it."
    );
    assert!(
        src.contains("GENLOCK_RECONNECT_STALE_NS"),
        "{NDI_SOURCE}: #767 patch missing — the GENLOCK_RECONNECT_STALE_NS stale-window constant \
         is gone. Re-apply the reconnect watchdog."
    );
}

#[test]
fn receiver_loop_wires_the_reconnect_watchdog() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The connection count is captured into a variable so the watchdog can gate on
    // "connected but silent" (no_conn > 0), not just the stock no_conn==0 early-continue.
    assert!(
        src.contains("int no_conn = ndiLib->recv_get_no_connections(ndi_receiver);"),
        "{NDI_SOURCE}: #767 patch missing — the receiver loop must capture the connection count \
         (`int no_conn = ndiLib->recv_get_no_connections(ndi_receiver);`) so the watchdog can tell \
         a CONNECTED-but-silent source (the stuck half-open case) from a genuinely disconnected one."
    );

    // The watchdog must be gated on the pure decision, fed the live source state.
    assert!(
        src.contains(
            "genlock_reconnect_decision(genlock_source_is_active(s->obs_source), no_conn,"
        ),
        "{NDI_SOURCE}: #767 patch missing — the receiver loop no longer calls \
         genlock_reconnect_decision() with the live source's genlock-active state + connection \
         count, so a stale genlocked source is never force-rebound. Re-apply the watchdog call."
    );

    // On a stale verdict it must force the EXISTING reset machinery (never a second reconnect
    // path). Anchor the UNIQUE mutex-guarded flag-set + timestamp-refresh trio, not the bare
    // `reset_ndi_receiver = true;` (which is aliased: the reset block sets it FALSE and
    // ndi_source_thread_start sets true elsewhere — a bare-substring anchor would pass even if
    // the watchdog's own flag-set were deleted).
    assert!(
        src.contains(
            "pthread_mutex_lock(&s->config_mutex); s->config.reset_ndi_receiver = true; \
             pthread_mutex_unlock(&s->config_mutex); s->last_frame_timestamp = os_gettime_ns();"
        ),
        "{NDI_SOURCE}: #767 patch missing — the watchdog must set reset_ndi_receiver (under \
         config_mutex) + refresh last_frame_timestamp so the existing reset block \
         recv_destroy+recv_create_v3 rebinds the (restarted) sender."
    );

    // The reconnect-epoch guard must be present: on the first iteration after a disconnect, give
    // the freshly (re)connected receiver a full stale window (otherwise the frozen absence-duration
    // timestamp forces a spurious rebind of a connection NDI just recovered on its own).
    assert!(
        src.contains("bool was_disconnected = true;")
            && src.contains(
                "if (was_disconnected) { s->last_frame_timestamp = os_gettime_ns(); \
                 was_disconnected = false; }"
            ),
        "{NDI_SOURCE}: #767 reconnect-epoch guard missing — without the `was_disconnected` \
         transition refresh, a genlocked source that reconnects after a >=stale-window absence \
         gets one spurious forced rebind. Re-apply the guard."
    );

    // The distinctive log line (unique substring — not the #764 keep-alive line).
    assert!(
        src.contains("genlock: NDI receiver stale while connected"),
        "{NDI_SOURCE}: #767 patch missing — the stale-reconnect log line is gone; operators lose \
         the only signal that a stuck receiver was force-rebound."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helper, compile it standalone, run it over a truth table that encodes
// the exact intended behaviour at every guard boundary. Proves the shipped C COMPUTES correctly.
// ----------------------------------------------------------------------------------------------

/// Lift the `genlock_reconnect_decision` helper VERBATIM from the vendored C (never retype it — a
/// retyped copy verifies your typing, not the shipped bytes).
fn lift_decision_helper() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline bool genlock_reconnect_decision(")
        .unwrap_or_else(|| {
            panic!(
                "#767: {NDI_SOURCE} no longer defines genlock_reconnect_decision — there is \
                 nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#767: genlock_reconnect_decision has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(genlock_active, no_connections, now_ns, last_frame_ns, stale_ns)` — the helper's arguments.
type ReconnectArgs = (bool, i32, u64, u64, u64);

/// `(args, expected)`. Each row asserts one guard/boundary of the spec.
fn vectors() -> Vec<(ReconnectArgs, bool)> {
    let s = 10_000_000_000u64; // 10 s stale window, in ns
    vec![
        // genlock OFF → never fires, whatever else holds.
        ((false, 5, 100_000_000_000, 10_000_000_000, s), false),
        ((false, 1, 100_000_000_000, 5_000_000_000, s), false),
        // not connected (no_conn <= 0) → NDI's finder handles it; watchdog stays out.
        ((true, 0, 100_000_000_000, 10_000_000_000, s), false),
        ((true, -1, 100_000_000_000, 5_000_000_000, s), false),
        // never received a frame yet (last == 0) → don't judge a warming-up receiver.
        ((true, 5, 100_000_000_000, 0, s), false),
        // clock not advanced past last frame (now <= last) → no measurable age.
        ((true, 1, 5_000_000_000, 10_000_000_000, s), false),
        ((true, 1, 10_000_000_000, 10_000_000_000, s), false),
        // connected + genlock + fresh (age < stale) → false; at/over the window → true.
        ((true, 1, 19_000_000_000, 10_000_000_000, s), false), // age 9 s
        ((true, 1, 19_999_999_999, 10_000_000_000, s), false), // age 9.999… s (just under)
        ((true, 1, 20_000_000_000, 10_000_000_000, s), true),  // age exactly 10 s (>=)
        ((true, 1, 25_000_000_000, 10_000_000_000, s), true),  // age 15 s
        ((true, 3, 100_000_000_000, 5_000_000_000, s), true),  // multi-connection, age 95 s
        // A DIFFERENT stale window — proves the helper honours the `stale_ns` PARAMETER and did
        // not hardcode 10 s: age 7 s over a 5 s window → true; age 4 s over a 5 s window → false.
        ((true, 1, 8_000_000_000, 1_000_000_000, 5_000_000_000), true),
        (
            (true, 1, 5_000_000_000, 1_000_000_000, 5_000_000_000),
            false,
        ),
    ]
}

#[test]
fn reconnect_decision_computes_the_spec_truth_table() {
    let helper = lift_decision_helper();
    let vs = vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((ga, nc, now, last, stale), _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_reconnect_decision({}, {nc}, {now}ULL, {last}ULL, {stale}ULL));\n",
            if *ga { "true" } else { "false" }
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_reconnect_767");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("reconnect.c");
    let bin = dir.join("reconnect.bin");
    fs::write(&cfile, &c).expect("write the harness");

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
                "#767: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 genlock_reconnect_decision to prove the C both COMPILES and computes the spec; it \
                 must FAIL rather than skip when the toolchain is absent (a gate that silently \
                 passes without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#767: genlock_reconnect_decision lifted from {NDI_SOURCE} does NOT COMPILE standalone \
         under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is otherwise \
         compiled only by the genlock workflows, so this is very likely a real compile error \
         heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#767: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#767: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<bool> = stdout.lines().map(|l| l.trim() == "1").collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#767: the harness printed {} results for {} vectors",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (((ga, nc, now, last, stale), want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!(
                "  genlock={ga} no_conn={nc} now={now} last={last} stale={stale} -> C {g}, expected {want}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#767: the vendored C genlock_reconnect_decision DIVERGED from the intended spec on {} of \
         {} vectors — the deployed rebind behaviour is not what this ticket requires:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
