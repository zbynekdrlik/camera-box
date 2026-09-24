//! issue 1242 — strih-lx pulls FULL-bandwidth NDI only for cameras that are SHOWN
//! (`vendor/distroav/src/ndi-source.cpp`).
//!
//! Owner ruling (24.9.2026): the multiview renders the always-connected low-bandwidth
//! `MV NDI camN` twins (the #501 `genlock_monitor` role), and a full program-path camera input
//! carries traffic only while it is in PVW, PGM, a projector, or the visible item of the Grading
//! NDI-output scene. This reverses the issue-761 same-source multiview and relaxes the issue-764
//! keep-alive for the program-path role only. A cold start in PVW is accepted.
//!
//! The mechanism is an in-thread PARK, not stock DistroAV's `ndi_behavior=STOP_RESUME_*`:
//! libobs calls `info.hide` from `obs_source_video_tick` (the GRAPHICS thread), so the stock
//! `ndi_source_hidden -> ndi_source_thread_stop -> pthread_join` would block the program render
//! on every hide (up to one recv_capture timeout / one fresh-finder wait) -- the issue-1320
//! render-freeze class. Instead the behavior stays KEEP_ACTIVE (the thread is never joined on
//! hide, so no lifecycle trap), and the receiver thread itself releases its NDI receiver through
//! the issue-1320 detached reaper while the source is hidden, then re-arms `reset_ndi_receiver`
//! on show for a fresh connect.
//!
//! The role is an EXPLICIT per-source flag (`genlock_connect_on_show`, default OFF), set only by
//! the strih scene role lib: the stream box's always-on genlocked `NDI 2ME PGM` and the resolume
//! cg OBS keep the issue-764 keep-alive exactly as before (a platform-blind rule would disconnect
//! 2ME PGM whenever stream sits on PRE/POST).
//!
//! Std-only + offline per `.claude/rules/vendored-libobs-change-safety.md` /
//! `.claude/rules/distroav-receiver-lifecycle.md`: Facet A source-anchors the patch (revert
//! protection against a `git subtree pull`), Facet B lifts the pure park decision VERBATIM,
//! compiles it with the strict C flags and runs its truth table (the truth table IS the spec).
//! Runs offline: `CARGO_MANIFEST_DIR=<worktree> rustc --test --edition 2021 <this file>`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";
const WF_FULL: &str = ".github/workflows/windows-genlock.yml";
const WF_FAST: &str = ".github/workflows/windows-genlock-fast.yml";
const PARK_MARKER: &str = "genlock-park '";
const PARK_FN: &str = "static inline bool genlock_connect_on_show_park_decision(";

fn repo(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn read(rel: &str) -> String {
    let p = repo(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()))
}

/// Collapse every run of ASCII whitespace to a single space so anchors survive reformatting.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ----------------------------------------------------------------------------------------------
// Facet A — source anchors.
// ----------------------------------------------------------------------------------------------

#[test]
fn connect_on_show_is_a_whitelisted_default_off_role_flag() {
    let src = squish(&read(NDI_SOURCE));
    assert!(
        src.contains(r#"#define PROP_GENLOCK_CONNECT_ON_SHOW "genlock_connect_on_show""#),
        "{NDI_SOURCE}: issue 1242 — the `genlock_connect_on_show` role-flag constant is gone."
    );
    assert!(
        src.contains("PROP_GENLOCK_MONITOR, /*") && src.contains("PROP_GENLOCK_CONNECT_ON_SHOW, /* issue 1295"),
        "{NDI_SOURCE}: issue 1242 — PROP_GENLOCK_CONNECT_ON_SHOW must be a GENLOCK_WHITELIST_PROPS \
         entry (an operator-visible per-source role, never a forced value)."
    );
    assert!(
        src.contains("obs_properties_add_bool(props, PROP_GENLOCK_CONNECT_ON_SHOW"),
        "{NDI_SOURCE}: issue 1242 — the whitelist UI must expose the connect-on-show bool."
    );
    assert!(
        src.contains("obs_data_set_default_bool(settings, PROP_GENLOCK_CONNECT_ON_SHOW, false);"),
        "{NDI_SOURCE}: issue 1242 — connect-on-show must default OFF so every box that does not \
         opt in (stream 2ME PGM, the resolume cg OBS) keeps the issue-764 keep-alive unchanged."
    );
}

#[test]
fn issue_764_keep_alive_behavior_stays_certified() {
    // The park must NOT be implemented through stock STOP_RESUME: that hide path joins the receiver
    // thread on the graphics thread. The forced behavior stays KEEP_ACTIVE.
    let src = squish(&read(NDI_SOURCE));
    assert!(
        src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_KEEP_ACTIVE, false}"),
        "{NDI_SOURCE}: issue 1242 — GENLOCK_FORCED_SETTINGS must still force KEEP_ACTIVE; the \
         connect-on-show role parks IN the thread, never via a graphics-thread join on hide."
    );
    assert!(
        !src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_STOP_RESUME_LAST_FRAME, false}")
            && !src.contains("{PROP_BEHAVIOR, false, PROP_BEHAVIOR_STOP_RESUME_BLANK, false}"),
        "{NDI_SOURCE}: issue 1242 — a STOP_RESUME behavior in the forced table would join the \
         receiver thread on the OBS graphics thread at every hide (issue-1320 render-freeze class)."
    );
}

#[test]
fn update_snapshots_the_role_flags_under_the_lockdown() {
    let src = squish(&read(NDI_SOURCE));
    assert!(
        src.contains("bool genlock_monitor;") && src.contains("bool connect_on_show;"),
        "{NDI_SOURCE}: issue 1242 — ndi_source_config_t must carry the genlock_monitor + \
         connect_on_show role snapshot the receiver thread reads."
    );
    assert!(
        src.contains(
            "s->config.genlock_monitor = genlock_lockdown && obs_data_get_bool(settings, PROP_GENLOCK_MONITOR);"
        ),
        "{NDI_SOURCE}: issue 1242 — ndi_source_update must snapshot the monitor role (genlock only)."
    );
    assert!(
        src.contains(
            "s->config.connect_on_show = genlock_lockdown && obs_data_get_bool(settings, PROP_GENLOCK_CONNECT_ON_SHOW);"
        ),
        "{NDI_SOURCE}: issue 1242 — ndi_source_update must snapshot the program-path role, gated on \
         the genlock lockdown (a non-genlock/aux input never parks)."
    );
}

#[test]
fn receiver_loop_parks_before_the_reset_block_and_reconnects_on_show() {
    let raw = read(NDI_SOURCE);
    let src = squish(&raw);
    let call = "genlock_connect_on_show_park_decision(genlock_source_is_active(s->obs_source), \
                s->config.genlock_monitor, s->config.connect_on_show, obs_source_showing(s->obs_source))";
    let call_at = src.find(call).unwrap_or_else(|| {
        panic!("{NDI_SOURCE}: issue 1242 — the receiver loop no longer CALLS the park decision with the live state")
    });
    let loop_at = src
        .find("while (s->running) {")
        .expect("receiver loop `while (s->running) {` not found");
    let reset_at = src
        .find("if (s->config.reset_ndi_receiver) {")
        .expect("reset block `if (s->config.reset_ndi_receiver) {` not found");
    assert!(
        loop_at < call_at && call_at < reset_at,
        "{NDI_SOURCE}: issue 1242 — the park check must sit at the TOP of the receiver loop, BEFORE \
         the reset block, so a hidden program-path input never creates a receiver (no connect churn)."
    );
    // Park: hand the receiver + framesync to the issue-1320 detached reaper (never an inline
    // recv_destroy -- a slow teardown would delay the next show's reconnect).
    assert!(
        src.contains("ndi_reap_receiver_detached(ndiLib, park_frame_sync, park_receiver);"),
        "{NDI_SOURCE}: issue 1242 — a park must release the receiver through ndi_reap_receiver_detached."
    );
    assert!(
        src.contains("NDIlib_recv_instance_t park_receiver = ndi_receiver;"),
        "{NDI_SOURCE}: issue 1242 — the park must snapshot ndi_receiver before nulling the local."
    );
    // A parked input is reported idle (not connected) to the issue-1299 lock facet so the
    // in-OBS LOCK indicator never reads a hidden-by-design input as DEGRADED.
    assert!(
        src.contains("set_genlock_connected(s->obs_source, false);"),
        "{NDI_SOURCE}: issue 1242 — a parked input must report connected=false to libobs (issue 1299 facet)."
    );
    // Unpark: re-arm a fresh connect + a fresh issue-767 stale window.
    let unpark = src
        .find("state=unparked")
        .expect("issue 1242: the unpark log line (state=unparked) is gone");
    let window = &src[unpark.saturating_sub(900)..unpark];
    assert!(
        window.contains("s->config.reset_ndi_receiver = true;") && window.contains("was_disconnected = true;"),
        "{NDI_SOURCE}: issue 1242 — on show the loop must re-arm reset_ndi_receiver (fresh connect) and \
         was_disconnected (a fresh issue-767 stale window) before reconnecting."
    );
    // The thread must never `break` out of the loop on park (a break leaves s->running true =
    // a reattach-proof permanent death, distroav-receiver-lifecycle.md).
    let park_region_end = reset_at;
    assert!(
        !src[call_at..park_region_end].contains("break;"),
        "{NDI_SOURCE}: issue 1242 — the park region must `continue`, never `break` (a break is a \
         permanent reattach-proof death)."
    );
}

#[test]
fn park_marker_is_mutually_non_substring_vs_existing_families() {
    let families = [
        "genlock-fifo audit",
        "multiview-audit:",
        "program-render-audit:",
        "genlock-relock",
        "genlock-ndi-output",
        "genlock-ndi-filter",
        "genlock-reap:",
        "genlock-lock:",
        "genlock-lock-json:",
        "recv-timing #797",
        "genlock: NDI receiver keep-alive",
    ];
    for fam in families {
        assert!(
            !fam.contains(PARK_MARKER) && !PARK_MARKER.contains(fam),
            "issue 1242: `{PARK_MARKER}` collides (substring) with `{fam}`."
        );
    }
    let src = read(NDI_SOURCE);
    for line in ["state=parked", "state=unparked"] {
        assert!(
            src.contains(line),
            "{NDI_SOURCE}: issue 1242 — the `{PARK_MARKER}...{line}` log line is gone (the dev1 \
             watchdogs + E2E gates read it to classify a hidden-by-design input as SKIP)."
        );
    }
    assert!(
        src.contains(PARK_MARKER),
        "{NDI_SOURCE}: issue 1242 — the park marker is gone."
    );
}

#[test]
fn windows_genlock_workflows_mirror_the_park_anchors() {
    for wf in [WF_FULL, WF_FAST] {
        let text = read(wf);
        for needle in [
            "static inline bool genlock_connect_on_show_park_decision(",
            "ndi_reap_receiver_detached(ndiLib, park_frame_sync, park_receiver);",
        ] {
            assert!(
                text.contains(needle),
                "{wf}: issue 1242 — the pwsh vendored-source gate must mirror `{needle}` (the fast \
                 path hot-swaps distroav.dll, so both workflows must guard it)."
            );
        }
    }
}

// ----------------------------------------------------------------------------------------------
// Facet B — lift the pure park decision VERBATIM, compile it strictly, run its truth table.
// ----------------------------------------------------------------------------------------------

fn lift_park_decision() -> String {
    let src = read(NDI_SOURCE);
    let start = src
        .find(PARK_FN)
        .unwrap_or_else(|| panic!("issue 1242: {NDI_SOURCE} no longer defines {PARK_FN}"));
    let end = src[start..]
        .find("\n}\n")
        .map(|i| start + i + 3)
        .expect("issue 1242: the park decision has no closing brace `\\n}\\n`");
    src[start..end].to_string()
}

/// `(genlock_active, monitor, connect_on_show, showing, expected_park)` — all 16 rows.
/// Park iff genlocked AND not a monitor twin AND program-path role AND not shown.
fn vectors() -> Vec<(bool, bool, bool, bool, bool)> {
    let mut v = Vec::new();
    for ga in [false, true] {
        for mon in [false, true] {
            for cos in [false, true] {
                for show in [false, true] {
                    let want = ga && !mon && cos && !show;
                    v.push((ga, mon, cos, show, want));
                }
            }
        }
    }
    v
}

#[test]
fn park_decision_computes_the_spec_truth_table() {
    let helper = lift_park_decision();
    let vs = vectors();
    let mut c = String::from("#include <stdbool.h>\n#include <stdio.h>\n");
    c.push_str(&helper);
    c.push_str("\nint main(void){\n");
    for (ga, mon, cos, show, _) in &vs {
        c.push_str(&format!(
            "    printf(\"%d\\n\", genlock_connect_on_show_park_decision({}, {}, {}, {}) ? 1 : 0);\n",
            *ga as u8, *mon as u8, *cos as u8, *show as u8
        ));
    }
    c.push_str("    return 0;\n}\n");

    let dir = std::env::temp_dir().join("distroav_connect_on_show_park_1242");
    fs::create_dir_all(&dir).expect("create the scratch dir");
    let cfile = dir.join("park.c");
    let bin = dir.join("park.bin");
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
                "issue 1242: could not run the C compiler `{cc}` ({e}); this gate must FAIL, never \
                 skip, when the toolchain is absent. Install a C compiler or set CC."
            )
        });
    assert!(
        out.status.success(),
        "issue 1242: the lifted park decision does NOT COMPILE standalone under -Werror:\n{}\n{c}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin)
        .output()
        .expect("issue 1242: harness failed to execute");
    assert!(run.status.success(), "issue 1242: harness exited non-zero");
    let got: Vec<bool> = String::from_utf8(run.stdout)
        .expect("utf-8")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.trim() == "1")
        .collect();
    assert_eq!(
        got.len(),
        vs.len(),
        "issue 1242: harness printed {} of {} rows",
        got.len(),
        vs.len()
    );
    let diffs: Vec<String> = vs
        .iter()
        .zip(&got)
        .filter(|((_, _, _, _, want), g)| *g != want)
        .map(|((ga, mon, cos, show, want), g)| {
            format!("  park(genlock={ga}, monitor={mon}, connect_on_show={cos}, showing={show}) -> C {g}, want {want}")
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "issue 1242: park truth table mismatch:\n{}",
        diffs.join("\n")
    );
}
