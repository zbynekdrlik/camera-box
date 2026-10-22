//! Issue 1439 -- a lost VBAN packet must become silence for exactly its own slot.
//!
//! The vendored obs-vban receive source (`vendor/obs-vban/src/vban-source.c`) used to pad a
//! `nuFrame` gap with COPIES of the next packet's audio, and for two or more lost packets it
//! back-dated those copies by the whole gap per copy, so they landed on top of audio that had
//! already been delivered (the stock 0.3.1 code, `lost_ns * (n_packets - i)`).
//!
//! This gate lifts the SHIPPED `vban-source.c` into a C driver, links it against small stubs of
//! the libobs calls it makes (`obs_source_output_audio` prints what OBS would receive), feeds it
//! scripted packets, and requires:
//!
//! - every packet's audio reaches OBS exactly once;
//! - the slots of the lost packets reach OBS as silence, exactly as many frames as were lost;
//! - the stamps are contiguous: each output starts where the previous one ended.
//!
//! It runs under `cargo test` in CI and standalone with no cargo:
//!
//! ```text
//! CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/vban_source_loss_1439.rs -o /tmp/t && /tmp/t
//! ```
//!
//! `cc` is required. Per the project's test-strictness rule this FAILS LOUDLY rather than skipping
//! when the toolchain is missing.

#[path = "support/vban_source_harness_1439.rs"]
mod harness;

use harness::*;

#[test]
fn two_lost_packets_are_silence_for_exactly_their_slots_1439() {
    let outs = run(&after_hold(&[300, 301, 302, 305, 306]));
    assert_eq!(
        audio_values(&outs),
        vec![1, 2, 3, 6, 7],
        "issue 1439: every packet's audio must reach OBS exactly once, in order (a lost packet was \
         padded with copies of the next one): {outs:?}"
    );
    let at3 = outs.iter().position(|o| !o.zero && o.first == 3).unwrap();
    let at6 = outs.iter().position(|o| !o.zero && o.first == 6).unwrap();
    let gap = &outs[at3 + 1..at6];
    assert!(
        gap.iter().all(|o| o.zero),
        "issue 1439: the lost slots must be silence: {outs:?}"
    );
    assert_eq!(
        gap.iter().map(|o| o.frames).sum::<u64>(),
        2 * u64::from(FRAMES),
        "issue 1439: exactly the two lost packets' frames must be silence: {outs:?}"
    );
    assert_contiguous(&outs);
}

#[test]
fn one_lost_packet_is_one_silent_slot_1439() {
    let outs = run(&after_hold(&[400, 401, 403, 404]));
    assert_eq!(audio_values(&outs), vec![1, 2, 4, 5], "{outs:?}");
    let silent: u64 = after_lead_in(&outs)
        .iter()
        .filter(|o| o.zero)
        .map(|o| o.frames)
        .sum();
    assert_eq!(
        silent,
        u64::from(FRAMES),
        "issue 1439: one lost packet = one packet of silence: {outs:?}"
    );
    assert_contiguous(&outs);
}

#[test]
fn a_stream_without_loss_has_no_silence_1439() {
    let outs = run(&after_hold(&[207, 208, 209, 210, 211, 212]));
    assert_eq!(audio_values(&outs), vec![1, 2, 3, 4, 5, 6], "{outs:?}");
    assert!(after_lead_in(&outs).iter().all(|o| !o.zero), "{outs:?}");
    assert_contiguous(&outs);
}

/// The 10 s status line the stream-role verify reads (`verify-strih.sh`): the received counter, the
/// fixed R, the port, the packet's own stream name and the OBS source name.
#[test]
fn the_status_line_reports_the_counters_r_port_and_stream_1439() {
    // 11 s of an ideal sender: the first packet arms the status clock, the line comes 10 s later.
    let nus: Vec<u32> = (1000..1000 + 2063).collect();
    let (outs, log) = run_with_log(&on_grid(&nus));
    assert_eq!(
        after_lead_in(&outs).len(),
        nus.len() - HELD as usize,
        "every packet past the held first second, none lost or padded"
    );
    let lines: Vec<&str> = log
        .lines()
        // the plugin's blog macro prefixes every line with `[obs-vban] `
        .filter_map(|l| l.strip_prefix("blog 300: [obs-vban] obs-vban receive: "))
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "issue 1439: one status line in 11 s:\n{log}"
    );
    let line = lines[0];
    let field = |k: &str| -> &str {
        line.split_whitespace()
            .find_map(|f| f.strip_prefix(k))
            .unwrap_or_else(|| panic!("issue 1439: no `{k}` in the status line `{line}`"))
    };
    let received: u64 = field("received=").parse().unwrap();
    // 10 s of 256-frame packets at 48 kHz is 1875 packets; the line is written by the packet that
    // crosses the 10 s mark, which it has already counted.
    assert!(
        (1875..=1877).contains(&received),
        "issue 1439: received={received} after 10 s: `{line}`"
    );
    for (k, want) in [
        ("lost=", "0"),
        ("late=", "0"),
        ("re_anchors=", "0"),
        ("receive_buffer_ms=", "40"),
        ("rate=", "48000"),
        ("ch=", "2"),
        ("port=", "6980"),
        ("stream=", "'stream'"),
    ] {
        assert_eq!(field(k), want, "issue 1439: `{k}` in `{line}`");
    }
    assert!(
        line.ends_with("source='Zvuk z mixu'"),
        "issue 1439: the line names its OBS source last: `{line}`"
    );
}

/// A lost packet's silence must reach OBS in the SAME sample format as the stream's packets. libobs
/// `process_audio` resets the source's resampler, and with it the genlock ASRC, whenever the format
/// changes, so silence in another format reset the ASRC twice per loss (review round 1).
#[test]
fn the_silence_keeps_the_streams_sample_format_1439() {
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
            "{name}: the lead-in, then four packets + one silent slot: {outs:?}"
        );
        let formats: Vec<i32> = outs.iter().map(|o| o.format).collect();
        assert!(
            formats.iter().all(|f| *f == formats[0]),
            "issue 1439: {name} -- every output of one stream must keep one sample format, else libobs \
             resets the resampler + the genlock ASRC at each change: {formats:?}"
        );
        assert_eq!(
            audio_values(&outs),
            vec![1, 2, 4, 5],
            "{name}: the audio itself is unchanged: {outs:?}"
        );
        assert_eq!(
            body.iter()
                .filter(|o| o.zero)
                .map(|o| o.frames)
                .sum::<u64>(),
            u64::from(FRAMES),
            "{name}: one lost packet = one packet of silence: {outs:?}"
        );
        assert_contiguous(&outs);
    }
}

/// Review round 1: `update()` registered the new port / stream / host with the UDP thread first and
/// asked for a new timeline only after it, so the new source's first packet could land on the OLD
/// timeline (a gap read as lost slots, or a counted re-anchor). From the moment the source lets the
/// new sender in, the reset must already be pending.
#[test]
fn a_reconfiguration_asks_for_a_new_timeline_before_the_new_source_can_deliver_1439() {
    let ms = 1_000_000u64;
    let mut tokens: Vec<String> = (0..4)
        .map(|k| format!("{}:{}:100", 10 + k, 100 * ms + k * 6 * ms))
        .collect();
    // a packet after each change, so every change has to ask for its own new timeline
    for (k, change) in ["port=6981", "name=program", "host=10.77.8.110"]
        .iter()
        .enumerate()
    {
        tokens.push(change.to_string());
        tokens.push(format!(
            "{}:{}:100",
            500 + 100 * k as u64,
            (200 + 100 * k as u64) * ms
        ));
    }
    let (stdout, log) = run_tokens(&tokens, VBAN_BITFMT_16_INT);
    let after = |change: &str| -> Vec<&str> {
        let a = stdout
            .find(&format!("update {change}\n"))
            .unwrap_or_else(|| panic!("no `update {change}` in:\n{stdout}"));
        stdout[a..]
            .lines()
            .skip(1)
            .take_while(|l| l.starts_with("udp "))
            .collect()
    };
    for (change, op) in [
        ("port=6981", "udp add"),
        ("port=6981", "udp name"),
        ("port=6981", "udp host"),
        ("name=program", "udp name"),
        ("host=10.77.8.110", "udp host"),
    ] {
        let lines = after(change);
        let line = lines
            .iter()
            .find(|l| l.starts_with(op))
            .unwrap_or_else(|| panic!("{change}: no `{op}` call ({lines:?})"));
        assert!(
            line.starts_with(&format!("{op} reset=1 ")),
            "issue 1439: after `{change}` the source let the new sender in before it asked for a \
             new timeline:\n{stdout}"
        );
    }
    // The packet after each change starts a new timeline: a fresh anchor, never a counted re-anchor.
    assert_eq!(
        log.matches("obs-vban receive anchored:").count(),
        4,
        "issue 1439: the packet after a reconfiguration must anchor a new timeline:\n{log}"
    );
    assert!(
        !log.contains("obs-vban receive re-anchor:"),
        "issue 1439: a reconfiguration is a new timeline, never a counted re-anchor:\n{log}"
    );
}

/// Review round 2: on create, `update()` registered the callback (`update_port`) before it read the
/// Receive Buffer, so a packet in that window anchored with R = 0 and the next one logged a counted
/// `reason=config` re-anchor. The source must hold its R before any sender can reach it.
#[test]
fn the_receive_buffer_is_in_place_before_the_first_sender_can_deliver_1439() {
    let (stdout, _log) = run_tokens(&[], VBAN_BITFMT_16_INT);
    let first = stdout
        .lines()
        .find(|l| l.starts_with("udp "))
        .unwrap_or_else(|| panic!("no UDP registration on create:\n{stdout}"));
    assert!(
        first.ends_with(" r=40"),
        "issue 1439: the source let a sender in before it held its Receive Buffer: `{first}`"
    );
    for line in stdout.lines().filter(|l| l.starts_with("udp ")) {
        assert!(
            line.ends_with(" r=40"),
            "issue 1439: a UDP registration without the Receive Buffer in place: `{line}`"
        );
    }
}

/// ROZHODNUTÉ 6345255813 (2): a packet past its stamp is LOST, never re-timed. The source drops its
/// audio, outputs its slot -- with the slots lost right before it -- as silence at once on the
/// unchanged line, logs ONE `obs-vban receive late:` line for the late run (never a re-anchor), and
/// the next packets go on contiguously on that same line.
#[test]
fn a_late_packet_is_dropped_and_its_slot_is_silence_on_the_same_line_1439() {
    // 15 s on the grid: the fit is valid
    let n = 15 * RATE / u64::from(FRAMES);
    let nus: Vec<u32> = (0..n as u32).collect();
    let mut pkts = on_grid(&nus);
    // packets n and n + 1 lost, n + 2 arrives 40 ms after its grid instant (5.3 ms past its stamp),
    // n + 3 .. n + 9 lost, then n + 10 .. n + 14 on the grid (in time again)
    let t_late = 1_000_000_000 + ns((n + 2) * u64::from(FRAMES)) + 40_000_000;
    pkts.push(((n + 2) as u32, t_late, 99));
    for k in n + 10..n + 15 {
        pkts.push((
            k as u32,
            1_000_000_000 + ns(k * u64::from(FRAMES)),
            (k - n) as i16,
        ));
    }
    let (outs, log) = run_with_log(&pkts);
    assert!(
        !log.contains("obs-vban receive re-anchor:"),
        "issue 1439: a late packet is never a re-anchor:\n{log}"
    );
    assert_eq!(
        log.matches("obs-vban receive late:").count(),
        1,
        "issue 1439: one line for the late run:\n{log}"
    );
    assert!(
        outs.iter().all(|o| o.zero || o.first != 99),
        "issue 1439: the late packet's audio is dropped: {outs:?}"
    );
    // after the last packet before the loss (value n): silence for the two lost slots, the late one
    // and the seven lost after it (in pieces of at most 1024 frames), then n + 10 .. n + 14
    let at = outs
        .iter()
        .rposition(|o| !o.zero && o.first == n as i64)
        .expect("the last packet before the loss reaches OBS");
    let after = &outs[at + 1..];
    let silent: Vec<&Out> = after.iter().take_while(|o| o.zero).collect();
    assert_eq!(
        silent.iter().map(|o| o.frames).sum::<u64>(),
        10 * u64::from(FRAMES),
        "issue 1439: the lost and late slots are silence: {after:?}"
    );
    assert_eq!(
        after[silent.len()..]
            .iter()
            .map(|o| o.first)
            .collect::<Vec<_>>(),
        vec![10, 11, 12, 13, 14],
        "issue 1439: the stream goes on: {after:?}"
    );
    // contiguous across the silence: the line never moved
    assert_contiguous(&outs[at..]);
}

/// Review round 6: a late packet before a stretch's first output is dropped with NOTHING filled (the
/// first output is always an in-time packet with its lead-in), so its `late:` line must say so -- it
/// said "its slot is silence". The first packet past the hold comes 60 ms late; the four after it are
/// late too (the receive thread hands them over in order), the sixth is in time and is the first
/// output.
#[test]
fn a_late_packet_before_the_first_output_logs_that_nothing_was_filled_1439() {
    let mut pkts = after_hold(&(400..411).collect::<Vec<u32>>());
    let i = HELD as usize;
    pkts[i].1 += 60_000_000;
    for j in i + 1..pkts.len() {
        pkts[j].1 = pkts[j].1.max(pkts[j - 1].1 + 1_000);
    }
    let (outs, log) = run_with_log(&pkts);
    let late: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("obs-vban receive late:"))
        .collect();
    assert_eq!(late.len(), 1, "issue 1439: one late run, one line:\n{log}");
    assert!(
        late[0].contains("nothing filled") && !late[0].contains("its slot is silence"),
        "issue 1439: a late packet before the first output fills nothing: `{}`",
        late[0]
    );
    // the stream starts at the first in-time packet, after its lead-in (shorter than R − one packet:
    // that packet came in just in time behind the late ones), nothing filled before it
    let (leads, body) = split_lead_ins(&outs);
    assert!(
        leads.len() == 1 && leads[0] > 0 && leads[0] < LEAD_IN,
        "issue 1439: one lead-in: {outs:?}"
    );
    assert!(body.iter().all(|o| !o.zero), "issue 1439: {outs:?}");
    assert_eq!(
        audio_values(&outs),
        vec![7, 8, 9, 10, 11],
        "issue 1439: {outs:?}"
    );
}

/// ROZHODNUTÉ 6343922049 (3): a Receive Buffer no longer than one packet + 10 ms is lifted to that
/// floor, and the source names both in ONE warning: not on every re-anchor, not again after a
/// reconfiguration with the same values.
#[test]
fn a_receive_buffer_under_one_packet_plus_10_ms_warns_once_naming_both_1439() {
    let ms = 1_000_000u64;
    let mut tokens = vec!["buffer=10".to_string()];
    // a first timeline, a sender restart (a counted re-anchor), then a reconfiguration
    // on the grid, one packet past the held first second
    let n = u64::from(HELD) + 1;
    tokens.extend((0..n).map(|k| format!("{}:{}:100", 10 + k, 100 * ms + ns(k * 256))));
    tokens.extend((0..5u64).map(|k| format!("{}:{}:100", 5000 + k, 2000 * ms + ns(k * 256))));
    tokens.push("port=6981".to_string());
    tokens.extend((0..5u64).map(|k| format!("{}:{}:100", 20 + k, 3000 * ms + ns(k * 256))));
    let (stdout, log) = run_tokens(&tokens, VBAN_BITFMT_16_INT);
    let warnings: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("obs-vban receive buffer:"))
        .collect();
    assert_eq!(
        warnings.len(),
        1,
        "issue 1439: one warning for one (R, packet) pair:\n{log}"
    );
    let w = warnings[0];
    assert!(
        w.starts_with("blog 200: ")
            && w.contains("receive_buffer_ms=10 ")
            && w.contains("5.33 ms")
            && w.contains("15.33 ms"),
        "issue 1439: the warning names R and one packet + 10 ms: `{w}`"
    );
    // the line is anchored with the effective R (arrival - one packet + 15.33 ms); its first emitted
    // packet is the one past the held first second, after its lead-in (review round 5: 10 ms from
    // its arrival to its stamp = 480 frames of silence)
    let audio: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("out ts=") && l.contains(" zero=0"))
        .collect();
    let silent: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("out ts=") && l.contains(" zero=1"))
        .collect();
    let first = audio
        .first()
        .and_then(|l| l.strip_prefix("out ts="))
        .and_then(|l| l.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
        .expect("an output");
    assert_eq!(
        first,
        100 * ms + 10 * ms + ns(u64::from(HELD) * 256),
        "issue 1439: `{stdout}`"
    );
    assert_eq!(
        audio.len(),
        1,
        "issue 1439: only the packet past the first second is output (after its lead-in), the \
         restarted timelines hold: `{stdout}`"
    );
    assert!(
        silent[0].contains(" frames=480 "),
        "issue 1439: the lead-in is 10 ms: `{stdout}`"
    );
    // round 5: the sender then pauses, so the worker keeps the line with one slot of silence per
    // missing packet until no packet came in time for a second
    assert!(
        silent[1..].iter().all(|l| l.contains(" frames=256 ")),
        "issue 1439: `{stdout}`"
    );
}

/// Issue 1462 + 1439: mbc.lan sends the SNV program to stream-snv as VBAN `stream` in 24-bit PCM
/// (format byte 0x02: `VBAN_BITFMT_24_INT`, PCM codec), 48 kHz stereo, 239 frames per packet. The
/// shipped source decodes it (sign-extended) into float planar, stamps it on the timeline, and a
/// lost packet is 239 frames of silence in the same format.
#[test]
fn mbcs_24_bit_239_frame_packets_decode_and_a_loss_is_one_slot_of_silence_1439() {
    let frames = 239u64;
    let t0 = 1_000_000_000u64;
    let values: [i16; 5] = [1234, -1234, 32767, -32768, 0];
    // a non-zero low byte on every sample: a decoder that drops it must fail
    let low = 0x5A;
    let mut tokens = vec![format!("low24={low}")];
    // the anchor's first second is held: packet k ends at (k + 1) x 239, so 0..=199 never reach OBS
    let held = 48_000 / frames;
    tokens.extend((0..held).map(|k| format!("{}:{}:999", 500 + k, t0 + ns(k * frames))));
    tokens.extend([0u64, 1, 3, 4, 5].iter().zip(values).map(|(&k, v)| {
        format!(
            "{}:{}:{}",
            500 + held + k,
            t0 + ns((held + k) * frames),
            v as u16
        )
    }));
    let (stdout, log) = run_tokens_frames(&tokens, frames as u32, VBAN_BITFMT_24_INT);
    #[allow(clippy::type_complexity)]
    let outs: Vec<(u64, u64, bool, i64, i32, i64)> = stdout
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
                f("first=").parse().unwrap(),
                f("fmt=").parse().unwrap(),
                f("first24=").parse().unwrap(),
            )
        })
        .collect();
    assert!(!log.contains("Unsupported format"), "issue 1462: {log}");
    // AUDIO_FORMAT_FLOAT_PLANAR (libobs enum audio_format) for every output, the silence included
    assert!(outs.iter().all(|o| o.4 == 8), "issue 1462: {outs:?}");
    for w in outs.windows(2) {
        let expect = w[0].0 + ns(w[0].1);
        assert!(
            w[1].0.abs_diff(expect) <= 2,
            "issue 1462: the stamps are contiguous, the lead-in included: {outs:?}"
        );
    }
    // review round 5: the lead-in first, R − one 239-frame packet = 35.02 ms = 1681 frames
    let n = outs.iter().position(|o| !o.2).expect("an audio output");
    assert_eq!(
        outs[..n].iter().map(|o| o.1).sum::<u64>(),
        1681,
        "issue 1439: the lead-in: {outs:?}"
    );
    let outs = outs[n..].to_vec();
    assert_eq!(outs.len(), 6, "five packets + one silent slot: {outs:?}");
    assert!(outs.iter().all(|o| o.1 == frames), "issue 1462: {outs:?}");
    assert!(
        outs[2].2,
        "issue 1462: the lost packet is one slot of silence: {outs:?}"
    );
    // every packet's samples on the 24-bit scale, sign-extended, the low byte kept
    let raw: Vec<i64> = outs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2)
        .map(|(_, o)| o.5)
        .collect();
    assert_eq!(
        raw,
        values
            .iter()
            .map(|&v| i64::from(v) * 256 + low)
            .collect::<Vec<_>>(),
        "issue 1462: every 24-bit sample reaches OBS whole, low byte included: {outs:?}"
    );
    for w in outs.windows(2) {
        let expect = w[0].0 + ns(w[0].1);
        assert!(
            w[1].0.abs_diff(expect) <= 2,
            "issue 1462: the stamps are contiguous: {outs:?}"
        );
    }
}

/// Review round 4 (the starve of ROZHODNUTÉ 6345255813 item 2): after a 1.5 s network outage the
/// first packet is no packet in time for a second: the source logs ONE `reason=starve` re-anchor,
/// fills nothing after the fact, holds the new anchor's first second and then goes on.
#[test]
fn an_outage_of_1_5_s_is_one_starve_re_anchor_and_no_late_fill_1439() {
    let n = 15 * RATE / u64::from(FRAMES);
    let gap = 282u64;
    let after = 300u64;
    let nus: Vec<u32> = (0..n as u32)
        .chain((n + gap) as u32..(n + gap + after) as u32)
        .collect();
    let (outs, log) = run_with_log(&on_grid(&nus));
    let reanchors: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("obs-vban receive re-anchor:"))
        .collect();
    assert_eq!(
        reanchors.len(),
        1,
        "issue 1439: one starve, one re-anchor:\n{log}"
    );
    assert!(
        reanchors[0].contains("reason=starve"),
        "issue 1439: `{}`",
        reanchors[0]
    );
    // review round 5: the line names the starve span (283 packet slots since the last in-time one)
    let span: f64 = reanchors[0]
        .split("starve_ms=")
        .nth(1)
        .and_then(|r| r.split(' ').next())
        .and_then(|v| v.parse().ok())
        .unwrap_or(f64::NAN);
    let want = (gap + 1) as f64 * f64::from(FRAMES) * 1000.0 / RATE as f64;
    assert!(
        (span - want).abs() < 0.1,
        "issue 1439: starve_ms={span} (want {want:.1}): `{}`",
        reanchors[0]
    );
    // review round 5: each stretch opens with its lead-in (the new one too: libobs resets its timing
    // after the gap and puts the first chunk at its arrival)
    let (leads, rest) = split_lead_ins(&outs);
    assert_eq!(
        leads,
        vec![LEAD_IN, LEAD_IN],
        "issue 1439: one lead-in per stretch: {outs:?}"
    );
    // ROZHODNUTÉ 6347872706: the worker keeps the line through the outage -- one slot of silence per
    // missing packet, at its deadline -- until no packet came in time for a second (the slots whose
    // deadline is at most a second after the last in-time arrival: 181 on this grid), then stops;
    // nothing is filled after the fact
    let silent: Vec<&Out> = rest.iter().filter(|o| o.zero).collect();
    assert!(
        silent.iter().all(|o| o.frames == u64::from(FRAMES)),
        "issue 1439: one slot each: {silent:?}"
    );
    assert_eq!(
        silent.len(),
        181,
        "issue 1439: a second of silent slots, then nothing: {silent:?}"
    );
    assert_eq!(
        rest.iter().filter(|o| !o.zero).count() as u64,
        (n - u64::from(HELD)) + (after - u64::from(HELD)),
        "issue 1439: both anchors hold their first second"
    );
}

/// ROZHODNUTÉ 6347872706 + review round 5: the source emits on its own clock, by every slot's
/// deadline, never in the receive callback. The idle worker the first in-time packet wakes sends the
/// lead-in and that packet at its arrival; from then on a stored packet goes out at the worker's
/// next look (at most 5 ms after it arrived, long before its deadline), and a missing slot's silence
/// exactly at its deadline (its stamp − the 2 ms margin), the packet behind it at the same wake.
#[test]
fn a_stored_slot_goes_out_at_the_workers_next_look_a_missing_one_at_its_deadline_1439() {
    let pkts = after_hold(&[300, 301, 303, 304, 305]);
    let first_arrival = pkts[HELD as usize].1;
    let arrival = |v: i64| pkts.iter().find(|p| i64::from(p.2) == v).expect("sent").1;
    let (outs, _log) = run_with_log(&pkts);
    let (leads, body) = split_lead_ins(&outs);
    assert_eq!(leads, vec![LEAD_IN], "issue 1439: {outs:?}");
    let lead = outs.iter().find(|o| o.zero).expect("the lead-in");
    assert_eq!(
        lead.at, first_arrival,
        "issue 1439: the lead-in goes out at the first in-time packet's arrival: {outs:?}"
    );
    assert_eq!(
        audio_values(&body),
        vec![1, 2, 4, 5, 6],
        "issue 1439: {outs:?}"
    );
    let margin = 2_000_000u64;
    let silence: Vec<&Out> = body.iter().filter(|o| o.zero).collect();
    assert_eq!(
        silence.iter().map(|o| o.frames).collect::<Vec<_>>(),
        vec![u64::from(FRAMES)],
        "issue 1439: the lost packet's slot is one slot of silence: {outs:?}"
    );
    assert_eq!(
        silence[0].at + margin,
        silence[0].ts,
        "issue 1439: the missing slot's silence goes out at its deadline: {outs:?}"
    );
    for o in body.iter().filter(|o| !o.zero) {
        let a = arrival(o.first);
        assert!(
            o.at >= a && o.at + margin < o.ts,
            "issue 1439: a packet goes out after its arrival, before its deadline: {o:?} in {outs:?}"
        );
        if o.first == 4 {
            assert_eq!(
                o.at, silence[0].at,
                "issue 1439: the packet behind a missing slot goes out right after its silence: {outs:?}"
            );
        } else {
            assert!(
                o.at <= a + 5_000_000,
                "issue 1439: a stored packet goes out at the worker's next look: {o:?} in {outs:?}"
            );
        }
        if o.first != 1 {
            assert!(
                o.at > a,
                "issue 1439: never in the receive callback (at its arrival): {o:?} in {outs:?}"
            );
        }
    }
}

/// ROZHODNUTÉ 6347872706: the playout worker is a REAL thread, one per source. Run on the real
/// clock: it is spawned with the source and again after an update, it hands the slots to OBS by
/// itself (a stored packet at its next look, ahead of its deadline: review round 5), and it is
/// stopped and JOINED in the update and in destroy -- in destroy before the source reads its
/// timeline for the summary and frees it.
#[test]
fn the_worker_thread_runs_and_is_joined_before_the_source_frees_its_timeline_1439() {
    let mut tokens = vec!["thread".to_string()];
    // 1.4 s of a clean sender (the hold, then output), a port change, 0.5 s more
    tokens.extend((0..264u64).map(|k| format!("{}:{}:100", 1000 + k, ns((k + 1) * 256))));
    tokens.push("port=6981".to_string());
    tokens.extend((264..364u64).map(|k| format!("{}:{}:100", 1000 + k, ns((k + 1) * 256))));
    let (stdout, log) = run_tokens(&tokens, VBAN_BITFMT_16_INT);
    assert_eq!(
        (
            stdout.matches("worker spawn").count(),
            stdout.matches("worker joined").count()
        ),
        (2, 2),
        "issue 1439: spawned at create and after the update, joined in the update and in destroy: \
         {stdout}"
    );
    let line_of = |needle: &str| {
        log.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("issue 1439: no `{needle}` in the log:\n{log}"))
    };
    let update = line_of("receive worker: source 'Zvuk z mixu' stopped (update)");
    let destroy = line_of("receive worker: source 'Zvuk z mixu' stopped (destroy)");
    let summary = line_of("source 'Zvuk z mixu': received ");
    assert!(
        update < destroy && destroy < summary,
        "issue 1439: the worker is joined in destroy before the timeline is read and freed:\n{log}"
    );
    let audio: Vec<(u64, u64)> = stdout
        .lines()
        .filter(|l| l.starts_with("out ts=") && l.contains(" zero=0"))
        .map(|l| {
            let f = |k: &str| -> u64 {
                l.split_whitespace()
                    .find_map(|x| x.strip_prefix(k))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| panic!("no `{k}` in `{l}`"))
            };
            (f("ts="), f("at="))
        })
        .collect();
    assert!(
        audio.len() > 20,
        "issue 1439: the worker thread handed the stored packets to OBS: {stdout}"
    );
    // a stored packet goes out at the worker's next look, ~30 ms ahead of its deadline: even a
    // loaded runner leaves nine in ten of them ahead of it
    let ahead = audio
        .iter()
        .filter(|&&(ts, at)| at + 2_000_000 < ts)
        .count();
    assert!(
        ahead * 10 >= audio.len() * 9,
        "issue 1439: the worker thread held the stored packets to their deadlines: {audio:?}"
    );
}

/// The `obs-vban receive:` status lines of a run, as `key=value` maps (`[obs-vban] ` prefixed).
fn status_lines(log: &str) -> Vec<std::collections::HashMap<String, String>> {
    log.lines()
        .filter_map(|l| l.strip_prefix("blog 300: [obs-vban] obs-vban receive: "))
        .map(|l| {
            l.split_whitespace()
                .filter_map(|f| f.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        })
        .collect()
}

/// Review round 4: packets that stay in time but sit more than the outlier limit off the line (an
/// on-time delay level step) never re-anchor, and the fit learns nothing from them -- the status
/// line must SAY so (`fit=frozen`) instead of `fit=locked`.
#[test]
fn a_fit_that_folds_nothing_reads_frozen_on_the_status_line_1439() {
    // 12 s on the grid (the fit is valid at ~10 s), then 10 s 15 ms later: in time, not folded
    let n = 12 * RATE / u64::from(FRAMES);
    let m = 10 * RATE / u64::from(FRAMES);
    let pkts: Vec<(u32, u64, i16)> = (0..n + m)
        .map(|k| {
            let late = if k >= n { 15_000_000 } else { 0 };
            (
                k as u32,
                1_000_000_000 + ns(k * u64::from(FRAMES)) + late,
                1,
            )
        })
        .collect();
    let (_outs, log) = run_with_log(&pkts);
    let lines = status_lines(&log);
    assert_eq!(
        lines.len(),
        2,
        "issue 1439: two status lines in 22 s:\n{log}"
    );
    assert_eq!(lines[0]["fit"], "locked", "issue 1439: {lines:?}");
    assert_eq!(
        lines[1]["fit"], "frozen",
        "issue 1439: nothing folded for 10 s while the fit is valid: {lines:?}"
    );
    assert_eq!(lines[1]["re_anchors"], "0", "{lines:?}");
}

/// Review round 4: the status line's `max_late_ms` (the measure R is chosen by) includes the late
/// packets that were dropped, not only the ones that came in time.
#[test]
fn max_late_ms_counts_the_dropped_late_packets_1439() {
    // 22 s on the grid: the status lines come at ~10 s and ~20 s
    let n = 22 * RATE / u64::from(FRAMES);
    let mut pkts = on_grid(&(0..n as u32).collect::<Vec<_>>());
    // at 15 s one packet 50 ms after its grid instant (past its stamp: R 40 - one packet = 34.7 ms
    // margin); the ones right behind it arrive with it
    let at = (15 * RATE / u64::from(FRAMES)) as usize;
    pkts[at].1 += 50_000_000;
    // the receive thread hands packets over in order
    for i in at + 1..n as usize {
        pkts[i].1 = pkts[i].1.max(pkts[i - 1].1 + 1_000);
    }
    let (_outs, log) = run_with_log(&pkts);
    let lines = status_lines(&log);
    let last = lines.last().expect("a status line");
    assert!(
        last["late"].parse::<u64>().unwrap() >= 1,
        "issue 1439: the late packet was dropped: {lines:?}"
    );
    let max_late: f64 = last["max_late_ms"].parse().unwrap();
    assert!(
        max_late > 10.0,
        "issue 1439: max_late_ms must show the dropped packet (~15 ms past its stamp): {lines:?}"
    );
}
