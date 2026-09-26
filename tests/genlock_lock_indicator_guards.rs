//! #1298 — vendored-source anchor guards for the in-OBS genlock lock indicator.
//!
//! The libobs genlock-stats API, the DistroAV output hook, and the Qt statusbar widget are
//! all in the vendored tree, compiled only by the Windows/Linux genlock workflows. These
//! guards (the `tests/genlock_preload.rs` convention) assert the new symbols + log strings are
//! PRESENT so a future `git subtree pull` can't silently revert them, and that the new
//! `genlock-lock:` OBS-log marker stays mutually non-substring with every existing `genlock-*`
//! / `*-audit:` family (the `.claude/rules/jitter-audit-parser.md` rule). They are the Linux-CI
//! twin of the 3-copy pwsh source-anchor gates in both `windows-genlock{,-fast}.yml`
//! (`.claude/rules/obs-titlebar-build-id.md`) — keep all three in lock-step.
//!
//! Std-only + path via a runtime env lookup (not the `env!` macro) so it runs both under cargo
//! and standalone (`rustc --test tests/genlock_lock_indicator_guards.rs` from the repo root).

use std::path::PathBuf;

fn vendor_file(rel: &str) -> String {
    let base = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    let path = PathBuf::from(base).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Collapse all runs of whitespace to a single space — the Rust twin of the pwsh gates'
/// `-replace '\s+', ' '`, so the same pinned substring works in both.
fn squish(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

const OBS_API: &str = "vendor/obs-studio/libobs/obs.h";
const OBS_SOURCE: &str = "vendor/obs-studio/libobs/obs-source.c";
const OBS_OUTPUT: &str = "vendor/obs-studio/libobs/obs-output.c";
const OBS_INTERNAL: &str = "vendor/obs-studio/libobs/obs-internal.h";
const HEADER: &str = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";
const STATUSBAR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp";
const STATUSBAR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.hpp";
const NDI_OUTPUT: &str = "vendor/distroav/src/ndi-output.cpp";
const NDI_SOURCE: &str = "vendor/distroav/src/ndi-source.cpp";

fn assert_has(file: &str, needle: &str) {
    let src = squish(&vendor_file(file));
    assert!(
        src.contains(needle),
        "{file}: #1298 anchor `{needle}` is GONE — the genlock lock indicator was partially \
         reverted (e.g. a subtree pull). Re-apply it and keep the windows-genlock{{,-fast}}.yml \
         pwsh gates in lock-step."
    );
}

#[test]
fn source_stats_api_present() {
    // #1303 bumped the stats struct to v2 (append-only: +audio_enabled/audio_delay_ms/
    // audio_pairing_offset_ms); #1299 bumped it to v3 (append-only: +connected). Pin the current
    // version so a subtree pull that reverts the bump is caught.
    assert_has(OBS_API, "#define OBS_GENLOCK_STATS_VERSION 3");
    assert_has(OBS_API, "struct obs_genlock_stats {");
    assert_has(
        OBS_API,
        "obs_source_get_genlock_stats(const obs_source_t *source, struct obs_genlock_stats *stats)",
    );
    assert_has(OBS_SOURCE, "static void genlock_fill_stats(const obs_source_t *source, struct obs_genlock_stats *stats)");
    assert_has(OBS_SOURCE, "bool obs_source_get_genlock_stats(const obs_source_t *source, struct obs_genlock_stats *stats)");
    // #1299 — the receiver-connection producer chain: the setter export (driven by DistroAV's
    // recv_get_no_connections) + its use in the shared genlock_fill_stats. A revert of either
    // silently re-opens the absent-sender false-page.
    assert_has(
        OBS_API,
        "obs_source_set_genlock_connected(obs_source_t *source, bool connected)",
    );
    assert_has(OBS_SOURCE, "stats->connected = source->genlock_connected;");
    assert_has(
        OBS_SOURCE,
        "void obs_source_set_genlock_connected(obs_source_t *source, bool connected)",
    );
    // #1299 — the DistroAV producer: the receiver loop drives the connection state from
    // recv_get_no_connections()>0. A subtree pull that drops this call leaves connected=default(true)
    // forever, silently re-opening the absent-sender false-page.
    assert_has(
        NDI_SOURCE,
        "set_genlock_connected(s->obs_source, no_conn > 0)",
    );
    assert_has(
        NDI_SOURCE,
        "resolve_obs_export(\"obs_source_set_genlock_connected\")",
    );
}

#[test]
fn audit_routes_through_the_shared_fill() {
    // The `genlock-fifo audit` log line must read the SAME snapshot the API does, or the two
    // can disagree — #1298's core invariant.
    assert_has(
        OBS_SOURCE,
        "struct obs_genlock_stats gs; genlock_fill_stats(source, &gs);",
    );
    assert_has(OBS_SOURCE, "(unsigned long long)gs.frames_received");
    // #1303 appended the audio facet args after wall_qpc_drift_ms, so the audit line no longer
    // CLOSES on it — it now closes on the audio pairing offset. Pin both: wall_qpc_drift_ms is
    // still routed through the shared fill, and the audio facet rides the SAME line.
    assert_has(OBS_SOURCE, "(long long)gs.wall_qpc_drift_ms,");
    assert_has(OBS_SOURCE, "(long long)gs.audio_pairing_offset_ms);");
}

#[test]
fn output_stats_api_present() {
    assert_has(OBS_API, "#define OBS_GENLOCK_OUTPUT_STATS_VERSION 1");
    assert_has(OBS_API, "struct obs_genlock_output_stats {");
    assert_has(
        OBS_API,
        "obs_output_set_genlock_wall_stamping(obs_output_t *output, bool stamping)",
    );
    assert_has(OBS_API, "obs_output_get_genlock_stats(const obs_output_t *output, struct obs_genlock_output_stats *stats)");
    assert_has(
        OBS_OUTPUT,
        "void obs_output_set_genlock_wall_stamping(obs_output_t *output, bool stamping)",
    );
    assert_has(OBS_OUTPUT, "bool obs_output_get_genlock_stats(const obs_output_t *output, struct obs_genlock_output_stats *stats)");
    assert_has(OBS_INTERNAL, "bool genlock_is_genlock_output;");
    assert_has(OBS_INTERNAL, "bool genlock_wall_stamping;");
}

#[test]
fn distroav_registers_wall_stamping() {
    assert_has(
        NDI_OUTPUT,
        "obs_output_set_genlock_wall_stamping(o->output, true);",
    );
    assert_has(
        NDI_OUTPUT,
        "obs_output_set_genlock_wall_stamping(o->output, false);",
    );
}

#[test]
fn statusbar_indicator_present() {
    assert_has(STATUSBAR_CPP, "#include \"GenlockLockState.hpp\"");
    assert_has(STATUSBAR_CPP, "genlock_decide_lock_state(&f, &reason)");
    assert_has(STATUSBAR_CPP, "genlock-lock: state=%s inputs=%d/%d");
    assert_has(STATUSBAR_CPP, "GENLOCK ● LOCKED");
    assert_has(STATUSBAR_HPP, "void UpdateGenlockLabel();");
}

#[test]
fn decision_header_present_and_pure() {
    assert_has(
        HEADER,
        "static inline genlock_lock_state_t genlock_decide_lock_state(",
    );
    assert_has(HEADER, "typedef enum genlock_lock_state {");
    assert_has(HEADER, "typedef struct genlock_lock_facets {");
    // pure: the decision header must NOT pull OBS/Qt — it is a byte-for-byte port liftable by cc.
    let raw = vendor_file(HEADER);
    assert!(
        !raw.contains("#include <obs") && !raw.contains("#include <Q"),
        "{HEADER}: #1298 the decision header gained an OBS/Qt include — it must stay pure so the \
         parity gate can lift + compile it standalone with cc."
    );
}

#[test]
fn audio_parity_lock_term_present_1303() {
    // #1303: the audio DEGRADE term in the LOCK decision. The pure decision + its C mirror gain an
    // additive `audio_unpaired` facet + a new `AudioPairing` reason, and the widget aggregates the
    // per-source audio pairing-offset breach into that facet. Linux-CI twin of the #1298 pwsh gate's
    // #1303 anchor in BOTH windows-genlock{,-fast}.yml — keep all three in lock-step.
    assert_has(HEADER, "GENLOCK_LOCK_REASON_AUDIO_PAIRING = 9,");
    assert_has(HEADER, "int audio_unpaired;");
    // the widget fills the facet from the per-source v2 stats aggregate, and the reason token flows
    // through genlock_reason_key into the genlock-lock: log + genlock-lock-json: line.
    assert_has(
        STATUSBAR_CPP,
        "f.audio_unpaired = scan.audio_unpaired ? 1 : 0;",
    );
    assert_has(STATUSBAR_CPP, "return \"audio_pairing\";");
    assert_has(STATUSBAR_CPP, "aoff > GENLOCK_AUDIO_PAIRING_BOUND_MS");
}

#[test]
fn audio_unexpected_lock_term_present_1303() {
    // #1303: the audio-UNEXPECTED DEGRADE term — a source AUDIBLE when the certified per-box audio
    // table expects it silent (a camera on any box; the double-audio hazard). The pure decision + C
    // mirror gain an additive `audio_unexpected` facet + a new `AudioUnexpected=10` reason; the
    // widget flags it via the header's parity-gated camera classifier (the C mirror of
    // genlock_forced_table_audit::is_camera_input). Linux-CI twin of the #1303 pwsh gate in BOTH
    // windows-genlock{,-fast}.yml — keep all three in lock-step.
    assert_has(HEADER, "GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED = 10,");
    assert_has(HEADER, "int audio_unexpected;");
    assert_has(
        HEADER,
        "static inline int genlock_name_is_camera(const char *name)",
    );
    assert_has(
        STATUSBAR_CPP,
        "f.audio_unexpected = scan.audio_unexpected ? 1 : 0;",
    );
    assert_has(STATUSBAR_CPP, "return \"audio_unexpected\";");
    assert_has(STATUSBAR_CPP, "genlock_name_is_camera(nm.c_str())");
}

#[test]
fn genlock_lock_marker_is_mutually_non_substring() {
    // The new OBS-log family `genlock-lock:` must be mutually non-substring with every existing
    // marker (jitter-audit-parser.md), so a grep / parser keyed on one never matches another.
    const NEW: &str = "genlock-lock:";
    let existing = [
        "genlock-fifo audit '",
        "genlock-ndi-output audit '",
        "genlock-ndi-filter audit '",
        "genlock-relock",
        "genlock-acquire-bracket '%s':",
        "multiview-audit:",
        "program-render-audit:",
        "recv-timing #797 '",
        "asrc: source '",
    ];
    for m in existing {
        assert!(
            !NEW.contains(m) && !m.contains(NEW),
            "#1298: the new `genlock-lock:` marker collides (substring) with existing marker `{m}` \
             — pick a marker mutually non-substring with every genlock-* / *-audit: family."
        );
    }
}

#[test]
fn qpc_drift_books_a_fleet_date_step_1372() {
    // Issue 1372: the widget re-baselines its qpc history by a booked dantesync date step (one
    // genlock-wall-step: line) instead of reading it DEGRADED for the whole 300 s window. The
    // decision is the parity-gated genlock_qpc_wall_step_rebase_ms; a clock set and a step storm
    // still degrade. Mirror of the issue-1372 pwsh block in both windows-genlock*.yml.
    assert_has(
        HEADER,
        "static inline int64_t genlock_qpc_wall_step_rebase_ms(int64_t jump_ms, int64_t step_bound_ms, int64_t book_max_ms, int64_t booked_in_window, int64_t steps_per_window)",
    );
    assert_has(
        STATUSBAR_CPP,
        "const int64_t rebase = genlock_qpc_wall_step_rebase_ms( jump, GENLOCK_QPC_STEP_BOUND_MS, GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS, (int64_t)genlockQpcBookedSteps.size(), GENLOCK_QPC_WALL_STEPS_PER_WINDOW);",
    );
    assert_has(
        STATUSBAR_CPP,
        "for (auto &sample : genlockQpcHistory) sample.second += rebase;",
    );
    assert_has(
        STATUSBAR_CPP,
        "\"genlock-wall-step: the wall clock stepped %lld ms",
    );
    assert_has(STATUSBAR_HPP, "std::deque<qint64> genlockQpcBookedSteps;");
    assert_has(
        STATUSBAR_HPP,
        "void BookGenlockWallStep(qint64 now_ms, int64_t qpc_signed_ms);",
    );
    assert_has(
        STATUSBAR_CPP,
        "void OBSBasicStatusBar::BookGenlockWallStep(qint64 now_ms, int64_t qpc_signed_ms)",
    );
    // the booking runs BEFORE this tick's sample joins the history (the jump is new vs back())
    let src = squish(&vendor_file(STATUSBAR_CPP));
    assert!(
        src.contains(
            "BookGenlockWallStep(now_ms, scan.qpc_signed_ms); genlockQpcHistory.emplace_back(now_ms, scan.qpc_signed_ms);"
        ),
        "issue 1372: the date-step booking must run right before the new sample is pushed"
    );
    // ... and that is the ONLY push of a qpc sample (a second, unbooked push would bypass it)
    assert_eq!(
        src.matches("genlockQpcHistory.emplace_back(").count(),
        1,
        "issue 1372: a second genlockQpcHistory push would skip the date-step booking"
    );
    // the widget's two booking constants stay equal to the Rust authority's
    let rust = vendor_file("src/genlock_lock_state.rs");
    for (widget, authority) in [
        (
            "static constexpr int64_t GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS = 66;",
            "pub const GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS: i64 = 66;",
        ),
        (
            "static constexpr int64_t GENLOCK_QPC_WALL_STEPS_PER_WINDOW = 1;",
            "pub const GENLOCK_QPC_WALL_STEPS_PER_WINDOW: i64 = 1;",
        ),
    ] {
        assert_has(STATUSBAR_CPP, widget);
        assert!(
            rust.contains(authority),
            "issue 1372: `{authority}` drifted in src/genlock_lock_state.rs — keep the widget and \
             the Rust authority in lock-step"
        );
    }
}

#[test]
fn wall_step_markers_are_mutually_non_substring_1372() {
    // Issue 1372's two new OBS-log families: the widget's booked date step and the render tick's
    // one-tick re-grid. Mutually non-substring with every existing marker and with each other.
    let new = ["genlock-wall-step:", "genlock-regrid:"];
    let existing = [
        "genlock-lock:",
        "genlock-lock-json:",
        "genlock-fifo audit '",
        "genlock-ndi-output audit '",
        "genlock-ndi-filter audit '",
        "genlock-relock",
        "genlock-acquire-bracket '%s':",
        "genlock-shallow-lock",
        "genlock-shallow-remeasure",
        "genlock-park '",
        "multiview-audit:",
        "program-render-audit:",
        "recv-timing #797 '",
        "asrc: source '",
    ];
    for n in new {
        for m in existing.iter().chain(new.iter().filter(|x| **x != n)) {
            assert!(
                !n.contains(m) && !m.contains(n),
                "issue 1372: marker `{n}` collides (substring) with `{m}`"
            );
        }
    }
}
