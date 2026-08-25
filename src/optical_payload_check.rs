//! #853 — is a decoded QR payload list evidence of a genuine OPTICAL (non-burn) read, or only the
//! easy, always-present digital node burns? Pure predicate (no `Payload`/probe-feature dependency),
//! so it Tier-0 unit-tests without `--features probe`; `probe::recording::extract_frames_png`'s
//! `sharp_qr_but_flagged_undecodable` self-check calls this with the decoded payloads' `run_id`s
//! and `probe::recording::NODE_BURN_RUN_IDS`, instead of asking "did ANY QR decode" — which is
//! guaranteed true on every #853 fleet-wide `undecodable` frame purely from the always-crisp node
//! burns (proven on run 1867252327: all 5879 `tick == None` stream frames carried exactly the 3
//! node burns and ZERO optical payload) and so proves NOTHING about the cam2 optical Vernier the
//! `undecodable` count actually measures (`RecordingFrame::tick` excludes node burns by design —
//! see that field's own doc). Mirrors `tick`'s own burn-exclusion filter exactly, so the self-check
//! and the real count can never again disagree about what "found something" means.

/// True when `run_ids` contains at least one id NOT present in `burn_run_ids` — i.e. a genuine
/// non-burn (optical) QR payload decoded, as opposed to only the crisp, always-decodable digital
/// node burns. `run_ids` may be empty or contain duplicates; both are handled correctly (empty ⇒
/// `false`, since there is nothing non-burn to find).
pub fn has_non_burn_payload(run_ids: impl IntoIterator<Item = u32>, burn_run_ids: &[u32]) -> bool {
    run_ids.into_iter().any(|id| !burn_run_ids.contains(&id))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact fixed node-burn ids from `probe::recording::NODE_BURN_RUN_IDS` (mirrored here so
    // this module stays probe-feature-free; a value drift would only ever make this test SUITE
    // stricter, never silently pass — the ids are load-bearing constants, not tunables).
    // 911_013 is the issue-1196 aux Vernier tick pair (AUX_TICK_RUN_ID): painted, not a burn,
    // but tick-excluded exactly like the burns — so it appears on stream frames and must NOT
    // count as "a genuine primary optical read" either.
    const BURNS: [u32; 4] = [911_001, 911_002, 911_004, 911_013];

    #[test]
    fn burn_only_run_ids_is_not_a_non_burn_payload() {
        // #853: the exact real-world shape — every one of run 1867252327's 5879 "undecodable"
        // stream frames carried ONLY these node burns (911002 strih, 911004 stream, plus the
        // active camera's own burn) and ZERO optical payload. The self-check must say NO — this
        // is precisely the case the pre-fix "did anything decode" check got wrong.
        assert!(!has_non_burn_payload([911_002, 911_004, 911_001], &BURNS));
    }

    #[test]
    fn empty_run_ids_is_not_a_non_burn_payload() {
        assert!(!has_non_burn_payload(std::iter::empty(), &BURNS));
    }

    #[test]
    fn a_genuine_optical_run_id_is_a_non_burn_payload() {
        // The cam2 Vernier's run_id is the harness's per-run RUN_ID (e.g. 1867252327 on the real
        // run this bug was found on) — never one of the fixed node-burn ids.
        assert!(has_non_burn_payload(
            [911_002, 911_004, 1_867_252_327],
            &BURNS
        ));
    }

    #[test]
    fn a_non_burn_id_alone_is_a_non_burn_payload() {
        assert!(has_non_burn_payload([42], &BURNS));
    }

    #[test]
    fn burns_plus_aux_tick_marks_only_is_not_a_non_burn_payload_1196() {
        // issue 1196: a frame whose PRIMARY dual-QR is corrupted while the aux tick pair
        // (911013) and the node burns decode is still an `undecodable` frame for the tick — the
        // self-check must agree (mirroring the tick filter), not report a false "sharp optical
        // read". The aux-alive-primary-dark shape is surfaced separately, report-only, by the
        // tear detector's discriminator fraction.
        assert!(!has_non_burn_payload(
            [911_002, 911_004, 911_013, 911_013],
            &BURNS
        ));
    }

    #[test]
    fn duplicates_do_not_change_the_answer() {
        assert!(!has_non_burn_payload(
            [911_001, 911_001, 911_002, 911_002],
            &BURNS
        ));
        assert!(has_non_burn_payload([1, 1, 2, 2], &BURNS));
    }
}
