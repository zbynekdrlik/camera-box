//! #1299 — vendored-source anchor guards for the fleet-visible genlock-lock-json: emission.
//!
//! The #1298 statusbar widget gained a machine-readable `genlock-lock-json:` line (the versioned
//! JSON the #1299 bundle-state facet + dev1 watchdog read). These guards assert the new symbols +
//! log string are PRESENT so a future `git subtree pull` can't silently revert them, and that the
//! new marker stays mutually non-substring with every existing `genlock-*` / `*-audit:` family
//! (the `.claude/rules/jitter-audit-parser.md` rule) — INCLUDING the #1298 `genlock-lock:` line it
//! sits beside. They are the Linux-CI twin of the 3-copy pwsh source-anchor gates in both
//! `windows-genlock{,-fast}.yml` (`.claude/rules/obs-titlebar-build-id.md`) — keep all three in
//! lock-step.
//!
//! Std-only + path via a runtime env lookup (not the `env!` macro) so it runs both under cargo and
//! standalone (`rustc --test tests/genlock_lock_json_guards.rs` from the repo root).

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

const STATUSBAR_CPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.cpp";
const STATUSBAR_HPP: &str = "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.hpp";

fn assert_has(file: &str, needle: &str) {
    let src = squish(&vendor_file(file));
    assert!(
        src.contains(needle),
        "{file}: #1299 anchor `{needle}` is GONE — the fleet-visible genlock-lock-json: emission \
         was partially reverted (e.g. a subtree pull). Re-apply it and keep the \
         windows-genlock{{,-fast}}.yml pwsh gates in lock-step."
    );
}

#[test]
fn genlock_lock_json_emission_present() {
    // the versioned machine-readable line the dev1 fleet watchdog + bundle-state read
    assert_has(STATUSBAR_CPP, "genlock-lock-json: %s (#1299)");
    // the pure builder + escaper (no obs_data dependency -> Facet B lift-compilable)
    assert_has(STATUSBAR_CPP, "std::string genlock_build_lock_json(");
    assert_has(
        STATUSBAR_CPP,
        "void genlock_json_append_escaped(std::string &out, const char *s)",
    );
    // the structured per-input record the builder walks
    assert_has(STATUSBAR_CPP, "struct GenlockInputRow {");
    // the heartbeat constant + its own change-tracking state (separate from genlockLastLogged*)
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int GENLOCK_JSON_HEARTBEAT_TICKS = 30;",
    );
    assert_has(STATUSBAR_HPP, "int genlockJsonHeartbeatTicks = 0;");
    assert_has(STATUSBAR_HPP, "int genlockJsonLastState = -1;");
}

#[test]
fn genlock_lock_recent_event_offender_present_1299_part3() {
    // #1299 Part 3: the builder emits the recent_event_inputs attribution (top offender name+count)
    // and the widget feeds recent_event from the CONNECTED-only, PHASE-only aggregate via the pure
    // genlock_input_phase_events rule (drops underruns + absent-sender rebind churn). A subtree pull
    // that reverts either silently re-opens the chronic recent_event false-page.
    assert_has(
        STATUSBAR_CPP,
        "genlock_json_append_escaped(j, recent_event_input_name);",
    );
    assert_has(
        STATUSBAR_CPP,
        "genlock_input_phase_events(r.connected ? 1 : 0, r.idle ? 1 : 0, r.relocks,",
    );
    // the enriched human reason (reason=recent_event:<name>) built from the offender name
    assert_has(
        STATUSBAR_CPP,
        "reason == GENLOCK_LOCK_REASON_RECENT_EVENT && !recent_event_input_name.empty()",
    );
    // the pure rule + its C mirror anchor (kept in lock-step by tests/genlock_lock_state_parity.rs)
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "static inline uint64_t genlock_input_phase_events(int connected, int idle, uint64_t relocks,",
    );
}

#[test]
fn genlock_lock_idle_input_class_present_1341() {
    // #1341: a CONNECTED-but-IDLE input (a keep-alive-only SongPlayer playlist input) is excluded
    // from the DEGRADED gate. The widget derives per-input idle from the received-frame delta over
    // the window, carries n_idle + per-input idle in the v6 JSON, and passes idle to the pure
    // phase-events rule. A subtree pull that reverts any of these silently re-opens the chronic
    // idle-sender DEGRADED/recent_event false page. Linux-CI twin of the #1341 pwsh gate in BOTH
    // windows-genlock{,-fast}.yml — keep all three in lock-step.
    // the top-level n_idle emitter + per-input idle key the bundle-state parser reads
    assert_has(STATUSBAR_CPP, "\\\"n_idle\\\":");
    assert_has(STATUSBAR_CPP, "\\\"idle\\\":");
    // the widget fills the facet from the post-scan idle classification
    assert_has(STATUSBAR_CPP, "f.n_idle = scan.n_idle;");
    // the idle floor + window constants driving the classification
    assert_has(
        STATUSBAR_CPP,
        "static constexpr uint64_t GENLOCK_IDLE_INPUT_MIN_FRAMES = 60;",
    );
    // the pure decision's C mirror gains the n_idle facet + the idle phase-events param (kept in
    // lock-step by tests/genlock_lock_state_parity.rs)
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "int n_idle;",
    );
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "static inline uint64_t genlock_input_phase_events(int connected, int idle, uint64_t relocks,",
    );
}

#[test]
fn genlock_lock_audio_unexpected_offender_present_1303() {
    // #1303: the widget flags an audio-ENABLED silent-by-contract source (a camera) via the C mirror
    // of the certified-table camera classifier, and emits the offender in the v4
    // audio_unexpected_inputs list so a DEGRADED/audio_unexpected page names it. A subtree pull that
    // reverts any of these silently re-opens the double-audio blind spot.
    // the widget scan uses the header's parity-gated camera classifier
    assert_has(STATUSBAR_CPP, "genlock_name_is_camera(nm.c_str())");
    // the v4 JSON key the bundle-state parser reads
    assert_has(STATUSBAR_CPP, "\\\"audio_unexpected_inputs\\\":[");
    // the greppable reason token
    assert_has(STATUSBAR_CPP, "return \"audio_unexpected\";");
    // the pure camera classifier + its private helper in the header (parity-gated by
    // tests/genlock_lock_state_parity.rs against the canonical genlock_forced_table_audit::is_camera_input)
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "static inline int genlock_name_is_camera(const char *name)",
    );
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "GENLOCK_LOCK_REASON_AUDIO_UNEXPECTED = 10,",
    );
}

#[test]
fn genlock_lock_qpc_windowed_drift_present_1299_part4() {
    // #1299 Part 4: the qpc_drift verdict is NOT the cumulative wall-vs-QPC offset that grew unbounded
    // and false-paged the fleet overnight (since #1357 it is the wall STEP only — see the test below).
    // A subtree pull that reverts any of these silently re-opens that chronic false page.
    // the widget calls the parity-gated pure decision (not an inline `> 100 ms` compare)
    assert_has(STATUSBAR_CPP, "genlock_qpc_drift_beyond_bound(");
    // it polls the expected slew the disciplined clock reports (f_ptp + f_phase)
    assert_has(STATUSBAR_CPP, "obs_data_get_double(d, \"f_ptp_ppm\")");
    assert_has(STATUSBAR_CPP, "obs_data_get_double(d, \"f_phase_ppm\")");
    // the v5 report-only telemetry keys the bundle-state parser reads
    assert_has(STATUSBAR_CPP, "\\\"qpc_drift_ppm\\\":");
    // issue 1372 part D bumped the schema literal to v7 (additive media_clock); pin the current version.
    assert_has(STATUSBAR_CPP, "{\\\"v\\\":7,\\\"state\\\":");
    // the windowed-rate ring member + the bounds
    assert_has(
        STATUSBAR_HPP,
        "std::deque<std::pair<qint64, int64_t>> genlockQpcHistory;",
    );
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int64_t GENLOCK_QPC_STEP_BOUND_MS = 33;",
    );
    // the pure decision's C mirror + its parity anchor (kept in lock-step by
    // tests/genlock_lock_state_parity.rs)
    assert_has(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
        "static inline int genlock_qpc_drift_beyond_bound(int rate_ready, long long drift_delta_ms,",
    );
}

#[test]
fn genlock_lock_qpc_drift_is_the_step_only_1357() {
    // #1357 scope C: the qpc_drift term DEGRADES on a wall STEP only — one semantics on every box.
    // The removed RATE branch compared a 300 s windowed wall-vs-monotonic rate against ONE
    // instantaneous dantesync `f_ptp + f_phase` sample: on Linux the measured side is 0 by construction
    // (CLOCK_MONOTONIC is kernel-disciplined), on Windows it is the free QPC — so the same clock event
    // gave different LOCK verdicts per box (28 false DEGRADED on strih-lx, 4 on stream, 24.9.2026).
    // A subtree pull that brings the rate bound back re-opens that per-box divergence.
    let widget = squish(&vendor_file(STATUSBAR_CPP));
    assert!(
        !widget.contains("GENLOCK_QPC_DRIFT_PPM_BOUND"),
        "{STATUSBAR_CPP}: the qpc_drift RATE bound is back — the verdict must be the wall STEP only (#1357)"
    );
    assert!(
        widget.contains(&squish(
            "genlock_qpc_drift_beyond_bound( qpc_rate_ready, qpc_delta_ms, qpc_elapsed_ms, qpc_max_step_ms, GENLOCK_QPC_STEP_BOUND_MS, &qpc_measured_ppm);"
        )),
        "{STATUSBAR_CPP}: the widget must call the step-only decision (rate_ready, delta, elapsed, step, bound, &measured) (#1357)"
    );
    let header = squish(&vendor_file(
        "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp",
    ));
    assert!(
        header.contains(&squish(
            "static inline int genlock_qpc_drift_beyond_bound(int rate_ready, long long drift_delta_ms, long long elapsed_ms, long long max_step_ms, long long step_bound_ms, double *measured_ppm_out)"
        )),
        "GenlockLockState.hpp: genlock_qpc_drift_beyond_bound must take no expected_ppm / ppm_bound (#1357)"
    );
    // the windowed rate + the dantesync slew survive as REPORT-ONLY JSON telemetry
    assert_has(STATUSBAR_CPP, "\\\"qpc_expected_ppm\\\":");
}

#[test]
fn genlock_lock_media_clock_term_present_1372_part_d() {
    // Issue 1372 part D: the MEDIA-clock (audio clock) term. The widget reduces the wall-vs-media drift
    // growth over the 600 s window (steps excluded) + the Windows discipline outcome into the pure,
    // parity-gated verdict, feeds it to the decision, names it in the label/log, and carries it on the
    // v7 json. A subtree pull that reverts any of these makes the indicator green again while the
    // audio mixer walks off the video grid. Linux-CI twin of the part-D pwsh gate in BOTH
    // windows-genlock{,-fast}.yml -- keep all three in lock-step.
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int GENLOCK_MEDIA_CLOCK_WINDOW_S = 600;",
    );
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int64_t GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US = 2000;",
    );
    assert_has(
        STATUSBAR_HPP,
        "std::deque<std::pair<qint64, int64_t>> genlockMediaClockHistory;",
    );
    // the pure reduction (steps left to the step verdict) + verdict, fed the widget's own samples
    assert_has(
        STATUSBAR_CPP,
        "genlock_media_clock_window_drift_us( sample_ms.data(), offset_us.data(), (int)offset_us.size(), GENLOCK_MEDIA_CLOCK_WINDOW_S, GENLOCK_MEDIA_CLOCK_MAX_GAP_MS, rate_scratch.data(), &counted_ms);",
    );
    assert_has(
        STATUSBAR_CPP,
        "genlock_media_clock_verdict(media_window_ready, media_drift_us, GENLOCK_MEDIA_CLOCK_DRIFT_BOUND_US, media_discipline, clock_present ? 1 : 0);",
    );
    // Windows reads the libobs discipline outcome; every other OS is not applicable
    assert_has(
        STATUSBAR_CPP,
        "const int media_discipline = os_gettime_discipline();",
    );
    assert_has(
        STATUSBAR_CPP,
        "const int media_discipline = GENLOCK_MEDIA_DISCIPLINE_NOT_APPLICABLE;",
    );
    assert_has(STATUSBAR_CPP, "f.media_clock = (int)media_clock;");
    // the reason token, the label text and the v7 json object the bundle-state parser reads
    assert_has(STATUSBAR_CPP, "return \"media_clock\";");
    assert_has(STATUSBAR_CPP, "QString(\"audio clock drift %1 ms/%2 min\")");
    assert_has(
        STATUSBAR_CPP,
        "QString(\"audio clock not disciplined (%1)\")",
    );
    assert_has(STATUSBAR_CPP, "\\\"media_clock\\\":{\\\"state\\\":");
    assert_has(STATUSBAR_CPP, "\\\"discipline\\\":");
    // the pure C mirror (parity-gated by tests/genlock_lock_state_parity.rs)
    let hpp = "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp";
    assert_has(hpp, "GENLOCK_LOCK_REASON_MEDIA_CLOCK = 11,");
    assert_has(hpp, "int media_clock;");
    assert_has(
        hpp,
        "static inline genlock_media_clock_t genlock_media_clock_verdict(int window_ready, int64_t drift_us,",
    );
    assert_has(
        hpp,
        "static inline int64_t genlock_media_clock_window_drift_us(const int64_t *t_ms, const int64_t *offset_us, int n, int64_t window_s, int64_t max_gap_ms, int64_t *scratch,",
    );
    // the offset is sampled in us by the widget itself (integer-ms libobs drift cannot tell a dantesync
    // step from a rate), and the rate is the median of the per-pair rates, so steps never move it
    assert_has(
        STATUSBAR_CPP,
        "static int64_t genlock_wall_minus_media_us()",
    );
    assert_has(
        STATUSBAR_CPP,
        "genlockMediaClockHistory.emplace_back(now_ms, genlock_wall_minus_media_us());",
    );
    assert_has(
        STATUSBAR_CPP,
        "genlock_media_clock_window_ready(counted_ms, GENLOCK_MEDIA_CLOCK_WINDOW_S);",
    );
    assert_has(
        STATUSBAR_CPP,
        "static constexpr int64_t GENLOCK_MEDIA_CLOCK_MAX_GAP_MS = 5000;",
    );
    // the reduction lives in its own helper (UpdateGenlockLabel stays readable)
    assert_has(
        STATUSBAR_CPP,
        "const GenlockMediaClockTick mc = ReduceGenlockMediaClock(now_ms, clock_present);",
    );
    // the media term must never become a rate bound on the qpc STEP verdict (issue 1357 stays intact)
    let widget = squish(&vendor_file(STATUSBAR_CPP));
    assert!(
        !widget.contains("GENLOCK_QPC_DRIFT_PPM_BOUND"),
        "{STATUSBAR_CPP}: the media-clock term must not bring back the qpc rate bound"
    );
}

#[test]
fn genlock_lock_json_marker_is_mutually_non_substring() {
    // The new OBS-log family `genlock-lock-json:` must be mutually non-substring with every
    // existing marker (jitter-audit-parser.md) — ESPECIALLY the #1298 `genlock-lock:` line it sits
    // beside, so a parser keyed on one never matches the other.
    const NEW: &str = "genlock-lock-json:";
    let existing = [
        "genlock-lock:",
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
            "#1299: the new `genlock-lock-json:` marker collides (substring) with existing marker \
             `{m}` — pick a marker mutually non-substring with every genlock-* / *-audit: family."
        );
    }
}
