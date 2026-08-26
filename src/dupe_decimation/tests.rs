use super::*;
// (#1165 split) VecDeque was an ambient file-top import before the module split; the moved
// test bodies still use it directly, so restate the import here (the sole include-path edit).
use std::collections::VecDeque;

// ── dupe_content_hash ──────────────────────────────────────────────────

#[test]
fn identical_frames_hash_equal() {
    let frame = vec![0x42u8; 4 * 2 * 8]; // width=4 (row_bytes=8), height=8
    let a = dupe_content_hash(&frame, 4, 8, 8);
    let b = dupe_content_hash(&frame, 4, 8, 8);
    assert_eq!(a, b, "identical byte content must hash identically");
}

#[test]
fn differing_frames_hash_differently() {
    let width = 4;
    let height = 8;
    let stride = width * 2;
    let mut a = vec![0x10u8; width * 2 * height];
    let b = vec![0x11u8; width * 2 * height];
    let hash_a = dupe_content_hash(&a, width, height, stride);
    let hash_b = dupe_content_hash(&b, width, height, stride);
    assert_ne!(
        hash_a, hash_b,
        "genuinely different content must not collide"
    );
    // Sanity: flipping ONE sampled byte also changes the hash.
    a[0] = 0xFF;
    let hash_a2 = dupe_content_hash(&a, width, height, stride);
    assert_ne!(
        hash_a, hash_a2,
        "a single changed sampled byte must change the hash"
    );
}

#[test]
fn honors_stride_padding_never_samples_garbage() {
    // width=2 (row_bytes=4), but the device pads each row to stride=16. The pad bytes must
    // never affect the hash -- only the real width*2 pixel bytes per row are sampled.
    let width = 2;
    let height = 4;
    let stride = 16;
    let mut frame_a = vec![0u8; stride * height];
    let mut frame_b = vec![0u8; stride * height];
    for y in 0..height {
        let row = y * stride;
        frame_a[row..row + 4].copy_from_slice(&[1, 2, 3, 4]);
        frame_b[row..row + 4].copy_from_slice(&[1, 2, 3, 4]);
        // Differing PADDING only (bytes beyond row_bytes=4, within stride=16).
        frame_a[row + 4] = 0xAA;
        frame_b[row + 4] = 0xBB;
    }
    let hash_a = dupe_content_hash(&frame_a, width, height, stride);
    let hash_b = dupe_content_hash(&frame_b, width, height, stride);
    assert_eq!(
        hash_a, hash_b,
        "padding bytes beyond the real row width must never affect the hash"
    );
}

#[test]
fn degenerate_frame_is_zero_and_never_panics() {
    assert_eq!(dupe_content_hash(&[], 0, 0, 0), 0);
    assert_eq!(dupe_content_hash(&[1, 2, 3], 4, 1, 0), 0);
}

// ── dupe_shed_action ───────────────────────────────────────────────────

#[test]
fn between_boundaries_blind_sheds_regardless_of_dupe() {
    // would_emit == false: lag/enough are irrelevant between boundaries -> BlindShed.
    assert_eq!(
        dupe_shed_action(false, false, false, 0, true, 0, false),
        ShedAction::BlindShed
    );
    assert_eq!(
        dupe_shed_action(false, true, false, 0, false, 0, false),
        ShedAction::BlindShed
    );
}

#[test]
fn fresh_on_time_dupe_at_boundary_is_deferred_not_emitted() {
    // An ON-TIME (lag == 0, surplus-regime) fresh dupe is the case #889 defers — a replacement
    // capture still lands inside the same interval, so the deferral is lag-neutral. Independent
    // of the unique-rate signal (deferral neither emits nor advances).
    assert_eq!(
        dupe_shed_action(true, true, false, 0, true, 0, false),
        ShedAction::Defer
    );
    assert_eq!(
        dupe_shed_action(true, true, false, 0, false, 0, false),
        ShedAction::Defer
    );
}

#[test]
fn already_deferred_on_time_dupe_falls_back_to_copy() {
    // A SECOND consecutive dupe for the SAME boundary (lag == 0, already deferred once) emits as
    // a copy — bounded to one deferral (validated dupes are isolated pairs, never triples).
    assert_eq!(
        dupe_shed_action(true, true, true, 0, true, 0, false),
        ShedAction::Emit { copy: true }
    );
}

#[test]
fn late_over_rate_dupe_is_retired_not_emitted_as_a_copy_1145() {
    // (#1145) A LATE fresh dupe (lag >= 1 — the crossed boundary is already stale) at a genuine
    // over-rate (`enough_unique_to_hold_target`) is RETIRED: shed the dupe AND advance the
    // stale boundary, emitting nothing. This is the fix — the pre-fix valve emitted a copy here
    // (the strih 15fps-judder), retirement drains the lag at no downstream cost.
    for lag in 1..=RETIRE_MAX_LAG_INTERVALS {
        assert_eq!(
            dupe_shed_action(true, true, false, lag, true, 0, false),
            ShedAction::Retire,
            "lag={lag}"
        );
    }
}

#[test]
fn late_dupe_without_enough_unique_is_emitted_as_a_copy_1145() {
    // (#1145) The late-dupe copy valve is now restricted to GENUINE STARVATION: when the source
    // does NOT carry enough distinct content to hold 60 (`!enough_unique` — a sub-60 source
    // padded by duplication, a 50->60 pulldown), a late dupe EMITS a copy exactly as before, so
    // the emit grid stays boundary-locked at 60 and the recording keeps the content-dupes the
    // duplication-masked pulldown detector reads.
    for lag in 1..=(RETIRE_MAX_LAG_INTERVALS + 3) {
        assert_eq!(
            dupe_shed_action(true, true, false, lag, false, 0, false),
            ShedAction::Emit { copy: true },
            "lag={lag}"
        );
    }
}

#[test]
fn retirement_stops_above_the_lag_ceiling_even_with_enough_unique_1145() {
    // Past RETIRE_MAX_LAG_INTERVALS WITHOUT a sustained over-rate (the 7th arg = false), a
    // genuine sustained deficit is building; the copy valve fires (the panic floor) rather than
    // the ordinary +1 retirement, so a non-over-rate lag can never creep toward the #707 resync
    // bound. (At a SUSTAINED over-rate the deep band instead FastDrains — see
    // `over_rate_deep_grid_backlog_converges_in_single_digit_seconds_1145`; that is the #1145
    // v2.1 accelerated convergence, gated on over-rate so this non-over-rate panic floor is
    // unchanged.)
    assert_eq!(
        dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS, true, 0, false),
        ShedAction::Retire
    );
    assert_eq!(
        dupe_shed_action(
            true,
            true,
            false,
            RETIRE_MAX_LAG_INTERVALS + 1,
            true,
            0,
            false
        ),
        ShedAction::Emit { copy: true }
    );
}

#[test]
fn fast_drain_band_decision_pins_1145() {
    // (#1145 v2.1) Positive + negative unit pins of the deep-backlog FastDrain band, so a
    // band-boundary regression fails with a one-line decision assertion instead of an opaque
    // convergence-time message (review 🔵). All at a SUSTAINED over-rate (7th arg = true).
    //
    // At/below the ceiling stays the ordinary +1 Retire (below 2x-target byte-identical):
    assert_eq!(
        dupe_shed_action(true, true, false, RETIRE_MAX_LAG_INTERVALS, true, 0, true),
        ShedAction::Retire,
        "lag == ceiling under over-rate is still the +1 Retire, not FastDrain"
    );
    // ABOVE the ceiling with enough distinct content -> the +2 FastDrain (the deep-backlog band):
    assert_eq!(
        dupe_shed_action(
            true,
            true,
            false,
            RETIRE_MAX_LAG_INTERVALS + 1,
            true,
            0,
            true
        ),
        ShedAction::FastDrain,
        "a deep over-rate backlog with enough unique fast-drains"
    );

    // (#1145 v2.1 review 🟡) The frozen/starved (#1052/#365) protection of the NEW band: a
    // SUSTAINED-OVER-RATE source that does NOT carry enough distinct content (a ShadowCast
    // capturing 61.x of a FROZEN picture — the realistic frozen case IS over-rate) must NOT
    // fast-drain; it stays on the #1111 copy valve so the emit grid holds a frozen PICTURE on a
    // live stream instead of blacking out. This is the exact combination the v1 review flagged
    // 🔴 and demanded be pinned; without the `enough_unique_to_hold_target` gate this assertion
    // fails.
    for lag in (RETIRE_MAX_LAG_INTERVALS + 1)..=(RETIRE_MAX_LAG_INTERVALS + 4) {
        assert_eq!(
            dupe_shed_action(true, true, false, lag, false, 0, true),
            ShedAction::Emit { copy: true },
            "frozen/starved over-rate at deep lag={lag} must emit a copy, never fast-drain"
        );
    }
}

#[test]
fn non_dupe_at_boundary_emits_unchanged() {
    // A genuine unique tick always emits (copy: false), regardless of lag / deferral / unique
    // flags — retirement and the copy valve only ever act on content-dupes.
    for (deferred, lag, enough) in [
        (false, 0u64, true),
        (true, 0, true),
        (false, 3, false),
        (false, 9, true),
    ] {
        assert_eq!(
            dupe_shed_action(true, false, deferred, lag, enough, 0, false),
            ShedAction::Emit { copy: false },
            "deferred={deferred} lag={lag} enough={enough}"
        );
    }
}

// ── DupeShedLog ────────────────────────────────────────────────────────

#[test]
fn shed_log_counts_and_resets_on_take() {
    let mut log = DupeShedLog::new();
    log.record_shed(true);
    log.record_shed(true);
    log.record_shed(false);
    log.record_dupe_emitted();
    log.record_retired();
    log.record_retired();
    log.record_retired();
    log.record_drained();
    log.record_drained();
    log.record_fast_drained();
    log.record_fast_drained();
    log.record_fast_drained();
    log.record_fast_drained();
    assert_eq!(log.take(), (2, 1, 1, 3, 2, 4));
    assert_eq!(log.take(), (0, 0, 0, 0, 0, 0), "take() must reset");
}

#[test]
fn summary_names_all_counts_and_the_ticket_tags() {
    // (#1145 review 🔵) Distinctive multi-digit counts that do NOT appear as substrings of the
    // ticket tags (889/1111/1145) or each other, so each assertion actually pins its own count
    // rather than being satisfied by a digit from a ticket number.
    let s = dupe_shed_summary(41, 23, 67, 94, 58, 72, 85, 36);
    assert!(s.contains("#889"));
    assert!(s.contains("#1111"), "names the late-dupe copy valve");
    assert!(
        s.contains("#1145"),
        "names the retirement + depth-drain mechanisms"
    );
    assert!(s.contains("#1167"), "names the v4 starvation slot-fill");
    assert!(s.contains("41"), "names the dupe-victim shed count");
    assert!(s.contains("23"), "names the blind-pacing shed count");
    assert!(s.contains("67"), "names the emitted-copy count");
    assert!(s.contains("94"), "names the retired-boundaries count");
    assert!(s.contains("58"), "names the depth-drained count (#1145 v2)");
    assert!(
        s.contains("72"),
        "names the fast-drained count (#1145 v2.1)"
    );
    assert!(
        s.contains("85"),
        "names the starvation last-frame-repeat count (#1167 v4)"
    );
    assert!(
        s.contains("depth-drained"),
        "names the v2 depth-drain mechanism"
    );
    assert!(
        s.contains("fast-drained"),
        "names the v2.1 fast-drain mechanism"
    );
    assert!(
        s.contains("starvation last-frame repeats"),
        "names the v4 empty-queue slot-fill mechanism"
    );
    assert!(s.contains("~36s"), "names the window seconds");
}

// ── the regression test: DecimationGate end-to-end on the validated pattern ─────

/// (#889) validated synthetic capture sequence: the rig's raw V4L2 grab on cam1 showed a
/// fast grabber (measured 64.14 fps against a 60 Hz source) repeating its OWN internal
/// buffer roughly once every 15 captures, always an ISOLATED pair (never a triple), every
/// other capture unique (camera sensor noise + painter motion). Builds `count` synthetic
/// captures at `capture_fps`; returns `(now_ns, content_id, is_dupe_ground_truth)` in
/// capture order. `content_id` is a strictly-increasing u64 for every REAL (non-dupe) tick;
/// a dupe capture repeats the immediately preceding capture's `content_id`.
fn synthetic_889_capture_sequence(
    capture_fps: f64,
    count: usize,
    dupe_period: usize,
) -> Vec<(u64, u64, bool)> {
    let interval_ns = (1_000_000_000.0 / capture_fps) as u64;
    let mut out = Vec::with_capacity(count);
    let mut next_id: u64 = 0;
    let mut prev_id: u64 = 0;
    for i in 0..count {
        let now_ns = i as u64 * interval_ns;
        let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
        let content_id = if is_dupe {
            prev_id
        } else {
            let id = next_id;
            next_id += 1;
            id
        };
        prev_id = content_id;
        out.push((now_ns, content_id, is_dupe));
    }
    out
}

#[test]
fn dupe_preferring_gate_never_emits_a_dupe_and_never_sheds_a_unique_tick_889() {
    // (#889) rig validation: cam1's ShadowCast measured 64.14 fps captured against a 60 Hz
    // source, 4.18 byte-identical dupes/s (an isolated pair roughly every 15 captures),
    // every non-dupe frame unique. Simulate ~3 seconds of that exact pattern against the
    // REAL 60fps genlock emit boundary math (`genlock_pacing::genlock_emit_gate`, unchanged by this
    // ticket) and assert the fix: zero dupes ever emitted, zero unique ticks shed in
    // steady state.
    let captures = synthetic_889_capture_sequence(64.14, 3 * 65, 15);
    let emit_interval_ns = 1_000_000_000u64 / 60;

    // The pacing gate's `next_boundary_ns` starts uninitialized (0) and latches its FIRST
    // boundary one interval after the very first capture's timestamp -- so the first
    // capture or two can land BEFORE that first boundary and get blind-decimated purely by
    // simulation-start phase, independent of dupe preference (this happens identically
    // whether or not dupe-awareness is on -- it is inherent to any genlock cold start, not
    // something this fix changes or is expected to fix). Excluding the first few captures'
    // ids from the "every unique tick must be emitted" requirement keeps the assertion about
    // the fix's actual invariant (steady-state behavior), not an unrelated simulation-start
    // artifact.
    const WARMUP_CAPTURES: usize = 4;

    let mut gate = DecimationGate::new();
    let mut emitted_ids: Vec<u64> = Vec::new();
    let mut emitted_a_dupe = false;
    for (now_ns, content_id, is_dupe) in &captures {
        if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
            emitted_ids.push(*content_id);
            if *is_dupe {
                emitted_a_dupe = true;
            }
        }
    }

    assert!(
        !emitted_a_dupe,
        "dupe-preferring decimation must never emit a grabber-dupe frame; emitted \
             {emitted_ids:?}"
    );

    // "no unique tick shed" (steady state): every distinct unique content id generated AFTER
    // the cold-start warm-up must appear in the emitted output -- the validated rig evidence
    // shows the dupe rate (~4.18/s) covers the over-rate shedding demand (~4.14/s) almost
    // exactly, so dupes alone should account for every required steady-state shed.
    let all_unique_ids: std::collections::BTreeSet<u64> = captures
        .iter()
        .skip(WARMUP_CAPTURES)
        .filter(|(_, _, is_dupe)| !is_dupe)
        .map(|(_, id, _)| *id)
        .collect();
    let emitted_set: std::collections::BTreeSet<u64> = emitted_ids.iter().copied().collect();
    let missing: Vec<u64> = all_unique_ids.difference(&emitted_set).copied().collect();
    assert!(
        missing.is_empty(),
        "dupe-preferring decimation must not drop a unique tick when dupes alone cover the \
             shedding demand (validated #889: dupe rate ~4.18/s ~= over-rate delta ~4.14/s); \
             missing unique ids: {missing:?}"
    );
}

// ── (#1111/#1145) over-60 excess-dupe grabber: no SKIPPED-boundary jumps, no unique dropped ─

/// (#1111 lineage, behavior updated by #1145) A GENKI ShadowCast 2 grabber delivering ~62 fps
/// with a byte-identical internal-buffer dupe ~every 15 captures — an EXCESS-dupe pattern whose
/// UNIQUE rate is genuinely sub-target (62 - 62/15 = ~57.9 unique fps, NOT the rig's true-60).
/// Before #1111 every #889 dupe DEFERRAL ratcheted the lag until it tripped the #707 resync
/// (~9-boundary leaps, `#707 SKIPPED boundaries` WARN, strih genlock-FIFO relock). #1111 stopped
/// the resync; it then EMITTED the late dupes as ~2 copies/s to hold a steady 60.
///
/// #1145 SUPERSEDES the "hold 60 via copies" behavior for THIS input: 57.9 unique fps is a
/// genuine sub-target deficit but sits ABOVE the #666 emit-deficit floor (57 fps), so v2 RETIRES
/// the surplus dupes and emits the HONEST ~57.9 fps (all unique, zero copies) rather than
/// fabricating copies — the strih FIFO absorbs the gentle, EVENLY-SPREAD 2.1 fps underrun exactly
/// as it would the 2.1 copies/s (same downstream visual), and there is no lag leap to relock it.
/// The LOAD-BEARING guarantees are unchanged and still asserted: ZERO #707 skips and NOT ONE
/// unique tick dropped. (A source BELOW 57 unique fps — a real 50->60 pulldown — still gets the
/// copy valve; see `starved_source_still_emits_copies_to_hold_60_not_retired_1145`.)
#[test]
fn over_rate_excess_dupe_input_stays_boundary_locked_without_skips_1145() {
    // ~8 s of the validated ShadowCast pattern: 62 fps captured, an isolated dupe every 15th.
    let seconds = 8usize;
    let captures = synthetic_889_capture_sequence(62.0, 62 * seconds, 15);
    let emit_interval_ns = 1_000_000_000u64 / 60;

    let mut gate = DecimationGate::new();
    let mut emitted: Vec<(u64, u64)> = Vec::new();
    let mut total_skips: u64 = 0;
    for (now_ns, content_id, _is_dupe) in &captures {
        // EXACT src/main.rs wiring: snapshot the boundary, poll, then measure the #707 skip.
        let prev_boundary_ns = gate.next_boundary_ns();
        let emit = gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0);
        let next_boundary_ns = gate.next_boundary_ns();
        total_skips += crate::genlock_pacing::boundary_skip_count(
            prev_boundary_ns,
            next_boundary_ns,
            emit_interval_ns,
        );
        if emit {
            emitted.push((*now_ns, *content_id));
        }
    }
    let (_dupe_shed, _blind_shed, dupe_emitted, retired, _drained, _fast_drained) =
        gate.take_shed_counts();
    let emitted_ids: Vec<u64> = emitted.iter().map(|(_, id)| *id).collect();

    // (1) LOAD-BEARING (#1111): a 62 fps over-rate + frequent dupes must NOT trip the #707 resync
    // — the boundary grid never leaps. Before #1111 this is ~18 (two ~9-interval leaps over 8 s).
    assert_eq!(
        total_skips, 0,
        "over-60 capture must stay boundary-locked (zero #707 SKIPPED boundaries); got \
             {total_skips} skipped interval(s) — the #889 dupe-deferral lag ratchet is back"
    );

    // (2) #1167 (SUPERSEDES the #1145 "retire, emit the honest 57.9" choice for STEADY over-rate):
    // the surplus shallow-lag dupes now FILL their slots with a copy (holding the emit at 60), so
    // `retired` -> 0 and the emitted stream carries content-copies (`dupe_emitted` -> nonzero). The
    // #1145 concern (the Δ0/Δ3 15fps-judder) was a copy PAIRED with a DROPPED UNIQUE; here NO unique
    // is dropped (verified in part (3) below), so a fill is an unpaired Δ0 — #1142-safe. The unique
    // rate is unmeasurably close between a true-60 jittery source and this ~57.9 one (the #1145
    // 2s-window limit), so the fill applies to the whole enough_unique band; the absolute copies/gaps
    // tolerance is the live-E2E re-check (`WINDOW_COPIES_GAPS_TOLERANCE`).
    assert_eq!(
        retired, 0,
        "a steady (non-converging) over-rate fills the shallow-lag slots, it does not retire them; \
             retired {retired}"
    );
    assert!(
        dupe_emitted > 0,
        "the shallow-lag slot-fills must show as #1111 copies; dupe_emitted {dupe_emitted}"
    );
    let emit_rate = emitted_ids.len() as f64 / seconds as f64;
    assert!(
        (59.0..=60.5).contains(&emit_rate),
        "the #1167 slot-fill must hold the emit at ~60 (not the #1145 honest ~57.9 under-run); got \
             {emit_rate:.2} fps ({} emitted over {seconds}s)",
        emitted_ids.len()
    );

    // (3) LOAD-BEARING: not one unique tick is dropped (a dropped unique = a genlock-FIFO gap).
    // Retirement sheds only the grabber's OWN dupes, never a genuine unique frame. Skip the
    // cold-start warm-up (the opening capture or two are blind-decimated by simulation-start
    // phase, unrelated to this fix — same WARMUP note as the #889 test above).
    const WARMUP_CAPTURES: usize = 4;
    let all_unique_ids: std::collections::BTreeSet<u64> = captures
        .iter()
        .skip(WARMUP_CAPTURES)
        .filter(|(_, _, is_dupe)| !is_dupe)
        .map(|(_, id, _)| *id)
        .collect();
    let emitted_set: std::collections::BTreeSet<u64> = emitted_ids.iter().copied().collect();
    let missing: Vec<u64> = all_unique_ids.difference(&emitted_set).copied().collect();
    assert!(
        missing.is_empty(),
        "no unique tick may be dropped by the over-60 gate; missing ids: {missing:?}"
    );
}

/// (#1111) Regression guard for the acceptance clause "behavior for an exact-60.0 input
/// (cam3/cam4 ezcap/NZXT) byte-identical". A perfectly-60.0, dupe-free capture stream never
/// engages the dupe-preference path at all, so the `on_time` gate added for #1111 is inert:
/// every capture but the cold-start one emits, zero boundaries are skipped, and zero dupe
/// victims are shed — identical with or without the fix.
#[test]
fn exact_60_input_is_boundary_locked_and_never_defers_1111() {
    // dupe_period 10_000 (> the 480 captures) => a dupe-free exact-60.0 stream.
    let captures = synthetic_889_capture_sequence(60.0, 480, 10_000);
    let emit_interval_ns = 1_000_000_000u64 / 60;

    let mut gate = DecimationGate::new();
    let mut emits = 0usize;
    let mut total_skips = 0u64;
    for (now_ns, content_id, is_dupe) in &captures {
        assert!(!is_dupe, "exact-60 fixture must be dupe-free");
        let prev = gate.next_boundary_ns();
        if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
            emits += 1;
        }
        total_skips += crate::genlock_pacing::boundary_skip_count(
            prev,
            gate.next_boundary_ns(),
            emit_interval_ns,
        );
    }
    let (dupe_shed, _blind_shed, _dupe_emitted, retired, _drained, _fast_drained) =
        gate.take_shed_counts();

    assert_eq!(total_skips, 0, "exact-60 input never skips a boundary");
    assert_eq!(
        dupe_shed, 0,
        "exact-60 dupe-free input never sheds a dupe victim"
    );
    assert_eq!(
        retired, 0,
        "exact-60 dupe-free input never retires a boundary (#1145 acts only on dupes)"
    );
    assert_eq!(
        emits, 479,
        "exact-60 emits every capture but the cold-start one (480 captures -> 479 emitted)"
    );
}

/// (#1131) The sick-grabber judder, end-to-end through the production `DecimationGate::poll`
/// wiring. The emit poll is blocked for ~10 emit-intervals (a send/processing hiccup on a sick
/// box); the V4L2 driver buffers the real captured frames meanwhile (0 capture-dropped — the
/// live symptom's signature), and on resume the loop drains them back-to-back at ~the same wall
/// clock, each flagged `queue_had_frame = true` (they returned from a non-empty queue). Every
/// drained frame must EMIT and `boundary_skip_count` must stay 0 — vs the queue-blind gate,
/// which emits 1 and skips ~9 (`#707 SKIPPED ... 9 boundary interval(s)`). RED before the
/// `!queue_had_frame` resync guard, GREEN after.
#[test]
fn buffered_drain_after_a_stall_emits_every_frame_zero_skip_1131() {
    let emit_interval_ns = 1_000_000_000u64 / 60;
    let mut gate = DecimationGate::new();

    // Warm the gate to a latched boundary with one on-time capture, then read where it sits.
    let start = 1_000_000_000u64;
    let _ = gate.poll(start, emit_interval_ns, 1, false, 0, 0);
    let boundary = gate.next_boundary_ns();
    assert!(boundary > 0, "gate latched a boundary");

    // Block ~10 intervals, then drain 6 buffered frames (unique content) at ~the same wall
    // clock, all from a NON-EMPTY queue (queue_had_frame = true).
    let block = 10u64;
    let buffered = 6u64;
    let resume = boundary + block * emit_interval_ns;
    let mut emitted = 0u64;
    let mut total_skips = 0u64;
    for k in 0..buffered {
        let now = resume + k;
        let content_id = 100 + k; // all unique — a real captured burst, not dupes
        let prev = gate.next_boundary_ns();
        if gate.poll(now, emit_interval_ns, content_id, true, 0, 0) {
            emitted += 1;
        }
        total_skips += crate::genlock_pacing::boundary_skip_count(
            prev,
            gate.next_boundary_ns(),
            emit_interval_ns,
        );
    }
    assert_eq!(
        emitted, buffered,
        "every buffered captured frame must emit through DecimationGate::poll, not be \
             leaped-past and discarded — the #1131 judder"
    );
    assert_eq!(
        total_skips, 0,
        "no boundary may be SKIPPED while buffered captured frames are available (#1131)"
    );
}

// ── (#1145) over-rate cadence: stale dupes retired, not emitted as content-copies ──────────

/// (#1145) A deterministic over-rate-with-jitter capture stream reproducing the live
/// ShadowCast cam1/cam2 pattern: a true-60 Hz source captured at `takt_fps` (isolated
/// content-dupes at the over-rate delta — the grabber repeats its internal buffer once per
/// surplus slot), with each capture's PROCESSING timestamp carrying `jitter_frac` of
/// pseudo-random scheduling jitter (a seeded LCG, so the whole sequence is reproducible off
/// rig). Returns `(now_ns, content_id)` in capture order; a dupe repeats the previous
/// `content_id` (content-dupeness is a hash property, INDEPENDENT of the jittered timestamp —
/// which is why the stream carries both a periodic dupe pattern AND independent timing jitter).
fn synthetic_over_rate_with_jitter(
    takt_fps: f64,
    jitter_frac: f64,
    seed: u64,
    seconds: usize,
) -> Vec<(u64, u64)> {
    let over_rate = takt_fps - 60.0;
    let dupe_period = if over_rate > 0.01 {
        (takt_fps / over_rate).round() as usize
    } else {
        10_000_000
    };
    let count = (takt_fps * seconds as f64) as usize;
    let nominal_interval_ns = 1_000_000_000.0 / takt_fps;
    let mut lcg: u64 = seed;
    let mut now: f64 = 0.0;
    let (mut next_id, mut prev_id): (u64, u64) = (0, 0);
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        lcg = lcg
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let unit = ((lcg >> 11) as f64) / ((1u64 << 53) as f64); // [0, 1)
        let jitter_ns = (unit * 2.0 - 1.0) * jitter_frac * nominal_interval_ns;
        if i > 0 {
            now += nominal_interval_ns + jitter_ns;
        }
        let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
        let content_id = if is_dupe {
            prev_id
        } else {
            let id = next_id;
            next_id += 1;
            id
        };
        prev_id = content_id;
        out.push((now.max(0.0) as u64, content_id));
    }
    out
}

#[test]
fn over_rate_stale_dupes_fill_the_slot_holding_60_not_retired_1167() {
    // (#1167 — SUPERSEDES the #1145 `over_rate_stale_dupes_retired_not_emitted_as_content_copies`
    // policy for STEADY over-rate.) cam1/cam2's ShadowCast drifts its capture takt to ~61.3 fps
    // against a true-60 source; jitter routinely pushes an isolated on-time deferral over the
    // boundary hair-trigger, so the next content-dupe crosses a SHALLOW-stale boundary. #1145 v1
    // RETIRED it (advance the stale boundary, emit nothing) to avoid a copy — but that SKIPS the
    // 60fps slot, and continuous jitter makes that a continuous strih-FIFO hold = the cam1
    // [4i/8align] sawtooth. #1167: in STEADY over-rate (never converging a deep backlog) `poll`
    // FILLS that slot with a copy of the nearest good frame instead, holding the emit at 60. The
    // copy is #1142-safe: it fills a slot a missed boundary left owed — no unique is displaced, so
    // it is NOT the paired Δ0/Δ3 lag-0 churn #1145's judder was (a copy PLUS a dropped unique). So
    // the emitted stream now carries content-copies (`retired` -> 0, `dupe_emitted` -> nonzero) AND
    // holds a steady ~60. The absolute per-window copies/gaps tolerance is re-verified on the live
    // E2E after deploy (`WINDOW_COPIES_GAPS_TOLERANCE`), per the standing data-first rule.
    let emit_interval_ns = 1_000_000_000u64 / 60;
    let seconds = 20u64;
    let mut total_copies = 0u64;
    let mut total_retired = 0u64;
    let mut total_fast = 0u64;
    let mut emit_rate_min = f64::INFINITY;
    for seed in [1u64, 7, 3, 42, 99] {
        let captures = synthetic_over_rate_with_jitter(61.3, 0.20, seed, seconds as usize);
        let mut gate = DecimationGate::new();
        let mut emitted = 0u64;
        for (now_ns, content_id) in &captures {
            if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
                emitted += 1;
            }
        }
        let (_dupe_shed, _blind_shed, dupe_emitted, retired, _drained, fast_drained) =
            gate.take_shed_counts();
        total_copies += dupe_emitted;
        total_retired += retired;
        total_fast += fast_drained;
        emit_rate_min = emit_rate_min.min(emitted as f64 / seconds as f64);
    }
    // The #1167 invariant: every slot filled -> a steady ~60, never the retire-driven under-run.
    assert!(
        emit_rate_min >= 59.5,
        "a steady over-rate box must FILL every slot and hold ~60; got a min emit rate of \
             {emit_rate_min:.2} fps across 5 seeds"
    );
    // Steady over-rate (no deep backlog) NEVER converges, so it NEVER retires a shallow-lag dupe —
    // it fills the slot with a copy instead (the mechanism actually engaged, not a no-op).
    assert_eq!(
        total_retired, 0,
        "a steady (non-converging) over-rate must fill, not retire, the shallow-lag dupes; retired \
             {total_retired} across 5 seeds"
    );
    assert_eq!(
        total_fast, 0,
        "steady over-rate with no injected backlog must never FastDrain; fast {total_fast}"
    );
    assert!(
        total_copies > 0,
        "the shallow-lag slot-fills must show as #1111 copies on the over-rate box; copies \
             {total_copies} across 5 seeds"
    );
}

#[test]
fn over_rate_retirement_holds_60_without_skips_1145() {
    // (#1145) At the rig over-rate (unique rate == 60), retiring every stale dupe keeps the
    // emitted rate at ~60 (all unique) with ZERO #707 boundary skips — no lag ratchet, no
    // resync leap, and no unique tick dropped in steady state.
    let emit_interval_ns = 1_000_000_000u64 / 60;
    let seconds = 20usize;
    let captures = synthetic_over_rate_with_jitter(61.3, 0.20, 1, seconds);
    let mut gate = DecimationGate::new();
    let mut emitted = 0usize;
    let mut total_skips = 0u64;
    for (now_ns, content_id) in &captures {
        let prev = gate.next_boundary_ns();
        if gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0) {
            emitted += 1;
        }
        total_skips += crate::genlock_pacing::boundary_skip_count(
            prev,
            gate.next_boundary_ns(),
            emit_interval_ns,
        );
    }
    assert_eq!(
        total_skips, 0,
        "retirement must never trip the #707 resync at a genuine over-rate; got {total_skips} \
             skipped boundary interval(s)"
    );
    let emit_rate = emitted as f64 / seconds as f64;
    assert!(
        (59.0..=60.5).contains(&emit_rate),
        "emitted rate must hold ~60 (all unique) at over-rate; got {emit_rate:.2} fps"
    );
}

#[test]
fn starved_source_still_emits_copies_to_hold_60_not_retired_1145() {
    // (#1145) A GENUINELY STARVED source — a 50 Hz source padded to a 60 fps capture by
    // DUPLICATION (a 5:6 pulldown: an exact content-dupe every 6th capture, unique rate ~50 <
    // the 59-fps retire floor) — must NOT be retired: retiring would silently drop the emit to
    // 50 fps (a strih-FIFO underrun) and STRIP the content-dupes the duplication-masked pulldown
    // detector reads. Retirement stays OFF; the late-dupe copy valve holds the emit grid at 60
    // and leaves the dupes in the stream, byte-identical to the pre-#1145 behavior.
    let emit_interval_ns = 1_000_000_000u64 / 60;
    let capture_interval_ns = 1_000_000_000u64 / 60; // padded 60 fps capture
    let seconds = 20u64;
    let count = 60 * seconds;
    let mut gate = DecimationGate::new();
    let (mut next_id, mut prev_id): (u64, u64) = (0, 0);
    let mut emitted = 0usize;
    for i in 0..count {
        let now_ns = i * capture_interval_ns;
        let is_dupe = i > 0 && i % 6 == 5; // 5:6 pulldown
        let content_id = if is_dupe {
            prev_id
        } else {
            let id = next_id;
            next_id += 1;
            id
        };
        prev_id = content_id;
        if gate.poll(now_ns, emit_interval_ns, content_id, false, 0, 0) {
            emitted += 1;
        }
    }
    let (_dupe_shed, _blind_shed, dupe_emitted, retired, _drained, _fast_drained) =
        gate.take_shed_counts();
    assert_eq!(
        retired, 0,
        "a starved (sub-60-unique) source must NEVER be retired — that would drop the emit rate \
             and blind the pulldown detector; retired {retired}"
    );
    assert!(
        dupe_emitted > 0,
        "the late-dupe copy valve must stay engaged for a starved source (it holds the emit grid \
             at 60 and keeps the content-dupes in the recording); dupe_emitted {dupe_emitted}"
    );
    let emit_rate = emitted as f64 / seconds as f64;
    assert!(
        (59.0..=60.5).contains(&emit_rate),
        "a starved source must still emit a steady ~60 (via copies), not silently drop; got \
             {emit_rate:.2} fps"
    );
}

#[test]
fn frozen_source_falls_back_to_copies_never_a_blackout_1145() {
    // (#1145 review 🔴) A genuinely FROZEN source (100% byte-identical captures — a dead painter
    // / wedged upstream feeding a still) must NOT collapse the emit: without the freshness gate,
    // the stale unique-rate window keeps `enough_unique` TRUE forever, so every frozen dupe
    // RETIRES (advancing the boundary without emitting) and the NDI emit falls to ~0 fps (a total
    // BLACKOUT — strictly worse than a frozen picture on a broadcast rig). The freshness gate
    // makes a freeze fall back to the late-dupe copy valve within a few intervals, holding a
    // steady ~60 fps of copies (a frozen PICTURE on a LIVE, FIFO-fed stream — the pre-#1145
    // behavior). RED before the freshness gate (emit ~0.2 fps), GREEN after.
    let emit_interval_ns = 1_000_000_000u64 / 60;
    // 5 s of a healthy over-rate stream (retirement engages), then 5 s frozen (all one hash).
    let captures = synthetic_over_rate_with_jitter(61.3, 0.20, 1, 5);
    let mut gate = DecimationGate::new();
    // drive the healthy warm-up so retirement is fully engaged before the freeze.
    let mut last_now = 0u64;
    for (now_ns, content_id) in &captures {
        let _ = gate.poll(*now_ns, emit_interval_ns, *content_id, false, 0, 0);
        last_now = *now_ns;
    }
    let _ = gate.take_shed_counts(); // reset counters; measure only the frozen span
                                     // now freeze for 5 s: same hash every capture at the ~61.3 fps takt.
    let cap_interval_ns = (1_000_000_000.0f64 / 61.3) as u64;
    let frozen_hash = 999_999_999u64;
    let frozen_captures = (61.3 * 5.0) as u64;
    let mut frozen_emitted = 0usize;
    for i in 1..=frozen_captures {
        let now_ns = last_now + i * cap_interval_ns;
        if gate.poll(now_ns, emit_interval_ns, frozen_hash, false, 0, 0) {
            frozen_emitted += 1;
        }
    }
    let (_dupe_shed, _blind_shed, dupe_emitted, _retired, _drained, _fast_drained) =
        gate.take_shed_counts();
    let frozen_emit_rate = frozen_emitted as f64 / 5.0;
    assert!(
            frozen_emit_rate >= 55.0,
            "a frozen source must keep emitting a steady ~60 fps of copies (a frozen picture on a \
             live stream), NEVER collapse to a blackout; got {frozen_emit_rate:.2} fps emitted over \
             the frozen span"
        );
    assert!(
        dupe_emitted > 0,
        "the frozen span must fall back to the late-dupe copy valve; dupe_emitted {dupe_emitted}"
    );
}

// ── (#1145 v2) queue-depth drain ───────────────────────────────────────

#[test]
fn queue_depth_intervals_guards_and_math_1145() {
    let i = 1_000_000_000u64 / 60; // ~16.667 ms
                                   // interval 0 (genlock off) -> 0
    assert_eq!(queue_depth_intervals(10 * i, 5 * i, 0), 0);
    // capture_mono 0 (no measurement sentinel) -> 0
    assert_eq!(queue_depth_intervals(10 * i, 0, i), 0);
    // now <= capture (non-advancing / bogus monotonic) -> 0
    assert_eq!(queue_depth_intervals(5 * i, 5 * i, i), 0);
    assert_eq!(queue_depth_intervals(4 * i, 5 * i, i), 0);
    // 2.5 intervals of residence -> 2 (whole intervals)
    assert_eq!(queue_depth_intervals(1000 + 5 * i / 2, 1000, i), 2);
    // a garbage-huge residence is clamped to the sane max (never a runaway shed)
    assert_eq!(
        queue_depth_intervals(1_000_000 * i, 1, i),
        QUEUE_DEPTH_SANE_MAX_INTERVALS
    );
}

#[test]
fn depth_drain_is_a_distinct_shed_action_1145() {
    assert_ne!(ShedAction::Drain, ShedAction::Retire);
    assert_ne!(ShedAction::Drain, ShedAction::BlindShed);
    assert_ne!(ShedAction::Drain, ShedAction::Emit { copy: false });
}

#[test]
fn depth_drain_only_fires_under_sustained_over_rate_1145() {
    // NOT over-rate: even a deep queue never drains — a healthy 60.00 card (and a #1131
    // buffered-drain stall-recovery on one) is byte-identical to v1. A unique at depth 3 emits.
    assert_eq!(
        dupe_shed_action(true, false, false, 0, true, 3, false),
        ShedAction::Emit { copy: false },
        "not over-rate -> no depth drain, even at depth 3"
    );
    // OVER-RATE + residence >= QUEUE_DEPTH_SHED_INTERVALS: shed the OLDEST (this) frame
    // regardless of dupeness — the sawtooth-bounding drain (a controlled single-frame drop).
    assert_eq!(
        dupe_shed_action(
            true,
            false,
            false,
            0,
            true,
            QUEUE_DEPTH_SHED_INTERVALS,
            true
        ),
        ShedAction::Drain,
        "over-rate + depth>=target -> drain the oldest (even a non-dupe)"
    );
    // OVER-RATE + a DETECTED dupe at the lower dupe-shed threshold: drain one interval earlier
    // (content-safe — the neighbour carries the same painted frame).
    assert_eq!(
        dupe_shed_action(
            true,
            true,
            false,
            0,
            true,
            QUEUE_DEPTH_DUPE_SHED_INTERVALS,
            true
        ),
        ShedAction::Drain,
        "over-rate + detected dupe at the dupe-shed depth -> drain"
    );
    // OVER-RATE but residence below BOTH thresholds -> falls through to the pre-v2 arms
    // (a unique emits; a fresh on-time dupe defers).
    assert_eq!(
        dupe_shed_action(true, false, false, 0, true, 0, true),
        ShedAction::Emit { copy: false }
    );
    assert_eq!(
        dupe_shed_action(true, true, false, 0, true, 0, true),
        ShedAction::Defer
    );
}

// ── (#1145 v2) end-to-end: the delivery-latency sawtooth REPRODUCER + the depth-drain fix ──

struct QueueSim {
    /// Post-warmup (>8 s) maximum queue RESIDENCE any processed frame reached, in whole emit
    /// intervals — the sawtooth's height. v1 lets this grow toward the V4L2 overflow; v2 bounds it.
    max_residence_post: u64,
    /// Post-warmup V4L2 overflow-drops (queue was full when a capture arrived) — the burst that
    /// shows as judder. v2 pre-empts these with a controlled continuous drain.
    overflow_steady: u64,
    emits: u64,
    drained: u64,
    repeats: u64,
}

/// Drive the REAL [`DecimationGate::poll`] with a capture->process queue whose consumer rate
/// depends on the shed decision (an EMITted frame costs ~one interval — the NDI send; a SHED
/// frame is cheap), so the loop's max emit rate sits BETWEEN 60.00 and the over-rate. A healthy
/// 60.00 card keeps up (and recovers from a transient stall); an over-rate card cannot recover,
/// so its queue residence grows into the sawtooth this ticket fixes. Dupes are NOT byte-detected
/// (every capture gets a distinct hash — the realistic ShadowCast-noise worst case where the
/// depth-drain, not the dupe-shed, must carry the absorption). wall == mono in the sim.
///
/// (#1145 v2 review 🔵) Why `send_cost` is just UNDER the interval (max emit ~60.5/s) + a
/// one-shot stall trigger, NOT `send_cost >= interval`: the field mechanism is that the loop's
/// max emit rate sits BETWEEN 60.00 and the over-rate — a 60.00 card RECOVERS from a transient
/// perturbation while an over-rate card CANNOT, so the over-rate residence only ever grows after
/// a perturbation and then never drains. A `send_cost >= interval` model would make even the
/// 60.00 card unable to keep up, destroying the constraint-c separation this harness must show.
/// The stall is the realistic trigger (a CPU/#752 hiccup); the over-rate is what sustains it.
fn run_queue_sim(capture_fps: f64, stall_at_frame: u64, secs: f64) -> QueueSim {
    let cap_int = (1e9 / capture_fps) as u64;
    let src_int = 1_000_000_000u64 / 60;
    let emit_int = 1_000_000_000u64 / 60;
    let send_cost = emit_int * 991 / 1000; // ~16.5 ms -> max emit ~60.5/s (between 60.0 and 61.x)
    let shed_cost = 1_000_000u64; // 1 ms (hash only)
    let stall_extra = emit_int * 6; // one deterministic CPU hiccup
    const MAXQ: usize = 4; // V4L2 buffers (capture.rs: Stream::with_buffers(.., 4))
    const WARMUP_NS: u64 = 8_000_000_000; // ignore the first 8 s (takt EMA warmup + stall settle)
    let n = (capture_fps * secs) as u64;

    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<(u64, u64)> = VecDeque::new();
    let mut next_cap = 0u64;
    let mut wall = 0u64;
    let (mut max_residence_post, mut overflow_steady, mut emits) = (0u64, 0u64, 0u64);
    let mut repeats = 0u64;

    loop {
        // admit all captures that have arrived by the loop's wall clock (drop if the queue is full)
        while next_cap < n {
            let cap_ns = next_cap * cap_int;
            if cap_ns > wall {
                break;
            }
            let src_id = cap_ns / src_int;
            if queue.len() >= MAXQ {
                if cap_ns > WARMUP_NS {
                    overflow_steady += 1;
                }
            } else {
                queue.push_back((cap_ns, src_id));
            }
            next_cap += 1;
        }
        if queue.is_empty() {
            if next_cap >= n {
                break;
            }
            wall = next_cap * cap_int; // the loop waits for the next capture
            continue;
        }
        let (cap_ns, src_id) = queue.pop_front().unwrap();
        let now = wall;
        let queue_had_frame = now.saturating_sub(cap_ns) < cap_int / 2;
        let residence = now.saturating_sub(cap_ns) / emit_int;
        if now > WARMUP_NS {
            max_residence_post = max_residence_post.max(residence);
        }
        // a distinct hash per capture -> is_dupe is always false (dupes NOT byte-detected)
        let content_hash = src_id.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(now);
        let emit = gate.poll(now, emit_int, content_hash, queue_had_frame, now, cap_ns);
        repeats += gate.last_poll_starvation_repeats();
        let mut cost = shed_cost;
        if emit {
            cost = send_cost;
            if next_cap == stall_at_frame {
                cost += stall_extra;
            }
            emits += 1;
        }
        wall += cost;
        if next_cap >= n && queue.is_empty() {
            break;
        }
    }
    let (_d, _b, _e, _r, drained, _fast_drained) = gate.take_shed_counts();
    QueueSim {
        max_residence_post,
        overflow_steady,
        emits,
        drained,
        repeats,
    }
}

#[test]
fn over_rate_queue_depth_drain_bounds_the_sawtooth_1145() {
    // The fix: at a sustained over-rate (cam1 ShadowCast ~61.5 fps vs a 60 fps source) the
    // queue-depth drain absorbs the surplus CONTINUOUSLY, so the delivery-latency sawtooth stays
    // bounded (residence <= QUEUE_DEPTH_SHED_INTERVALS) and the V4L2 buffer never overflow-drops
    // in a burst. Without the drain (the lag-based v1) the residence grows toward the 4-deep
    // overflow and bursts — this assertion is the RED that the drain turns GREEN.
    let s = run_queue_sim(61.5, 120, 30.0);
    assert!(
        s.drained > 0,
        "the depth drain must engage at a sustained over-rate; drained={}",
        s.drained
    );
    assert!(
        s.max_residence_post <= QUEUE_DEPTH_SHED_INTERVALS,
        "over-rate delivery latency must stay bounded at the depth target; \
             max post-warmup residence {} intervals (target {})",
        s.max_residence_post,
        QUEUE_DEPTH_SHED_INTERVALS
    );
    assert_eq!(
        s.overflow_steady, 0,
        "the continuous drain must pre-empt every V4L2 overflow-drop burst; \
             steady overflow-drops {}",
        s.overflow_steady
    );
}

#[test]
fn over_rate_depth_drain_holds_emit_rate_above_the_666_floor_1145() {
    // (#1145 v2 review 🟡) Zero-loss is the project's HARD acceptance bar, so the DRAIN path —
    // which at a genuine over-rate sheds the OLDEST frame regardless of dupeness (the noise-blind
    // oldest-drop) — must never collapse the emit rate below the #666 emit-deficit floor
    // (5% of 60 == 57 fps). This is the drain-path counterpart of the retirement path's own
    // `over_rate_retirement_holds_60_without_skips_1145` guard, closing the review-found asymmetry:
    // it pins the emit rate + bounded residence against a future constant retune.
    for &fps in &[61.5_f64, 62.0] {
        let s = run_queue_sim(fps, 120, 30.0);
        let emit_fps = s.emits as f64 / 30.0;
        assert!(
            emit_fps >= 57.0,
            "the depth drain must hold the emit rate above the #666 floor (57 fps); \
                 got {emit_fps:.2} fps at {fps} capture (drained={})",
            s.drained
        );
        assert!(
            s.max_residence_post <= QUEUE_DEPTH_SHED_INTERVALS,
            "residence must stay bounded at {fps} capture; max {} intervals",
            s.max_residence_post
        );
        assert_eq!(
            s.overflow_steady, 0,
            "no V4L2 overflow-drop burst at {fps} capture; steady overflow {}",
            s.overflow_steady
        );
    }
}

#[test]
fn healthy_60fps_never_depth_drains_even_through_a_stall_1145() {
    // Constraint (c) + #1131: a healthy 60.00 card is NOT over-rate, so the depth drain NEVER
    // fires — even when a transient stall pushes its queue residence past the depth target
    // (a #1131 buffered-drain, which must emit all buffered frames, not shed them). The takt
    // gate keeps v2 provably inert here, so behaviour is byte-identical to v1.
    let s = run_queue_sim(60.0, 120, 30.0);
    assert_eq!(
        s.drained, 0,
        "a 60.00 card must NEVER depth-drain (takt gate off); drained={}",
        s.drained
    );
    // and it still emits a full ~60 fps (no frames sacrificed by v2).
    assert!(
        s.emits >= (60.0 * 30.0 * 0.98) as u64,
        "a healthy card must keep emitting ~60 fps; emits={}",
        s.emits
    );
}

// ── (#1145 v2.1) fast-drain: accelerated grid-backlog convergence ─────────────────────────

/// (#1145 v2.1) Result of [`run_grid_backlog_sim`].
struct GridBacklogSim {
    /// Wall (monotonic) seconds from the injected backlog until the emit-grid lag returns to
    /// parity (<= 1 interval) — the "time to parity" the ticket's LIVE CONVERGENCE DATA names.
    time_to_parity_s: f64,
    emit_fps: f64,
    /// Fraction of emitted-frame boundary steps that advanced exactly ONE interval (the uniform
    /// 60 fps cadence) DURING and after the accelerated drain.
    uniformity: f64,
    /// (#1145 v2.1) How many times the FastDrain arm engaged over the run — 0 on a healthy 60.00
    /// card and in steady over-rate with no backlog (the byte-identical proof), > 0 when a deep
    /// grid backlog was accelerated.
    fast_drained: u64,
    /// (#1145 v2.1 review 🔵) The emit rate measured ONLY within the drain window
    /// (inject..converged), so a sub-#666-floor dip confined to the ~single-digit-second drain is
    /// not diluted by the steady-state remainder of the run (the full-run `emit_fps` was).
    drain_window_emit_fps: f64,
    /// (#1145 v2.1 review 🟡) Accumulated NET `#707` boundary skips after injection == the count
    /// `main.rs` would feed `emit_skip_log` == `boundary_skip_count` MINUS the intentional
    /// fast-drain extra advance, summed per poll. Must stay 0 (well under `leg-health-guard.sh`'s
    /// sick-leg threshold) so an intentional fast-drain never trips the #707 clock-step alarm.
    net_707_skips_after_inject: u64,
}

/// (#1145 v2.1) Drive the REAL [`DecimationGate::poll`] with a send-bound emit loop whose
/// MONOTONIC capture takt (residence + takt + capture instants — CONTINUOUS) is SEPARATE from
/// the REALTIME emit-grid clock (`now_ns`, which grids the boundary). A downstream reconnect /
/// burn-toggle adds a one-time REALTIME forward offset (`backlog` intervals) — the emit grid
/// falls behind == delivery lag — WITHOUT disrupting the cam-box's monotonic capture takt, so
/// `sustained_over_rate` stays TRUE and residence stays low (the faithful reconnect scenario the
/// two-clock split of #1145 v2 makes representable). Measures wall time until the grid lag
/// returns to parity, the emit rate, and the emitted-cadence uniformity. Dupes are modelled as
/// isolated content-PAIRS (a dupe repeats the previous content id — the same model as
/// [`synthetic_over_rate_with_jitter`]). wall == monotonic; realtime == monotonic + offset.
fn run_grid_backlog_sim(capture_fps: f64, backlog_intervals: u64, secs: f64) -> GridBacklogSim {
    let cap_int = (1e9 / capture_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let send_cost = emit_int * 995 / 1000; // ~0.5% slack -> unblocked max emit ~60.3/s
    let shed_cost = 1_000_000u64; // 1 ms (hash only)
    const MAXQ: usize = 4;
    const WARMUP_NS: u64 = 6_000_000_000; // establish the takt EMA before injecting the backlog
    let n = (capture_fps * secs) as u64;

    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<u64> = VecDeque::new(); // capture-monotonic instants
    let mut next_cap = 0u64;
    let mut mono = 0u64;
    let mut rt_off: i64 = 0;
    let (mut injected, mut inject_mono, mut converged_at): (bool, u64, Option<u64>) =
        (false, 0, None);
    let mut emits = 0u64;
    let mut emits_in_window = 0u64; // emits during inject..converged (review 🔵 #4)
    let mut net_707_skips = 0u64; // boundary_skip_count - fast-drain extra, after inject (🟡 #1)
    let mut last_emit_bidx: Option<u64> = None;
    let (mut uni_ok, mut uni_tot) = (0u64, 0u64);
    let (mut next_id, mut prev_id): (u64, u64) = (0, 0);

    loop {
        while next_cap < n {
            let cap_ns = next_cap * cap_int;
            if cap_ns > mono {
                break;
            }
            if queue.len() < MAXQ {
                queue.push_back(cap_ns);
            }
            next_cap += 1;
        }
        if queue.is_empty() {
            if next_cap >= n {
                break;
            }
            mono = next_cap * cap_int; // wait for the next capture
            continue;
        }
        if !injected && mono > WARMUP_NS {
            rt_off = (backlog_intervals * emit_int) as i64; // reconnect: grid falls behind
            inject_mono = mono;
            injected = true;
        }
        let cap_ns = queue.pop_front().unwrap();
        let now_mono = mono;
        let now_rt = (mono as i64 + rt_off) as u64;
        let over_rate = capture_fps - 60.0;
        let dupe_period = if over_rate > 0.01 {
            (capture_fps / over_rate).round() as u64
        } else {
            u64::MAX
        };
        let is_dupe = dupe_period != u64::MAX && next_cap % dupe_period == dupe_period - 1;
        let cid = if is_dupe {
            prev_id
        } else {
            let id = next_id;
            next_id += 1;
            id
        };
        prev_id = cid;
        let content_hash = cid.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        let prev_boundary = gate.next_boundary_ns();
        // poll: now_ns (boundary / lag) is REALTIME; residence + takt are MONOTONIC.
        let emit = gate.poll(now_rt, emit_int, content_hash, true, now_mono, cap_ns);
        // (#1145 v2.1 review 🟡) exactly what main.rs feeds emit_skip_log: the raw #707 skip
        // MINUS the fast-drain's intentional extra advance. Accumulate after injection.
        if injected && converged_at.is_none() {
            let raw_skip = crate::genlock_pacing::boundary_skip_count(
                prev_boundary,
                gate.next_boundary_ns(),
                emit_int,
            );
            net_707_skips += raw_skip.saturating_sub(gate.last_poll_intentional_extra_advance());
        }
        let bidx = gate.next_boundary_ns() / emit_int;
        let mut cost = shed_cost;
        if emit {
            cost = send_cost;
            emits += 1;
            if injected && converged_at.is_none() {
                emits_in_window += 1;
            }
            if let Some(prev) = last_emit_bidx {
                uni_tot += 1;
                if bidx.saturating_sub(prev) == 1 {
                    uni_ok += 1;
                }
            }
            last_emit_bidx = Some(bidx);
        }
        mono += cost;
        if injected && converged_at.is_none() {
            let rt = (mono as i64 + rt_off) as u64;
            let lag =
                crate::genlock_pacing::genlock_lag_intervals(rt, gate.next_boundary_ns(), emit_int);
            if lag <= 1 {
                converged_at = Some(mono);
            }
        }
        if next_cap >= n && queue.is_empty() {
            break;
        }
    }
    let (_ds, _bl, _cp, _ret, _drn, fast_drained) = gate.take_shed_counts();
    let time_to_parity_s = converged_at.map_or(f64::NAN, |w| (w - inject_mono) as f64 / 1e9);
    let uniformity = if uni_tot > 0 {
        uni_ok as f64 / uni_tot as f64
    } else {
        1.0
    };
    // (#1145 v2.1 review 🔵 #4) emit rate measured ONLY across the drain window (inject..converged),
    // undiluted by steady state. NaN if it never converged (nothing to bound).
    let drain_window_emit_fps = match converged_at {
        Some(w) if w > inject_mono => emits_in_window as f64 / ((w - inject_mono) as f64 / 1e9),
        _ => f64::NAN,
    };
    GridBacklogSim {
        time_to_parity_s,
        emit_fps: emits as f64 / secs,
        uniformity,
        fast_drained,
        drain_window_emit_fps,
        net_707_skips_after_inject: net_707_skips,
    }
}

#[test]
fn over_rate_deep_grid_backlog_converges_in_single_digit_seconds_1145() {
    // (#1145 v2.1) RED before the fix / GREEN after. The merged v2 retires over-rate dupes only
    // while lag <= RETIRE_MAX_LAG_INTERVALS (4); ABOVE that a late dupe EMITS a copy (no grid
    // advance), so a deep emit-grid backlog (the owner's painter-QR delivery lag, 12+ frames
    // after a reconnect / restart / burn toggle) catches up ONLY via the send-slack — the
    // owner's measured ~0.3 frame/s (~35 s live). The fast-drain RETIRES those deep late dupes
    // and advances TWO stale boundaries per retire, converging the backlog in single-digit
    // seconds.
    //
    // Measured against the REAL poll (send-bound loop, realtime/monotonic split): the current v2
    // takes ~15.3 s for a 24-frame backlog and ~7.3 s for a 12-frame one (RED — over the bounds
    // below); the fast-drain takes ~9.3 s and ~5.3 s (GREEN).
    let deep = run_grid_backlog_sim(61.5, 24, 120.0);
    assert!(
        deep.time_to_parity_s <= 12.0,
        "a 24-frame grid backlog must converge in single-digit-ish seconds at a sustained \
             over-rate; time_to_parity {:.2}s (v2 baseline ~15.3s)",
        deep.time_to_parity_s
    );
    let twelve = run_grid_backlog_sim(61.5, 12, 120.0);
    assert!(
        twelve.time_to_parity_s <= 6.5,
        "a 12-frame grid backlog must converge fast; time_to_parity {:.2}s (v2 baseline ~7.3s)",
        twelve.time_to_parity_s
    );
    // The HARD zero-loss bar holds through the accelerated drain: emit stays above the #666
    // floor (57 fps) and the emitted 60 fps cadence stays uniform (>= 0.95) — the ticket's
    // "without cadence damage".
    for s in [&deep, &twelve] {
        assert!(
            s.emit_fps >= 57.0,
            "emit rate must stay above the #666 floor during the accelerated drain; got {:.2}",
            s.emit_fps
        );
        assert!(
            s.uniformity >= 0.95,
            "emitted cadence uniformity must stay >= 0.95 during the accelerated drain; got {:.3}",
            s.uniformity
        );
        // (#1145 v2.1 review 🔵 #4) the #666 floor holds WITHIN the drain window itself, not just
        // averaged over the whole run (a sub-floor dip confined to the ~single-digit-second drain
        // would otherwise be diluted ~13:1 and pass trivially).
        assert!(
            s.drain_window_emit_fps >= 57.0,
            "emit rate within the drain window must stay above the #666 floor; got {:.2}",
            s.drain_window_emit_fps
        );
        // (#1145 v2.1 review 🟡 #1) an intentional fast-drain must NOT register as a #707
        // un-emitted-content boundary SKIP (the sick-leg / clock-step signal leg-health-guard.sh
        // hard-fails on) — main.rs deducts the fast-drain's extra advance, so the NET count stays 0.
        assert_eq!(
            s.net_707_skips_after_inject, 0,
            "fast-drain must not inflate the #707 boundary-skip diagnostic; net skips {}",
            s.net_707_skips_after_inject
        );
        // the mechanism actually engaged (not merely a no-op faster-by-luck).
        assert!(
            s.fast_drained > 0,
            "the v2.1 fast-drain must engage on a deep over-rate backlog; fast_drained={}",
            s.fast_drained
        );
    }
}

#[test]
fn fast_drain_never_engages_on_a_healthy_60fps_card_1145() {
    // Constraint: a healthy 60.00 card is NOT over-rate, so the fast-drain NEVER fires even when
    // a reconnect leaves it with the SAME deep grid backlog — the takt gate keeps v2.1 provably
    // inert, so behaviour is byte-identical to v2. (The 60.00 card's own slow slack-only
    // convergence is a separate, pre-existing issue, not this ticket's over-rate scope.)
    let s = run_grid_backlog_sim(60.0, 24, 120.0);
    assert_eq!(
        s.fast_drained, 0,
        "a 60.00 card must NEVER fast-drain (takt gate off); fast_drained={}",
        s.fast_drained
    );
}

#[test]
fn fast_drain_is_inert_in_steady_over_rate_without_a_backlog_1145() {
    // Constraint: below the 2x-target grid lag (steady over-rate, NO injected backlog -> lag
    // stays ~0), the fast-drain never fires -> byte-identical to v2. `backlog_intervals = 0`
    // exercises exactly that (an over-rate card with no reconnect event).
    let s = run_grid_backlog_sim(61.5, 0, 120.0);
    assert_eq!(
        s.fast_drained, 0,
        "steady over-rate with no deep backlog must NEVER fast-drain; fast_drained={}",
        s.fast_drained
    );
}

// ── (#1145 round 3) noise-tolerant content-dupe detection ──────────────────

/// Tiny deterministic LCG for repeatable per-capture "sensor noise" in the round-3 sims.
fn r3_lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state >> 33
}

/// Render a small YUYV422 frame for painted `frame_id` with per-capture noise (`seed`): a static
/// grey gradient background + a "QR/burn" region whose modules derive from `frame_id` (each id
/// increment flips ~half its modules — what makes two DIFFERENT painted frames diverge across the
/// sampled lattice while two SAME-id captures differ only by noise). Y (luma) at even byte
/// offsets; chroma neutral. `sigma` = ± per-byte luma noise amplitude.
fn r3_render(frame_id: u64, seed: u64, w: usize, h: usize, stride: usize, sigma: i32) -> Vec<u8> {
    let mut buf = vec![0u8; stride * h];
    let mut st = seed ^ 0x1234_5678;
    for y in 0..h {
        for x in 0..w {
            let mut yv: i32 = 90 + (y as i32 * 3 % 40);
            if (4..28).contains(&y) && (4..60).contains(&x) {
                // per-module bit from a splitmix avalanche of (id, y, x) so each module flips
                // ~independently (~half per id increment) — a popcount-parity model flips ALL
                // modules together or NONE (parity is position-independent), a degenerate "QR".
                let mut z = frame_id ^ ((y as u64) << 20) ^ ((x as u64) << 40);
                z = z.wrapping_mul(0x9E37_79B9_7F4A_7C15);
                z ^= z >> 29;
                z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
                let bit = (z >> 33) & 1;
                yv = if bit == 1 { 235 } else { 16 };
            }
            let noise = (r3_lcg(&mut st) % (2 * sigma as u64 + 1)) as i32 - sigma;
            yv = (yv + noise).clamp(0, 255);
            let px = y * stride + x * 2;
            buf[px] = yv as u8;
            buf[px + 1] = 128;
        }
    }
    buf
}

struct R3Sim {
    uniformity: f64,
    copies: u64,
    skips: u64,
    emit_fps: f64,
    emitted_ids: Vec<u64>,
}

/// Drive the REAL [`DecimationGate::poll`] (send-bound loop, monotonic residence, realtime==
/// monotonic — no reconnect) with a marginal over-rate card. `byte_identical` = a dupe re-renders
/// with the SAME noise seed (a CAM1 buffer-repeat → identical bytes → the exact hash catches it);
/// else a FRESH seed (a CAM2 noisy optical re-sample → distinct hash → only the luma comparator
/// can catch it). `with_luma` = call [`note_frame_luma`](DecimationGate::note_frame_luma) before
/// each poll (production wiring) vs not (the legacy path). Measures the emitted painted-id cadence
/// decimated 60→30 (the `presentation_cadence` uniformity metric) plus the pre-decimation Δ1
/// copies (a held id) and Δ3 skips (a skipped id).
fn run_r3_sim(
    capture_fps: f64,
    secs: f64,
    sigma: i32,
    byte_identical: bool,
    with_luma: bool,
) -> R3Sim {
    let (w, h, stride) = (64usize, 32usize, 160usize);
    let cap_int = (1e9 / capture_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let send_cost = emit_int * 995 / 1000; // ~0.5% slack -> unblocked max emit ~60.3/s
    let shed_cost = 1_000_000u64; // 1 ms (hash + compare only)
    const MAXQ: usize = 4;
    let n = (capture_fps * secs) as u64;
    let over_rate = capture_fps - 60.0;
    let dupe_period = if over_rate > 0.01 {
        (capture_fps / over_rate).round() as u64
    } else {
        u64::MAX
    };

    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<(u64, u64, u64, Vec<u8>)> = VecDeque::new(); // (cap_mono, id, hash, luma)
    let mut next_cap = 0u64;
    let mut mono = 0u64;
    let mut jitter = 0xABCDu64;
    let mut next_id = 0u64;
    let mut prev_id = 0u64;
    let mut prev_seed = 0u64;
    let mut emitted_ids: Vec<u64> = Vec::new();
    let mut emits = 0u64;

    loop {
        while next_cap < n {
            let base = next_cap * cap_int;
            let span = (cap_int / 3).max(1);
            let jit = (r3_lcg(&mut jitter) % span) as i64 - (span / 2) as i64;
            let cap_ns = (base as i64 + jit).max(0) as u64;
            if cap_ns > mono {
                break;
            }
            let is_dupe = dupe_period != u64::MAX && next_cap % dupe_period == dupe_period - 1;
            let (pid, seed) = if is_dupe {
                let s = if byte_identical {
                    prev_seed
                } else {
                    next_cap.wrapping_mul(0x9E37).wrapping_add(0x5151)
                };
                (prev_id, s)
            } else {
                let id = next_id;
                next_id += 1;
                (id, next_cap.wrapping_mul(0x9E37))
            };
            prev_id = pid;
            prev_seed = seed;
            let frame = r3_render(pid, seed, w, h, stride, sigma);
            let (hash, luma) = dupe_content_sig(&frame, w, h, stride);
            if queue.len() < MAXQ {
                queue.push_back((cap_ns, pid, hash, luma));
            }
            next_cap += 1;
        }
        if queue.is_empty() {
            if next_cap >= n {
                break;
            }
            mono = next_cap * cap_int; // wait for the next capture
            continue;
        }
        let (cap_mono, pid, hash, luma) = queue.pop_front().unwrap();
        let now_mono = mono;
        let now_rt = mono; // rig realtime == monotonic (no reconnect offset)
        if with_luma {
            gate.note_frame_luma(luma);
        }
        let emit = gate.poll(now_rt, emit_int, hash, true, now_mono, cap_mono);
        let mut cost = shed_cost;
        if emit {
            cost = send_cost;
            emits += 1;
            emitted_ids.push(pid);
        }
        mono += cost;
        if next_cap >= n && queue.is_empty() {
            break;
        }
    }

    // pre-decimation Δ1 copies (step 0 = held id) / Δ3 skips (step 2 = skipped id).
    let mut copies = 0u64;
    let mut skips = 0u64;
    for pair in emitted_ids.windows(2) {
        match pair[1] as i64 - pair[0] as i64 {
            0 => copies += 1,
            2 => skips += 1,
            _ => {}
        }
    }
    // decimate 60->30 (the downstream recording cadence) — uniformity = frac(step == 2).
    let kept: Vec<u64> = emitted_ids.iter().step_by(2).copied().collect();
    let (mut uni, mut tot) = (0u64, 0u64);
    for pair in kept.windows(2) {
        tot += 1;
        if pair[1] as i64 - pair[0] as i64 == 2 {
            uni += 1;
        }
    }
    R3Sim {
        uniformity: if tot > 0 {
            uni as f64 / tot as f64
        } else {
            1.0
        },
        copies,
        skips,
        emit_fps: emits as f64 / secs,
        emitted_ids,
    }
}

#[test]
fn frames_are_content_dupes_catches_noise_rejects_flip_1145() {
    let (w, h, stride) = (64usize, 32usize, 160usize);
    // two noisy captures of the SAME painted frame -> a content-dupe (noise below theta).
    let (_, la) = dupe_content_sig(&r3_render(100, 1, w, h, stride, 4), w, h, stride);
    let (_, lb) = dupe_content_sig(&r3_render(100, 2, w, h, stride, 4), w, h, stride);
    assert!(
        frames_are_content_dupes(&la, &lb),
        "two noisy captures of the SAME painted frame must be a content-dupe"
    );
    // a DIFFERENT painted frame (QR flip) -> NOT a dupe, even with noise.
    let (_, lc) = dupe_content_sig(&r3_render(101, 3, w, h, stride, 4), w, h, stride);
    assert!(
        !frames_are_content_dupes(&la, &lc),
        "two DIFFERENT painted frames (a QR/burn flip) must NOT be a content-dupe"
    );
    // a global exposure offset on the SAME frame -> still a dupe (the median compensates).
    let bright: Vec<u8> = lb
        .iter()
        .map(|&v| (v as i32 + 20).clamp(0, 255) as u8)
        .collect();
    assert!(
        frames_are_content_dupes(&la, &bright),
        "a uniform exposure offset on the same painted frame must still read as a content-dupe"
    );
    // fail-safe: mismatched / empty lattices are NOT dupes.
    assert!(!frames_are_content_dupes(&[], &[]));
    assert!(!frames_are_content_dupes(&la, &lb[..lb.len() - 1]));
}

#[test]
fn dupe_content_sig_hash_matches_legacy_and_lattice_nonempty_1145() {
    let (w, h, stride) = (64usize, 32usize, 160usize);
    let f = r3_render(7, 9, w, h, stride, 3);
    let (sig_hash, lattice) = dupe_content_sig(&f, w, h, stride);
    assert_eq!(
        sig_hash,
        dupe_content_hash(&f, w, h, stride),
        "dupe_content_sig's hash must be byte-identical to the legacy dupe_content_hash"
    );
    assert!(!lattice.is_empty(), "the luma lattice must be populated");
    assert_eq!(
        dupe_content_sig(&[], 0, 0, 0),
        (0u64, Vec::new()),
        "a degenerate frame must sign to (0, empty)"
    );
}

#[test]
fn marginal_over_rate_noisy_dupes_content_detection_holds_uniformity_1145() {
    // (#1145 round 3) RED before the [green] `is_dupe` wiring / GREEN after. A marginal jittery
    // over-rate card (CAM2, the painter box) whose surplus dupes are NOISY optical re-samples:
    // the exact content_hash misses them, so each emits as a "unique" (a held painted-id = Δ1)
    // and forces a compensating shed (a skipped painted-id = Δ3) — the balanced-pair churn the
    // #1142 uniformity gate REDs (live CAM2 0.93-0.95; the off-rig sim lands ~0.94 at 61.3).
    //
    // BASELINE — the legacy exact-hash-only path (no note_frame_luma) CHURNS. Pins the mechanism
    // and holds in BOTH [red] and [green] (the legacy path never changes).
    let base = run_r3_sim(61.3, 20.0, 4, false, false);
    assert!(
        base.uniformity < 0.95,
        "legacy exact-hash-only path MUST churn on noisy re-samples (mechanism pin); \
             uniformity {:.4}",
        base.uniformity
    );
    assert!(
        base.copies > 0 && base.skips > 0,
        "legacy path's churn MUST carry the balanced Δ1 copies / Δ3 skips pairs; \
             copies={} skips={}",
        base.copies,
        base.skips
    );
    // FIXED — content-compare detection armed via note_frame_luma holds the cadence. FAILS on
    // [red] (poll ignores the lattice), PASSES on [green].
    let fixed = run_r3_sim(61.3, 20.0, 4, false, true);
    assert!(
        fixed.uniformity >= 0.95,
        "content-compare detection MUST hold uniformity >= 0.95 on the marginal noisy card; \
             got {:.4} (baseline {:.4})",
        fixed.uniformity,
        base.uniformity
    );
    assert!(
        fixed.skips * 4 < base.skips.max(1),
        "content-compare detection MUST collapse the compensating unique-skips (Δ3); \
             fixed skips={} vs baseline {}",
        fixed.skips,
        base.skips
    );
    // The over-rate absorption must NOT collapse the emit rate — shedding PROVEN dupes keeps it
    // above the #666 emit-deficit floor (57 fps); the uniformity gate alone catches a rate drop
    // only indirectly, so pin it directly.
    assert!(
        fixed.emit_fps >= 57.0,
        "noise-tolerant detection must hold the emit rate above the #666 floor (57 fps); \
             got {:.2}",
        fixed.emit_fps
    );
}

#[test]
fn cam1_byte_identical_and_healthy_60_unchanged_by_note_frame_luma_1145() {
    // (#1145 round 3) note_frame_luma must NOT change a card the exact hash already handles: a
    // CAM1 byte-identical buffer-repeat (the exact short-circuit fires FIRST) and a healthy 60.00
    // card (never `sustained_over_rate` → the comparator is gated OFF). Drive each WITH and
    // WITHOUT note_frame_luma; the emitted painted-id sequences must be IDENTICAL — the
    // byte-untouched proof, differentially in one binary (the legacy WITHOUT variant IS the
    // pre-round-3 behavior by construction).
    let cam1_with = run_r3_sim(64.0, 20.0, 4, true, true);
    let cam1_without = run_r3_sim(64.0, 20.0, 4, true, false);
    assert_eq!(
        cam1_with.emitted_ids, cam1_without.emitted_ids,
        "CAM1 byte-identical dupes: note_frame_luma must not change the emitted cadence (the \
             exact hash short-circuits first)"
    );
    assert!(
        cam1_with.uniformity >= 0.95,
        "CAM1 byte-identical must decimate cleanly; uniformity {:.4}",
        cam1_with.uniformity
    );
    let h_with = run_r3_sim(60.0, 20.0, 4, false, true);
    let h_without = run_r3_sim(60.0, 20.0, 4, false, false);
    assert_eq!(
        h_with.emitted_ids, h_without.emitted_ids,
        "healthy 60.00: note_frame_luma must be inert (never sustained_over_rate)"
    );
}

// ── (#1145 v3) arming-signal robustness through a capture hiccup ──────────

#[test]
fn takt_ema_survives_a_capture_gap_1145() {
    // (#1145 v3) RED before the gap-excluded takt fold / GREEN after. The 61.5-fps capture EMA
    // sits at ~16.26ms, the sustained_over_rate threshold (RETIRE_MIN_TAKT_INTERVAL_NS) at
    // ~16.584ms — a 0.32ms margin. A SINGLE dequeue hiccup (a blocked V4L2 dequeue, NOT a takt
    // change) folds one huge sample into the ~256-frame EMA and disarms sustained_over_rate for
    // ~7s (a 500ms gap), during which depth-Drain, FastDrain AND the round-3 noisy-dupe compare
    // are ALL dead → the over-rate surplus leaks into the strih FIFO (the #1145 v3 residual).
    // A genuine takt change shows in EVERY sample; a delivery gap in ONE — so the fold must skip
    // the outlier. RED: current folds it and stays disarmed for hundreds of post-gap frames.
    let cap_int = (1_000_000_000.0f64 / 61.5) as u64; // ~16.26 ms over-rate takt
    let mut gate = DecimationGate::new();
    let mut t = 0u64;
    for _ in 0..800 {
        t += cap_int;
        gate.note_capture_takt(t);
    }
    assert!(
        gate.sustained_over_rate(),
        "a warm 61.5 fps capture EMA must arm sustained_over_rate"
    );
    // ONE 500 ms dequeue hiccup — a blocked dequeue, NOT a rate change.
    t += 500_000_000;
    gate.note_capture_takt(t);
    // then steady over-rate again; sustained_over_rate must SURVIVE (re-arm within a few frames).
    let mut rearmed_within = None;
    for k in 1..=8u64 {
        t += cap_int;
        gate.note_capture_takt(t);
        if gate.sustained_over_rate() {
            rearmed_within = Some(k);
            break;
        }
    }
    assert!(
        rearmed_within.is_some(),
        "sustained_over_rate must survive a single dequeue hiccup (gap-excluded takt fold); it \
             stayed disarmed for >8 post-gap frames — the arming-poisoning residual disarms every \
             over-rate drain for seconds"
    );
}

#[test]
fn takt_ema_disarms_on_a_sustained_collapse_1145() {
    // (#1145 v3 review 🟡 F1) the counterpart to the hiccup test: a SUSTAINED rate COLLAPSE (a
    // card dropping to ~15 fps — EVERY interval over TAKT_GAP_EXCLUDE_NS) must DISARM
    // `sustained_over_rate`, never latch it on forever. B.1's one-off gap-exclude alone would keep
    // skipping every sample and never re-learn (the review-found latch); the consecutive-gap
    // counter RESETS the EMA after TAKT_GAP_SUSTAINED_COUNT so a collapsed (non-over-rate) card
    // stops arming the over-rate drains. RED on the pre-F1 one-sided exclude, GREEN with the counter.
    let cap_int = (1_000_000_000.0f64 / 61.5) as u64;
    let mut gate = DecimationGate::new();
    let mut t = 0u64;
    for _ in 0..800 {
        t += cap_int;
        gate.note_capture_takt(t);
    }
    assert!(
        gate.sustained_over_rate(),
        "a warm 61.5 fps EMA must arm sustained_over_rate"
    );
    // sustained ~15 fps: every interval ~66 ms, all above the 50 ms exclude bound.
    let slow_int = 1_000_000_000u64 / 15;
    let mut disarmed_within = None;
    for k in 1..=8u64 {
        t += slow_int;
        gate.note_capture_takt(t);
        if !gate.sustained_over_rate() {
            disarmed_within = Some(k);
            break;
        }
    }
    assert!(
        disarmed_within.is_some(),
        "a sustained sub-20fps collapse must disarm sustained_over_rate (F1 consecutive-gap \
             reset); it stayed armed for >8 collapsed frames — the one-sided gap-exclude latch"
    );
}

/// (#1145 v3) Drive the REAL [`DecimationGate::poll`] through a send-bound over-rate loop with
/// CAM1-style byte-identical dupes at the over-rate cadence (a true-60 source captured faster),
/// real monotonic clocks (wall == mono, no reconnect offset), and ONE injected dequeue GAP after
/// a 10 s warm-up. Returns the copy-valve emissions (a DUPE that EMITTED) in the 8 s window AFTER
/// the gap — the surplus that, once a hiccup disarms the cam-side drains, leaks downstream into
/// the strih FIFO. Send-bound: an EMITted frame costs ~one interval (the NDI send), a SHED frame
/// is cheap, so the loop cannot keep up with the over-rate and the queue rides full.
fn run_hiccup_copy_export(capture_fps: f64, seed: u64, gap_ns: u64) -> (u64, u64) {
    let ei = 1_000_000_000u64 / 60;
    let ci = (1e9 / capture_fps) as u64;
    let send_cost = ei * 995 / 1000; // ~16.58 ms -> send-bound max emit ~60.3/s
    let shed_cost = 1_000_000u64; // 1 ms (hash only)
    const MAXQ: usize = 4; // V4L2 buffers (capture.rs)
    let warm_ns = 10_000_000_000u64;
    let post_ns = 8_000_000_000u64;
    let over = capture_fps - 60.0;
    let dupe_period = if over > 0.01 {
        (capture_fps / over).round() as u64
    } else {
        u64::MAX
    };
    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<(u64, u64, bool)> = VecDeque::new(); // (cap_mono, hash, is_dupe)
    let (mut next_cap, mut mono, mut jit, mut nid, mut prev_hash) = (0u64, 0u64, seed, 0u64, 0u64);
    let mut nc = 0u64;
    let (mut gap_done, mut post_start) = (false, 0u64);
    let end = warm_ns + post_ns + 4_000_000_000;
    let mut post_copies = 0u64;
    let mut post_emits = 0u64;
    loop {
        while next_cap <= mono {
            // inject ONE gap the first time a capture would land at/after the warm mark.
            if !gap_done && next_cap >= warm_ns {
                next_cap += gap_ns;
                gap_done = true;
                post_start = next_cap;
            }
            let cap = next_cap;
            jit = jit
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let span = (ci / 3).max(1);
            let j = ((jit >> 33) % span) as i64 - (span / 2) as i64;
            let cap_j = (cap as i64 + j).max(0) as u64;
            if cap_j > mono {
                break;
            }
            let is_dupe = dupe_period != u64::MAX && nc % dupe_period == dupe_period - 1;
            let h = if is_dupe {
                prev_hash
            } else {
                let x = nid.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
                nid += 1;
                x
            };
            prev_hash = h;
            if queue.len() < MAXQ {
                queue.push_back((cap_j, h, is_dupe));
            }
            nc += 1;
            next_cap += ci;
        }
        if queue.is_empty() {
            if next_cap > end {
                break;
            }
            mono = next_cap; // the loop waits for the next capture
            continue;
        }
        let (cap, h, is_dupe) = queue.pop_front().unwrap();
        let now = mono;
        if post_start > 0 && now >= post_start + post_ns {
            break; // past the measurement window
        }
        // (#1145 v3 review 🔵 F5) `queue_had_frame=true` on every poll is a harness
        // simplification: the REAL loop would pass `false` for the FIRST post-gap frame (its
        // dequeue genuinely blocked for the gap), letting the #131/#1131 resync clear most of the
        // deep lag. Passing `true` keeps the gate in the deep-lag catch-up regime (the more
        // demanding case for this test); the pinned copy-export outcome holds either way (verified
        // via the scratch route with both variants).
        let emit = gate.poll(now, ei, h, true, now, cap);
        if post_start > 0 && now >= post_start {
            // a DUPE that EMITTED is a copy-valve emission (Emit{copy:true}) — the surplus that
            // leaks downstream when the cam-side drains are disarmed.
            if is_dupe && emit {
                post_copies += 1;
            }
            if emit {
                post_emits += 1;
            }
        }
        mono += if emit { send_cost } else { shed_cost };
        if next_cap > end && queue.is_empty() {
            break;
        }
    }
    (post_copies, post_emits)
}

#[test]
fn over_rate_copy_export_survives_a_capture_hiccup_1145() {
    // (#1145 v3) RED before B.1 (gap-excluded takt fold) + B.2 (occupancy-relative unique floor)
    // / GREEN after. A single dequeue hiccup poisons BOTH arming signals (the takt EMA disarms
    // sustained_over_rate; the absolute unique-count floor drops below `retire_min_uniques` for
    // ~the gap duration), so every over-rate dupe hits the late-dupe COPY valve instead of being
    // retired — those copies ride at wire rate into the strih FIFO (the ±5-frame cam1 wobble the
    // qr-align gate REDs). With the fix the drains stay armed through the hiccup and the surplus
    // is retired at SOURCE, so ~ZERO copies are exported. Summed across seeds past the gap.
    let gap_ns = 500_000_000u64; // a 500 ms hiccup
    let mut total_post_copies = 0u64;
    for seed in [1u64, 7, 3, 42, 99] {
        total_post_copies += run_hiccup_copy_export(61.5, seed, gap_ns).0;
    }
    assert!(
        total_post_copies <= 5,
        "a single capture hiccup must NOT disarm the cam-side over-rate drains (B.1+B.2); the \
             surplus must be retired at source, not exported as copy-valve dupes into the strih \
             FIFO. Got {total_post_copies} post-gap copy-valve emissions across 5 seeds (RED: the \
             arming-poisoning residual exports ~10/seed)"
    );
}

#[test]
fn steady_over_rate_no_hiccup_never_over_sheds_1145() {
    // (#1145 v3) Anti-over-shed pin: with NO hiccup, the arming retunes (B.1 gap-excluded fold,
    // B.2 occupancy floor) must be provably INERT — a steady over-rate card is byte-identical to
    // the pre-v3 behaviour (the drains never disarm anyway, so nothing new fires). Two directions
    // (review 🔵 F2): ZERO copy-valve export AND a held emit rate — so a future regression that
    // either starts spuriously shedding OR over-sheds (e.g. a mistaken open-loop credit shedder,
    // or Drain firing every frame) is caught. `gap_ns == 0` = the same harness, no dead time.
    let (mut total_copies, mut min_emit_fps) = (0u64, f64::MAX);
    let post_secs = 8.0; // the harness's post-window length
    for seed in [1u64, 7, 3, 42, 99] {
        let (copies, emits) = run_hiccup_copy_export(61.5, seed, 0);
        total_copies += copies;
        min_emit_fps = min_emit_fps.min(emits as f64 / post_secs);
    }
    assert!(
        total_copies <= 5,
        "a steady over-rate card with NO hiccup must export ~0 copy-valve dupes (the retirement \
             path absorbs the surplus at source); got {total_copies} across 5 seeds"
    );
    assert!(
            min_emit_fps >= 57.0,
            "the arming retunes must NOT over-shed in steady state — the emit rate must stay >= the \
             #666 floor (57 fps); got {min_emit_fps:.2} fps (a catastrophic over-shed reads far below)"
        );
}

/// (#1167) Drive the REAL [`DecimationGate::poll`] with a TRUE-60 source over-captured at 62 fps
/// (2 dupes/s so the unique rate is exactly 60) and inject a corrupted-buffer drop at the live
/// ~0.8/s rate via [`DecimationGate::note_corrupted_frame`] (a corrupted buffer never reaches
/// `poll` — `src/capture.rs::process_frame` drops it before the callback). Residence is 0
/// (`now_mono == capture_mono`) to isolate the over-rate RETIRE path; the takt EMA still arms
/// `sustained_over_rate`. Returns (emits, corrupted, dupe_emitted).
///
/// The invariant (#1167): while ANY captured frame is buffered, every 60 fps emit slot must be
/// filled with the nearest good frame — a single-slot dupe is acceptable, a skipped slot never
/// is. Without the make-up, each corrupted drop removes a would-be-emitted good frame and the
/// over-rate absorption skips its slot → emit under-runs by exactly the corrupted rate
/// (measured 59.13 fps == the live "~59.1"). With the make-up, the deficit is reclaimed 1:1 so
/// emit holds the same ~60 as the no-corruption control.
fn run_corrupted_over_rate_1167(
    dupe_period: usize,
    corrupt_period: usize,
    secs: f64,
) -> (u64, u64, u64) {
    let cap_fps = 62.0f64;
    let cap_int = (1e9 / cap_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let n = (cap_fps * secs) as usize;
    let mut gate = DecimationGate::new();
    let (mut emits, mut corrupted) = (0u64, 0u64);
    let (mut prev_id, mut next_id) = (0u64, 0u64);
    for i in 0..n {
        let now = i as u64 * cap_int;
        let is_corrupt = i > 0 && corrupt_period > 0 && i % corrupt_period == corrupt_period - 1;
        if is_corrupt {
            // Corrupted buffer: dropped before the gate (never polled), exactly as
            // `src/capture.rs::process_frame` does — main.rs then calls note_corrupted_frame.
            gate.note_corrupted_frame();
            corrupted += 1;
            continue;
        }
        let is_dupe = i > 0 && dupe_period > 0 && i % dupe_period == dupe_period - 1;
        let content_id = if is_dupe {
            prev_id
        } else {
            let v = next_id;
            next_id += 1;
            v
        };
        prev_id = content_id;
        let content_hash = content_id.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // now_mono == capture_mono == now -> residence 0 (isolate the retire path); the takt
        // EMA still reads the 62 fps over-rate and arms `sustained_over_rate`.
        if gate.poll(now, emit_int, content_hash, true, now, now) {
            emits += 1;
        }
    }
    let (_ds, _bl, dupe_emitted, _r, _d, _fd) = gate.take_shed_counts();
    (emits, corrupted, dupe_emitted)
}

#[test]
fn over_rate_plus_corrupted_holds_target_emit_1167() {
    let secs = 30.0;
    // TRUE-60 source over-captured at 62 fps: dupe every 31st -> 2 dupes/s -> unique rate 60.
    let (control_emits, control_corrupt, _c_copies) = run_corrupted_over_rate_1167(31, 0, secs);
    // Same source + a corrupted-buffer drop every 77th capture (~0.8/s, the live cam1 rate).
    let (corrupt_emits, corrupted, makeup_copies) = run_corrupted_over_rate_1167(31, 77, secs);

    assert_eq!(control_corrupt, 0, "control run must inject no corruption");
    let control_fps = control_emits as f64 / secs;
    let corrupt_fps = corrupt_emits as f64 / secs;

    // (1) BASELINE: the over-rate itself holds ~60 with no corruption (the honest 60-unique
    // rate). Passes with or without the fix — it establishes the target the corrupted run
    // must also reach.
    assert!(
        (59.8..=60.05).contains(&control_fps),
        "over-rate control must hold ~60 fps (60-unique source); got {control_fps:.3} fps"
    );

    // (2) THE FIX (#1167): the corrupted run must reclaim EVERY corrupted-induced slot, holding
    // the same ~60 as the control. WITHOUT the make-up the over-rate absorption skips each
    // corrupted slot and this under-runs by exactly the corrupted count (measured 59.13 fps ==
    // the live "~59.1") — the RED this test pins. `corrupted > 0` guards a mis-modelled fixture.
    assert!(
        corrupted > 0,
        "the corrupted run must actually inject corruption"
    );
    assert!(
        control_emits.saturating_sub(corrupt_emits) <= 1,
        "every corrupted-induced slot must be reclaimed (a single-slot dupe is acceptable, a \
             skipped slot never is): control {control_emits} emits vs corrupted {corrupt_emits} \
             emits (deficit {} over {corrupted} corrupted); WITHOUT the make-up the deficit == the \
             corrupted count",
        control_emits.saturating_sub(corrupt_emits)
    );
    assert!(
        corrupt_fps >= 59.8,
        "an over-rate box WITH corruption must still hold ~60 fps emit; got {corrupt_fps:.3} \
             fps ({corrupt_emits} emits, {corrupted} corrupted) — the emit under-runs by the \
             corrupted rate when the corrupted slot is not made up"
    );

    // (3) The make-up fires as copies of the nearest good frame (reusing the #1111 copy
    // counter): a non-zero make-up count is the mechanism proof.
    assert!(
        makeup_copies >= corrupted.saturating_sub(1),
        "the corrupted slots must be made up with copies of the nearest good frame; \
             {makeup_copies} copies emitted for {corrupted} corrupted slots"
    );
}

#[test]
fn no_corruption_is_byte_identical_1167() {
    // The #1167 fields/logic must be INERT with no corruption: the over-rate control emits the
    // same 60-unique rate and emits ZERO make-up copies (the #1111 valve stays at its genuine
    // starvation semantics — 0 here).
    let secs = 30.0;
    let (emits, corrupted, makeup_copies) = run_corrupted_over_rate_1167(31, 0, secs);
    assert_eq!(corrupted, 0);
    assert_eq!(
        makeup_copies, 0,
        "no corruption -> no make-up copy (the #1167 path is inert without note_corrupted_frame)"
    );
    let fps = emits as f64 / secs;
    assert!(
        (59.8..=60.05).contains(&fps),
        "no-corruption over-rate holds ~60 fps; got {fps:.3}"
    );
}

#[test]
fn corrupted_makeup_reclaims_only_slot_skipping_sheds_when_owed_1167() {
    // No deficit -> never reclaim, for EVERY action (byte-identical to today).
    for a in [
        ShedAction::Retire,
        ShedAction::Drain,
        ShedAction::FastDrain,
        ShedAction::Defer,
        ShedAction::BlindShed,
        ShedAction::Emit { copy: false },
        ShedAction::Emit { copy: true },
    ] {
        assert!(
            !corrupted_makeup_reclaims(a, 0),
            "deficit 0 must never reclaim ({a:?})"
        );
    }
    // Deficit owed: reclaim ONLY the slot-skipping over-rate sheds (Retire / Drain).
    assert!(corrupted_makeup_reclaims(ShedAction::Retire, 1));
    assert!(corrupted_makeup_reclaims(ShedAction::Drain, 3));
    // FastDrain (deep-backlog convergence), Defer (boundary held -> slot still filled),
    // BlindShed (between boundaries) and Emit (already fills the slot) are NEVER reclaimed.
    assert!(!corrupted_makeup_reclaims(ShedAction::FastDrain, 3));
    assert!(!corrupted_makeup_reclaims(ShedAction::Defer, 3));
    assert!(!corrupted_makeup_reclaims(ShedAction::BlindShed, 3));
    assert!(!corrupted_makeup_reclaims(
        ShedAction::Emit { copy: false },
        3
    ));
    assert!(!corrupted_makeup_reclaims(
        ShedAction::Emit { copy: true },
        3
    ));
}

#[test]
fn note_corrupted_frame_accrues_a_bounded_deficit_1167() {
    let mut gate = DecimationGate::new();
    assert_eq!(gate.corrupted_makeup_deficit(), 0);
    gate.note_corrupted_frame();
    gate.note_corrupted_frame();
    assert_eq!(gate.corrupted_makeup_deficit(), 2);
    // Bounded by CORRUPTED_MAKEUP_MAX_DEFICIT so a corruption burst cannot force a runaway copy
    // tail after corruption stops.
    for _ in 0..50 {
        gate.note_corrupted_frame();
    }
    assert_eq!(
        gate.corrupted_makeup_deficit(),
        CORRUPTED_MAKEUP_MAX_DEFICIT
    );
}

/// (#1167) Warm `sustained_over_rate` on a fresh gate: ~400 polls at a 62 fps takt with residence
/// 0 (`now_mono == capture_mono`) — no drain fires (queue_depth 0), all uniques emit, and the
/// takt EMA converges over-rate. Continue the timeline from index `next_i`.
fn warm_over_rate_gate(next_i: &mut u64) -> DecimationGate {
    let cap_int = (1e9 / 62.0) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let mut gate = DecimationGate::new();
    for i in 0..400u64 {
        let now = i * cap_int;
        let hash = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        let _ = gate.poll(now, emit_int, hash, true, now, now);
    }
    *next_i = 400;
    gate
}

#[test]
fn drain_leg_make_up_emits_instead_of_dropping_1167() {
    // Directly pins the DRAIN-leg conversion through the real `poll` (the retire path is covered
    // by over_rate_plus_corrupted_holds_target_emit_1167; the pure helper covers both). A frame
    // whose queue RESIDENCE >= QUEUE_DEPTH_SHED_INTERVALS at a sustained over-rate is DRAINED
    // (shed, emits nothing). With a corrupted make-up owed, the SAME drain is reclaimed (emitted).
    let cap_int = (1e9 / 62.0) as u64;
    let emit_int = 1_000_000_000u64 / 60;

    let mut next_i = 0u64;
    // capture_mono continues the monotonic sequence; now_mono is 2.5 emit-intervals AHEAD, so the
    // queue residence floors to 2 (>= QUEUE_DEPTH_SHED_INTERVALS) -> the first Drain arm fires.
    let capture_mono = 400u64 * cap_int;
    let now = capture_mono + emit_int * 5 / 2;

    let mut a = warm_over_rate_gate(&mut next_i);
    let hash = 0xDEAD_BEEFu64.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let emit_a = a.poll(now, emit_int, hash, true, now, capture_mono);

    let mut b = warm_over_rate_gate(&mut next_i);
    b.note_corrupted_frame();
    let emit_b = b.poll(now, emit_int, hash, true, now, capture_mono);

    assert!(
        !emit_a,
        "without a make-up owed, a residence>=2 over-rate frame is DRAINED (emits nothing) — the \
             #1145 v2 over-rate absorption"
    );
    assert!(
            emit_b,
            "with a corrupted make-up owed, the SAME drained slot is RECLAIMED (emitted) — the \
             Drain-leg #1167 conversion fills the corrupted-vacated slot with the nearest good frame"
        );
    assert_eq!(
        b.corrupted_makeup_deficit(),
        0,
        "the Drain-leg make-up consumed the one owed deficit unit"
    );
}

// ── (#1167) the invariant: fill every 60fps slot while a good frame is buffered ──────────────

#[test]
fn over_rate_fills_every_60fps_slot_holds_60_not_skipped_1167() {
    // (#1167 [red] -> [green]) The invariant (owner acceptance): at a sustained over-rate, while
    // ANY captured frame is buffered, every 60fps emit slot must be FILLED with the nearest good
    // frame (a single-slot dupe is acceptable; a skipped slot is NEVER). The merged #1145 v2 Drain
    // (residence>=2) ADVANCES the boundary emitting nothing — it drops the oldest AND skips its slot,
    // so an over-rate box under a send-bound loop emits BELOW 60 (the cam1 [4i/8align] sawtooth) even
    // though the buffered surplus could have filled the slot. RED before the fix: the sanctioned
    // send-bound `run_queue_sim` (all-unique captures — the Drain, not the dupe-shed, carries the
    // absorption) emits ~59.7 at a genuine over-rate. GREEN after: ~60 (the Drain HOLDS the boundary
    // so the next fresher frame fills the slot). Residence stays bounded (the fix still drops the
    // oldest), so the #1145 v2 sawtooth fix + V4L2-overflow pre-emption are preserved.
    for &fps in &[62.0_f64, 63.0] {
        let s = run_queue_sim(fps, 120, 30.0);
        // (#1167 v5) count the WIRE rate: a post-stall empty-queue slot that v2's Drain-hold
        // filled via a poll-emit is, under v5, filled by a starvation REPEAT instead (the fill now
        // fires at over-rate). Both land a frame on the wire, so the invariant is (poll-emits + repeats).
        let emit_fps = (s.emits + s.repeats) as f64 / 30.0;
        assert!(
            emit_fps >= 59.9,
            "an over-rate box must hold ~60 by filling every slot (issue-1167 invariant); got \
                 {emit_fps:.2} fps at {fps} capture (drained={}) — the merged v2 Drain skips the \
                 slot instead of filling it with the next good frame",
            s.drained
        );
        assert!(
            s.max_residence_post <= QUEUE_DEPTH_SHED_INTERVALS,
            "residence must stay bounded at the depth target even while filling slots; max {} \
                 intervals (target {})",
            s.max_residence_post,
            QUEUE_DEPTH_SHED_INTERVALS
        );
        assert_eq!(
            s.overflow_steady, 0,
            "filling slots must still pre-empt every V4L2 overflow-drop burst; steady overflow {}",
            s.overflow_steady
        );
    }
}

#[test]
fn steady_shallow_lag_trickle_drains_paced_then_fills_1167() {
    // (#1167 v3 [red] -> [green]) SUPERSEDES the v2/eleventh-piece "always FILL" application of a
    // steady shallow-lag dupe. v2 filled EVERY shallow-lag dupe and never drained the grid lag, so
    // the ~3.5 fps surplus CREEPS lag past RETIRE_MAX_LAG_INTERVALS, FastDrain fires + LATCHES, and
    // the shallow tail drains as a BURST (the 300/293 window oscillation + the +2 presented-id jump).
    // v3 adds a PACED trickle-drain: once lag has crept to SHALLOW_DRAIN_LAG_MIN and the shared
    // monotonic pace budget allows, a shallow-lag dupe takes ONE single-slot skip (advance, emit
    // nothing) to bleed the creep off BEFORE it reaches the FastDrain band. When PACED-OUT (a skip
    // just happened) it still FILLS the slot (the fill-every-slot invariant holds between trickle
    // skips), so at the slow steady creep rate the trickle fires ~1 skip per gap = 299-300 windows,
    // never a burst. The DECISION stays Retire (so #1145's decision tests + deep-backlog convergence
    // are preserved); only poll's application splits.
    let emit_int = 1_000_000_000u64 / 60;
    let cap_int = (1e9 / 62.0) as u64;
    let dupe_hash = 0xABCD_1234u64.wrapping_mul(0x9E37_79B9_7F4A_7C15);

    // CASE A — lag == SHALLOW_DRAIN_LAG_MIN, FRESH pace budget -> the trickle SKIPS (drains the creep).
    {
        let mut next_i = 0u64;
        let mut gate = warm_over_rate_gate(&mut next_i); // sustained_over_rate armed, not converging
        let cap = 400u64 * cap_int;
        // a UNIQUE at its on-time boundary sets prev_hash for the dupe below (lag 0, residence 0).
        let b0 = gate.next_boundary_ns();
        let _ = gate.poll(b0, emit_int, dupe_hash, true, cap, cap);
        // the DUPE crossing a shallow-stale boundary at lag == SHALLOW_DRAIN_LAG_MIN, budget fresh
        // (warm-up never skipped, so last_converge_skip_mono_ns == 0 and now_mono is far past it).
        let now = gate.next_boundary_ns() + emit_int * SHALLOW_DRAIN_LAG_MIN;
        let cap2 = cap + cap_int;
        let emit = gate.poll(now, emit_int, dupe_hash, true, cap2, cap2);
        let (_ds, _bl, copies, retired, drained, _fast) = gate.take_shed_counts();
        assert!(
            !emit,
            "a fresh-budget shallow-lag trickle must SKIP (drain the creep), not fill"
        );
        assert_eq!(
            (retired, drained, copies),
            (1, 0, 0),
            "the trickle is exactly ONE Retire skip (no drain/copy); \
                 retired={retired} drained={drained} copies={copies}"
        );
    }

    // CASE B — lag == SHALLOW_DRAIN_LAG_MIN but PACED-OUT (a trickle skip just happened) -> FILL.
    {
        let mut next_i = 0u64;
        let mut gate = warm_over_rate_gate(&mut next_i);
        let cap = 400u64 * cap_int;
        let b0 = gate.next_boundary_ns();
        let _ = gate.poll(b0, emit_int, dupe_hash, true, cap, cap);
        // first shallow-lag dupe -> trickle SKIP, stamps the pace budget at now_mono = cap2.
        let now1 = gate.next_boundary_ns() + emit_int * SHALLOW_DRAIN_LAG_MIN;
        let cap2 = cap + cap_int;
        let _ = gate.poll(now1, emit_int, dupe_hash, true, cap2, cap2);
        let _ = gate.take_shed_counts();
        // second shallow-lag dupe one cap_int later -> now_mono only ~16 ms past cap2, far under
        // CONVERGE_SKIP_MIN_GAP_INTERVALS * emit_int (500 ms) -> paced-out -> FILL the slot.
        let now2 = gate.next_boundary_ns() + emit_int * SHALLOW_DRAIN_LAG_MIN;
        let cap3 = cap2 + cap_int;
        let emit = gate.poll(now2, emit_int, dupe_hash, true, cap3, cap3);
        let (_ds, _bl, copies, retired, _drn, _fast) = gate.take_shed_counts();
        assert!(
            emit,
            "a paced-out shallow-lag dupe must FILL the slot (the fill-every-slot invariant between skips)"
        );
        assert!(
            copies >= 1,
            "the paced-out fill is counted as a #1111 copy; copies={copies}"
        );
        assert_eq!(
            retired, 0,
            "a paced-out shallow-lag dupe must not skip; retired={retired}"
        );
    }

    // The DECISION for that band is STILL Retire (only the poll application changed) — and the DEEP
    // band above the ceiling is STILL FastDrain, so #1145's deep-backlog convergence is untouched.
    assert_eq!(
        dupe_shed_action(true, true, false, SHALLOW_DRAIN_LAG_MIN, true, 0, true),
        ShedAction::Retire,
        "the shallow-lag DECISION stays Retire (poll reinterprets it) so #1145 is preserved"
    );
    assert_eq!(
        dupe_shed_action(
            true,
            true,
            false,
            RETIRE_MAX_LAG_INTERVALS + 1,
            true,
            0,
            true
        ),
        ShedAction::FastDrain,
        "the deep-backlog band above the ceiling stays FastDrain (a skip to converge a reconnect)"
    );
}

#[test]
fn drain_holds_the_boundary_so_the_next_frame_fills_the_slot_1167() {
    // (#1167 [red] -> [green]) A residence>=2 over-rate Drain must HOLD the boundary (drop the
    // oldest to bound residence, but NEVER advance/skip) so the NEXT fresher frame fills the same
    // 60fps slot. RED before the fix: the merged v2 Drain advanced the boundary (skipped the slot);
    // GREEN after: it holds the boundary (emits nothing THIS poll, but the slot survives to be
    // filled). Same warmed-over-rate + residence-2 setup as drain_leg_make_up_emits_instead_of_
    // dropping_1167 (proven to trigger the first Drain arm).
    let cap_int = (1e9 / 62.0) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let mut next_i = 0u64;
    let mut gate = warm_over_rate_gate(&mut next_i);
    let _ = gate.take_shed_counts(); // clear the warm-up's blind-pacing sheds so this poll is isolated
    let capture_mono = 400u64 * cap_int;
    let now = capture_mono + emit_int * 5 / 2; // residence floors to 2 -> the first Drain arm
    let boundary_before = gate.next_boundary_ns();
    let hash = 0x00C0_FFEEu64.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let emit = gate.poll(now, emit_int, hash, true, now, capture_mono);
    assert!(
        !emit,
        "a residence>=2 over-rate frame is still SHED (drained) this poll — the over-rate \
             absorption still drops the oldest to bound residence"
    );
    assert_eq!(
        gate.next_boundary_ns(),
        boundary_before,
        "the Drain must HOLD the boundary (not advance/skip) so the next fresher frame fills the \
             slot — issue-1167 invariant (a skipped slot is never acceptable)"
    );
    // Self-sufficient: prove it was the DRAIN arm that fired (a BlindShed would also be !emit +
    // boundary-unchanged), so the assertions above cannot pass for the wrong reason.
    let (_ds, blind, copies, retired, drained, _fast) = gate.take_shed_counts();
    assert_eq!(
        (drained, blind, copies, retired),
        (1, 0, 0, 0),
        "the HOLD must be recorded as exactly one DRAIN (not a blind-shed / copy / retire)"
    );
}

#[test]
fn drain_hold_panic_floor_fills_after_max_consecutive_holds_1167() {
    // (#1167) The Drain-hold PANIC FLOOR: a BOGUS stuck-high residence would otherwise hold the SAME
    // boundary forever (fail-black). After DRAIN_HOLD_PANIC_FLOOR consecutive Drain-HOLDS the gate
    // must FILL the slot with a copy (advance + emit) — fail-SAFE. Decouple the two clocks: keep the
    // REALTIME grid on-time (now_ns just past the held boundary, lag ~1 so `would_emit` stays true and
    // the Drain arm — checked before the lag branches — keeps firing) while the MONOTONIC residence is
    // pegged at the clamp (now_mono ≫ capture_mono) so `queue_depth >= QUEUE_DEPTH_SHED_INTERVALS`
    // every poll. UNIQUE hashes (never dupes) so it is the first, dupeness-blind Drain arm.
    let cap_int = (1e9 / 62.0) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let mut next_i = 0u64;
    let mut gate = warm_over_rate_gate(&mut next_i); // over-rate armed, not converging (no FastDrain)
    let _ = gate.take_shed_counts(); // clear the warm-up counters so the floor accounting is isolated
    let boundary = gate.next_boundary_ns();
    let now_ns = boundary + emit_int; // lag ~1: would_emit true, but the residence Drain arm wins
    for hold in 1..DRAIN_HOLD_PANIC_FLOOR {
        let cap_mono = (399 + hold) * cap_int; // continues the warm-up takt (each interval = cap_int)
        let now_mono = cap_mono + emit_int * 8; // residence pegged at the clamp -> Drain every poll
        let hash = hold.wrapping_mul(0x9E37_79B9_7F4A_7C15); // unique -> the dupeness-blind Drain arm
        let emit = gate.poll(now_ns, emit_int, hash, true, now_mono, cap_mono);
        assert!(
            !emit,
            "hold #{hold} must HOLD (emit nothing), not fill, before the floor"
        );
        assert_eq!(
            gate.next_boundary_ns(),
            boundary,
            "hold #{hold} must keep the SAME boundary (a held, un-advanced slot)"
        );
    }
    // The DRAIN_HOLD_PANIC_FLOOR-th consecutive hold trips the floor: FILL (emit a copy) + advance.
    let cap_mono = (399 + DRAIN_HOLD_PANIC_FLOOR) * cap_int;
    let now_mono = cap_mono + emit_int * 8;
    let hash = 0xF100_0000u64.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let emit = gate.poll(now_ns, emit_int, hash, true, now_mono, cap_mono);
    assert!(
        emit,
        "the {DRAIN_HOLD_PANIC_FLOOR}th consecutive Drain-hold must trip the PANIC FLOOR and FILL \
             the slot (fail-SAFE, never fail-black)"
    );
    assert_eq!(
        gate.next_boundary_ns(),
        boundary + emit_int,
        "the floor fill must ADVANCE the boundary exactly one interval (a single slot)"
    );
    let (_ds, _bl, copies, _ret, drained, _fast) = gate.take_shed_counts();
    assert_eq!(
        (drained, copies),
        (DRAIN_HOLD_PANIC_FLOOR - 1, 1),
        "the floor fill is a COPY (not a drain), so drained counts only the holds before it"
    );
}

// ── (#1167 v3) PACE the convergence: amortize the skips, never a burst ─────────────────────────

/// (#1167 v3) Result of [`run_over_rate_creep_sim`].
struct CreepSim {
    /// The MAX boundary delta between two consecutive EMITs (1 = perfect 60fps cadence; 2 = one
    /// boundary skipped between emits = a +1 presented-id jump; >= 3 = a +2 FastDrain jump / a burst
    /// = the cam1 [4i/8align] sawtooth this ticket kills). v3 keeps it <= 2 (single-slot skips only).
    max_emit_boundary_delta: u64,
    /// The WORST count of convergence SKIPS (advance-emit-nothing sheds) within any single sliding
    /// window of [`CONVERGE_SKIP_MIN_GAP_INTERVALS`] emit intervals — the BURST size. v2 lets the
    /// shallow tail drain as a burst (>= 2); v3 paces it to <= 1 per gap.
    max_burst_in_gap: u64,
    /// Total convergence skips post-warmup (proves the machinery engaged — not a no-op pass).
    skips_total: u64,
    /// Emitted fps over the whole run (holds near 60 — every slot still filled but for the paced skips).
    emit_fps: f64,
}

/// (#1167 v3) Drive the REAL [`DecimationGate::poll`] with a SEND-BOUND over-rate emit loop (the
/// degrading grabber at `capture_fps` > 60 with the NDI send just under one 60fps interval), single
/// wall==monotonic==grid clock exactly like the live cam box (NO reconnect offset). The send-bound
/// loop lets the ~(capture-60) fps surplus CREEP grid lag upward (Drain-HOLDs advance the wall clock
/// but not the boundary) — the live steady mechanism. On v2 the creep reaches the FastDrain band and
/// the shallow tail drains as a BURST; v3's paced trickle bleeds it off smoothly. `slack_num`/1000 is
/// the send cost as a fraction of the emit interval (999 = 0.1% slack = send-bound creep). Dupes are
/// isolated content-pairs at the over-rate delta (the byte-hash `is_dupe` model — same as
/// [`run_grid_backlog_sim`]).
fn run_over_rate_creep_sim(capture_fps: f64, secs: f64, slack_num: u64) -> CreepSim {
    let cap_int = (1e9 / capture_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let send_cost = emit_int * slack_num / 1000;
    let shed_cost = 1_000_000u64; // 1 ms (hash only)
    const MAXQ: usize = 4;
    const WARMUP_NS: u64 = 8_000_000_000; // establish the takt EMA + settle before measuring
    let gap_ns = CONVERGE_SKIP_MIN_GAP_INTERVALS * emit_int;
    let n = (capture_fps * secs) as u64;

    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<u64> = VecDeque::new(); // capture-monotonic instants
    let mut next_cap = 0u64;
    let mut wall = 0u64; // single clock: wall == monotonic == grid

    let over_rate = capture_fps - 60.0;
    let dupe_period = if over_rate > 0.01 {
        (capture_fps / over_rate).round() as u64
    } else {
        u64::MAX
    };
    let (mut next_id, mut prev_id): (u64, u64) = (0, 0);

    let mut emits = 0u64;
    let mut last_emit_bidx: Option<u64> = None;
    let mut max_emit_boundary_delta = 0u64;
    let mut skips_total = 0u64;
    let mut skip_walls: VecDeque<u64> = VecDeque::new();
    let mut max_burst_in_gap = 0u64;

    loop {
        while next_cap < n {
            let cap_ns = next_cap * cap_int;
            if cap_ns > wall {
                break;
            }
            if queue.len() < MAXQ {
                queue.push_back(cap_ns);
            }
            next_cap += 1;
        }
        if queue.is_empty() {
            if next_cap >= n {
                break;
            }
            wall = next_cap * cap_int;
            continue;
        }
        let cap_ns = queue.pop_front().unwrap();
        let now = wall;

        let is_dupe = dupe_period != u64::MAX && next_cap % dupe_period == dupe_period - 1;
        let cid = if is_dupe {
            prev_id
        } else {
            let id = next_id;
            next_id += 1;
            id
        };
        prev_id = cid;
        let content_hash = cid.wrapping_mul(0x9E37_79B9_7F4A_7C15);

        let prev_boundary = gate.next_boundary_ns();
        let emit = gate.poll(now, emit_int, content_hash, true, now, cap_ns);
        let new_boundary = gate.next_boundary_ns();
        let advanced = new_boundary.saturating_sub(prev_boundary) / emit_int;
        let post_warm = now > WARMUP_NS;

        // a convergence SKIP = the boundary advanced but nothing was emitted this poll.
        if !emit && advanced >= 1 && post_warm {
            skips_total += 1;
            skip_walls.push_back(now);
            while let Some(&front) = skip_walls.front() {
                if now.saturating_sub(front) >= gap_ns {
                    skip_walls.pop_front();
                } else {
                    break;
                }
            }
            max_burst_in_gap = max_burst_in_gap.max(skip_walls.len() as u64);
        }

        let mut cost = shed_cost;
        if emit {
            cost = send_cost;
            emits += 1;
            if let Some(prev) = last_emit_bidx {
                if post_warm {
                    max_emit_boundary_delta =
                        max_emit_boundary_delta.max((new_boundary / emit_int).saturating_sub(prev));
                }
            }
            last_emit_bidx = Some(new_boundary / emit_int);
        }
        wall += cost;
        if next_cap >= n && queue.is_empty() {
            break;
        }
    }
    CreepSim {
        max_emit_boundary_delta,
        max_burst_in_gap,
        skips_total,
        emit_fps: emits as f64 / secs,
    }
}

#[test]
fn over_rate_convergence_is_paced_not_bursty_1167() {
    // (#1167 v3 [red] -> [green]) The v2 fill-every-slot fix held cam1's AVERAGE emit at ~59.94 but
    // per-5s windows oscillated 300/300/293: at the degrading grabber's ~3.5 fps surplus the grid lag
    // CREEPS past RETIRE_MAX_LAG_INTERVALS, FastDrain fires and LATCHES, and the whole shallow tail
    // drains as a BURST of advance-emit-nothing sheds in a fraction of a second -> cam1's presented
    // frame_id jumps several ahead of its siblings -> [4i/8align] "mutual stability <=1 id" abort.
    //
    // v3 PACES the convergence: a steady trickle-drain bleeds the creep off before it reaches the
    // FastDrain band, and the shared monotonic min-gap budget smears any convergence tail to at most
    // one single-slot skip per CONVERGE_SKIP_MIN_GAP_INTERVALS. So the presented-id trace becomes
    // monotone-smooth (a +1 id jump at most) instead of the sawtooth.
    //
    // RED before the fix (send-bound creep, 63.5 fps, the REAL poll): max emit-boundary delta = 3 (a
    // +2 FastDrain jump) and up to 2-3 skips bunch inside one gap window. GREEN after: delta <= 2
    // (single-slot skips only) and never more than one skip per gap.
    let s = run_over_rate_creep_sim(63.5, 90.0, 999);
    assert!(
        s.skips_total > 0,
        "the sim must actually exercise the convergence path (over-rate creep); skips={}",
        s.skips_total
    );
    assert!(
        s.max_emit_boundary_delta <= 2,
        "v3 must keep every presented-id jump to +1 (single-slot skips only, no +2 FastDrain burst); \
             max emit-boundary delta {} (v2 bursts to 3)",
        s.max_emit_boundary_delta
    );
    assert!(
        s.max_burst_in_gap <= 1,
        "v3 must PACE convergence skips to <= 1 per CONVERGE_SKIP_MIN_GAP_INTERVALS (no burst); \
             worst burst {} skips in one gap window (v2 bursts to 2-3)",
        s.max_burst_in_gap
    );
    // the emit rate still holds near 60 (every slot filled but for the paced single-slot skips).
    assert!(
        s.emit_fps >= 58.0,
        "the paced convergence must still hold emit above the #666 floor; got {:.2} fps",
        s.emit_fps
    );
}

// ── (#1167 v4) under-rate empty-queue STARVATION: bounded last-frame repeat ─────────────────────

/// (#1167 v4) Under-rate STARVATION model driving the REAL [`DecimationGate::poll`]. The sick
/// grabber captures BELOW 60 fps, so fewer than 60 frames arrive per second and EACH comes from an
/// EMPTY V4L2 queue (`queue_had_frame = false`: the blocking dequeue genuinely WAITED ~one capture
/// interval — exactly what `capture_stall::frame_from_nonempty_queue` reads on the live box). Single
/// wall==mono==grid clock; `now == capture instant` (the loop processed each frame on arrival, the
/// under-rate has send-slack). `all_unique`: a distinct hash per frame (a live-but-slow source);
/// `!all_unique`: one constant hash (a FROZEN/wedged painter — every frame a byte-identical dupe).
/// Returns the total EMIT EVENTS (poll-true PLUS the starvation repeats poll asked main.rs to emit —
/// exactly the frames main.rs sends), the poll-emit count, the repeat count, the NET #707
/// boundary-skips (after deducting the intentional fill-advance, mirroring the main.rs #707 wiring),
/// and the per-5s-window emit-event counts.
struct StarvationSim {
    emit_events: u64,
    poll_emits: u64,
    repeats: u64,
    net_skips: u64,
    windows: Vec<u64>,
}

fn run_starvation_sim(capture_fps: f64, secs: f64, all_unique: bool) -> StarvationSim {
    let cap_int = (1e9 / capture_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let n = (capture_fps * secs) as u64;
    let mut gate = DecimationGate::new();
    let (mut emit_events, mut poll_emits, mut repeats, mut net_skips) = (0u64, 0u64, 0u64, 0u64);
    let mut windows: Vec<u64> = Vec::new();
    let (mut win_events, mut win_start) = (0u64, 0u64);
    for i in 0..n {
        let cap_ns = i * cap_int;
        let now = cap_ns; // UNDER-rate: the loop waited for this frame -> empty queue
        let queue_had_frame = false;
        let content_hash = if all_unique {
            i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1)
        } else {
            0xF00D
        };
        let prev_b = gate.next_boundary_ns();
        let emit = gate.poll(now, emit_int, content_hash, queue_had_frame, now, cap_ns);
        let next_b = gate.next_boundary_ns();
        let r = gate.last_poll_starvation_repeats();
        let s = crate::genlock_pacing::boundary_skip_count(prev_b, next_b, emit_int)
            .saturating_sub(gate.last_poll_intentional_extra_advance());
        net_skips += s;
        repeats += r;
        let this_events = (emit as u64) + r; // exactly the frames main.rs emits this poll
        if emit {
            poll_emits += 1;
        }
        emit_events += this_events;
        win_events += this_events;
        if now.saturating_sub(win_start) >= 5_000_000_000 {
            windows.push(win_events);
            win_events = 0;
            win_start = now;
        }
    }
    StarvationSim {
        emit_events,
        poll_emits,
        repeats,
        net_skips,
        windows,
    }
}

#[test]
fn under_rate_starvation_fills_every_slot_to_hold_60_1167() {
    // 57.9 fps captured (the live sick-ShadowCast sub-60 wander floor), all-unique, empty queue.
    // GREEN: bounded last-frame repeats fill the empty-queue slots -> emit holds ~60 (300/window).
    // RED (pre-fix): poll emits ~57.9/s and asks for ZERO repeats -> ~290/window under-run.
    let s = run_starvation_sim(57.9, 20.0, true);
    let rate = s.emit_events as f64 / 20.0;
    assert!(
        (59.0..=60.5).contains(&rate),
        "under-rate starvation must fill every empty-queue slot to hold ~60; got {rate:.2} fps \
         ({} emit events / 20s, {} poll-emits, {} repeats, windows {:?})",
        s.emit_events,
        s.poll_emits,
        s.repeats,
        s.windows
    );
    assert!(
        s.repeats > 0,
        "the fill must come from starvation last-frame repeats; got {} repeats",
        s.repeats
    );
}

#[test]
fn under_rate_starvation_never_skips_a_slot_1167() {
    // Every empty-queue boundary is FILLED (a repeat), so the #707 resync never fires and the net
    // boundary-skip count — after deducting the intentional fill-advance, exactly the main.rs #707
    // wiring — is 0. RED (pre-fix): the accumulating grid lag trips the #131 resync -> tens of skips.
    let s = run_starvation_sim(57.9, 20.0, true);
    assert_eq!(
        s.net_skips, 0,
        "under-rate starvation must not leave a skipped (un-emitted) 60fps slot; got {} net #707 \
         skips (windows {:?})",
        s.net_skips, s.windows
    );
}

#[test]
fn frozen_source_gets_no_starvation_repeat_and_stays_exposed_1167() {
    // A FROZEN/wedged painter delivers only content-DUPES -> the #1111 copy valve (Emit{copy:true}),
    // which the v4 `!copy` gate excludes: it receives ZERO starvation repeats, so it emits only at
    // its (sub-60) capture rate and stays visible to #666 / the frozen-leg attribution — a dead
    // camera must still look down, never masked to 60 (the ticket's hard constraint).
    let s = run_starvation_sim(57.9, 20.0, false);
    assert_eq!(
        s.repeats, 0,
        "a frozen (all-dupe) source must never receive a starvation fill; got {} repeats",
        s.repeats
    );
    let rate = s.emit_events as f64 / 20.0;
    assert!(
        rate < 59.0,
        "a frozen source must under-run (stay exposed), not be filled to 60; got {rate:.2} fps"
    );
}

#[test]
fn genuinely_half_rate_leg_is_bounded_by_the_repeat_cap_1167() {
    // A source so slow it NEVER delivers an on-time capture (30 fps — every frame >= 1 interval late)
    // can never reset the consecutive-repeat cap, so after STARVATION_REPEAT_MAX repeats the fill
    // stops and the leg under-runs -> it stays exposed, NOT masked to 60. The cap is the fail-safe
    // that prevents an infinite freeze-loop from papering over a genuinely half-dead leg.
    let s = run_starvation_sim(30.0, 20.0, true);
    let rate = s.emit_events as f64 / 20.0;
    assert!(
        rate < 58.0,
        "the consecutive cap must keep a genuinely half-rate leg exposed (well under 60), not fill \
         it to target; got {rate:.2} fps ({} repeats)",
        s.repeats
    );
}

#[test]
fn over_rate_with_a_full_queue_never_starvation_repeats_1167() {
    // (#1167 v5) At a genuine OVER-rate (62 fps, real monotonic takt so `sustained_over_rate` arms),
    // a frame drained from a NON-EMPTY queue (`queue_had_frame=true` — the physical reality of an
    // over-rate box: the queue is backed up, the dequeue is instant) must never trigger a starvation
    // repeat. v5 made the fill REGIME-INDEPENDENT (a STALL on an over-rate box DOES fill — see
    // over_rate_empty_queue_stall_fills_every_slot_regardless_of_regime_1167_v5), so the guard that
    // keeps the over-rate v2/v3 machinery byte-inert is now the EMPTY-queue gate, not a rate gate:
    // this proves `!queue_had_frame` alone keeps the fill off for the non-empty-queue over-rate case.
    // (v4 forced `queue_had_frame=false` here to prove its `sustained_under_rate` gate did the work;
    // under v5 that combination is exactly what SHOULD fill, so the premise is corrected to the real
    // over-rate state — this test passes on BOTH the pre-fix and post-fix code.)
    let captures = synthetic_889_capture_sequence(62.0, 62 * 8, 15);
    let emit_int = 1_000_000_000u64 / 60;
    let mut gate = DecimationGate::new();
    let mut repeats = 0u64;
    for (now_ns, content_id, _is_dupe) in &captures {
        let _ = gate.poll(*now_ns, emit_int, *content_id, true, *now_ns, *now_ns);
        repeats += gate.last_poll_starvation_repeats();
    }
    assert_eq!(
        repeats, 0,
        "an over-rate source with a full (non-empty) queue must never starvation-repeat; got {repeats}"
    );
}

#[test]
fn healthy_60_never_starvation_repeats_1167() {
    // A healthy 60.00 card (takt ~16.667 ms, on the grid: lag 0 every poll) must be byte-inert:
    // even under v5 (a live capture takt arms `has_live_capture_takt`), the fill's `1 <= lag` gate
    // excludes an on-grid card, so ZERO starvation repeats.
    let cap_int = 1_000_000_000u64 / 60;
    let emit_int = 1_000_000_000u64 / 60;
    let mut gate = DecimationGate::new();
    let mut repeats = 0u64;
    for i in 0u64..600 {
        let now = i * cap_int;
        let hash = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        let _ = gate.poll(now, emit_int, hash, false, now, now);
        repeats += gate.last_poll_starvation_repeats();
    }
    assert_eq!(
        repeats, 0,
        "a healthy 60.00 card must never trigger a starvation repeat; got {repeats}"
    );
}

// ── (#1167 v5) empty-queue fill must be REGIME-INDEPENDENT (over-rate + stall) ──────────────

struct OverRateStallSim {
    /// The capture-takt EMA settled to a genuine OVER-rate (the v4-inert regime this exercises).
    over_rate: bool,
    /// Total starvation last-frame repeats emitted post-warmup (the v5 fill firing).
    repeats: u64,
    /// Net #707 boundary SKIPS post-warmup (a skipped emit-slot = a wire gap >= 2 intervals).
    net_skips: u64,
    /// Max grid lag (intervals) any emit reached post-warmup — how deep the grid fell behind
    /// before it was caught up (the "no sustained catch-up burst" / recv-cap_max proxy).
    max_lag: u64,
    /// Per-5s emit events (poll-emits + repeats), clean post-warmup windows only.
    windows: Vec<u64>,
}

/// (#1167 v5) Drive the REAL [`DecimationGate::poll`] with an OVER-rate stream punctuated by
/// PERIODIC empty-queue capture STALLS (a sick ShadowCast's VIDIOC_DQBUF stalls) — the regime v4
/// left unfilled because its fill was gated on a POSITIVE sustained-under-rate. Faithful #1131
/// `queue_had_frame`: FALSE only for the frame the loop BLOCKED on (the post-stall dequeue), TRUE
/// for every frame drained from a non-empty queue (so the over-rate machinery is byte-inert on
/// those, exactly as production reads the real dequeue-duration signal). Any `stall_ms` up to ~45
/// stays < [`TAKT_GAP_EXCLUDE_NS`] (50 ms), so it is folded into the EMA and the card stays a
/// genuine over-rate — the v4 hole (the live cam1 stall is ~25.6 ms; the test drives 45 ms as a
/// harsher-than-nominal stress that still folds).
fn run_over_rate_stall_sim(
    capture_fps: f64,
    stall_ms: u64,
    stall_period_s: f64,
    secs: f64,
) -> OverRateStallSim {
    let cap_int = (1e9 / capture_fps) as u64;
    let emit_int = 1_000_000_000u64 / 60;
    let send_cost = emit_int * 991 / 1000; // ~16.5 ms -> max emit ~60.5/s (run_queue_sim model)
    let shed_cost = 1_000_000u64;
    let stall_ns = stall_ms * 1_000_000;
    let stall_period_frames = (capture_fps * stall_period_s) as u64;
    const MAXQ: usize = 4; // V4L2 buffers (capture.rs: Stream::with_buffers(.., 4))
    const WARMUP_NS: u64 = 8_000_000_000; // ignore the takt-EMA warmup
    let n = (capture_fps * secs) as u64;

    // Capture ARRIVAL schedule: an over-rate base with a periodic empty-queue stall gap injected.
    let mut cap_time = vec![0u64; n as usize];
    for i in 1..n as usize {
        let mut gap = cap_int;
        if (i as u64).is_multiple_of(stall_period_frames) {
            gap += stall_ns;
        }
        cap_time[i] = cap_time[i - 1] + gap;
    }

    let mut gate = DecimationGate::new();
    let mut queue: VecDeque<(u64, u64)> = VecDeque::new();
    let mut next_cap = 0usize;
    let mut wall = 0u64;
    let (mut repeats, mut net_skips, mut max_lag) = (0u64, 0u64, 0u64);
    let mut windows: Vec<u64> = Vec::new();
    let (mut win_events, mut win_start, mut warmup_anchored) = (0u64, 0u64, false);
    let src_int = emit_int;
    let mut waited_for_this = false; // FAITHFUL #1131: did the loop BLOCK for the next popped frame?

    loop {
        while next_cap < n as usize {
            let c = cap_time[next_cap];
            if c > wall {
                break;
            }
            let src_id = c / src_int;
            if queue.len() < MAXQ {
                queue.push_back((c, src_id));
            }
            next_cap += 1;
        }
        if queue.is_empty() {
            if next_cap >= n as usize {
                break;
            }
            wall = cap_time[next_cap]; // loop BLOCKS on the empty queue for the next capture
            waited_for_this = true;
            continue;
        }
        let (c, src_id) = queue.pop_front().unwrap();
        let now = wall;
        let queue_had_frame = !waited_for_this;
        waited_for_this = false;
        let prev_b = gate.next_boundary_ns();
        let lag = crate::genlock_pacing::genlock_lag_intervals(now, prev_b, emit_int);
        let hash = src_id.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(now);
        let emit = gate.poll(now, emit_int, hash, queue_had_frame, now, c);
        let next_b = gate.next_boundary_ns();
        let r = gate.last_poll_starvation_repeats();
        let s = crate::genlock_pacing::boundary_skip_count(prev_b, next_b, emit_int)
            .saturating_sub(gate.last_poll_intentional_extra_advance());
        let ev = (emit as u64) + r;
        if now > WARMUP_NS {
            repeats += r;
            net_skips += s;
            max_lag = max_lag.max(lag);
            if !warmup_anchored {
                win_start = now;
                warmup_anchored = true;
            }
            win_events += ev;
            if now.saturating_sub(win_start) >= 5_000_000_000 {
                windows.push(win_events);
                win_events = 0;
                win_start = now;
            }
        }
        wall += if emit { send_cost } else { shed_cost };
        if next_cap >= n as usize && queue.is_empty() {
            break;
        }
    }
    OverRateStallSim {
        over_rate: gate.sustained_over_rate(),
        repeats,
        net_skips,
        max_lag,
        windows,
    }
}

#[test]
fn over_rate_empty_queue_stall_fills_every_slot_regardless_of_regime_1167_v5() {
    // (#1167 v5 [red]->[green]) The v4 empty-queue fill was gated on a POSITIVE sustained-under-rate,
    // so a sick grabber on AVERAGE over-rate (61-63 fps) but with periodic VIDIOC_DQBUF stalls read
    // over-rate and left every post-stall empty-queue slot UNFILLED: the grid crept behind, the #131
    // resync skipped the accumulated boundaries (net_skips > 0 = wire gaps), emit under-ran (windows
    // 295-299) and the strih receiver saw the gap+burst -> the cam1 [4i/8align] sawtooth. v5 keys the
    // fill on a LIVE capture takt (regime-INDEPENDENT), so an empty-queue slot at a boundary is
    // filled AT the boundary regardless of the over-rate EMA. RED (v4): over_rate=true, repeats=0,
    // net_skips>0, max_lag deep (>8, into the resync band), windows drop below 299. GREEN (v5):
    // repeats>0, net_skips=0, max_lag bounded (the fill catches the grid up immediately), windows
    // 299-301. The receiver-side recv-cap_max smoothing is the supervisor's live-rig verification;
    // off-rig these are the faithful sender-side analogues.
    let s = run_over_rate_stall_sim(62.0, 45, 1.0, 30.0);
    assert!(
        s.over_rate,
        "the sim must stay a genuine OVER-rate EMA (the v4-inert regime this exercises); \
         sustained_over_rate was false"
    );
    assert!(
        s.repeats > 0,
        "v5 must FILL the empty-queue slots at over-rate (v4 leaves 0 — the hole); got {} repeats \
         (windows {:?})",
        s.repeats,
        s.windows
    );
    assert_eq!(
        s.net_skips, 0,
        "no emit-slot may be skipped on the wire (v4 lets the grid creep + #131-resync past them); \
         got {} net #707 skips (windows {:?})",
        s.net_skips, s.windows
    );
    assert!(
        s.max_lag <= STARVATION_REPEAT_MAX,
        "the fill must catch the grid up immediately (never a deep catch-up burst -> no receiver \
         depth sawtooth); max_lag {} intervals (v4 falls to ~9, into the #131 resync band)",
        s.max_lag
    );
    for &w in &s.windows {
        // The discriminating invariant is the anti-UNDER-run: v4 leaves the empty-queue slots
        // unfilled so each stall costs a slot -> windows sag to 295-299; v5 fills them so no window
        // under-runs. The upper bound is a loose runaway guard — a window may transiently read 302
        // (60.4 fps) as the fill catches up an accrued lag in the first steady window, still bounded.
        assert!(
            w >= 299,
            "no steady 5s window may under-run below ~60 (v4 drops to 295-297 without the fill); \
             got {w} in {:?}",
            s.windows
        );
        assert!(
            w <= 303,
            "each 5s window stays bounded near 60 (no runaway catch-up burst); got {w} in {:?}",
            s.windows
        );
    }
}
