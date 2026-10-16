//! Issue 1425 (E2 slice 1, design 6205974120) — an EXECUTABLE C-vs-Rust parity gate for the ASRC
//! basis of the per-source ASRC: `genlock_audio_asrc_placement` (arrival / timecode / stamp), the
//! measure rule `genlock_audio_place_measured` and the `asrc:` line token
//! `genlock_audio_asrc_basis_token`, lifted VERBATIM out of the contiguous `genlock_audio_*` block of
//! `vendor/obs-studio/libobs/obs-source.c` (the shared `tests/genlock_audio_pairing_lift/mod.rs`),
//! compiled under `-Wall -Wextra -Wconversion -Wformat=2 -Werror` and compared with
//! [`camera_box::genlock_audio_pairing`] on every input. Its own file: the issue-1303 parity file is
//! at the line budget. FAILS LOUDLY (never skips) when the C toolchain is missing.

use camera_box::genlock_audio_pairing::{
    audio_asrc_placement, audio_asrc_rate_point, audio_place_measured, AsrcBasis, AsrcRatePending,
    AudioHoldMode,
};
// The lift / compile / run helpers are shared with the other audio-pairing parity gates; a directory
// module, so cargo does not build it as a test target of its own.
mod genlock_audio_pairing_lift;
use genlock_audio_pairing_lift::run_c;

fn mode_of(c: u8) -> AudioHoldMode {
    match c {
        0 => AudioHoldMode::Off,
        1 => AudioHoldMode::Latency,
        2 => AudioHoldMode::Timecode,
        _ => AudioHoldMode::Pending,
    }
}

/// Issue 1425 (E2 slice 1) — the ASRC basis (`genlock_audio_asrc_placement`: arrival / timecode /
/// stamp), the measure rule (`genlock_audio_place_measured`) and the `asrc:` line token
/// (`genlock_audio_asrc_basis_token`) must match the Rust authority on every hold mode x direct
/// stamps x MONITOR_ONLY x resampler, and every mode x basis.
#[test]
fn c_asrc_placement_matches_the_rust_authority_1425() {
    let b = |v: bool| i32::from(v);
    let flags = [false, true];
    let mut body = String::new();
    let mut want: Vec<String> = Vec::new();
    for m in 0..4_u8 {
        for direct in flags {
            for mo in flags {
                for rs in flags {
                    body.push_str(&format!(
                        "    printf(\"%d\\n\", genlock_audio_asrc_placement({m}, {}, {}, {}));\n",
                        b(direct),
                        b(mo),
                        b(rs)
                    ));
                    want.push(
                        audio_asrc_placement(mode_of(m), direct, mo, rs)
                            .code()
                            .to_string(),
                    );
                }
            }
        }
    }
    let bases = [AsrcBasis::Arrival, AsrcBasis::Timecode, AsrcBasis::Stamp];
    for m in 0..4_u8 {
        for basis in bases {
            body.push_str(&format!(
                "    printf(\"%d\\n\", genlock_audio_place_measured({m}, {}) ? 1 : 0);\n",
                basis.code()
            ));
            want.push(b(audio_place_measured(mode_of(m), basis)).to_string());
        }
    }
    for basis in bases {
        body.push_str(&format!(
            "    printf(\"%s\\n\", genlock_audio_asrc_basis_token({}));\n",
            basis.code()
        ));
        want.push(basis.token().to_string());
    }
    let out = run_c(&body, "asrc_placement_1425");
    assert_eq!(
        out, want,
        "issue 1425: the C ASRC basis / measure rule / token diverged from the Rust authority"
    );
    // the decided design, not only C == Rust: a directly stamped mixed source with a resampler is
    // stamp-judged and measured; a timecode hold keeps its basis; everything else is arrival.
    assert_eq!(
        audio_asrc_placement(AudioHoldMode::Off, true, false, true),
        AsrcBasis::Stamp
    );
    assert_eq!(
        audio_asrc_placement(AudioHoldMode::Timecode, false, false, true),
        AsrcBasis::Timecode
    );
    assert_eq!(
        audio_asrc_placement(AudioHoldMode::Off, true, true, true),
        AsrcBasis::Arrival
    );
    assert_eq!(
        audio_asrc_placement(AudioHoldMode::Latency, true, false, true),
        AsrcBasis::Arrival
    );
    assert!(audio_place_measured(AudioHoldMode::Off, AsrcBasis::Stamp));
    assert!(!audio_place_measured(
        AudioHoldMode::Off,
        AsrcBasis::Arrival
    ));
}

/// Issue 1425 round 2 (design 6206822792) — the rate point of each measured packet
/// (`genlock_audio_asrc_rate_point`: the TIMECODE basis drops a non-positive stamp advance, the STAMP
/// basis waits one packet and carries a packet with no advance into the waiting point) must match the
/// Rust authority step for step, the waiting point included, over a packet script that covers every
/// branch: a first advance with nothing waiting, an emitted point, equal and backward stamps carried,
/// a non-contiguous packet dropping the waiting point, a run with nothing waiting, and a basis change.
#[test]
fn c_asrc_rate_point_matches_the_rust_authority_1425() {
    let p = 128.0 / 48000.0;
    let q = 1024.0 / 48000.0;
    // (basis, contiguous, master_s, prev_raw_s)
    let script: &[(AsrcBasis, bool, f64, f64)] = &[
        (AsrcBasis::Stamp, false, 0.0, 0.0),
        (AsrcBasis::Stamp, true, p, p),
        (AsrcBasis::Stamp, true, 1.5 * p, p),
        (AsrcBasis::Stamp, true, 0.0, p),
        (AsrcBasis::Stamp, true, -0.000001, p),
        (AsrcBasis::Stamp, true, 0.0, q),
        (AsrcBasis::Stamp, true, 0.5 * p, p),
        (AsrcBasis::Stamp, true, p, q),
        (AsrcBasis::Stamp, false, 2.0 * p, p),
        (AsrcBasis::Stamp, true, 0.0, p),
        (AsrcBasis::Stamp, true, p, p),
        (AsrcBasis::Stamp, true, 0.0, p),
        (AsrcBasis::Timecode, true, p, p),
        (AsrcBasis::Timecode, true, 0.0, p),
        (AsrcBasis::Timecode, true, -p, p),
        (AsrcBasis::Timecode, false, p, p),
        (AsrcBasis::Stamp, true, p, q),
        (AsrcBasis::Stamp, true, q, q),
        (AsrcBasis::Arrival, true, q, q),
        (AsrcBasis::Stamp, true, q, q),
    ];
    let mut body = String::from(
        "    double pend_raw_s = 0.0, pend_master_s = 0.0;\n    bool pend_due = false;\n    struct genlock_audio_rate_point pt;\n",
    );
    let mut want = Vec::new();
    let mut pending = AsrcRatePending::default();
    for &(basis, contiguous, master_s, prev_raw_s) in script {
        body.push_str(&format!(
            "    pt = genlock_audio_asrc_rate_point({}, {}, {master_s:.17e}, {prev_raw_s:.17e}, &pend_raw_s, &pend_master_s, &pend_due);\n    printf(\"%d %.12f %.12f %d | %.12f %.12f %d\\n\", pt.due ? 1 : 0, pt.raw_s, pt.master_s, pt.keep_prev_stamp ? 1 : 0, pend_raw_s, pend_master_s, pend_due ? 1 : 0);\n",
            basis.code(),
            u8::from(contiguous)
        ));
        let pt = audio_asrc_rate_point(basis, contiguous, master_s, prev_raw_s, &mut pending);
        want.push(format!(
            "{} {:.12} {:.12} {} | {:.12} {:.12} {}",
            u8::from(pt.due),
            pt.raw_s,
            pt.master_s,
            u8::from(pt.keep_prev_stamp),
            pending.raw_s,
            pending.master_s,
            u8::from(pending.due)
        ));
    }
    let out = run_c(&body, "asrc_rate_point_1425");
    assert_eq!(
        out, want,
        "issue 1425: the C rate point diverged from the Rust authority"
    );
    // the decided design, not only C == Rust: the stamp basis emitted the waiting point carrying the
    // equal and backward stamps' samples (p + p + p + q over 1.5 p), and the timecode basis dropped
    // its duplicated slot
    let emitted = |line: &String| line.starts_with("1 ");
    assert!(
        emitted(&want[6]) && !emitted(&want[3]) && !emitted(&want[4]) && !emitted(&want[5]),
        "issue 1425: the stamp basis waits through a run with no advance and emits on the next one"
    );
    assert!(
        want[6].starts_with(&format!("1 {:.12} {:.12}", 3.0 * p + q, 1.5 * p)),
        "issue 1425: the waiting point carries the run: {}",
        want[6]
    );
    assert!(
        emitted(&want[12]) && !emitted(&want[13]) && !emitted(&want[14]),
        "issue 1425: the timecode basis emits at once and drops a duplicated slot"
    );
}
