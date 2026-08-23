//! #1180 — P0 (2026-08-23 Sunday service): strih's NIC failed twice mid-production; the operator
//! rebooted strih; after the OBS restart the NDI sender PORTS RESHUFFLED and `STRIH-SNV (Grading)`
//! inherited `2ME PGM`'s old port. Every receiver reconnecting by CACHED URL (our own #1096 BY-URL
//! connect, AND stock NDI Studio Monitor on the building TVs) latched onto the WRONG sender —
//! showing the full-screen Grading feed (= cam3 at the time) under the stored "STRIH-SNV (2ME PGM)"
//! label. NDI connect-by-URL never verifies the sender's NAME, and once frames flow the receiver is
//! "healthy" so nothing re-checks (the #767 stale watchdog is silence-based).
//!
//! The camera-box-side fix (`vendor/distroav/src/ndi-source.cpp`): after a BY-URL-connected receiver
//! starts DELIVERING FRAMES following a reset, re-run a bounded FRESH finder, resolve the configured
//! `ndi_source_name` → URL, and confirm it still maps to the URL the receiver is bound to. On a
//! CONFIRMED mismatch, force a fresh reset that connects BY-NAME (a loud, grep-able `#1180` log
//! line), abandoning the wrong-sender URL. One-shot at first-frames (the required minimum) + a
//! low-rate periodic re-verify while a BY-URL bind stays active. The compare is the pure
//! `static inline ndi_by_url_identity_mismatch(...)` decision helper. Scoped to genlocked sources
//! (mirrors #767/#1096); a BY-NAME-connected receiver never enters the verify path, so its behaviour
//! is byte-identical. Distinct from #1096 (which fires the BY-URL connect but never re-validates it)
//! — see `.claude/rules/distroav-receiver-lifecycle.md`.
//!
//! Why std-only + offline: camera-box's `# airuleset:build-ok` bypass is disabled and the vendored C
//! compiles only on CI, so per `.claude/rules/distroav-receiver-lifecycle.md` (the #767/#1096
//! pattern) this file (a) SOURCE-ANCHORS the C tokens with a `fs::read_to_string` guard runnable via
//! `rustc --test` (revert protection against a future `git subtree pull`), and (b) LIFTS the pure
//! `ndi_by_url_identity_mismatch` helper VERBATIM, compiles it against a tiny stub, and runs it over
//! a hand-written truth table encoding the intended verdict at every guard boundary — proving the
//! SHIPPED bytes COMPUTE, not just SAY, the right thing. Nothing in the Rust appliance consumes the
//! helper, so the truth table IS the spec. Per test-strictness the lift-compile FAILS LOUDLY when no
//! C compiler is present, never skips. The LIVE wrong-source cure is NOT offline-verifiable (vendored
//! receive path, the reshuffle reproduces only live) — flag UNVERIFIED for the supervisor's
//! post-deploy rig repro.

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

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors (revert protection: a `git subtree pull` re-importing stock DistroAV
// silently drops the whole patch; CI then fails loudly here. ALSO mirrored as pwsh token checks in
// BOTH windows-genlock*.yml, because the fast path hot-swaps distroav.dll un-gated).
// ----------------------------------------------------------------------------------------------

#[test]
fn identity_mismatch_helper_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline bool ndi_by_url_identity_mismatch("),
        "{NDI_SOURCE}: #1180 patch missing — the pure `ndi_by_url_identity_mismatch(...)` decision \
         helper is gone. Without it a BY-URL receiver never re-validates that it is bound to the \
         RIGHT sender, so a reshuffled port shows the wrong camera under the stored label. A \
         `git subtree pull` likely reverted it."
    );
}

#[test]
fn reset_block_honours_the_force_by_name_flag() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The fresh-finder BY-URL resolution must be GATED on !force_by_name_1180 so a confirmed
    // identity mismatch can force the next reset to connect BY-NAME (abandon the wrong-sender URL).
    assert!(
        src.contains("if (!force_by_name_1180 && owned_source_name && owned_source_name[0]) {"),
        "{NDI_SOURCE}: #1180 patch missing — the reset block's fresh-finder BY-URL resolution is no \
         longer gated on !force_by_name_1180, so a confirmed identity mismatch cannot force a BY-NAME \
         reset and the receiver would re-resolve the SAME wrong-sender URL. Re-apply the #1180 \
         force-by-name gate."
    );
    // The forced-BY-NAME flag is consumed exactly here (one reset), and set only by the verify below.
    assert!(
        src.contains("bool force_by_name_1180 = force_by_name_next_reset_1180;")
            && src.contains("force_by_name_next_reset_1180 = false;"),
        "{NDI_SOURCE}: #1180 — the reset block no longer CONSUMES force_by_name_next_reset_1180 (read \
         into a local + clear), so the forced BY-NAME reset would either never fire or stick forever. \
         Re-apply the #1180 flag consume."
    );
    // The forced-BY-NAME branch logs a distinct loud line (not the #1096 fresh-finder-miss fallback).
    assert!(
        src.contains("#1180 connect BY-NAME"),
        "{NDI_SOURCE}: #1180 — the forced-BY-NAME reset no longer emits its distinct `#1180 connect \
         BY-NAME` log line, so a wrong-sender recovery leaves no grep-able trace. Re-apply it."
    );
}

#[test]
fn verify_block_uses_a_fresh_finder_the_picker_and_the_decision_helper() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The post-connect verify re-resolves the configured name through a FRESH finder (the SAME
    // create+wait+read+destroy sequence #1096 uses) and feeds the pure picker.
    assert!(
        src.contains("ndiLib->find_create_v2(") && src.contains("ndiLib->find_wait_for_sources(")
            && src.contains("ndiLib->find_get_current_sources(")
            && src.contains("ndi_find_url_for_source_name(owned_source_name,"),
        "{NDI_SOURCE}: #1180 — the verify block no longer re-resolves the configured name through a \
         fresh finder (find_create_v2 / find_wait_for_sources / find_get_current_sources / \
         ndi_find_url_for_source_name). Re-apply the #1180 verify."
    );
    // The picked URL is compared to the bound URL via the pure decision helper.
    assert!(
        src.contains("ndi_by_url_identity_mismatch(owned_source_url, verify_url_1180)"),
        "{NDI_SOURCE}: #1180 — the verify block no longer calls \
         ndi_by_url_identity_mismatch(owned_source_url, verify_url_1180) to decide MISMATCH. Re-apply \
         the #1180 wiring."
    );
    // On a mismatch it forces a BY-NAME reset (sets the flag + re-arms the receiver reset).
    assert!(
        src.contains("force_by_name_next_reset_1180 = true;")
            && src.contains("BY-URL identity MISMATCH"),
        "{NDI_SOURCE}: #1180 — a confirmed mismatch no longer sets force_by_name_next_reset_1180 = \
         true (with the loud `BY-URL identity MISMATCH` warning), so the wrong-sender feed is never \
         corrected. Re-apply the #1180 corrective reset."
    );
    // The verify's own finder is destroyed (no per-verify finder leak), and the picked URL copy is
    // freed on every path (the finder owns the source pointers; the copy is bstrdup'd).
    assert!(
        src.contains("bfree(verify_url_1180);"),
        "{NDI_SOURCE}: #1180 — verify_url_1180 is never bfree'd, leaking the picked URL copy on every \
         verify. Re-apply the frees."
    );
}

#[test]
fn verify_is_armed_only_for_by_url_binds_and_gated_on_frames_and_genlock() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // The verify is armed ONLY when the reset connected BY-URL — a BY-NAME bind leaves it false so
    // the whole verify path never runs (upstream behaviour byte-identical).
    assert!(
        src.contains("connected_by_url_1180 = url_resolved_1096;")
            && src.contains("identity_verify_pending_1180 = url_resolved_1096;"),
        "{NDI_SOURCE}: #1180 — the post-connect state is no longer armed from url_resolved_1096 \
         (connected_by_url_1180 / identity_verify_pending_1180), so either a BY-NAME bind would be \
         wrongly verified (behaviour change) or a BY-URL bind never verified. Re-apply the #1180 arm."
    );
    // The verify only runs once frames actually flow AND the source is genlocked (mirrors #767/#1096
    // scope; keeps stock/non-genlock inputs untouched).
    assert!(
        src.contains(
            "if (connected_by_url_1180 && frames_seen_since_reset_1180 && genlock_source_is_active(s->obs_source)) {"
        ),
        "{NDI_SOURCE}: #1180 — the verify gate (connected_by_url_1180 && frames_seen_since_reset_1180 \
         && genlock_source_is_active) changed, so it could verify a warming-up receiver, a non-\
         genlock/stock input, or a BY-NAME bind. Re-apply the exact #1180 gate."
    );
    // frames_seen_since_reset must be SET on a delivered frame (the issue's "starts delivering
    // frames" trigger) in BOTH video branches — the non-framesync PRODUCTION path AND the
    // (dormant, framesync-forced-off) framesync path. Assert >= 2: losing ONLY the production site
    // would silently disarm the whole #1180 verify with every gate still green (review #1180 🔵).
    assert!(
        src.matches("frames_seen_since_reset_1180 = true;").count() >= 2,
        "{NDI_SOURCE}: #1180 — frames_seen_since_reset_1180 is set in fewer than BOTH video branches \
         (found < 2), so the frame-delivered trigger may be missing from the production non-framesync \
         path and the verify's one-shot can never fire there. Re-apply the marker in both branches."
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure decision helper, compile standalone under -Werror -Wconversion
// -Wformat=2, run it over a truth table encoding the intended verdict at every guard boundary.
// Proves the shipped C COMPUTES correctly (not just that it says the right thing). Nothing in Rust
// consumes it, so the truth table IS the spec.
// ----------------------------------------------------------------------------------------------

/// Lift the `ndi_by_url_identity_mismatch` helper VERBATIM (never retype it — a retyped copy verifies
/// your typing, not the shipped bytes).
fn lift_helper() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline bool ndi_by_url_identity_mismatch(")
        .unwrap_or_else(|| {
            panic!(
                "#1180: {NDI_SOURCE} no longer defines ndi_by_url_identity_mismatch — there is \
                 nothing to compile/behaviour-check."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1180: ndi_by_url_identity_mismatch has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// One vector: `(connected_url, resolved_url_for_name)`, `None` => a C `NULL` pointer; expected
/// mismatch verdict; and what boundary the row pins.
struct Vector {
    connected: Option<&'static str>,
    resolved: Option<&'static str>,
    expect_mismatch: bool,
    why: &'static str,
}

/// A C string literal for `Some("x")` or the token `NULL` for `None`. The rig URLs used here contain
/// no quotes/backslashes, so a plain wrap is safe.
fn c_str(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("\"{s}\""),
        None => "NULL".to_string(),
    }
}

fn vectors() -> Vec<Vector> {
    vec![
        Vector {
            connected: None,
            resolved: Some("10.77.9.202:5964"),
            expect_mismatch: false,
            why: "NULL connected url (not a BY-URL bind) -> never verify",
        },
        Vector {
            connected: Some(""),
            resolved: Some("10.77.9.202:5964"),
            expect_mismatch: false,
            why: "EMPTY connected url -> nothing to verify",
        },
        Vector {
            connected: Some("10.77.9.202:5964"),
            resolved: None,
            expect_mismatch: false,
            why: "name not discoverable (NULL) -> INCONCLUSIVE, keep the feed",
        },
        Vector {
            connected: Some("10.77.9.202:5964"),
            resolved: Some(""),
            expect_mismatch: false,
            why: "name resolves EMPTY -> INCONCLUSIVE, keep the feed",
        },
        Vector {
            connected: Some("10.77.9.202:5964"),
            resolved: Some("10.77.9.202:5964"),
            expect_mismatch: false,
            why: "name still maps to our URL -> identity OK",
        },
        Vector {
            connected: Some("10.77.9.202:5964"),
            resolved: Some("10.77.9.202:5965"),
            expect_mismatch: true,
            why: "reshuffled PORT (the live P0: 2ME PGM moved :5964 -> :5965) -> MISMATCH",
        },
        Vector {
            connected: Some("10.77.9.202:5964"),
            resolved: Some("10.77.9.201:5964"),
            expect_mismatch: true,
            why: "different HOST, same port -> MISMATCH",
        },
        Vector {
            connected: Some("tcp://10.0.0.5:5962"),
            resolved: Some("tcp://10.0.0.5:5962"),
            expect_mismatch: false,
            why: "full tcp:// form, exact match -> OK",
        },
        Vector {
            connected: None,
            resolved: None,
            expect_mismatch: false,
            why: "neither known -> never verify (connected-url guard wins)",
        },
    ]
}

#[test]
fn identity_mismatch_computes_the_spec_truth_table() {
    let helper = lift_helper();
    let vs = vectors();

    let mut c = String::from(
        "#include <stdint.h>\n#include <stddef.h>\n#include <stdbool.h>\n#include <string.h>\n\
         #include <stdio.h>\n",
    );
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for v in vs.iter() {
        c.push_str(&format!(
            "    printf(\"%s\\n\", ndi_by_url_identity_mismatch({}, {}) ? \"MISMATCH\" : \"ok\");\n",
            c_str(v.connected),
            c_str(v.resolved),
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_by_url_identity_verify_1180");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("verify.c");
    let bin = dir.join("verify.bin");
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
                "#1180: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 ndi_by_url_identity_mismatch to prove the C both COMPILES and computes the spec; it \
                 must FAIL rather than skip when the toolchain is absent (a gate that silently passes \
                 without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1180: ndi_by_url_identity_mismatch lifted from {NDI_SOURCE} does NOT COMPILE standalone \
         under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is otherwise compiled \
         only by the genlock workflows, so this is very likely a real compile error heading for \
         CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1180: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1180: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<String> = stdout.lines().map(|l| l.to_string()).collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1180: the harness printed {} results for {} vectors:\n{stdout}",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (v, g) in vs.iter().zip(&got) {
        let want = if v.expect_mismatch { "MISMATCH" } else { "ok" };
        if g != want {
            diffs.push(format!(
                "  connected={:?} resolved={:?} -> C {:?}, expected {:?}  [{}]",
                v.connected, v.resolved, g, want, v.why
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1180: the vendored C ndi_by_url_identity_mismatch DIVERGED from the intended verdict spec \
         on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
