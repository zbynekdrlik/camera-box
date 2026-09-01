//! #756 — imag-nb Multiview/Program projector STRAY-ACCUMULATION fix.
//!
//! Root-cause investigation (2026-07-15): the #756 ticket's "MV render divisor does not
//! ENGAGE on imag" conclusion was reached by grepping the imag OBS frontend log for
//! "#276/#278/divisor/multiview" text markers — but NO such log line has ever existed anywhere
//! in the vendored source (obs-display.c / OBSProjector.cpp never call `blog()` for the
//! divisor); a "zero markers" search result was therefore never real evidence either way.
//! Live-verified INSTEAD via `nm -D -u /usr/bin/obs | grep obs_display_set_render_divisor`
//! (the same #499 provisioning-time capability check) on the CURRENTLY deployed imag frontend
//! (`GENLOCK_BUILD_SHA.txt` = 26de1c3c2, byte-identical to the staged parity build) — the
//! symbol genuinely IS referenced (count=1), confirming the #276/#278/#293 divisor patch
//! (commit a50fa5a18, an ANCESTOR of 26de1c3c2) is compiled into the deployed frontend and the
//! WS `OpenVideoMixProjector` -> `obs_frontend_open_projector("Multiview")` -> `OpenSavedProjector`
//! -> `new OBSProjector(type=Multiview)` path (all stock, unmodified code) correctly reaches our
//! patched constructor. So the divisor mechanism was never structurally broken.
//!
//! The REAL defect, found live via `DISPLAY=:0 wmctrl -l` over SSH: imag was running **7 stray
//! Multiview + 7 stray Program projector windows** simultaneously (should be exactly 1 + 1).
//! `OBSBasic::OpenProjector()` (vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp) only
//! closes an existing projector on the same monitor when the OBS user-config key
//! `BasicWindow.CloseExistingProjectors` is true — that key has NO compiled-in default
//! (`config_get_bool` on a missing key with no registered default returns false) and imag's
//! `global.ini` never carried it. The #758 `[0/8]` preflight calls
//! `obs_phase2.py open-projectors` (-> `OpenVideoMixProjector`) UNCONDITIONALLY on every single
//! `recording-e2e.sh` run without ever closing the previous pair, so every run since imag's last
//! OBS restart added ANOTHER Multiview+Program pair — 7 accumulated over one afternoon of
//! testing. Seven independently-throttled Multiview renders (each divisor-engaged, but each
//! STILL costing real graphics-thread time) easily explain the render-health preflight's
//! intermittent sub-58fps failures (measured live: 57.77fps on CI run 29373998624) even though
//! the per-display divisor mechanism itself works correctly.
//!
//! The fix is two-layered, and — deliberately — touches ONLY `scripts/` + OBS user config, never
//! `vendor/obs-studio/**`: a vendor-source change would retrigger BOTH `linux-genlock.yml` AND
//! `windows-genlock.yml` (the ~150-min FULL frontend build) and put strih/stream/imag out of the
//! #756 cross-box genlock-build-SHA parity gate until all three boxes are redeployed — an
//! unjustified, high-risk rebuild cascade for a problem that is not actually a vendor-source bug.
//!
//!   1. `scripts/setup-imag.sh` seeds `BasicWindow.CloseExistingProjectors=true` (mirrors the
//!      existing `SaveProjectors=false` #522 seed) — every future `OpenVideoMixProjector` call
//!      then correctly REPLACES the same-monitor window instead of stacking a new one on top,
//!      fixing the root cause at its stock-OBS-native control point.
//!   2. `scripts/recording-e2e.sh`'s `[0/8]` preflight — right after `open-projectors` — asserts
//!      via `wmctrl -l` over SSH that imag shows EXACTLY 1 Multiview + 1 Program window, hard-
//!      failing loud (never silently self-healing) if not, so a future regression (a reprovision
//!      that forgets the config, an OBS upgrade that changes the default) is caught immediately
//!      rather than silently degrading render health run after run.
//!
//! Separately, the #758 MV render-divisor CAPABILITY check (WARN-only, `IMAG_DIVISOR_CAPABILITY_
//! FAIL` defaulting to `"0"`) is flipped to FAIL-by-default (`"1"`) — the nm-verified evidence
//! above proves the Linux divisor symbol genuinely ships (not a "known gap" anymore), so a
//! missing symbol on any future imag build is now a hard preflight failure, matching the #758
//! commit's own telegraphed one-line flip.
//!
//! Structural, source-text assertions — same discipline as the rest of this repo's harness suite
//! (see tests/harness_render_health_divisor_758.rs): both new checks are read-only preflight
//! probes against a live imag SSH host + live OBS-WS that only the rig itself can exercise
//! end-to-end.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    let p = manifest_dir().join(rel);
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

const SETUP: &str = "scripts/setup-imag.sh";
const RECORDING_E2E: &str = "scripts/recording-e2e.sh";

// ---------------------------------------------------------------------------------------------
// 1. setup-imag.sh seeds CloseExistingProjectors=true (the stock-OBS-native fix)
// ---------------------------------------------------------------------------------------------

#[test]
fn setup_imag_seeds_close_existing_projectors_756() {
    let body = read(SETUP);
    assert!(
        body.contains("CloseExistingProjectors=true"),
        "{SETUP} must seed BasicWindow.CloseExistingProjectors=true (#756) — without it, \
         OBSBasic::OpenProjector() never closes a same-monitor projector before opening a new \
         one (config_get_bool on this key has NO compiled-in default -> false), so every \
         `obs_phase2.py open-projectors` call the #758 preflight makes on every single \
         recording-e2e.sh run stacks ANOTHER Multiview+Program pair on top of the last -- \
         live-caught as 7 stray pairs accumulated on imag"
    );
}

// ---------------------------------------------------------------------------------------------
// 1b. FUNCTIONAL guard: seed_ini() must land the key INSIDE the file's EXISTING [BasicWindow]
//     section, never as a second, duplicate `[BasicWindow]` header appended later in the file.
//
//     Live-caught (2026-07-15): a first attempt at this fix appended a brand-new
//     `[BasicWindow]\nCloseExistingProjectors=true\n` block to the END of imag's ALREADY-
//     populated user.ini (which already had its own `[BasicWindow]` section from an earlier
//     provisioning run). libobs's util/config-file.c is a CUSTOM ini parser (NOT Qt's
//     QSettings — an earlier comment in this file assuming section-merging was wrong) that
//     stores sections in a uthash table keyed by name; a second `[BasicWindow]` header does
//     NOT merge into the first section — every config_get_bool()/config_find_item() lookup
//     only ever resolves the FIRST-inserted section, so a key seeded into a later duplicate
//     section is silently unreachable, and gets DROPPED ENTIRELY the next time OBS itself
//     cleanly saves the file (confirmed live: after one OBS restart, the appended block had
//     vanished from disk). Proven on the live rig: 4 open-projectors calls in a row still
//     stacked 4 stray Multiview + 4 stray Program windows with the buggy seed applied.
//
//     A purely textual check (`body.contains("CloseExistingProjectors=true")`, test 1 above)
//     cannot catch this — the string legitimately IS in the script either way. This test
//     extracts the REAL seed_ini() function body from the script (sed range-match, no
//     hand-duplicated copy that could drift) and actually RUNS it against synthetic fixtures.
// ---------------------------------------------------------------------------------------------

/// Extract the real `seed_ini() { ... }` function body verbatim from setup-imag.sh (the
/// function is defined WELL AFTER the file's `BASH_SOURCE[0] != $0` early-return guard, so it
/// cannot be reached via a plain `source` the way the top-of-file pure functions are in
/// tests/setup_imag_pure_functions.rs — extract it as text instead).
fn extract_seed_ini() -> String {
    let body = read(SETUP);
    let start = body
        .find("seed_ini() {")
        .expect("scripts/setup-imag.sh must define seed_ini()");
    let after_start = &body[start..];
    let end = after_start
        .find("\n}\n")
        .expect("seed_ini()'s closing brace (a bare `}` on its own line) must exist");
    after_start[..end + 2].to_string()
}

/// Run the extracted seed_ini() function against a fixture file's INITIAL content, return the
/// file's content afterward.
fn run_seed_ini_on(initial_content: &str) -> String {
    let tmp = tempfile::NamedTempFile::new().expect("tempfile");
    fs::write(tmp.path(), initial_content).expect("write fixture");
    let harness = format!(
        "set -euo pipefail\n{}\nseed_ini \"$F\"\n",
        extract_seed_ini()
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&harness)
        .env("F", tmp.path())
        .output()
        .expect("failed to run bash harness");
    assert!(
        out.status.success(),
        "seed_ini() harness failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    fs::read_to_string(tmp.path()).expect("read fixture back")
}

#[test]
fn seed_ini_inserts_close_existing_projectors_into_the_existing_basicwindow_section_756() {
    // Simulates the REAL live state that broke: a file that already has a populated
    // [BasicWindow] section (from an earlier provisioning run) AND a [General] section after
    // it — exactly imag's actual user.ini shape before this fix.
    let fixture = "[OBSWebSocket]\nServerEnabled=true\n\n\
                   [BasicWindow]\nSaveProjectors=true\nPreviewEnabled=true\n\n\
                   [General]\nLastVersion=536936450\n";
    let result = run_seed_ini_on(fixture);

    assert!(
        result.contains("CloseExistingProjectors=true"),
        "seed_ini() must add the key somewhere: {result}"
    );

    // The critical assertion: the key must land BEFORE the [General] section header that was
    // already in the file — i.e. INSIDE the existing [BasicWindow] section — never appended
    // as a NEW section further down (which would be silently unreachable, see the module doc
    // comment above).
    let key_idx = result
        .find("CloseExistingProjectors=true")
        .expect("key must be present");
    let general_idx = result
        .find("[General]")
        .expect("the fixture's original [General] section must survive");
    assert!(
        key_idx < general_idx,
        "seed_ini() must insert CloseExistingProjectors=true INSIDE the file's EXISTING \
         [BasicWindow] section (before the next section header), never append a duplicate \
         [BasicWindow] block later in the file -- a duplicate section is silently unreachable \
         via config_get_bool()/config_find_item() (libobs util/config-file.c resolves only the \
         FIRST-inserted section for a given name) and gets dropped entirely the next time OBS \
         itself saves the file. Got:\n{result}"
    );

    // Must never create a SECOND `[BasicWindow]` header — that is the exact bug shape.
    let basicwindow_count = result.matches("[BasicWindow]").count();
    assert_eq!(
        basicwindow_count, 1,
        "seed_ini() must never produce a second [BasicWindow] section header -- got \
         {basicwindow_count} occurrences in:\n{result}"
    );
}

#[test]
fn seed_ini_appends_a_fresh_basicwindow_section_when_none_exists_756() {
    // The genuinely-fresh-file case (first-ever provision, no [BasicWindow] section at all
    // yet) must still work via the fallback append path.
    let fixture = "[OBSWebSocket]\nServerEnabled=true\n";
    let result = run_seed_ini_on(fixture);
    assert!(
        result.contains("[BasicWindow]") && result.contains("CloseExistingProjectors=true"),
        "seed_ini() must append a fresh [BasicWindow] section carrying the key when the file \
         has none yet: {result}"
    );
    let basicwindow_count = result.matches("[BasicWindow]").count();
    assert_eq!(
        basicwindow_count, 1,
        "must produce exactly one [BasicWindow] section on a fresh file -- got \
         {basicwindow_count} in:\n{result}"
    );
}

// ---------------------------------------------------------------------------------------------
// 2. recording-e2e.sh hard-fails if imag ever shows more than 1 Multiview or 1 Program window
// ---------------------------------------------------------------------------------------------

#[test]
fn projector_count_preflight_checks_exactly_one_of_each_via_wmctrl() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("projector count must be EXACTLY 1 Multiview + 1 Program"),
        "{RECORDING_E2E} must name the #756 projector-count preflight step"
    );
    assert!(
        s.contains("wmctrl -l")
            && s.contains("Projector - Multiview")
            && s.contains("Projector - Program"),
        "{RECORDING_E2E}: the #756 projector-count check must read imag's actual window list \
         over SSH (wmctrl -l, DISPLAY=:0) and count the Multiview/Program projector windows by \
         their exact wmctrl titles -- obs-websocket itself has no projector-list introspection"
    );
}

#[test]
fn projector_count_preflight_fails_loud_on_any_mismatch_never_silently_self_heals() {
    let s = read(RECORDING_E2E);
    let idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 projector-count preflight step must exist");
    // Window covers the whole #756 block including the issue-1152 M4 lease-aware case
    // statement (lease-connector resolve + verdict lib + both fail arms) -- the lease slice
    // legitimately grew the block past the original 1200 chars, pushing `exit 1` to ~2300.
    let block = &s[idx..(idx + 3000).min(s.len())];
    assert!(
        block.contains("exit 1"),
        "#756: a projector count other than exactly 1+1 must hard-fail the preflight (exit 1) \
         -- never silently continue with stray render load already accumulating, and never a \
         silent self-heal that could mask a real regression: {block}"
    );
    assert!(
        block.contains("_mv_count") && block.contains("_pgm_count"),
        "#756: the check must track the Multiview and Program counts SEPARATELY -- a fused \
         single counter could pass with e.g. 2 Multiview + 0 Program: {block}"
    );
}

#[test]
fn windowed_stray_heal_runs_between_open_projectors_and_count_769() {
    // #769 (live ping-pong, 3 gate refusals 2026-07-15): OBS's launch-restore can recreate a
    // projector as a WINDOWED window (internal monitor = -1); OpenVideoMixProjector's
    // CloseExistingProjectors replace loop matches GetMonitor()==target only, so the windowed
    // stray is INVISIBLE to it and every ensure-open stacks one more window. The gate must
    // self-heal the mechanical stray (keep the NEWEST window per kind -- the one ensure-open
    // just opened on the proper monitor -- close older ones) BETWEEN ensure-open and the count
    // check; the count check still hard-fails if the heal does not converge (a genuinely
    // regressed config stays loud, #756).
    let s = read(RECORDING_E2E);
    assert!(
        s.contains("lib/imag-projector-heal.sh") && s.contains("imag_projector_heal_cmds"),
        "#769: recording-e2e.sh must source lib/imag-projector-heal.sh and run          imag_projector_heal_cmds on imag between open-projectors and the count check"
    );
    let open_idx = s
        .find("imag-nb Multiview + Program projectors must be OPEN")
        .expect("open-projectors preflight must exist");
    let heal_idx = s
        .find("imag_projector_heal_cmds")
        .expect("the #769 heal call must exist");
    let count_idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 count preflight must exist");
    assert!(
        open_idx < heal_idx && heal_idx < count_idx,
        "#769: heal must run AFTER ensure-open (so the newest window is the proper one to keep)          and BEFORE the count check (which stays the loud non-convergence backstop)"
    );

    let lib = read("scripts/lib/imag-projector-heal.sh");
    assert!(
        lib.contains("imag_projector_heal_cmds()"),
        "#769: the lib must define imag_projector_heal_cmds()"
    );
    for needle in ["Projector - $kind", "tail -1", "wmctrl -i -c"] {
        assert!(
            lib.contains(needle),
            "#769: heal snippet must enumerate projector windows per kind, keep the NEWEST              (highest id via sort | tail -1) and close the others (wmctrl -i -c) -- missing {needle:?}"
        );
    }
}

#[test]
fn projector_count_preflight_runs_after_open_projectors_and_before_studio_mode() {
    let s = read(RECORDING_E2E);
    let open_idx = s
        .find("imag-nb Multiview + Program projectors must be OPEN")
        .expect("the #758 open-projectors preflight must exist");
    let count_idx = s
        .find("projector count must be EXACTLY 1 Multiview + 1 Program")
        .expect("the #756 projector-count preflight must exist");
    let studio_idx = s
        .find("imag Studio Mode must be ON")
        .expect("the #767 studio-mode-on preflight must exist (inverted from the #758 force-OFF)");
    assert!(
        open_idx < count_idx,
        "#756: the projector-count check must run AFTER open-projectors opens/confirms the pair \
         (checking before they even exist would always fail)"
    );
    assert!(
        count_idx < studio_idx,
        "#756: the projector-count check should complete before moving on to the (unrelated) \
         Studio Mode preflight -- keeps the projector-lifecycle checks grouped together"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. the #758 divisor CAPABILITY check flips from WARN-only to FAIL-by-default (#756 evidence:
//    nm -D -u proves the symbol genuinely ships on the currently-deployed imag frontend)
// ---------------------------------------------------------------------------------------------

#[test]
fn divisor_capability_check_now_fails_by_default_756() {
    let s = read(RECORDING_E2E);
    assert!(
        s.contains(r#"IMAG_DIVISOR_CAPABILITY_FAIL="${IMAG_DIVISOR_CAPABILITY_FAIL:-1}""#),
        "#756: the divisor capability check must now default to FAIL (1) -- nm -D -u on the \
         live imag frontend (GENLOCK_BUILD_SHA.txt=26de1c3c2) confirms \
         obs_display_set_render_divisor genuinely IS referenced (count=1); this is no longer a \
         'known gap' the #758 WARN-only stance was hedging against"
    );
    // The flip stays a single conditional on the ONE env var (never a second, independently
    // hardcoded FAIL path) -- same discipline the #758 test suite already established.
    let fail_lines = s
        .lines()
        .filter(|l| l.contains("IMAG_DIVISOR_CAPABILITY_FAIL"))
        .count();
    assert_eq!(
        fail_lines, 2,
        "#756: IMAG_DIVISOR_CAPABILITY_FAIL must be referenced on exactly two LINES (the \
         default-assignment line + the one `if` check) -- got {fail_lines}"
    );
}
