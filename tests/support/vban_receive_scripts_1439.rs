//! Issue 1439 — the packet scripts the receive-timeline parity gate
//! (`tests/vban_receive_timeline_parity_1439.rs`) replays through both twins, split out of the gate
//! when it passed its line budget (review round 5). Each script aims a packet sequence at boundaries
//! of the decision; the gate's coverage test requires every boundary to be reached.

use crate::vban_receive_timeline::{
    samples_to_ns, Decision, Timeline, JUMP_S, OUTLIER_MS, STALE_PACKETS,
};
use crate::MS;

/// One scripted packet: its arrival, its header fields, the Receive Buffer, and whether the source
/// was reconfigured right before it (`vban_recv_init`).
#[derive(Clone, Copy)]
pub(crate) struct Pkt {
    pub(crate) now: u64,
    pub(crate) nu: u32,
    pub(crate) frames: u32,
    pub(crate) rate: u32,
    pub(crate) channels: u32,
    pub(crate) reserve_ns: u64,
    pub(crate) reset: bool,
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

/// Builds a packet script while running the Rust model, so that a packet can be steered onto the
/// model's own stamps and lines.
struct Builder {
    t: Timeline,
    pkts: Vec<Pkt>,
    now: u64,
}

impl Builder {
    fn new() -> Self {
        Builder {
            t: Timeline::new(),
            pkts: Vec::new(),
            now: 0,
        }
    }

    fn send(&mut self, p: Pkt) -> Decision {
        let p = Pkt {
            now: p.now.max(self.now),
            ..p
        };
        self.now = p.now;
        self.pkts.push(p);
        if p.reset {
            self.t = Timeline::new();
        }
        self.t
            .packet(p.now, p.nu, p.frames, p.rate, p.channels, p.reserve_ns)
    }
}

/// A two-clock sender (`ppm` off, positive = fast) with burst jitter, steered onto the boundaries.
/// The events that keep the timeline running (losses, packets on their stamp or on the outlier
/// line) are frequent, the re-anchoring ones rare, so the fit becomes valid between them.
#[allow(clippy::too_many_arguments)]
fn steered_script(
    seed: u64,
    ppm: f64,
    frames: u32,
    rate: u32,
    channels: u32,
    reserve_ms: u64,
    seconds: u64,
    events: bool,
) -> Vec<Pkt> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut b = Builder::new();
    let pkt_ns = f64::from(frames) * 1e9 / (f64::from(rate) * (1.0 + ppm * 1e-6));
    let n = seconds * u64::from(rate) / u64::from(frames);
    let t0 = 3_000_000_000u64;
    let mut reserve = reserve_ms * MS;
    let mut channels = channels;
    let mut nu: u32 = (seed as u32).wrapping_mul(7919);
    let mut burst = t0 as f64;
    let mut k: u64 = 0;
    while k < n {
        let mut reset = false;
        let mut this_nu = nu.wrapping_add(1);
        let mut lost = 0u64;
        let mut forced: Option<u64> = None;
        if events && b.t.anchored {
            let pos_next = b.t.next_pos;
            let ev = rng.below(4000);
            let rare = rng.below(10) == 0;
            match ev {
                // a loss of 1..3 packets
                0..=79 => lost = 1 + rng.below(3),
                // exactly on its stamp, and (rarely) one ns past it
                80..=99 => forced = Some(b.t.stamp_at(pos_next)),
                100..=104 if rare => forced = Some(b.t.stamp_at(pos_next) + 1),
                // on the outlier line and one ns either side
                105..=164 => {
                    let end = pos_next + u64::from(frames);
                    let line = b.t.reference(end) + (OUTLIER_MS * MS) as f64;
                    let y = (line.ceil() as u64 + rng.below(3)).saturating_sub(1);
                    forced = Some((b.t.anchor_ns + y).min(b.t.stamp_at(pos_next)));
                }
                // a network duplicate or a reorder within the stale window: an EXTRA packet whose
                // slot has passed (dropped); the sender's own counter and schedule are untouched
                165 | 166 => {
                    let back = if ev == 165 {
                        0
                    } else {
                        1 + rng.below(u64::from(STALE_PACKETS)) as u32
                    };
                    let now = b.now + 1 + rng.below(500_000);
                    b.send(Pkt {
                        now,
                        nu: nu.wrapping_sub(back),
                        frames,
                        rate,
                        channels,
                        reserve_ns: reserve,
                        reset: false,
                    });
                }
                // an index far behind the stale window: a sender restart from there
                172 if rare => this_nu = nu.wrapping_sub(STALE_PACKETS + 1 + rng.below(9) as u32),
                // a loss of exactly two seconds of audio, or one packet more
                167 | 168 if rare => {
                    lost = JUMP_S * u64::from(rate) / u64::from(frames) + u64::from(ev == 168)
                }
                169 if rare => reserve = (10 + rng.below(80)) * MS,
                170 if rare => channels = 1 + (channels % 2),
                171 if rare => reset = true,
                _ => {}
            }
        }
        // a lost packet is a slot the sender produced: the next one arrives on its own schedule
        this_nu = this_nu.wrapping_add(lost as u32);
        k += lost;
        let ready = t0 as f64 + (k + 1) as f64 * pkt_ns;
        while burst < ready {
            burst += if rng.below(70) == 0 {
                5.0e6 + rng.below(25_000_000) as f64
            } else {
                5.0e6 + rng.below(2_000_000) as f64
            };
        }
        let now = forced.unwrap_or(burst as u64 + 300_000 + rng.below(400_000));
        b.send(Pkt {
            now,
            nu: this_nu,
            frames,
            rate,
            channels,
            reserve_ns: reserve,
            reset,
        });
        nu = this_nu;
        k += 1;
    }
    b.pkts
}

/// Each re-anchoring event once, on a clean 48 kHz sender whose fit is already valid: a loss of
/// exactly two seconds (silence), a packet on its stamp and one ns past it (a late shift), two lost
/// slots before a late packet, a duplicate, a backwards index, a jump of two seconds and one
/// packet, another R, another channel count, a reset.
fn boundary_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let mut k: u64 = 0;
    let mut nu: u32 = 1000;
    let mut channels = 2;
    let mut reserve = 40 * MS;
    let mut reset = false;
    let clean =
        |b: &mut Builder, k: &mut u64, nu: &mut u32, n: u64, ch: u32, r: u64, reset: &mut bool| {
            for _ in 0..n {
                b.send(Pkt {
                    now: 2_000_000_000 + samples_to_ns((*k + 1) * 256, 48_000),
                    nu: *nu,
                    frames: 256,
                    rate: 48_000,
                    channels: ch,
                    reserve_ns: r,
                    reset: *reset,
                });
                *reset = false;
                *k += 1;
                *nu = nu.wrapping_add(1);
            }
        };
    clean(
        &mut b,
        &mut k,
        &mut nu,
        12 * 375,
        channels,
        reserve,
        &mut reset,
    );
    // exactly two seconds of indexes skipped while the packets keep arriving: silence, the timeline
    // runs on (a 2 s gap in ARRIVALS is a starve, round 4)
    nu = nu.wrapping_add(375);
    clean(
        &mut b,
        &mut k,
        &mut nu,
        4 * 375,
        channels,
        reserve,
        &mut reset,
    );
    // on its stamp, then one ns past it
    let at = b.t.stamp_at(b.t.next_pos);
    b.send(Pkt {
        now: at,
        nu,
        frames: 256,
        rate: 48_000,
        channels,
        reserve_ns: reserve,
        reset: false,
    });
    k += 1;
    nu = nu.wrapping_add(1);
    let late = b.t.stamp_at(b.t.next_pos) + 1;
    b.send(Pkt {
        now: late,
        nu,
        frames: 256,
        rate: 48_000,
        channels,
        reserve_ns: reserve,
        reset: false,
    });
    k += 1;
    nu = nu.wrapping_add(1);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    // two slots lost, then a packet 100 ms late (the shift above already moved the line ~35 ms
    // later): one more shift, the lost slots silence on the new line
    k += 2;
    nu = nu.wrapping_add(2);
    b.send(Pkt {
        now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000) + 100 * MS,
        nu,
        frames: 256,
        rate: 48_000,
        channels,
        reserve_ns: reserve,
        reset: false,
    });
    k += 1;
    nu = nu.wrapping_add(1);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    // a duplicate and a packet exactly STALE_PACKETS behind (both dropped), then one more behind (a
    // restart); a dropped packet takes no slot, so the next clean packet follows the last real one
    let last = nu.wrapping_sub(1);
    for back in [0, STALE_PACKETS] {
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns(k * 256, 48_000) + 1_000_000,
            nu: last.wrapping_sub(back),
            frames: 256,
            rate: 48_000,
            channels,
            reserve_ns: reserve,
            reset: false,
        });
    }
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    nu = nu.wrapping_sub(STALE_PACKETS + 2);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    // two seconds and one packet of indexes skipped: a sender restart
    nu = nu.wrapping_add(376);
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    reserve = 25 * MS;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    channels = 1;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    reset = true;
    clean(&mut b, &mut k, &mut nu, 375, channels, reserve, &mut reset);
    b.pkts
}

/// A clean sender `ppm` off for `seconds`, anchored `anchor_late_ns` late.
fn clean_script(ppm: f64, seconds: u64, anchor_late_ns: u64, reserve_ms: u64) -> Vec<Pkt> {
    let mut b = Builder::new();
    let pkt_ns = 256.0 * 1e9 / (48_000.0 * (1.0 + ppm * 1e-6));
    for k in 0..(seconds * 48_000 / 256) {
        let mut now = 2_000_000_000 + ((k + 1) as f64 * pkt_ns) as u64;
        if k == 0 {
            now += anchor_late_ns;
        }
        b.send(Pkt {
            now,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: reserve_ms * MS,
            reset: false,
        });
    }
    b.pkts
}

/// A clean sender whose network delay drops by 25 ms after 70 s (the queue drains: those packets
/// arrive back to back until they are on time). The fit follows the earlier arrivals, the stamps
/// leave the dead band (the phase term acts) and then the hold of R = 20 ms (a fit re-anchor:
/// phase beyond R). The anchor itself came ~25 ms late against nothing (every packet of its first
/// second is delayed alike), so the first second keeps it.
fn delay_drop_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    for k in 0..(90 * 48_000 / 256u64) {
        let delay = if k * 256 < 70 * 48_000 { 25 * MS } else { 0 };
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000) + delay,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 20 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// A clean 48 kHz sender for `secs` seconds from packet `k0` on (index = packet), at the grid plus
/// `delay` from packet `from` on; packets in `lost` never arrive.
fn grid_script(
    b: &mut Builder,
    k0: u64,
    secs: u64,
    from: u64,
    delay: u64,
    lost: std::ops::Range<u64>,
) {
    for k in k0..k0 + secs * 48_000 / 256 {
        if lost.contains(&k) {
            continue;
        }
        b.send(Pkt {
            now: 2_000_000_000
                + samples_to_ns((k + 1) * 256, 48_000)
                + if k >= from { delay } else { 0 },
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
}

/// Review round 4 (the starve on our clock): a 1.5 s network outage after 15 s (the first packet
/// after it is on time: a starve re-anchor), then a 1.5 s sender pause (every later packet 1.5 s
/// late against the line: a starve re-anchor at once, no drops).
fn outage_and_pause_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let gap = 15 * 48_000 / 256..15 * 48_000 / 256 + 282;
    grid_script(&mut b, 0, 40, 30 * 48_000 / 256, 1_500 * MS, gap);
    b.pkts
}

/// Review round 4: the starve boundary on our clock (a late packet exactly one second after the last
/// in-time arrival is a drop, one ns more re-anchors), then a re-anchor DURING the hold (another
/// channel count inside the new anchor's first second), and a first packet past the hold that comes
/// late (dropped, nothing filled).
fn starve_edges_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let n = 15 * 48_000 / 256u64;
    grid_script(&mut b, 0, 15, u64::MAX, 0, 0..0);
    let last = b.now;
    for (i, now) in [last + 1_000_000_000, last + 1_000_000_001]
        .iter()
        .enumerate()
    {
        b.send(Pkt {
            now: *now,
            nu: (n + i as u64) as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    // the re-anchored line's first second, a channel change inside it, then the new hold
    let t0 = b.now;
    for k in 1..400u64 {
        let channels = if k >= 50 { 1 } else { 2 };
        // the first packet past the second hold comes 60 ms late
        let late = if k == 50 + 187 { 60 * MS } else { 0 };
        b.send(Pkt {
            now: t0 + samples_to_ns(k * 256, 48_000) + late,
            nu: (n + 1 + k) as u32,
            frames: 256,
            rate: 48_000,
            channels,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// A clean sender whose network delay steps up by 90 ms after 15 s and stays: one second of late
/// drops, then ONE persistent-late re-anchor through the hold onto the new delay + R.
fn delay_step_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    for k in 0..(25 * 48_000 / 256u64) {
        let delay = if k * 256 >= 15 * 48_000 { 90 * MS } else { 0 };
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000) + delay,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// Every packet after the anchor 20 ms late: in time (within R), but more than the outlier limit
/// off the line, so nothing folds -- and nothing re-anchors (ROZHODNUTÉ 6345255813 item 2).
fn starve_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    for k in 0..800u64 {
        let late = if k == 0 { 0 } else { 20 * MS };
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000) + late,
            nu: k as u32,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// Review round 5: a FROZEN fit and its 3 s boundary. 240-frame packets (200 a second, so three
/// seconds of audio is exactly 600 packets): 12 s on the grid (the fit is valid), then 4 s of
/// packets 20 ms later -- in time, but more than the outlier limit off the line, so nothing folds:
/// the 600th of them sits exactly `FROZEN_S` past the last fold (not frozen yet), the 601st is
/// frozen -- then back on the grid, where the fit folds again (thawed).
fn frozen_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let per_s = 48_000 / 240u64;
    for k in 0..18 * per_s {
        let off = if (12 * per_s..16 * per_s).contains(&k) {
            20 * MS
        } else {
            0
        };
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 240, 48_000) + off,
            nu: k as u32,
            frames: 240,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
    }
    b.pkts
}

/// Review round 5: a starve on the FIRST packet after an anchor (it comes 1.5 s after it), then a
/// starve inside the new anchor's hold (100 packets in, a 1.5 s gap), then a clean stretch.
fn hold_starve_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    let pkt = |now: u64, nu: u64| Pkt {
        now,
        nu: nu as u32,
        frames: 256,
        rate: 48_000,
        channels: 2,
        reserve_ns: 40 * MS,
        reset: false,
    };
    let at = |k: u64| 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000);
    b.send(pkt(at(0), 0));
    // the first packet after the anchor, 1.5 s later (its index runs on: a pause)
    let t1 = at(1) + 1_500 * MS;
    for k in 1..101u64 {
        b.send(pkt(t1 + samples_to_ns((k - 1) * 256, 48_000), k));
    }
    // 1.5 s of nothing inside the new anchor's hold, then a clean stretch
    let t2 = b.now + 1_500 * MS;
    for k in 101..600u64 {
        b.send(pkt(t2 + samples_to_ns((k - 101) * 256, 48_000), k));
    }
    b.pkts
}

/// Arrivals OUT of order (never from the real monotonic clock, built without the Builder's clamp):
/// one before the anchor, one before its bucket's first packet. The C wraps the unsigned offsets;
/// the Rust twin must wrap identically (review round 3: it panicked in a debug build).
fn out_of_order_script() -> Vec<Pkt> {
    let at = |k: u64| 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000);
    let pkt = |now: u64, k: u64| Pkt {
        now,
        nu: k as u32,
        frames: 256,
        rate: 48_000,
        channels: 2,
        reserve_ns: 40 * MS,
        reset: false,
    };
    let mut v = vec![pkt(at(0), 0), pkt(at(0) - MS, 1)];
    for k in 2..=187u64 {
        v.push(pkt(at(k), k));
    }
    v.push(pkt(at(187) - 1_000, 188));
    for k in 189..400u64 {
        v.push(pkt(at(k), k));
    }
    v
}

/// The index wraps through u32::MAX, with a loss across the wrap (after the held first second).
fn wrap_script() -> Vec<Pkt> {
    let mut b = Builder::new();
    // the first 187 packets are the held first second: the wrap comes after it, in the emitted stream
    let mut nu = u32::MAX - 20 - 187;
    for k in 0..(187 + 60u64) {
        if k == 187 + 30 {
            nu = nu.wrapping_add(2);
        }
        b.send(Pkt {
            now: 2_000_000_000 + samples_to_ns((k + 1) * 256, 48_000),
            nu,
            frames: 256,
            rate: 48_000,
            channels: 2,
            reserve_ns: 40 * MS,
            reset: false,
        });
        nu = nu.wrapping_add(1);
    }
    b.pkts
}

/// Every script the gate replays.
pub(crate) fn scripts() -> Vec<Vec<Pkt>> {
    let mut v = vec![
        boundary_script(),
        clean_script(-100.0, 30, 0, 40),
        clean_script(600.0, 16, 0, 40),
        clean_script(-499.0, 30, 0, 40),
        clean_script(0.0, 16, 60 * MS, 40),
        delay_drop_script(),
        delay_step_script(),
        outage_and_pause_script(),
        starve_edges_script(),
        starve_script(),
        wrap_script(),
        out_of_order_script(),
        frozen_script(),
        hold_starve_script(),
    ];
    let configs: [(f64, u32, u32, u32, u64, u64); 6] = [
        (50.0, 256, 48_000, 2, 40, 60),
        (-50.0, 256, 48_000, 2, 40, 60),
        (-50.0, 103, 96_000, 2, 40, 20),
        (1.4, 128, 44_100, 1, 20, 40),
        (-20.0, 64, 8_000, 2, 100, 60),
        (0.0, 239, 48_000, 8, 10, 40),
    ];
    for (i, &(ppm, frames, rate, ch, r, secs)) in configs.iter().enumerate() {
        v.push(steered_script(
            i as u64 + 1,
            ppm,
            frames,
            rate,
            ch,
            r,
            secs,
            true,
        ));
    }
    v
}
