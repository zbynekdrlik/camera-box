//! #1287 — a BY-URL bind that delivers ZERO frames is its own wedge class: after a graceful cambox
//! sender restart the #1096 "fresh" per-reset finder keeps serving the DYING sender's cached
//! advertisement, so the receiver re-resolves the SAME now-dead cached port on every reset. That
//! BY-URL bind delivers no frames, so `frames_seen_since_reset_1180` never becomes true and #1180's
//! post-connect identity verify (which is frames-gated) never fires — nothing sets
//! `force_by_name_next_reset_1180`, so both reset-forcing arms (the `no_connections==0` arm and the
//! #767 stale-while-connected arm) take the DEFAULT BY-URL path forever. Live: strih `'NDI cam7'`
//! sat 6.5 min at `received=` Δ0 with ~40 identical-URL rebinds while a BY-NAME sibling recovered in
//! seconds. Distinct from #1096 (poisoned name — BY-URL is its cure), #1080/#1097 (`break` thread
//! death — the thread is alive and rebinding here), and #1180 (wrong-sender WITH frames flowing).
//! See `.claude/rules/distroav-receiver-lifecycle.md` ("A FRAME-LESS BY-URL bind is its own wedge
//! class").
//!
//! The fix (`vendor/distroav/src/ndi-source.cpp` `ndi_source_thread`): a PURE decision helper
//! `ndi_force_by_name_after_frameless(connected_by_url, frames_seen_since_reset)` returns true iff
//! the current bind delivered ZERO frames AND was BY-URL; BOTH reset-forcing arms call it and set the
//! SAME `force_by_name_next_reset_1180` flag #1180 already owns, so the NEXT reset connects BY-NAME.
//! Because the un-forced default reset is BY-URL, "force BY-NAME only after a frame-less BY-URL bind"
//! ALTERNATES BY-URL <-> BY-NAME across consecutive frame-less rebinds, so neither this stale-URL
//! wedge nor the #1096 poisoned-name wedge can pin a leg. No new state, no new counter.
//!
//! Why std-only + offline: camera-box's `# airuleset:build-ok` bypass is disabled and the vendored C
//! compiles only on CI, so per `.claude/rules/vendored-libobs-change-safety.md` (the #767/#1096/#1180
//! pattern) this file (a) SOURCE-ANCHORS the C tokens with a `fs::read_to_string` guard runnable via
//! `rustc --test` (revert protection against a future `git subtree pull`), and (b) LIFTS the pure
//! `ndi_force_by_name_after_frameless` helper VERBATIM, compiles it standalone under -Werror, and runs
//! it over a hand-written truth table encoding the intended decision at every guard boundary — proving
//! the SHIPPED bytes COMPUTE, not just SAY, the right thing. Nothing in the Rust appliance consumes the
//! helper, so the truth table IS the spec. Per test-strictness the lift-compile FAILS LOUDLY when no C
//! compiler is present, never skips. The LIVE receive-path cure is NOT offline-verifiable (vendored
//! receive path, the wedge reproduces only live) — flag UNVERIFIED for the supervisor's post-deploy
//! rig repro.

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
fn frameless_decision_helper_present() {
    let src = squish(&vendor_file(NDI_SOURCE));
    assert!(
        src.contains("static inline bool ndi_force_by_name_after_frameless("),
        "{NDI_SOURCE}: #1287 patch missing — the pure `ndi_force_by_name_after_frameless(...)` decision \
         helper is gone. Without it a frame-less BY-URL bind (a dead cached sender port after a graceful \
         restart) loops BY-URL forever because #1180's force flag is frames-gated. A `git subtree pull` \
         likely reverted it. Re-apply the #1287 fix."
    );
}

#[test]
fn both_reset_arms_force_by_name_after_a_frameless_by_url_bind() {
    let src = squish(&vendor_file(NDI_SOURCE));
    // BOTH reset-forcing arms (the no_connections==0 arm AND the #767 stale-while-connected arm)
    // must call the helper with the two per-bind gate flags. The definition uses the PARAMETER names
    // (connected_by_url / frames_seen_since_reset), so this exact call form (with the _1180 thread-
    // local names) can only be a CALL SITE — it must appear at least twice (once per arm).
    let call =
        "ndi_force_by_name_after_frameless(connected_by_url_1180, frames_seen_since_reset_1180)";
    let n = src.matches(call).count();
    assert!(
        n >= 2,
        "{NDI_SOURCE}: #1287 — the helper is called at only {n} site(s); it must be wired into BOTH \
         reset-forcing arms (the no_connections==0 arm AND the #767 stale-while-connected arm) so a \
         frame-less BY-URL bind flips to BY-NAME regardless of whether the dead bind is no_conn==0 \
         (graceful FIN) or no_conn>0-but-silent. Re-apply the #1287 wiring at both sites."
    );
    // The corrective action is to set the SAME flag #1180 owns, so a forced BY-NAME reset abandons
    // the dead-port URL. (The flag is set elsewhere too — #1080 create-fail preservation and the
    // #1180 verify — so this only proves the flag exists, the call-count check above proves wiring.)
    assert!(
        src.contains("force_by_name_next_reset_1180 = true;"),
        "{NDI_SOURCE}: #1287 — the corrective `force_by_name_next_reset_1180 = true;` flag is gone; a \
         frame-less BY-URL bind can no longer be forced onto BY-NAME. Re-apply the fix."
    );
}

#[test]
fn no_conn_arm_and_stale_arm_gate_the_force_on_the_helper_result() {
    // Prove the helper's result is what GATES the corrective flag in BOTH arms (not an unconditional
    // set that would force BY-NAME even for a HEALTHY bind delivering frames). Slice each arm's region
    // and require the helper call inside it, immediately guarding a force.
    let src = squish(&vendor_file(NDI_SOURCE));

    // The no_connections==0 arm: from its window-check anchor to the sleep+continue that closes it.
    let nc_start = src
        .find(">= GENLOCK_RECONNECT_STALE_NS && genlock_source_is_active(s->obs_source)) {")
        .expect("#1287: the no_connections==0 window-check anchor not found");
    let nc_end_rel = src[nc_start..]
        .find("std::this_thread::sleep_for(std::chrono::milliseconds(100)); continue;")
        .expect(
            "#1287: the no_connections==0 sleep+continue close not found after the window check",
        );
    let nc_arm = &src[nc_start..nc_start + nc_end_rel];
    // Anchor the WHOLE adjacency (the helper gate AND the corrective set it guards) so deleting only
    // the `force_by_name_next_reset_1180 = true;` line — which the bare flag string tolerates, that
    // token appears at 3 other unrelated sites — still fails here (review #1287 🟡).
    assert!(
        nc_arm.contains("if (ndi_force_by_name_after_frameless(connected_by_url_1180, frames_seen_since_reset_1180)) { force_by_name_next_reset_1180 = true;"),
        "{NDI_SOURCE}: #1287 — the no_connections==0 arm does not gate the force (or drops the \
         corrective force_by_name_next_reset_1180 set) on the helper result. A frame-less BY-URL bind \
         (the ticket's own `systemctl restart camera-box` graceful-FIN case) is never flipped to \
         BY-NAME:\n{nc_arm}"
    );

    // The #767 stale-while-connected arm: from its decision call to its `continue;`.
    let st_start = src
        .find("if (genlock_reconnect_decision(genlock_source_is_active(s->obs_source), no_conn,")
        .expect("#1287: the #767 genlock_reconnect_decision arm anchor not found");
    let st_end_rel = src[st_start..]
        .find("s->last_frame_timestamp = os_gettime_ns(); continue;")
        .expect("#1287: the #767 arm's last_frame refresh + continue close not found");
    let st_arm = &src[st_start..st_start + st_end_rel];
    assert!(
        st_arm.contains("if (ndi_force_by_name_after_frameless(connected_by_url_1180, frames_seen_since_reset_1180)) { force_by_name_next_reset_1180 = true;"),
        "{NDI_SOURCE}: #1287 — the #767 stale-while-connected arm does not gate the force (or drops the \
         corrective force_by_name_next_reset_1180 set) on the helper result, so a connected-but-silent \
         frame-less BY-URL bind (no_conn>0) can loop BY-URL forever:\n{st_arm}"
    );
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure helper, compile standalone under -Werror, run it over a truth table
// encoding the intended decision at every guard boundary. Proves the shipped C COMPUTES correctly
// (not just that it says the right thing). Nothing in Rust consumes it, so the truth table IS the spec.
// ----------------------------------------------------------------------------------------------

/// Lift the `ndi_force_by_name_after_frameless` helper VERBATIM (never retype it — a retyped copy
/// verifies your typing, not the shipped bytes).
fn lift_helper() -> String {
    let src = vendor_file(NDI_SOURCE);
    let start = src
        .find("static inline bool ndi_force_by_name_after_frameless(")
        .unwrap_or_else(|| {
            panic!(
                "#1287: {NDI_SOURCE} no longer defines ndi_force_by_name_after_frameless — there is \
                 nothing to compile/behaviour-check. Re-apply the #1287 fix."
            )
        });
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("#1287: ndi_force_by_name_after_frameless has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

struct Vector {
    connected_by_url: bool,
    frames_seen: bool,
    expect: bool,
    why: &'static str,
}

fn vectors() -> Vec<Vector> {
    vec![
        Vector {
            connected_by_url: true,
            frames_seen: false,
            expect: true,
            why: "frame-less BY-URL bind -> force BY-NAME next (break the dead-port wedge)",
        },
        Vector {
            connected_by_url: false,
            frames_seen: false,
            expect: false,
            why: "frame-less BY-NAME bind -> DON'T force (default reset is BY-URL, so this alternates back)",
        },
        Vector {
            connected_by_url: true,
            frames_seen: true,
            expect: false,
            why: "BY-URL bind that DELIVERED frames -> never force (the #1180 identity path owns it)",
        },
        Vector {
            connected_by_url: false,
            frames_seen: true,
            expect: false,
            why: "BY-NAME bind that delivered frames -> never force (steady state)",
        },
    ]
}

#[test]
fn frameless_decision_computes_the_spec_truth_table() {
    let helper = lift_helper();
    let vs = vectors();

    // The real ndi-source.cpp is C++ (bool is native); the lift compiles as C, so pull in
    // <stdbool.h> for bool/true/false. This is a harness prelude, not part of the lifted helper.
    let mut c = String::from("#include <stdio.h>\n#include <stdbool.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for v in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", ndi_force_by_name_after_frameless({}, {}) ? 1 : 0);\n",
            if v.connected_by_url { "1" } else { "0" },
            if v.frames_seen { "1" } else { "0" },
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_frameless_by_url_escape_1287");
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
                "#1287: could not run the C compiler `{cc}` ({e}). This gate compiles the vendored \
                 ndi_force_by_name_after_frameless to prove the C both COMPILES and computes the spec; \
                 it must FAIL rather than skip when the toolchain is absent (a gate that silently \
                 passes without running is worse than none). Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "#1287: ndi_force_by_name_after_frameless lifted from {NDI_SOURCE} does NOT COMPILE standalone \
         under -Wall -Wextra -Wformat=2 -Wconversion -Werror. The vendored tree is otherwise compiled \
         only by the genlock workflows, so this is very likely a real compile error heading for \
         CI:\n--- cc stderr ---\n{}\n--- harness ---\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );

    let run = Command::new(&bin)
        .output()
        .expect("#1287: the compiled harness failed to execute");
    assert!(
        run.status.success(),
        "#1287: the harness exited non-zero: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8(run.stdout).expect("harness stdout is utf-8");
    let got: Vec<String> = stdout.lines().map(|l| l.to_string()).collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "#1287: the harness printed {} results for {} vectors:\n{stdout}",
        got.len(),
        vs.len()
    );

    let mut diffs = Vec::new();
    for (v, g) in vs.iter().zip(&got) {
        let want = if v.expect { "1" } else { "0" };
        if g != want {
            diffs.push(format!(
                "  connected_by_url={} frames_seen={} -> C {:?}, expected {:?}  [{}]",
                v.connected_by_url, v.frames_seen, g, want, v.why
            ));
        }
    }
    assert!(
        diffs.is_empty(),
        "#1287: the vendored C ndi_force_by_name_after_frameless DIVERGED from the intended decision \
         spec on {} of {} vectors:\n{}",
        diffs.len(),
        vs.len(),
        diffs.join("\n")
    );
}
