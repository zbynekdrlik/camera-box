//! Issue 1439 -- ROZHODNUTÉ 6352253147 item (b) in the SHIPPED source: a run of up to three lost
//! packets between two received ones is CONCEALED -- the previous packet's samples, faded in from
//! the last sample handed out and (the run's last slot) out toward the packet that ends the run --
//! instead of silence; a run of four or more stays silence. Timing is unchanged: a concealed slot
//! has the slot's frames and stamp.
//!
//! Live on stream-snv (23.10.2026): the network loses mbc's packets in bursts of 1 to 3 (a 5-min
//! wire capture); each lost slot was 5 ms of zeros in the program, a click.
//!
//! The same lift-and-run driver as `tests/vban_source_loss_1439.rs`. Standalone, no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_source_conceal_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required; a missing compiler FAILS, never skips.

#[allow(dead_code)]
#[path = "support/vban_source_harness_1439.rs"]
mod harness;

use harness::*;

/// The packets 300 .. 315 behind a held first second with `lost` missing, the ones before the gap
/// carrying 1000, the ones after it 3000 (the held ones never reach OBS).
fn around_a_gap(lost: &[u32]) -> Vec<(u32, u64, i16)> {
    let nus: Vec<u32> = (300..=315).filter(|n| !lost.contains(n)).collect();
    let gap = lost[0];
    after_hold(&nus)
        .into_iter()
        .map(|(nu, at, v)| {
            (
                nu,
                at,
                if nu < 300 {
                    v
                } else if nu < gap {
                    1000
                } else {
                    3000
                },
            )
        })
        .collect()
}

/// ROZHODNUTÉ 6352253147 (b): runs of one, two and three lost packets are concealed: every slot of
/// the run is the previous packet (1000) -- faded in from the last sample handed out, which equals
/// it -- and only the run's last slot fades toward the next packet (3000), ending within a sample of
/// it. One loss line per run says so. The stamps stay contiguous.
#[test]
fn a_run_of_up_to_three_lost_packets_is_concealed_toward_the_next_1439() {
    for run in 1..=3u32 {
        let lost: Vec<u32> = (305..305 + run).collect();
        let (outs, log) = run_with_log(&around_a_gap(&lost));
        let body = after_lead_in(&outs);
        assert!(
            body.iter().all(|o| !o.zero),
            "issue 1439: run {run}: no slot goes out silent: {outs:?}"
        );
        assert_eq!(
            body.len(),
            16,
            "issue 1439: run {run}: one output per slot: {outs:?}"
        );
        let gap = &body[5..5 + run as usize];
        for (i, o) in gap.iter().enumerate() {
            let last_of_run = i + 1 == run as usize;
            assert_eq!(
                (o.frames, o.first, o.mid),
                (256, 1000, 1000),
                "issue 1439: run {run}, slot {i}: the previous packet again: {outs:?}"
            );
            if last_of_run {
                assert!(
                    (2990..=3000).contains(&o.last),
                    "issue 1439: run {run}: the run's last slot ends next to the packet after it: \
                     {outs:?}"
                );
            } else {
                assert_eq!(o.last, 1000, "issue 1439: run {run}, slot {i}: {outs:?}");
            }
        }
        assert_eq!(body[5 + run as usize].first, 3000, "{outs:?}");
        assert_contiguous(&outs);
        let lines: Vec<&str> = log.lines().filter(|l| l.contains(": missing ")).collect();
        assert_eq!(
            lines.len(),
            1,
            "issue 1439: run {run}: one loss line:\n{log}"
        );
        assert!(
            lines[0].ends_with(&format!(
                "missing {run} packet(s) before frame {} -- concealed: the previous packet \
                 repeated, crossfaded",
                305 + run
            )),
            "issue 1439: run {run}: `{}`",
            lines[0]
        );
    }
}

/// A concealed slot keeps the stream's own sample format (libobs resets the resampler and the
/// genlock ASRC at every format change) and carries the previous packet's value in every format.
#[test]
fn a_concealed_slot_keeps_the_streams_sample_format_1439() {
    for (bitfmt, name) in [
        (VBAN_BITFMT_8_INT, "8-bit"),
        (VBAN_BITFMT_16_INT, "16-bit"),
        (VBAN_BITFMT_24_INT, "24-bit"),
        (VBAN_BITFMT_32_INT, "32-bit int"),
        (VBAN_BITFMT_32_FLOAT, "32-bit float"),
    ] {
        let (outs, _log) = run_as(&after_hold(&[300, 301, 303, 304]), bitfmt);
        let body = after_lead_in(&outs);
        assert_eq!(
            body.len(),
            5,
            "{name}: four packets + one concealed slot: {outs:?}"
        );
        assert!(
            outs.iter().all(|o| o.format == outs[0].format),
            "issue 1439: {name}: one sample format for every output: {outs:?}"
        );
        assert_eq!(
            body.iter().map(|o| (o.zero, o.first)).collect::<Vec<_>>(),
            vec![(false, 1), (false, 2), (false, 2), (false, 4), (false, 5)],
            "issue 1439: {name}: the lost slot carries the previous packet: {outs:?}"
        );
    }
}

/// The 10 s status line counts the concealed slots: `lost` counts every slot whose packet was
/// missing at its deadline, `concealed=` the ones concealed, `silence_ms` only the real silence.
#[test]
fn the_status_line_counts_the_concealed_slots_1439() {
    let nus: Vec<u32> = (1000..1000 + 2063)
        .filter(|&n| n != 1700 && !(1900..1904).contains(&n))
        .collect();
    let (_outs, log) = run_with_log(&on_grid(&nus));
    let line = log
        .lines()
        .find_map(|l| l.strip_prefix("blog 300: [obs-vban] obs-vban receive: "))
        .unwrap_or_else(|| panic!("issue 1439: no status line:\n{log}"));
    let field = |k: &str| -> &str {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(k))
            .unwrap_or_else(|| panic!("issue 1439: no `{k}` in the status line `{line}`"))
    };
    for (k, want) in [("lost=", "5"), ("concealed=", "1"), ("silence_ms=", "21.3")] {
        assert_eq!(field(k), want, "issue 1439: `{k}` in `{line}`");
    }
    assert!(
        log.contains("lost 5 (1 concealed)"),
        "issue 1439: the summary names the concealed slots:\n{log}"
    );
}
