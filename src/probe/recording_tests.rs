//! issue 1404 — the unit tests of `recording` (a `#[path]` child, split out like
//! `recording_decode_tests.rs` to keep `recording.rs` inside its size budget). Moved verbatim.

use super::*;
use crate::probe::luma::bgra_to_luma;
use crate::probe::qr::render_qr_dual_bgra;

fn dual_qr_luma(left_id: u32, right_id: u32) -> GrayImage {
    let (cw, ch, qs) = (960u32, 540u32, 260u32);
    let l = Payload {
        run_id: 6519,
        frame_id: left_id,
        gen_ts_ns: 1,
    };
    let r = Payload {
        run_id: 6519,
        frame_id: right_id,
        gen_ts_ns: 2,
    };
    let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    bgra_to_luma(&bgra, cw, ch, cw * 4)
}

/// #707 — a frame carrying only ONE of the dual-QR Vernier's two halves (the run_id
/// [`dual_qr_luma`] uses, 6519, with only `held_id` painted; the other half was never
/// painted onto this frame at all). Models the real-world "one Vernier half missed" case.
fn single_qr_luma(held_id: u32) -> GrayImage {
    let (cw, ch, qs) = (960u32, 540u32, 260u32);
    let p = Payload {
        run_id: 6519,
        frame_id: held_id,
        gen_ts_ns: 1,
    };
    let bgra = crate::probe::qr::render_qr_bgra(&p, cw, ch, qs);
    bgra_to_luma(&bgra, cw, ch, cw * 4)
}

#[test]
fn decode_recording_frame_returns_both_dual_qrs() {
    // A strih-6519-type frame (two sharp QRs) decodes to BOTH — the exact case
    // opencv silently returned 0/1 on.
    let f = decode_recording_frame(0, dual_qr_luma(6518, 6519));
    assert_eq!(f.frame_index, 0);
    assert_eq!(f.payloads.len(), 2, "both QRs decode: {:?}", f.payloads);
    let ids: Vec<u32> = f.payloads.iter().map(|p| p.frame_id).collect();
    assert!(ids.contains(&6518) && ids.contains(&6519), "ids {ids:?}");
}

/// #632 gap 1: the new grouped wrapper (mandatory/any-of split) must decode IDENTICALLY to
/// the plain flat-list function when the any-of group is empty — proving the #632 addition
/// is a pure superset, never a behavior change for any existing (mandatory-only) caller.
/// (issue 1367: the grouped path also runs the node-burn echo gate and the flat one does not,
/// so the two differ once a node burn sits outside its slot; this frame carries no node burn.)
#[test]
fn decode_recording_frame_with_grouped_burns_matches_flat_when_any_of_empty() {
    let luma = dual_qr_luma(200, 201);
    let flat = decode_recording_frame_with_burns(5, luma.clone(), &[]);
    let grouped = decode_recording_frame_with_grouped_burns(5, luma, &[], &[]);
    assert_eq!(
        flat, grouped,
        "empty mandatory + empty any-of must decode identically to the flat function"
    );
}

/// #632 gap 1: the any-of group is satisfied by ANY of its members — using the dual-QR's
/// own run_id (always present) as a stand-in "camera burn" alongside an id that never
/// appears, proving the grouped decode still returns the full correct payload set.
#[test]
fn decode_recording_frame_with_grouped_burns_decodes_fully_when_any_of_satisfied() {
    let f = decode_recording_frame_with_grouped_burns(
        0,
        dual_qr_luma(300, 301),
        &[],
        &[999_999, 6519], // 6519 = dual_qr_luma's own run_id; 999_999 never appears
    );
    assert_eq!(
        f.payloads.len(),
        2,
        "both QRs still decode: {:?}",
        f.payloads
    );
    assert_eq!(f.tick, Some(301));
}

/// #707 wiring proof: `decode_recording_frame_with_grouped_burns_optical` with
/// `min_distinct_optical` unset (`None`) must decode IDENTICALLY to the plain grouped
/// function — every pre-#707 caller is unaffected by this parameter existing at all.
#[test]
fn grouped_burns_optical_matches_grouped_when_optical_requirement_is_none() {
    let luma = dual_qr_luma(400, 401);
    let plain = decode_recording_frame_with_grouped_burns(9, luma.clone(), &[], &[]);
    let optical = decode_recording_frame_with_grouped_burns_optical(9, luma, &[], &[], None);
    assert_eq!(
        plain, optical,
        "`None` must be byte-for-byte identical to the pre-#707 grouped decode"
    );
}

/// #707's actual fix, proven through the FULL wiring (recording.rs → qr.rs), not just the
/// pure gate decision: a frame with only ONE Vernier half decodes in the plain pass still
/// resolves the SAME tick either way (nothing else painted a second value to recover here),
/// but with `min_distinct_optical` requiring 2, the decode must have taken the ROBUST path
/// — proven indirectly via `DecodePath` at the qr.rs layer already; here we confirm the
/// call reaches recording.rs's own decode function without dropping the parameter and still
/// returns a coherent `RecordingFrame` (frame_index/tick correct) through the full chain.
#[test]
fn grouped_burns_optical_requires_both_vernier_halves_through_the_full_wiring() {
    let luma = single_qr_luma(700);
    let f = decode_recording_frame_with_grouped_burns_optical(3, luma, &[], &[], Some((6519, 2)));
    assert_eq!(f.frame_index, 3);
    // Only one Vernier half was ever painted onto this frame, so even the #202 robust
    // tiled retry (correctly attempted, per the qr.rs-level tests) cannot recover a second
    // id that was never there — the resolved tick is still the one real id present. The
    // POINT of this test is that the call compiles/threads through cleanly end-to-end and
    // returns a coherent frame, not a panic or a silently-dropped parameter.
    assert_eq!(f.tick, Some(700));
    assert_eq!(
        f.payloads.len(),
        1,
        "only the one painted half: {:?}",
        f.payloads
    );
}

#[test]
fn tick_is_max_frame_id() {
    // Effective Vernier tick = max(left, right).
    let f = decode_recording_frame(7, dual_qr_luma(200, 201));
    assert_eq!(f.frame_index, 7);
    assert_eq!(f.tick, Some(201));
}

#[test]
fn tick_excludes_node_burns_even_when_a_burn_id_exceeds_the_optical_tick() {
    // #202 regression: with the robust decode recovering node burns on most frames, a
    // burn's frame_id (independent counter, can be LARGER than the optical Vernier tick)
    // must NOT hijack `tick`. Compose a frame whose optical dual-QR maxes at 201 and add
    // a cam1 burn with frame_id 9999 (> 201). `tick` MUST be the optical 201, never 9999.
    use super::NODE_BURN_RUN_IDS as N;
    use crate::probe::qr::render_qr_bgra;
    let (cw, ch) = (960u32, 540u32);
    let l = Payload {
        run_id: 6519,
        frame_id: 200,
        gen_ts_ns: 1,
    };
    let r = Payload {
        run_id: 6519,
        frame_id: 201,
        gen_ts_ns: 2,
    };
    let burn = Payload {
        run_id: N[0], // a cam1 node burn
        frame_id: 9999,
        gen_ts_ns: 3,
    };
    let bgra = render_qr_dual_bgra(&l, &r, cw, ch, 260);
    let mut luma = bgra_to_luma(&bgra, cw, ch, cw * 4);
    // Composite the burn in the bottom-left corner (inside the recovery band).
    let burn_bgra = render_qr_bgra(&burn, 200, 200, 160);
    let burn_luma = bgra_to_luma(&burn_bgra, 200, 200, 200 * 4);
    let (ox, oy) = (20u32, ch - 200 - 20);
    for y in 0..200 {
        for x in 0..200 {
            luma.put_pixel(ox + x, oy + y, *burn_luma.get_pixel(x, y));
        }
    }
    let f = decode_recording_frame(0, luma);
    // The burn IS recovered into payloads (the #202 fix) …
    assert!(
        f.payloads
            .iter()
            .any(|p| p.run_id == N[0] && p.frame_id == 9999),
        "the node burn must be recovered into payloads: {:?}",
        f.payloads
    );
    // … but the Vernier tick stays the OPTICAL max, never the burn's larger id.
    assert_eq!(
        f.tick,
        Some(201),
        "tick must be the optical Vernier tick (201), NOT the burn id 9999: {:?}",
        f.payloads
    );
}

#[test]
fn extract_self_check_is_not_fooled_by_burn_only_payloads_853() {
    // #853 regression: `extract_frames_png`'s `sharp_qr_but_flagged_undecodable` self-check
    // re-decodes an "undecodable" frame's pixels with `decode_qr_luma_all_robust` and used to
    // ask "did ANYTHING decode" — which a frame carrying ONLY node burns (no optical Vernier
    // at all, the exact real-world shape proven on run 1867252327: all 5879 tick==None stream
    // frames had exactly this shape) always answers yes, regardless of whether the cam2
    // optical read ever succeeded. Build exactly that frame — a lone cam1 node burn painted,
    // NO optical dual-QR anywhere — and confirm the FIXED self-check (has_non_burn_payload)
    // correctly says NO, while the raw robust decode is (as expected) non-empty.
    use super::NODE_BURN_RUN_IDS as N;
    use crate::probe::qr::{decode_qr_luma_all_robust, render_qr_bgra};
    let (cw, ch) = (960u32, 540u32);
    let burn = Payload {
        run_id: N[0], // a cam1 node burn — always crisp, always decodable
        frame_id: 42,
        gen_ts_ns: 3,
    };
    let mut luma = GrayImage::new(cw, ch); // all-black canvas: NO optical Vernier painted
    let burn_bgra = render_qr_bgra(&burn, 200, 200, 160);
    let burn_luma = bgra_to_luma(&burn_bgra, 200, 200, 200 * 4);
    let (ox, oy) = (20u32, ch - 200 - 20);
    for y in 0..200 {
        for x in 0..200 {
            luma.put_pixel(ox + x, oy + y, *burn_luma.get_pixel(x, y));
        }
    }

    let recheck = decode_qr_luma_all_robust(luma);
    // Sanity: the burn genuinely decoded (this is NOT a case where nothing was found at all —
    // the pre-#853-fix bug and the real fleet-wide bug both depend on the burn decoding fine).
    assert!(
        recheck.iter().any(|p| p.run_id == N[0]),
        "the node burn must decode: {recheck:?}"
    );
    // The FIXED self-check: a burn-only decode must NOT be reported as a genuine optical read.
    assert!(
        !crate::optical_payload_check::has_non_burn_payload(recheck.iter().map(|p| p.run_id), &N),
        "burn-only payloads must not count as sharp_qr_but_flagged_undecodable: {recheck:?}"
    );
}

#[test]
fn node_burn_run_ids_includes_imag_463() {
    // #463 GOTCHA (caught by CI, not locally): imag's OWN digital corner burn
    // (BURN_RUN_ID_IMAG) must be excluded from the Vernier tick computation exactly like
    // cam1/strih/stream — otherwise a decoded imag burn payload competes with the cam2
    // optical tick in decode_recording_frame_with_burns's max(), silently corrupting
    // imag's zero-loss contiguity check whenever the burn's frame_id exceeds cam2's on a
    // frame. A unit test with artificially large fixture ids first caught this end-to-end
    // in CI (a burn id of 500+ hijacked the "optical" tick every frame); this direct
    // assertion locks the fix so it can never silently regress again.
    assert!(
        NODE_BURN_RUN_IDS.contains(&crate::probe::recording_latency::BURN_RUN_ID_IMAG),
        "BURN_RUN_ID_IMAG must be in NODE_BURN_RUN_IDS so its frame_id never hijacks the \
         Vernier tick (mirrors tick_excludes_node_burns_even_when_a_burn_id_exceeds_the_\
         optical_tick's proof for cam1)"
    );
}

/// #312 (BUG, found in code review before merge): `NODE_BURN_RUN_IDS` was never extended for
/// cam3/cam4 (#624) nor for this PR's new cam2/cam5/cam6 burns — the exact #463 gotcha
/// documented on [`RecordingFrame::tick`] and locked by `node_burn_run_ids_includes_imag_463`
/// above, recurring for five more camera-under-test ids. Any one of them missing here means
/// that camera's own capture-burn frame_id can hijack the cam2 optical Vernier tick whenever
/// it exceeds cam2's on a frame, silently corrupting the ALL-CAMBOX per-segment continuity
/// (`segment_frames_from_recording`) this PR's own items 1+3 depend on.
#[test]
fn node_burn_run_ids_includes_every_camera_under_test_312() {
    use crate::probe::recording_latency::{
        BURN_RUN_ID_CAM2, BURN_RUN_ID_CAM3, BURN_RUN_ID_CAM4, BURN_RUN_ID_CAM5, BURN_RUN_ID_CAM6,
        BURN_RUN_ID_CAM7,
    };
    for (label, id) in [
        ("cam2", BURN_RUN_ID_CAM2),
        ("cam3", BURN_RUN_ID_CAM3),
        ("cam4", BURN_RUN_ID_CAM4),
        ("cam5", BURN_RUN_ID_CAM5),
        ("cam6", BURN_RUN_ID_CAM6),
        ("cam7", BURN_RUN_ID_CAM7),
    ] {
        assert!(
            NODE_BURN_RUN_IDS.contains(&id),
            "#312: BURN_RUN_ID_{} ({id}) must be in NODE_BURN_RUN_IDS so its frame_id never \
             hijacks the Vernier tick",
            label.to_uppercase()
        );
    }
}

#[test]
fn node_burn_run_ids_includes_the_aux_tick_pair_1196() {
    // issue 1196: the painted aux Vernier tick pair (bottom burn-gap QRs) must be
    // tick-EXCLUDED exactly like the digital burns — on a torn or band-corrupted frame its
    // frame_ids carry a DIFFERENT paint generation than the primary pair, and letting them
    // feed the max() would silently shift the undecodable/continuity metrics the strict
    // gates are calibrated on. Only the report-only tear surface reads them, by run_id.
    assert!(
        NODE_BURN_RUN_IDS.contains(&crate::probe::recording_latency::AUX_TICK_RUN_ID),
        "AUX_TICK_RUN_ID must be in NODE_BURN_RUN_IDS so the aux marks never hijack the \
         Vernier tick"
    );
}

#[test]
fn node_burn_run_ids_includes_the_cg_chain_burns_1301() {
    // #1301: the SongPlayer-origin (911014) + cg-OBS-hop (911015) CG-chain burns can ride into
    // a strih/stream recording during a CG_CHAIN run, so both MUST be tick-excluded exactly
    // like the camera/strih/stream/imag burns (the #463/#312 gotcha) — a stray CG frame_id
    // must never hijack the cam2 Vernier tick.
    for (label, id) in [
        (
            "SONGPLAYER",
            crate::probe::recording_latency::BURN_RUN_ID_SONGPLAYER,
        ),
        ("CG", crate::probe::recording_latency::BURN_RUN_ID_CG),
    ] {
        assert!(
            NODE_BURN_RUN_IDS.contains(&id),
            "#1301: BURN_RUN_ID_{label} ({id}) must be in NODE_BURN_RUN_IDS"
        );
    }
}

#[test]
fn node_burn_run_ids_includes_the_measurement_clip_1404() {
    // issue 1404: the measurement clip's painted dual-QR rides into a strih/stream recording
    // during a CG segment; its frame_id is the clip's own tick and must never become the cam2
    // Vernier tick.
    let clip = crate::probe::recording_latency::MEASUREMENT_CLIP_RUN_ID;
    assert_eq!(clip, 911_016);
    assert!(
        NODE_BURN_RUN_IDS.contains(&clip),
        "MEASUREMENT_CLIP_RUN_ID must be in NODE_BURN_RUN_IDS"
    );
}

#[test]
fn blank_frame_decodes_to_zero_and_none_tick() {
    let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
    let f = decode_recording_frame(3, blank);
    assert!(f.payloads.is_empty(), "no QR in a blank frame");
    assert_eq!(f.tick, None);
}

#[test]
fn deterministic_same_frame_same_result() {
    // STRICT acceptance: same frame → same result.
    let a = decode_recording_frame(0, dual_qr_luma(10, 11));
    let b = decode_recording_frame(0, dual_qr_luma(10, 11));
    assert_eq!(a, b);
}

// ---- #166: parallel decode (order-preserving across the worker pool) ----

// #423: these three tests pass `&[]` (no expected node burns) instead of
// `&NODE_BURN_RUN_IDS`. `dual_qr_luma` renders ONLY the optical dual-QR (run_id
// 6519) — it never carries a cam1/strih/stream node burn — so requiring the full
// `NODE_BURN_RUN_IDS` set meant `decode_qr_luma_all_fast_then_robust`'s
// `all_burns_present` check could NEVER pass, and EVERY synthetic frame silently
// fell through to `robust_tile_passes` (the ~10×-cost bottom-band tile
// crop+upscale+re-decode, #202/#207) chasing burns that structurally cannot ever
// be found. That gratuitous robust-path tax on every frame — not real fixture
// size or genuine contention — was the dominant cost behind the >300s CI
// timeout (#416/#423): these tests exist to prove parallel/serial ORDERING and
// EQUIVALENCE, not burn-recovery (that is `burn_fixture_decode.rs`'s job, on
// real hard fixtures where the ~10× cost is the point). `&[]` makes
// `all_burns_present` vacuously true, so both sides take the cheap plain+Otsu
// fast path — the exact behavior a burn-free (pure-optical) recording gets in
// production — while still exercising the real rqrr encode→decode roundtrip and
// the real worker-pool threading this test is actually about.
#[test]
fn parallel_decode_preserves_capture_order_and_decodes_every_frame() {
    // The parallel decoder fans frames across N workers that complete OUT of
    // order, then re-sorts by frame_index. With >1 worker, a sequence of real
    // dual-QR frames must come back in EXACT capture order with EVERY frame
    // decoded — proving the reorder logic, not just that it ran. (The single
    // -threaded loop trivially preserved order; this is the regression guard
    // that the parallelization did not corrupt it.) n=16 comfortably exceeds the
    // workers*2=8 bounded job-channel capacity (so the producer must block and
    // refill it at least once — the backpressure/reorder path is genuinely
    // exercised), without paying for dozens of needless frames.
    let n: u64 = 16;
    let frames = decode_stream_parallel(4, &[], &[], None, |emit| {
        for i in 0..n {
            // left = even tick 2i, right = odd tick 2i+1 → tick = 2i+1, unique
            // per frame so an order bug is detectable.
            emit(i, dual_qr_luma(2 * i as u32, 2 * i as u32 + 1));
        }
        Ok(n)
    })
    .expect("parallel decode");

    assert_eq!(frames.len() as u64, n, "every frame returned");
    for (i, f) in frames.iter().enumerate() {
        assert_eq!(f.frame_index, i as u64, "frames in capture order at {i}");
        assert_eq!(
            f.tick,
            Some(2 * i as u32 + 1),
            "frame {i} decoded its own ticks"
        );
        assert_eq!(f.payloads.len(), 2, "both QRs decoded for frame {i}");
    }
}

#[test]
fn parallel_decode_matches_single_threaded_result_exactly() {
    // Byte-for-byte equivalence with the serial reference for the SAME input —
    // the parallelization must not change the decode result, only its speed.
    // n=16 still crosses the workers*2=8 job-channel bound for the workers=4
    // case (see the module note above for why `&[]`/no expected burns).
    let n: u64 = 16;
    let make = |i: u64| dual_qr_luma(7 * i as u32, 7 * i as u32 + 1);

    let serial: Vec<RecordingFrame> = (0..n)
        .map(|i| decode_recording_frame_with_burns(i, make(i), &[]))
        .collect();

    // workers=1 and workers=4 must both equal the serial reference. Both sides use
    // the SAME (empty) burn set, so the #207 fast/robust gate is identical on each
    // path — the comparison is purely about ordering/parallelism, not the decode
    // path.
    for w in [1usize, 4] {
        let par = decode_stream_parallel(w, &[], &[], None, |emit| {
            for i in 0..n {
                emit(i, make(i));
            }
            Ok(n)
        })
        .unwrap();
        assert_eq!(
            par.len(),
            serial.len(),
            "parallel (workers={w}) returns the same frame count as single-threaded"
        );
        assert_eq!(par, serial, "parallel (workers={w}) == single-threaded");
    }
}

#[test]
fn parallel_decode_propagates_producer_error() {
    // A producer (ffmpeg/ffprobe) error must surface as an Err, not a silent
    // short read — otherwise a truncated decode could be read as "0 loss". The
    // burn set is irrelevant to error propagation; `&[]` keeps the one decoded
    // frame on the cheap fast path (see the module note above).
    let r = decode_stream_parallel(4, &[], &[], None, |emit| {
        emit(0, dual_qr_luma(0, 1));
        anyhow::bail!("simulated ffmpeg failure");
    });
    assert!(r.is_err(), "producer error propagates");
}

// ---- #166: pixel-proof PNG cap (pure policy) ----

#[test]
fn select_frames_caps_to_first_n_and_reports_dropped() {
    let flagged: Vec<u64> = (0..100).collect();
    let (sel, dropped) = select_frames_to_extract(&flagged, 30);
    assert_eq!(sel.len(), 30, "capped to N");
    assert_eq!(sel, (0..30).collect::<Vec<u64>>(), "first N by index");
    assert_eq!(dropped, 70, "dropped count = total - N");
}

#[test]
fn select_frames_no_cap_when_under_limit_or_zero() {
    let flagged = vec![5u64, 1, 9, 3];
    // Under the limit: all kept (sorted, deduped), 0 dropped.
    let (sel, dropped) = select_frames_to_extract(&flagged, 30);
    assert_eq!(sel, vec![1, 3, 5, 9]);
    assert_eq!(dropped, 0);
    // max=0 means "no cap": all kept even when large.
    let big: Vec<u64> = (0..500).collect();
    let (sel0, dropped0) = select_frames_to_extract(&big, 0);
    assert_eq!(sel0.len(), 500);
    assert_eq!(dropped0, 0);
}

#[test]
fn select_frames_dedups_before_capping() {
    // Duplicate flagged indices must not consume cap slots twice.
    let flagged = vec![1u64, 1, 2, 2, 3, 3, 4, 4];
    let (sel, dropped) = select_frames_to_extract(&flagged, 3);
    assert_eq!(sel, vec![1, 2, 3]);
    assert_eq!(dropped, 1, "4 unique - 3 cap = 1 dropped");
}

// ---- #187: bound the parallel-decode peak memory so a big 4K recording never OOMs ----

#[test]
fn mem_budget_caps_workers_on_a_small_box_with_a_4k_frame() {
    // #187 BUG: the parallel decode picked `workers = min(cpus, 8)` with NO memory
    // bound, so a full multi-recording verdict run on a tight box — every worker
    // holding source luma + clone + rqrr's prepared pixels (≈DECODE_PEAK_BYTES_PER_PIXEL
    // × area) on a 3840×2160 frame, alongside ffmpeg + the grab buffers — blew past free
    // RAM and got OOM-killed (EXIT=137) mid-decode, aborting the whole verdict. The fix
    // caps workers by an available-memory budget: when free RAM during the run is tight,
    // the CPU count must be throttled DOWN, never used as-is.
    let cpu_workers = 8;
    let (w, h) = (3840u32, 2160u32);
    let per_worker = DECODE_PEAK_BYTES_PER_PIXEL as u64 * (w as u64) * (h as u64);
    // Choose a genuinely TIGHT available figure so the budget (half of it) cannot hold
    // all 8 workers: budget must be < 8 × per_worker. With per_worker ≈ 47.5 MB for 4K,
    // 8 workers need ≈ 380 MB of budget ⇒ ≈ 760 MB available. 600 MB is below that, the
    // "barely any free RAM left during a heavy run" case the OOM happened in.
    let avail = 600_000_000u64;
    let budget = (avail as f64 * MEM_BUDGET_FRACTION) as u64;
    let expected_max = (budget / per_worker).max(1) as usize;
    assert!(
        expected_max < cpu_workers,
        "test premise: at {avail} B avail the budget ({budget}) must NOT hold all \
         {cpu_workers} 4K workers (per_worker={per_worker}, cap={expected_max})"
    );
    let got = workers_within_mem_budget(cpu_workers, w, h, avail);
    assert!(got >= 1, "always at least one worker (forward progress)");
    assert_eq!(
        got, expected_max,
        "workers must be throttled to the memory budget ({expected_max} for 4K \
         @ {avail} B avail), not the {cpu_workers} CPU count"
    );
    assert!(
        got < cpu_workers,
        "a 4K frame on a {avail}-byte box must throttle below the {cpu_workers} CPU \
         workers (got {got}) — this is the #187 OOM fix"
    );
}

#[test]
fn mem_budget_keeps_all_cpus_when_ram_is_ample() {
    // A roomy box (e.g. a 64 GB CI runner) with a 4K frame must NOT be throttled —
    // the memory bound only kicks in when RAM is the binding constraint, so the #166
    // parallel speedup is preserved everywhere it is safe.
    let cpu_workers = 8;
    let got = workers_within_mem_budget(cpu_workers, 3840, 2160, 64_000_000_000);
    assert_eq!(got, cpu_workers, "ample RAM → keep all CPU workers");
}

#[test]
fn mem_budget_always_makes_forward_progress() {
    // Pathological: almost no memory reported. We must still return at least ONE
    // worker (a hung 0-worker pool would never decode) — bounded, not dead.
    assert_eq!(
        workers_within_mem_budget(8, 3840, 2160, 1),
        1,
        "near-zero memory still yields exactly one worker"
    );
    // Degenerate frame dims must not divide-by-zero / panic.
    assert_eq!(workers_within_mem_budget(8, 0, 0, 2_500_000_000), 8);
}

#[test]
fn mem_budget_small_frame_is_never_throttled() {
    // A small (downscaled / SD) frame is cheap, so even a tight box keeps all CPUs.
    let got = workers_within_mem_budget(8, 640, 480, 2_500_000_000);
    assert_eq!(got, 8, "a 640×480 frame never hits the memory bound");
}
