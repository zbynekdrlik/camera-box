//! Issue 1425 (E3 slice S4, design 6225701358) — the unit tests of the ARRIVAL rebase: a locked
//! arrival-basis source whose stamps drifted 70 ms or more from its own continuation continues the
//! timeline instead of being re-placed by its stamps. A `#[path]` child of `genlock_audio_pairing`
//! (split to keep each file under the ~1000-line budget). The C port is held equal by
//! `tests/genlock_audio_asrc_basis_parity_1425.rs`; the behaviour through OBS's ingest by
//! `tests/genlock_audio_arrival_rebase_1425.rs` and the bench `src/asrc_arrival_rebase_bench.rs`.

use super::*;

const AT: u64 = AUDIO_TS_SMOOTHING_NS;

#[test]
fn the_threshold_is_obs_ts_smoothing_threshold_1425() {
    // the 70 ms of obs-source.c's TS_SMOOTHING_THRESHOLD (the C copy is _Static_assert-ed to it)
    assert_eq!(AUDIO_TS_SMOOTHING_NS, 70_000_000);
}

#[test]
fn a_locked_mixed_arrival_source_rebases_from_70_ms_on_1425() {
    let locked = |diff| audio_arrival_rebase(AsrcBasis::Arrival, false, true, true, diff);
    assert!(
        !locked(0),
        "issue 1425: a packet on its continuation needs no rebase"
    );
    assert!(
        !locked(AT - 1),
        "issue 1425: under 70 ms OBS snaps the packet onto its continuation anyway"
    );
    assert!(
        locked(AT),
        "issue 1425: at exactly 70 ms OBS would PLACE the packet: the locked arrival source rebases"
    );
    assert!(locked(AT + 1));
    assert!(
        locked(3_000_000_000),
        "issue 1425: past 2 s OBS would drop the buffer and re-latch timing_adjust: rebases"
    );
    assert!(locked(u64::MAX));
}

#[test]
fn every_other_source_keeps_obs_own_path_1425() {
    for diff in [AT, AT + 1, 3_000_000_000] {
        for basis in [AsrcBasis::Timecode, AsrcBasis::Stamp] {
            assert!(
                !audio_arrival_rebase(basis, false, true, true, diff),
                "issue 1425: a {} source is judged by its stamps: never rebased",
                basis.token()
            );
        }
        assert!(
            !audio_arrival_rebase(AsrcBasis::Arrival, false, true, false, diff),
            "issue 1425: an unlocked arrival source (the launch alignment) keeps OBS's placement"
        );
        assert!(
            !audio_arrival_rebase(AsrcBasis::Arrival, true, true, true, diff),
            "issue 1425: a listen-only source never enters the mix: OBS's own path"
        );
        assert!(
            !audio_arrival_rebase(AsrcBasis::Arrival, false, false, true, diff),
            "issue 1425: a source without a resampler has no arrival servo: OBS's own path"
        );
    }
}

#[test]
fn the_ingest_predicts_arrival_for_exactly_the_rebased_modes_1425() {
    // the ingest passes the basis genlock_audio_asrc_placement decides for this packet: an Off hold
    // on non-direct stamps and a Latency hold are arrival; a direct-stamped Off source is stamp, a
    // timecode hold timecode
    let rebased = |mode, direct| {
        audio_arrival_rebase(
            audio_asrc_placement(mode, direct, false, true),
            false,
            true,
            true,
            AT,
        )
    };
    assert!(rebased(AudioHoldMode::Off, false));
    assert!(rebased(AudioHoldMode::Latency, false));
    assert!(rebased(AudioHoldMode::Latency, true));
    assert!(!rebased(AudioHoldMode::Off, true));
    assert!(!rebased(AudioHoldMode::Timecode, false));
}
