//! Issue 1425 (E2 slice 1, design 6206822792): the STAMP basis books a placement jump only once it
//! is CONFIRMED. A `#[path]` child of the timecode module (`asrc_bench_timecode.rs`), so neither
//! file grows past its budget. The two-clock proof (stalls, a real loss, equal-stamp bursts) is the
//! bench `src/asrc_stamp_bench_stall.rs`; the C mirror is gated by
//! `tests/asrc_compensator_stamp_parity_1425.rs`.

use super::*;

/// One packet: observe its placement error, then feed it to the level loop (the timecode ingest
/// order), and take the payment the next packet would carry.
fn feed(c: &mut RealtimeAsrcCompensator, err_ms: f64, packet_s: f64) {
    c.observe_placement(err_ms, packet_s * 1000.0, false);
    c.compensate_with_level(packet_s, packet_s, err_ms);
    c.take_step_recover_ppm();
}

/// A locked, captured servo on `basis` with a steady feed (placement error 0).
fn locked(basis: AsrcBasis, packet_s: f64) -> RealtimeAsrcCompensator {
    let mut c = RealtimeAsrcCompensator::new();
    c.set_basis(basis);
    for _ in 0..(120.0 / packet_s) as usize {
        feed(&mut c, 0.0, packet_s);
    }
    assert!(
        c.level_captured && c.timecode() && c.basis() == basis,
        "the servo must lock, capture and keep the basis within 120 s"
    );
    c
}

/// A delivery stall of `stall_ms` on an arrival-stamped driver: the burst's first packet sits
/// `stall_ms` early against its (late) stamp and every next burst packet `packet_ms` less, until
/// the burst caught up.
fn stall(c: &mut RealtimeAsrcCompensator, stall_ms: f64, packet_s: f64) {
    let packet_ms = packet_s * 1000.0;
    let mut err = -stall_ms;
    while err < 0.0 {
        feed(c, err, packet_s);
        err += packet_ms;
    }
}

/// The packets a candidate needs on a steady `packet_ms` feed: the confirmation sums the packets'
/// own durations, so 375 packets of 128 samples are a hair under one second and the 376th confirms.
fn packets_to_confirm(packet_ms: f64) -> usize {
    let (mut sum_ms, mut n) = (0.0, 0);
    loop {
        sum_ms += packet_ms;
        n += 1;
        if sum_ms >= PLACE_CONFIRM_MS && n >= PLACE_CONFIRM_PACKETS as usize {
            return n;
        }
    }
}

const MBC_S: f64 = 128.0 / 48000.0;
const PULSE_S: f64 = 1024.0 / 48000.0;
const SLOT_S: f64 = 1600.0 / 48000.0;

#[test]
fn a_delivery_stall_books_nothing_on_the_stamp_basis_1425() {
    for packet_s in [MBC_S, PULSE_S] {
        for stall_ms in [8.0, 12.0, 15.0, 20.0, 30.0, 60.0, 120.0] {
            let mut c = locked(AsrcBasis::Stamp, packet_s);
            stall(&mut c, stall_ms, packet_s);
            for _ in 0..(2.0 / packet_s) as usize {
                feed(&mut c, 0.0, packet_s);
            }
            assert!(
                c.place_jump_count() == 0 && c.step_recover_ms() == 0.0,
                "issue 1425: a {stall_ms} ms delivery stall on {:.0}-sample packets returns by \
                 itself, so the stamp basis must book nothing: jumps {} owed {}",
                packet_s * 48000.0,
                c.place_jump_count(),
                c.step_recover_ms()
            );
        }
    }
}

#[test]
fn the_timecode_basis_still_books_a_jump_at_once_1425() {
    // anti-tautology for the stall test, and the issue-1367 contract: the same transient books on
    // the timecode basis the moment it leaves the band
    let mut c = locked(AsrcBasis::Timecode, MBC_S);
    c.observe_placement(-20.0, MBC_S * 1000.0, false);
    assert!(
        c.place_jump_count() == 1 && (c.step_recover_ms() - 20.0).abs() < 1e-9,
        "issue 1425: the timecode basis keeps the immediate booking: jumps {} owed {}",
        c.place_jump_count(),
        c.step_recover_ms()
    );
}

#[test]
fn a_persistent_loss_is_booked_once_after_one_second_of_audio_1425() {
    let mut c = locked(AsrcBasis::Stamp, MBC_S);
    let needed = packets_to_confirm(MBC_S * 1000.0);
    for k in 0..needed {
        assert_eq!(
            c.place_jump_count(),
            0,
            "issue 1425: nothing is booked before one second of audio (packet {k})"
        );
        feed(&mut c, -44.0, MBC_S);
    }
    assert!(
        c.place_jump_count() == 1 && (c.last_place_jump_ms() + 44.0).abs() < 1e-9,
        "issue 1425: a lasting 44 ms loss is booked once, at its full size, after {needed} \
         packets: jumps {} last {}",
        c.place_jump_count(),
        c.last_place_jump_ms()
    );
    assert!(
        c.level_err_ema_ms == 0.0,
        "issue 1425: the booking puts the smoothed error back to its value at the open, so the \
         whole loss is paid at 1000 ppm: ema {}",
        c.level_err_ema_ms
    );
    // the payment stretches the audio back, so the error follows what is still owed
    for _ in 0..(2.0 / MBC_S) as usize {
        let err = -c.step_recover_ms();
        feed(&mut c, err, MBC_S);
    }
    assert_eq!(
        c.place_jump_count(),
        1,
        "issue 1425: the loss is booked once, never again while it is paid"
    );
}

#[test]
fn the_packet_count_gates_a_large_packet_source_1425() {
    // 200 ms packets: a second of audio after 5 packets, but the window needs 8
    let packet_s = 0.2;
    let mut c = locked(AsrcBasis::Stamp, packet_s);
    for k in 0..PLACE_CONFIRM_PACKETS {
        assert_eq!(
            c.place_jump_count(),
            0,
            "issue 1425: nothing is booked before {PLACE_CONFIRM_PACKETS} packets ({k})"
        );
        c.observe_placement(-150.0, 200.0, false);
    }
    assert!(
        c.place_jump_count() == 1,
        "issue 1425: the 8th packet confirms: jumps {}",
        c.place_jump_count()
    );
}

#[test]
fn re_entering_the_band_drops_the_candidate_1425() {
    let mut c = locked(AsrcBasis::Stamp, MBC_S);
    let second = packets_to_confirm(MBC_S * 1000.0);
    for _ in 0..second - 10 {
        c.observe_placement(-44.0, MBC_S * 1000.0, false);
    }
    // one packet back inside the band (9.9 ms off the reference) drops it
    c.observe_placement(-9.9, MBC_S * 1000.0, false);
    for _ in 0..second - 1 {
        c.observe_placement(-44.0, MBC_S * 1000.0, false);
    }
    assert_eq!(
        c.place_jump_count(),
        0,
        "issue 1425: the candidate restarted at the re-entry, so one more second is needed"
    );
    c.observe_placement(-44.0, MBC_S * 1000.0, false);
    assert_eq!(
        c.place_jump_count(),
        1,
        "issue 1425: a full second after the re-open"
    );
}

#[test]
fn the_other_sign_starts_a_new_candidate_1425() {
    let mut c = locked(AsrcBasis::Stamp, MBC_S);
    let second = packets_to_confirm(MBC_S * 1000.0);
    for _ in 0..second / 2 {
        c.observe_placement(-44.0, MBC_S * 1000.0, false);
    }
    for _ in 0..second - 1 {
        c.observe_placement(44.0, MBC_S * 1000.0, false);
    }
    assert_eq!(
        c.place_jump_count(),
        0,
        "issue 1425: a sign change beyond the band restarts the window"
    );
    c.observe_placement(44.0, MBC_S * 1000.0, false);
    assert!(
        c.place_jump_count() == 1 && (c.step_recover_ms() + 44.0).abs() < 1e-9,
        "issue 1425: the late side is booked as a duplicate (compress): owed {}",
        c.step_recover_ms()
    );
}

#[test]
fn the_booking_is_the_median_of_the_last_half_1425() {
    // a window that starts 80 ms early (the first 225 packets) and settles at 40 ms early: the last
    // half of the 376-packet window (188 packets) holds 151 at -40 and 37 at -80, its median -40
    let mut c = locked(AsrcBasis::Stamp, MBC_S);
    assert_eq!(packets_to_confirm(MBC_S * 1000.0), 376);
    for k in 0..376 {
        let err = if k < 225 { -80.0 } else { -40.0 };
        c.observe_placement(err, MBC_S * 1000.0, false);
    }
    assert!(
        c.place_jump_count() == 1 && (c.last_place_jump_ms() + 40.0).abs() < 1e-9,
        "issue 1425: the booked amount is the median of the last half of the window, not the \
         first excursion: {}",
        c.last_place_jump_ms()
    );
}

#[test]
fn an_even_last_half_averages_its_two_middle_errors_1425() {
    // 1600-sample packets: a second is 30 packets, the last half 15 (odd); 31.25 ms packets (1500
    // samples) need 32, the last half 16 -- its two middle values are averaged
    let packet_ms = 1500.0 / 48.0;
    let mut c = locked(AsrcBasis::Stamp, packet_ms / 1000.0);
    for k in 0..32 {
        let err = if k < 24 { -40.0 } else { -50.0 };
        c.observe_placement(err, packet_ms, false);
    }
    assert!(
        c.place_jump_count() == 1 && (c.last_place_jump_ms() + 45.0).abs() < 1e-9,
        "issue 1425: 8 at -40 and 8 at -50 in the last half average to -45: {}",
        c.last_place_jump_ms()
    );
}

#[test]
fn tiny_packets_book_the_median_of_the_newest_ring_1425() {
    // 16-sample packets (0.33 ms): a second is 3000 packets, its last half 1500, of which the ring
    // keeps the newest 256
    let packet_ms = 16.0 / 48.0;
    let mut c = locked(AsrcBasis::Stamp, 0.002);
    assert_eq!(packets_to_confirm(packet_ms), 3000);
    // the newest 256 of the 3000 hold 97 at -50 and 159 at -30; the 3 packets after the booking
    // (20 ms off the moved setpoint) only open a new candidate
    for k in 0..3003 {
        let err = if k < 2903 { -30.0 } else { -50.0 };
        c.observe_placement(err, packet_ms, false);
    }
    assert!(
        c.place_jump_count() == 1 && (c.last_place_jump_ms() + 30.0).abs() < 1e-9,
        "issue 1425: the newest {PLACE_CONFIRM_RING} errors give -30: {}",
        c.last_place_jump_ms()
    );
}

#[test]
fn a_basis_change_or_a_placement_restarts_the_window_1425() {
    let second = packets_to_confirm(SLOT_S * 1000.0);
    let half = second / 2;
    // a basis change (stamp -> timecode -> stamp, no flush: both are placement-judged)
    let mut c = locked(AsrcBasis::Stamp, SLOT_S);
    for _ in 0..half {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    c.set_basis(AsrcBasis::Timecode);
    c.set_basis(AsrcBasis::Stamp);
    assert!(
        c.level_captured,
        "a stamp/timecode change keeps the capture"
    );
    for _ in 0..second - 1 {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    assert_eq!(
        c.place_jump_count(),
        0,
        "issue 1425: a basis change drops the open candidate"
    );
    // a placement
    let mut c = locked(AsrcBasis::Stamp, SLOT_S);
    for _ in 0..half {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    c.observe_placement(-44.0, SLOT_S * 1000.0, true);
    for _ in 0..second - 2 {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    assert_eq!(
        c.place_jump_count(),
        0,
        "issue 1425: a placement drops the open candidate"
    );
}

#[test]
fn leaving_the_mode_drops_the_candidate_1425() {
    let second = packets_to_confirm(SLOT_S * 1000.0);
    let half = second / 2;
    let mut c = locked(AsrcBasis::Stamp, SLOT_S);
    for _ in 0..half {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    // the capture is dropped (as a flush drops it): observe is inert and forgets the candidate
    c.level_captured = false;
    c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    c.level_captured = true;
    for _ in 0..second - 1 {
        c.observe_placement(-44.0, SLOT_S * 1000.0, false);
    }
    assert_eq!(
        c.place_jump_count(),
        0,
        "issue 1425: an observe without a capture drops the candidate"
    );
}
