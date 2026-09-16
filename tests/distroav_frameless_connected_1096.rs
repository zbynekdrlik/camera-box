//! #1096 (reopen 16.9.2026) CONNECTED-but-FRAMELESS receiver-rebind guard for the vendored DistroAV
//! NDI source receiver thread (`vendor/distroav/src/ndi-source.cpp`).
//!
//! Background: the 15.9 finder-blind ladder (last-known + fleet-map BY-URL) is landed. The 12:10
//! fleet deploy of 1.7.0-dev.631 then produced a DIFFERENT wedge: strih reset `NDI cam5`/`cam2`/
//! `cam6` BY-NAME into the poisoned long-lived finder, the bind CONNECTED (`no_connections > 0`) but
//! delivered ZERO frames -- `genlock-fifo audit received=` frozen for over an hour. The issue-767
//! stale watchdog `genlock_reconnect_decision(...)` did not cure it; the `no_connections == 0`
//! last-known/fleet-map ladder cannot re-arm once the receiver believes it is connected; the
//! issue-1287 alternation runs only inside a reset-forcing arm that never fired. So the receiver had
//! NO clock that ages a connected bind which never delivered a frame SINCE ITS BIND.
//!
//! The fix: record a bind timestamp at every recv_create_v3 and add a sibling pure decision
//! `genlock_frameless_bind_reconnect_decision(...)` -- a strict COMPLEMENT of genlock_reconnect_decision
//! keyed on frames_seen_since_reset (a real frame on THIS bind), NOT last_frame_ns. Keying on
//! last_frame_ns == 0 would be DEAD CODE: the issue-767 reconnect-epoch refresh (`if
//! (was_disconnected) { s->last_frame_timestamp = os_gettime_ns(); ... }`) runs one branch earlier
//! and makes last_frame non-zero the instant a bind connects. frames_seen_since_reset is set true
//! ONLY by a delivered frame (never by the refresh) and re-armed false on every reset, so it is
//! reachable AND immune to whatever keeps last_frame fresh without real frames. Fires when
//! frames_seen is false, connected, genlock-active, and now - bind_ns >= FRAMELESS_BIND_STALE_NS --
//! forcing the SAME reset ladder + issue-1287 alternation.
//!
//! Why this test is std-only + runs offline: camera-box's `# airuleset:build-ok` bypass is disabled
//! and the vendored C compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md`
//! this file (a) SOURCE-ANCHORS the C tokens with a std-only `fs::read_to_string` guard (revert
//! protection against a future `git subtree pull`), and (b) LIFTS the pure decision helper VERBATIM,
//! compiles it with the C toolchain against a tiny stub, and runs it over a hand-written truth table
//! that encodes the exact intended behaviour at every guard boundary -- proving the SHIPPED bytes
//! COMPUTE, not just SAY, the right thing (the helper is the sole authority; the truth table IS the
//! spec). Per the project's test-strictness rule the lift-compile FAILS LOUDLY if no C compiler is
//! present, never skips. Offline: `CARGO_MANIFEST_DIR=$PWD rustc --edition 2021 --test
//! tests/distroav_frameless_connected_1096.rs -o /tmp/x && /tmp/x`.

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

/// Collapse every run of ASCII whitespace to a single space so the anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection).
// ----------------------------------------------------------------------------------------------

#[test]
fn frameless_bind_decision_helper_and_const_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline bool genlock_frameless_bind_reconnect_decision("),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the pure \
         `genlock_frameless_bind_reconnect_decision(...)` decision helper is gone. Without it a \
         receiver that CONNECTS but never delivers a frame since its bind sits frameless forever \
         (the 12:10 fleet-deploy wedge on cam2/5/6). A `git subtree pull` likely reverted it."
    );
    assert!(
        src.contains("FRAMELESS_BIND_STALE_NS"),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the FRAMELESS_BIND_STALE_NS window \
         constant is gone. Re-apply the connected-but-frameless watchdog."
    );
}

#[test]
fn receiver_loop_records_bind_ts_and_wires_the_frameless_watchdog() {
    let src = squish(&vendor_file(NDI_SOURCE));

    // The bind timestamp must be recorded at the successful create so the frameless watchdog has a
    // baseline to age a bind that has delivered no frame since it was created.
    assert!(
        src.contains("recv_bind_ns_1096 = os_gettime_ns();"),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the receiver loop no longer records \
         recv_bind_ns_1096 at the successful recv_create_v3, so a connected-but-frameless bind has \
         no aging clock and never re-binds."
    );
    assert!(
        src.contains("uint64_t recv_bind_ns_1096 = 0;"),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the recv_bind_ns_1096 thread-local is \
         gone. Re-apply the frameless-bind timer."
    );

    // The watchdog must be gated on the pure decision, fed the live source state + the bind ts.
    assert!(
        src.contains(
            "genlock_frameless_bind_reconnect_decision(genlock_source_is_active(s->obs_source), no_conn,"
        ),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the receiver loop no longer calls \
         genlock_frameless_bind_reconnect_decision() with the live source's genlock-active state + \
         connection count, so a connected-but-frameless bind is never force-rebound."
    );
    assert!(
        src.contains("recv_bind_ns_1096, FRAMELESS_BIND_STALE_NS)"),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the frameless watchdog call no longer \
         passes recv_bind_ns_1096 + FRAMELESS_BIND_STALE_NS as the bind baseline + window."
    );

    // REACHABILITY (the #1096 reopen dead-code catch): the call MUST key on frames_seen_since_reset_1180,
    // NOT s->last_frame_timestamp. The #767 reconnect-epoch refresh (`if (was_disconnected) {
    // s->last_frame_timestamp = os_gettime_ns(); ... }`) runs one branch earlier and makes last_frame
    // non-zero the instant a bind connects, so a last_frame-keyed arm here would NEVER fire (dead code).
    assert!(
        src.contains(
            "genlock_frameless_bind_reconnect_decision(genlock_source_is_active(s->obs_source), no_conn, os_gettime_ns(), frames_seen_since_reset_1180,"
        ),
        "{NDI_SOURCE}: #1096 (reopen 16.9) DEAD-CODE regression -- the frameless watchdog must be fed \
         frames_seen_since_reset_1180 (a real frame on THIS bind), NOT s->last_frame_timestamp. The \
         #767 reconnect-epoch refresh sets last_frame non-zero the instant a bind connects, so a \
         last_frame-keyed arm can never fire."
    );

    // The distinctive log marker (unique substring, mutually non-substring vs every other genlock: line).
    assert!(
        src.contains("genlock: NDI receiver connected but FRAMELESS for"),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the connected-but-frameless rebind log \
         marker is gone; operators lose the only signal that a frameless bind was force-rebound."
    );

    // The new arm must force the EXISTING reset machinery, and reuse the issue-1287 alternation so a
    // frame-less BY-URL bind cannot pin the leg (the whole gate-and-set adjacency is anchored below).
    assert!(
        ([
            "ndi_force_by_name_after_frameless(connected_by_url_1180, frames_seen_since_reset_1180)) { force_by_name_next_reset_1180 = true;",
        ]
        .iter()
        .all(|a| src.contains(a))),
        "{NDI_SOURCE}: #1096 (reopen 16.9) patch missing -- the frameless arm must reuse the \
         issue-1287 ndi_force_by_name_after_frameless(...) gate-and-set so a frame-less BY-URL bind \
         alternates to BY-NAME instead of looping BY-URL."
    );
    // Anchor that the corrective set now appears at THREE reset-forcing arms (no_connections==0, the
    // #767 stale-while-connected arm, AND this new frameless arm).
    let n = src
        .matches(
            "ndi_force_by_name_after_frameless(connected_by_url_1180, frames_seen_since_reset_1180)) { force_by_name_next_reset_1180 = true;",
        )
        .count();
    assert!(
        n >= 3,
        "{NDI_SOURCE}: #1096 (reopen 16.9) -- the issue-1287 helper-gated corrective set is wired at \
         {n} reset-forcing arms; it must be present at all THREE (no_connections==0, #767 \
         stale-while-connected, AND the new connected-but-frameless arm)."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helper, compile it standalone, run it over a truth table.
// ----------------------------------------------------------------------------------------------

/// Lift the `genlock_frameless_bind_reconnect_decision` helper VERBATIM from the vendored C.
fn lift_decision_helper() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline bool genlock_frameless_bind_reconnect_decision(")
        .unwrap_or_else(|| {
            panic!(
                "#1096 (reopen 16.9): {NDI_SOURCE} no longer defines \
                 genlock_frameless_bind_reconnect_decision -- there is nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1096: genlock_frameless_bind_reconnect_decision has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(genlock_active, no_connections, now_ns, frames_seen_since_reset, bind_ns, frameless_stale_ns)`.
type Args = (bool, i32, u64, bool, u64, u64);

fn vectors() -> Vec<(Args, bool)> {
    let s = 5_000_000_000u64; // 5 s frameless window, in ns
    vec![
        // genlock OFF -> never fires.
        ((false, 5, 100_000_000_000, false, 10_000_000_000, s), false),
        // not connected (no_conn <= 0) -> the no_connections==0 ladder owns it.
        ((true, 0, 100_000_000_000, false, 10_000_000_000, s), false),
        ((true, -1, 100_000_000_000, false, 10_000_000_000, s), false),
        // has delivered a frame on THIS bind (frames_seen == true) -> genlock_reconnect_decision owns
        // it, not this. "a receiver that delivered must NOT" fire here even though age from bind is huge.
        ((true, 1, 100_000_000_000, true, 10_000_000_000, s), false),
        // no bind timestamp recorded yet (bind == 0) -> nothing to age.
        ((true, 1, 100_000_000_000, false, 0, s), false),
        // clock not advanced past bind (now <= bind) -> no measurable age.
        ((true, 1, 10_000_000_000, false, 10_000_000_000, s), false),
        ((true, 1, 9_000_000_000, false, 10_000_000_000, s), false),
        // connected + genlock + no frame since bind + bind set: age < window -> false, at/over -> true.
        // "a bind 2 s old must NOT" (age 2 s < 5 s window).
        ((true, 1, 12_000_000_000, false, 10_000_000_000, s), false), // age 2 s
        ((true, 1, 14_999_999_999, false, 10_000_000_000, s), false), // age 4.999... s
        ((true, 1, 15_000_000_000, false, 10_000_000_000, s), true),  // age exactly 5 s (>=)
        // "today's shape: connected, no frame since bind, bind ~65 min ago -> RECONNECT".
        ((true, 1, 3_910_000_000_000, false, 10_000_000_000, s), true), // age ~65 min
        ((true, 3, 3_910_000_000_000, false, 10_000_000_000, s), true), // multi-connection, same
        // Honour the frameless_stale_ns PARAMETER (not a hardcoded 5 s): age 7 s over a 10 s window
        // -> false; age 7 s over a 5 s window -> true.
        (
            (
                true,
                1,
                17_000_000_000,
                false,
                10_000_000_000,
                10_000_000_000,
            ),
            false,
        ),
        ((true, 1, 17_000_000_000, false, 10_000_000_000, s), true),
    ]
}

#[test]
fn frameless_bind_decision_computes_the_spec_truth_table() {
    let helper = lift_decision_helper();
    let vs = vectors();

    let mut c = String::from("#include <stdint.h>\n#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for ((ga, nc, now, frames_seen, bind, stale), _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_frameless_bind_reconnect_decision({}, {nc}, {now}ULL, {}, {bind}ULL, {stale}ULL));\n",
            if *ga { "true" } else { "false" },
            if *frames_seen { "true" } else { "false" }
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_frameless_1096");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("frameless.c");
    let bin = dir.join("frameless.bin");
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
                "#1096: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 genlock_frameless_bind_reconnect_decision to prove the C both COMPILES and computes \
                 the spec; it must FAIL rather than skip when the toolchain is absent. Install a C \
                 compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1096: genlock_frameless_bind_reconnect_decision lifted from {NDI_SOURCE} does NOT COMPILE \
         standalone under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is \
         otherwise compiled only by the genlock workflows, so this is very likely a real compile \
         error heading for CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1096: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1096: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<bool> = stdout.lines().map(|l| l.trim() == "1").collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1096: the harness printed {} results for {} vectors",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (((ga, nc, now, frames_seen, bind, stale), want), g) in vs.iter().zip(&got) {
        if g != want {
            diffs.push(format!(
                "  genlock={ga} no_conn={nc} now={now} frames_seen={frames_seen} bind={bind} stale={stale} -> C {g}, expected {want}"
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1096: the vendored C genlock_frameless_bind_reconnect_decision DIVERGED from the intended \
         spec on {} of {} vectors -- the deployed rebind behaviour is not what this ticket \
         requires:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
