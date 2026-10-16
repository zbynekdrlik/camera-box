//! Issue 1425 (E2 slice 1, design 6205974120) — an EXECUTABLE C-vs-Rust parity gate for the ASRC
//! basis of the per-source ASRC: `genlock_audio_asrc_placement` (arrival / timecode / stamp), the
//! measure rule `genlock_audio_place_measured` and the `asrc:` line token
//! `genlock_audio_asrc_basis_token`, lifted VERBATIM out of the contiguous `genlock_audio_*` block of
//! `vendor/obs-studio/libobs/obs-source.c` (the shared `tests/genlock_audio_pairing_lift/mod.rs`),
//! compiled under `-Wall -Wextra -Wconversion -Wformat=2 -Werror` and compared with
//! [`camera_box::genlock_audio_pairing`] on every input. Its own file: the issue-1303 parity file is
//! at the line budget. FAILS LOUDLY (never skips) when the C toolchain is missing.

use camera_box::genlock_audio_pairing::{
    audio_asrc_placement, audio_place_measured, AsrcBasis, AudioHoldMode,
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
