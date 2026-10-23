//! Issue 1439 phase 2 -- the strih OBS receives the FOH console's `strih-obs` VBAN stream (comment
//! 6361250274, read on strih-snv's VLAN-70 leg): **96 kHz**, 239 frames per packet (2.49 ms), 2 ch, PCM24,
//! about 400 packets a second, into the obs-vban source with strih-snv's low Receive Buffer (20 ms).
//!
//! The shipped `vban-source.c` (lifted into the C driver, `tests/support/vban_source_driver_1439.c`, the
//! VBAN rate index set with `sr=4`) must hand OBS that stream at its own rate: every output at 96 kHz,
//! the stamps contiguous on the 96 kHz sample clock, the 24-bit samples whole, and a lost packet between
//! two received ones one concealed 239-frame slot.
//!
//! Standalone, no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_source_96k_1439.rs -o /tmp/t && /tmp/t
//! ```

// the harness serves several test crates; this one reads only part of it
#[allow(dead_code)]
#[path = "support/vban_source_harness_1439.rs"]
mod harness;

use harness::*;

const RATE_96K: u64 = 96_000;

fn ns96(samples: u64) -> u64 {
    samples * 1_000_000_000 / RATE_96K
}

/// One `out` line of the driver: (ts, frames, zero, rate, first24).
fn outs(stdout: &str) -> Vec<(u64, u64, bool, u64, i64)> {
    stdout
        .lines()
        .filter_map(|l| l.strip_prefix("out "))
        .map(|l| {
            let f = |k: &str| -> &str {
                l.split_whitespace()
                    .find_map(|x| x.strip_prefix(k))
                    .unwrap_or_else(|| panic!("no `{k}` in `{l}`"))
            };
            (
                f("ts=").parse().unwrap(),
                f("frames=").parse().unwrap(),
                f("zero=") == "1",
                f("rate=").parse().unwrap(),
                f("first24=").parse().unwrap(),
            )
        })
        .collect()
}

#[test]
fn the_consoles_96_khz_239_frame_pcm24_stream_reaches_obs_at_96_khz_1439() {
    let frames = 239u64;
    let t0 = 1_000_000_000u64;
    let values: [i16; 5] = [1234, -1234, 32767, -32768, 0];
    let low = 0x5A;
    // strih-snv's fact: R 20 ms; the VBAN rate index 4 = 96 kHz (vban.h VBanSRList)
    let mut tokens = vec![
        "buffer=20".to_string(),
        "sr=4".to_string(),
        format!("low24={low}"),
    ];
    // the anchor's first second is held: packet k ends at (k + 1) x 239 on the 96 kHz clock
    let held = RATE_96K / frames;
    tokens.extend((0..held).map(|k| format!("{}:{}:999", 700 + k, t0 + ns96(k * frames))));
    tokens.extend([0u64, 1, 3, 4, 5].iter().zip(values).map(|(&k, v)| {
        format!(
            "{}:{}:{}",
            700 + held + k,
            t0 + ns96((held + k) * frames),
            v as u16
        )
    }));
    let (stdout, log) = run_tokens_frames(&tokens, frames as u32, VBAN_BITFMT_24_INT);
    let outs = outs(&stdout);
    assert!(!outs.is_empty(), "issue 1439: nothing reached OBS: {stdout}\n{log}");
    assert!(
        log.contains("rate=96000"),
        "issue 1439: the source anchors the stream at 96 kHz: {log}"
    );
    assert!(
        outs.iter().all(|o| o.3 == RATE_96K),
        "issue 1439: every output (the lead-in and the concealed slot included) at the stream's 96 kHz: {outs:?}"
    );
    for w in outs.windows(2) {
        let expect = w[0].0 + ns96(w[0].1);
        assert!(
            w[1].0.abs_diff(expect) <= 2,
            "issue 1439: the stamps are contiguous on the 96 kHz clock: {outs:?}"
        );
    }
    // the lead-in: R - one 239-frame packet (2.49 ms) - the 2 ms deadline margin on the 96 kHz clock
    let n = outs.iter().position(|o| !o.2).expect("an audio output");
    let lead_in: u64 = outs[..n].iter().map(|o| o.1).sum();
    let want = (20.0 - frames as f64 * 1000.0 / RATE_96K as f64 - 2.0) * 96.0;
    assert!(
        (lead_in as f64 - want).abs() <= 2.0,
        "issue 1439: the lead-in is R on the 96 kHz clock ({lead_in} frames, about {want:.0}): {outs:?}"
    );
    let outs = outs[n..].to_vec();
    assert_eq!(outs.len(), 6, "five packets + one concealed slot: {outs:?}");
    assert!(outs.iter().all(|o| o.1 == frames), "{outs:?}");
    assert!(
        !outs[2].2 && outs[2].4 == outs[1].4,
        "issue 1439: the lost packet is one slot of the previous packet: {outs:?}"
    );
    let raw: Vec<i64> = outs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2)
        .map(|(_, o)| o.4)
        .collect();
    assert_eq!(
        raw,
        values
            .iter()
            .map(|&v| i64::from(v) * 256 + low)
            .collect::<Vec<_>>(),
        "issue 1439: every 24-bit sample reaches OBS whole: {outs:?}"
    );
}
