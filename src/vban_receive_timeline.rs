//! Issue 1439 — the receive timeline of the vendored obs-vban source, the Tier-0 authority.
//!
//! Stock obs-vban 0.3.1 (`vendor/obs-vban/src/vban-source.c`) stamped every packet at its arrival
//! minus its length, and padded a lost packet with copies of the next one. A program feed from a
//! network sender (lv1's VBAN `stream` into the Linux stream box, design revision 6338127636)
//! then carried the sender's network jitter straight into OBS, which mixes at 0 ms audio
//! buffering: a late packet was re-anchored by OBS, and that was a click.
//!
//! This module is the pure decision the source's receive callback asks for every packet:
//!
//! * **Anchor.** The first packet is stamped at its arrival − its length + R, R being the fixed
//!   "Receive Buffer" (default [`RESERVE_MS_DEFAULT`] = 40 ms, read at create/update only, never
//!   adapted at runtime). R is measured from the packet's START, so the timeline uses
//!   max(R, one packet + [`RESERVE_MARGIN_MS`]) (`reserve_eff_ns`, fixed at the anchor; the source
//!   warns once when it lifts R). Within the first [`ACQUIRE_S`] s of the sender's audio (the packet's END
//!   position) a packet that arrived earlier against the line moves the line to it, so the anchor
//!   is the EARLIEST arrival of that second (ROZHODNUTÉ 6343922049): later packets are never early
//!   against it. After that second the line's phase is fixed.
//! * **Hold** (ROZHODNUTÉ 6345255813 item 1). Nothing is emitted for that first second: the anchor
//!   and every packet ending within it are [`Event::Hold`] (they refine the anchor and fold, no
//!   slot is filled). The first packet past it is the first [`Event::Packet`], on the final line,
//!   so libobs never sees a stamp step and its stamp-basis ASRC never walks; the slots lost just
//!   before it are not filled (libobs has nothing from the source to keep contiguous yet). Every
//!   full re-anchor holds again: ~1 s of silence after it.
//! * **Fit.** A least-squares line of arrival time over the packet's END position in the sender's
//!   samples (its `nuFrame` order), over the last [`FIT_WINDOW_S`] one-second buckets of the
//!   sender's audio. A packet later than the reference line by more than [`OUTLIER_MS`] is not
//!   folded into it, so a gap's delayed packets never pull the line late. Until the fit is used
//!   (once its window holds [`FIT_MIN_BUCKETS`] buckets) the reference is the anchored line − R.
//! * **Stamps.** `stamp(pos) = base + T(pos − base_pos)`, where `T(s)` is the duration of `s`
//!   samples times `1 + step_ppb / 1e9`. Once per fitted bucket the step moves toward the fitted
//!   RATE by at most [`PPB_SLEW`] per update, clamped to ±[`PPB_CLAMP`]: the latency set at the
//!   anchor is held, only the rate is followed. A phase term (phase error / [`PHASE_TAU_S`] s, at
//!   most [`PHASE_PPB_MAX`]) joins the target ONLY while the stamps are more than R /
//!   [`PHASE_BAND_DIV`] off `fit + R` (a real sender-clock event, never the arrival jitter), until
//!   they are back inside that dead band. The line is rebased on every change, so the stamps never
//!   jump. The genlock ASRC (`basis=stamp`) then resamples the sender's rate to the mix clock.
//! * **Loss.** A `nuFrame` gap is silence for exactly the lost slots, stamped on the same line.
//! * **Stale.** A duplicate, or a `nuFrame` at most [`STALE_PACKETS`] behind the last one (a
//!   network reorder), is dropped and counted: its slot has passed (it went out as silence).
//! * **Late shift** (counted as one re-anchor, ROZHODNUTÉ 6343922049): a packet arriving after its
//!   stamp while the fit is valid shifts the line ONCE to the stamp it would anchor (arrival −
//!   length + R = `fit + R` + its excess over the fit). The fit and its buckets are kept, the slots
//!   lost before it are silence on the shifted line, and the excess becomes the line's held
//!   `shift_ns`: the dead band and the hold below are centred on `fit + R + shift`, so the shifted
//!   latency is held and the next fit does not re-anchor again.
//! * **Re-anchor** (counted, one per event): a packet arriving after its stamp before the fit is
//!   valid (late), a `nuFrame` further behind (a sender restart), a forward jump of more than
//!   [`JUMP_S`] of audio (a sender restart), another rate or channel count, another R, or a fit
//!   that cannot hold: the fitted rate beyond ±[`PPB_CLAMP`], the stamps more than R off
//!   `fit + R + shift`, or no packet folded for [`STARVE_S`] of the sender's audio. The packet that
//!   re-anchors is the new anchor.
//!
//! The C twin is `vendor/obs-vban/src/vban-receive-timeline.h`;
//! `tests/vban_receive_timeline_parity_1439.rs` compiles the shipped header and requires identical
//! decisions and state (the doubles compared bit for bit). Every floating-point expression here is
//! written in the C header's operation order. Packet contents are the caller's.

/// The Receive Buffer when the source setting is unset (0) or negative.
pub const RESERVE_MS_DEFAULT: i64 = 40;
/// The smallest accepted Receive Buffer.
pub const RESERVE_MS_MIN: i64 = 10;
/// The largest accepted Receive Buffer.
pub const RESERVE_MS_MAX: i64 = 500;
/// The fit window, in one-second buckets of the sender's audio.
pub const FIT_WINDOW_S: usize = 64;
/// The fit is used once its window holds this many non-empty buckets.
pub const FIT_MIN_BUCKETS: usize = 10;
/// A packet later than the reference line by more than this is not folded into the fit.
pub const OUTLIER_MS: u64 = 10;
/// No packet folded for this much of the sender's audio = the fit cannot hold.
pub const STARVE_S: u64 = 3;
/// A forward `nuFrame` jump over this much audio is a sender restart.
pub const JUMP_S: u64 = 2;
/// A duplicate, or an index at most this many packets behind the last one, is a STALE packet (a
/// network reorder or duplicate): dropped and counted, never a re-anchor. Further behind = a restart.
pub const STALE_PACKETS: u32 = 64;
/// The effective Receive Buffer is at least one packet + this (ROZHODNUTÉ 6343922049): R is
/// measured from the packet's start, its arrival is its end.
pub const RESERVE_MARGIN_MS: u64 = 10;
/// The anchor is the earliest arrival among the packets that end within this many seconds of the
/// anchor packet's start (ROZHODNUTÉ 6343922049).
pub const ACQUIRE_S: u64 = 1;
/// The phase term acts only while the stamps are more than R / this off `fit + R`.
pub const PHASE_BAND_DIV: i64 = 4;
/// The stamp step stays within ±500 ppm of nominal.
pub const PPB_CLAMP: i64 = 500_000;
/// The stamp step moves by at most 5 ppm per fit update (about one a second).
pub const PPB_SLEW: i64 = 5_000;
/// Outside the dead band a phase error is walked out at error / 20 s ...
pub const PHASE_TAU_S: i64 = 20;
/// ... with at most 100 ppm of the step spent on it.
pub const PHASE_PPB_MAX: i64 = 100_000;
/// The `obs-vban receive:` status line cadence.
pub const LOG_INTERVAL_NS: u64 = 10_000_000_000;
/// Silence for lost slots goes to OBS in pieces of at most this many frames.
pub const SILENCE_CHUNK_FRAMES: u32 = 1024;

const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_MS: u64 = 1_000_000;

/// What happened to a packet (the C `enum vban_recv_event`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Event {
    /// Stamped on the running timeline.
    #[default]
    Packet = 0,
    /// The first packet: the timeline starts here.
    Anchor = 1,
    /// The timeline restarts at this packet (counted).
    Reanchor = 2,
    /// A duplicate or reordered packet whose slot has passed: dropped (never output), counted.
    Stale = 3,
    /// Inside the first second after an anchor (ROZHODNUTÉ 6345255813 item 1): it refines the
    /// anchor and folds into the fit, but is never output, and no slot is filled for it.
    Hold = 4,
}

/// Why a timeline restarted (the C `enum vban_recv_reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reason {
    #[default]
    None = 0,
    /// The packet arrived after its stamp.
    Late = 1,
    /// A `nuFrame` more than [`STALE_PACKETS`] behind the last one (a sender restart).
    Backward = 2,
    /// A forward `nuFrame` jump over [`JUMP_S`].
    Jump = 3,
    /// Another sample rate or channel count.
    Format = 4,
    /// Another Receive Buffer.
    Config = 5,
    /// The fit cannot hold.
    Fit = 6,
}

impl Reason {
    /// The name the C source logs.
    pub fn name(self) -> &'static str {
        match self {
            Reason::None => "none",
            Reason::Late => "late",
            Reason::Backward => "backward",
            Reason::Jump => "jump",
            Reason::Format => "format",
            Reason::Config => "config",
            Reason::Fit => "fit",
        }
    }
}

/// One second of the sender's audio in the fit (the C `struct vban_recv_bucket`).
#[derive(Debug, Clone, Copy)]
pub struct Bucket {
    /// The bucket index (the end position / rate), −1 = empty.
    pub k: i64,
    /// Folded packets.
    pub n: u64,
    /// The first folded end position (samples since the anchor).
    pub x0: u64,
    /// Its arrival (ns since the anchor's arrival).
    pub y0: u64,
    pub su: f64,
    pub sv: f64,
    pub suu: f64,
    pub suv: f64,
}

impl Default for Bucket {
    fn default() -> Self {
        Bucket {
            k: -1,
            n: 0,
            x0: 0,
            y0: 0,
            su: 0.0,
            sv: 0.0,
            suu: 0.0,
            suv: 0.0,
        }
    }
}

/// What the source does with one packet: silence first (when any), then the packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Decision {
    pub event: Event,
    pub reason: Reason,
    pub silence_samples: u64,
    /// The first silent sample's position ([`Timeline::stamp_at`]).
    pub silence_pos: u64,
    /// The packet's stamp.
    pub stamp_ns: u64,
    /// Arrival − stamp (negative = the margin left).
    pub lateness_ns: i64,
    /// A LATE re-anchor: how far after its old stamp the packet came.
    pub late_ns: i64,
    /// A LATE re-anchor that shifted the line and kept the fit (the timeline runs on).
    pub shifted: bool,
}

/// The receive timeline of one source (the C `struct vban_recv_timeline`).
#[derive(Debug, Clone)]
pub struct Timeline {
    pub anchored: bool,
    pub rate: u32,
    pub channels: u32,
    /// The Receive Buffer as configured (another value re-anchors).
    pub reserve_ns: u64,
    /// The Receive Buffer the line uses: max(`reserve_ns`, the anchor packet's length +
    /// [`RESERVE_MARGIN_MS`]), fixed at the anchor.
    pub reserve_eff_ns: u64,
    /// The anchor packet's arrival.
    pub anchor_ns: u64,
    /// The anchor packet's end position (its frame count).
    pub anchor_end: u64,
    /// The last packet's `nuFrame`.
    pub last_nu: u32,
    /// The next packet's start, in samples since the anchor.
    pub next_pos: u64,
    /// The stamp line: `stamp(pos) = base + T(pos − base_pos)`.
    pub base_stamp_ns: u64,
    pub base_pos: u64,
    /// The stamp step's deviation from nominal.
    pub step_ppb: i64,
    /// The bucket of the last packet's end position.
    pub last_k: i64,
    /// The end position of the last folded packet.
    pub last_fold_pos: u64,
    pub buckets: [Bucket; FIT_WINDOW_S],
    pub fit_valid: bool,
    /// The fitted step deviation (positive = the sender's clock is slow).
    pub fit_ppb: i64,
    /// ns per sample.
    pub fit_b: f64,
    pub fit_xbar: f64,
    pub fit_ybar: f64,
    /// `fit + R + shift − stamp` at the last update.
    pub phase_ns: i64,
    /// The line's held offset from `fit + R`, set by a late shift (its excess over the fit); 0 at
    /// an anchor.
    pub shift_ns: i64,
    pub received: u64,
    /// Lost packets (silence slots).
    pub lost: u64,
    /// The silence those slots carried.
    pub silence_samples: u64,
    /// Packets that arrived after their stamp (each a re-anchor).
    pub late: u64,
    pub re_anchors: u64,
    /// Duplicate / reordered packets dropped (their slot had passed).
    pub stale: u64,
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

/// The Receive Buffer in ms for a source setting value: 0 or less = the default.
pub fn clamp_reserve_ms(ms: i64) -> u32 {
    if ms <= 0 {
        return RESERVE_MS_DEFAULT as u32;
    }
    if ms < RESERVE_MS_MIN {
        return RESERVE_MS_MIN as u32;
    }
    if ms > RESERVE_MS_MAX {
        return RESERVE_MS_MAX as u32;
    }
    ms as u32
}

/// `floor(samples × 1e9 / rate)` without overflowing 64 bits for any realistic sample count.
pub fn samples_to_ns(samples: u64, rate: u32) -> u64 {
    let r = u64::from(rate.max(1));
    (samples / r) * NS_PER_SEC + (samples % r) * NS_PER_SEC / r
}

/// `ns × ppb / 1e9`, truncated toward zero, without overflowing 64 bits.
pub fn scale_ppb(ns: u64, ppb: i64) -> i64 {
    let mag = ppb.unsigned_abs();
    let v = (ns / NS_PER_SEC) * mag + (ns % NS_PER_SEC) * mag / NS_PER_SEC;
    if ppb < 0 {
        -(v as i64)
    } else {
        v as i64
    }
}

/// `VBAN_RECV_ROUND_LIMIT`: the rounding saturates at +-2^62 (a C cast of an out-of-range double is
/// undefined; 2^62 is far beyond any sane fit, yet clear of overflow when the anchor and R are added).
pub const ROUND_LIMIT: i64 = 1 << 62;

/// Round half away from zero (the C header uses no libm). At or beyond +-[`ROUND_LIMIT`] it
/// saturates there, and NaN reads 0, exactly like the C twin.
pub fn round_half_away(x: f64) -> i64 {
    let lim = ROUND_LIMIT as f64;
    if x.is_nan() {
        return 0;
    }
    if x >= lim {
        return ROUND_LIMIT;
    }
    if x <= -lim {
        return -ROUND_LIMIT;
    }
    if x >= 0.0 {
        (x + 0.5) as i64
    } else {
        -((-x + 0.5) as i64)
    }
}

fn clamp_i64(v: i64, lim: i64) -> i64 {
    v.clamp(-lim, lim)
}

impl Timeline {
    /// An empty, not yet anchored timeline (`vban_recv_init`).
    pub fn new() -> Self {
        Timeline {
            anchored: false,
            rate: 0,
            channels: 0,
            reserve_ns: 0,
            reserve_eff_ns: 0,
            anchor_ns: 0,
            anchor_end: 0,
            last_nu: 0,
            next_pos: 0,
            base_stamp_ns: 0,
            base_pos: 0,
            step_ppb: 0,
            last_k: 0,
            last_fold_pos: 0,
            buckets: [Bucket::default(); FIT_WINDOW_S],
            fit_valid: false,
            fit_ppb: 0,
            fit_b: 0.0,
            fit_xbar: 0.0,
            fit_ybar: 0.0,
            phase_ns: 0,
            shift_ns: 0,
            received: 0,
            lost: 0,
            silence_samples: 0,
            late: 0,
            re_anchors: 0,
            stale: 0,
        }
    }

    /// The stamp of the sample at `pos` (samples since the anchor, never before the line's base).
    pub fn stamp_at(&self, pos: u64) -> u64 {
        let span = pos.saturating_sub(self.base_pos);
        let ns = samples_to_ns(span, self.rate);
        self.base_stamp_ns
            .wrapping_add((ns as i64 + scale_ppb(ns, self.step_ppb)) as u64)
    }

    fn fold(&mut self, k: i64, x: u64, y: u64) {
        let b = &mut self.buckets[(k % FIT_WINDOW_S as i64) as usize];
        if b.k != k {
            *b = Bucket {
                k,
                n: 0,
                x0: x,
                y0: y,
                su: 0.0,
                sv: 0.0,
                suu: 0.0,
                suv: 0.0,
            };
        }
        // unsigned and wrapping like the C twin (an out-of-order arrival never panics)
        let u = x.wrapping_sub(b.x0) as f64;
        let v = y.wrapping_sub(b.y0) as f64;
        b.n += 1;
        b.su += u;
        b.sv += v;
        b.suu += u * u;
        b.suv += u * v;
        self.last_fold_pos = x;
    }

    /// The arrival (ns since the anchor's arrival) the reference line expects at end position
    /// `x`: the fit once it is valid, else the anchored line − R (the earliest arrival of the
    /// first second at the nominal rate; the first packet itself may have come late).
    pub fn reference(&self, x: u64) -> f64 {
        if self.fit_valid {
            return self.fit_ybar + self.fit_b * (x as f64 - self.fit_xbar);
        }
        (self.stamp_at(x) as i64 - self.reserve_eff_ns as i64 - self.anchor_ns as i64) as f64
    }

    /// The sender's clock offset in ppm from the fit (positive = the sender runs fast), 0 before
    /// the fit is valid. Display only.
    pub fn sender_ppm(&self) -> f64 {
        if self.fit_valid {
            -(self.fit_ppb as f64) / 1000.0
        } else {
            0.0
        }
    }

    fn anchor(
        &mut self,
        now_ns: u64,
        nu: u32,
        frames: u32,
        rate: u32,
        channels: u32,
        reserve_ns: u64,
    ) {
        self.anchored = true;
        self.rate = rate;
        self.channels = channels;
        self.reserve_ns = reserve_ns;
        let len = samples_to_ns(u64::from(frames), rate);
        self.reserve_eff_ns = reserve_ns.max(len + RESERVE_MARGIN_MS * NS_PER_MS);
        self.anchor_ns = now_ns;
        self.anchor_end = u64::from(frames);
        self.last_nu = nu;
        self.next_pos = u64::from(frames);
        self.base_stamp_ns = now_ns.saturating_sub(len) + self.reserve_eff_ns;
        self.base_pos = 0;
        self.step_ppb = 0;
        for b in self.buckets.iter_mut() {
            b.k = -1;
        }
        self.fit_valid = false;
        self.fit_ppb = 0;
        self.fit_b = 0.0;
        self.fit_xbar = 0.0;
        self.fit_ybar = 0.0;
        self.phase_ns = 0;
        self.shift_ns = 0;
        self.last_k = i64::from(frames / rate);
        self.fold(self.last_k, u64::from(frames), 0);
    }

    /// Refit over the completed buckets and move the stamp step, rebasing the line at `pos`.
    /// Returns false when the fit cannot hold.
    pub fn update(&mut self, pos: u64) -> bool {
        let lo = self.last_k - FIT_WINDOW_S as i64 + 1;
        let mut total: u64 = 0;
        let mut buckets = 0usize;
        let mut sx = 0.0f64;
        let mut sy = 0.0f64;
        for k in lo..=self.last_k {
            if k < 0 {
                continue;
            }
            let b = &self.buckets[(k % FIT_WINDOW_S as i64) as usize];
            if b.k != k || b.n == 0 {
                continue;
            }
            let n = b.n as f64;
            total += b.n;
            buckets += 1;
            sx += n * (b.x0 as f64 + b.su / n);
            sy += n * (b.y0 as f64 + b.sv / n);
        }
        if buckets < FIT_MIN_BUCKETS {
            return true;
        }
        let nt = total as f64;
        let xbar = sx / nt;
        let ybar = sy / nt;
        let mut sxx = 0.0f64;
        let mut sxy = 0.0f64;
        for k in lo..=self.last_k {
            if k < 0 {
                continue;
            }
            let b = &self.buckets[(k % FIT_WINDOW_S as i64) as usize];
            if b.k != k || b.n == 0 {
                continue;
            }
            let n = b.n as f64;
            let dx = b.x0 as f64 + b.su / n - xbar;
            let dy = b.y0 as f64 + b.sv / n - ybar;
            sxx += (b.suu - b.su * b.su / n) + n * dx * dx;
            sxy += (b.suv - b.su * b.sv / n) + n * dx * dy;
        }
        // The C reads `!(sxx > 0.0)`: a NaN must not pass either.
        if sxx.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            return true;
        }
        let slope = sxy / sxx;
        let fit_ppb = round_half_away(slope * f64::from(self.rate) - NS_PER_SEC as f64);
        if !(-PPB_CLAMP..=PPB_CLAMP).contains(&fit_ppb) {
            return false;
        }
        self.fit_valid = true;
        self.fit_ppb = fit_ppb;
        self.fit_b = slope;
        self.fit_xbar = xbar;
        self.fit_ybar = ybar;
        let target = self.anchor_ns as i64
            + round_half_away(ybar + slope * (pos as f64 - xbar))
            + self.reserve_eff_ns as i64
            + self.shift_ns;
        let stamp = self.stamp_at(pos);
        let phase = target - stamp as i64;
        self.phase_ns = phase;
        let hold = self.reserve_eff_ns as i64;
        if !(-hold..=hold).contains(&phase) {
            return false;
        }
        // the dead band: inside it the latency is held and only the rate is followed
        let band = hold / PHASE_BAND_DIV;
        let corr = if (-band..=band).contains(&phase) {
            0
        } else {
            clamp_i64(phase / PHASE_TAU_S, PHASE_PPB_MAX)
        };
        let want = clamp_i64(fit_ppb + corr, PPB_CLAMP);
        let delta = clamp_i64(want - self.step_ppb, PPB_SLEW);
        if delta != 0 {
            self.base_stamp_ns = stamp;
            self.base_pos = pos;
            self.step_ppb += delta;
        }
        true
    }

    /// The decision for a packet arriving at `now_ns` (`vban_recv_packet`).
    pub fn packet(
        &mut self,
        now_ns: u64,
        nu: u32,
        frames: u32,
        rate: u32,
        channels: u32,
        reserve_ns: u64,
    ) -> Decision {
        let mut d = Decision::default();
        let frames = frames.max(1);
        let rate = rate.max(1);
        self.received += 1;
        if !self.anchored {
            self.anchor(now_ns, nu, frames, rate, channels, reserve_ns);
            d.event = Event::Anchor;
            d.stamp_ns = self.base_stamp_ns;
            d.lateness_ns = now_ns as i64 - d.stamp_ns as i64;
            return d;
        }
        let mut reason = Reason::None;
        let mut lost: u64 = 0;
        let mut lost_samples: u64 = 0;
        if rate != self.rate || channels != self.channels {
            reason = Reason::Format;
        } else if reserve_ns != self.reserve_ns {
            reason = Reason::Config;
        } else {
            let delta = nu.wrapping_sub(self.last_nu);
            if delta == 0 || delta >= 0x8000_0000 {
                if self.last_nu.wrapping_sub(nu) <= STALE_PACKETS {
                    self.stale += 1;
                    d.event = Event::Stale;
                    return d;
                }
                reason = Reason::Backward;
            } else {
                lost = u64::from(delta - 1);
                lost_samples = lost * u64::from(frames);
                if lost_samples > JUMP_S * u64::from(rate) {
                    reason = Reason::Jump;
                }
            }
        }
        let mut pos = 0u64;
        let mut end = 0u64;
        let mut k = 0i64;
        if reason == Reason::None {
            pos = self.next_pos + lost_samples;
            end = pos + u64::from(frames);
            k = (end / u64::from(rate)) as i64;
            if k > self.last_k {
                let at = self.next_pos;
                if !self.update(at) {
                    reason = Reason::Fit;
                }
            }
            if reason == Reason::None && end - self.last_fold_pos > STARVE_S * u64::from(rate) {
                reason = Reason::Fit;
            }
        }
        // the first second: a packet that arrived earlier against the line moves the line to it.
        // The line is still `base + T(pos)` there (no fit has moved it), so the candidate is the
        // anchor this packet would give (arrival − length + R) carried back to position 0 by the
        // line's own arithmetic: a packet exactly on the line never moves it by a rounding ns.
        if reason == Reason::None
            && end <= ACQUIRE_S * u64::from(rate)
            && self.base_pos == 0
            && self.step_ppb == 0
        {
            let back = samples_to_ns(u64::from(frames), rate) + samples_to_ns(pos, rate);
            let cand = now_ns.saturating_sub(back) + self.reserve_eff_ns;
            if cand < self.base_stamp_ns {
                self.base_stamp_ns = cand;
            }
        }
        if reason == Reason::None {
            let mut stamp = self.stamp_at(pos);
            let mut shifted = false;
            // the hold (ROZHODNUTÉ 6345255813 item 1): nothing is emitted while the packet ends
            // within the first second, so libobs never sees the refinement's stamp steps; a slot is
            // filled only once the previous packet was emitted
            let window = ACQUIRE_S * u64::from(rate);
            let held = end <= window;
            let emitting = self.next_pos > window;
            if !held && now_ns > stamp {
                d.late_ns = (now_ns - stamp) as i64;
                self.late += 1;
                if self.fit_valid {
                    // one network event = one re-anchor: the line shifts once to the stamp this
                    // packet would anchor, rebased at the first lost slot (silence on the new
                    // line), the fit kept; the excess over the fit becomes the held shift
                    let want = now_ns.saturating_sub(samples_to_ns(u64::from(frames), rate))
                        + self.reserve_eff_ns;
                    let fitted = self.anchor_ns as i64
                        + round_half_away(
                            self.fit_ybar + self.fit_b * (pos as f64 - self.fit_xbar),
                        )
                        + self.reserve_eff_ns as i64;
                    self.shift_ns = want as i64 - fitted;
                    let span = samples_to_ns(pos - self.next_pos, rate);
                    self.base_stamp_ns =
                        want.wrapping_sub((span as i64 + scale_ppb(span, self.step_ppb)) as u64);
                    self.base_pos = self.next_pos;
                    self.re_anchors += 1;
                    stamp = self.stamp_at(pos);
                    shifted = true;
                } else {
                    reason = Reason::Late;
                }
            }
            if reason == Reason::None {
                if shifted {
                    d.event = Event::Reanchor;
                    d.reason = Reason::Late;
                    d.shifted = true;
                } else if held {
                    d.event = Event::Hold;
                } else {
                    d.event = Event::Packet;
                }
                d.stamp_ns = stamp;
                d.lateness_ns = now_ns as i64 - stamp as i64;
                if lost > 0 && emitting {
                    d.silence_samples = lost_samples;
                    d.silence_pos = self.next_pos;
                    self.lost += lost;
                    self.silence_samples += lost_samples;
                }
                // fold the packet into the fit unless it came late against the reference line
                // wrapping like the C twin (the real clock never delivers an arrival before the anchor)
                let y = now_ns.wrapping_sub(self.anchor_ns);
                if y as f64 - self.reference(end) <= (OUTLIER_MS * NS_PER_MS) as f64 {
                    self.fold(k, end, y);
                }
                if k > self.last_k {
                    self.last_k = k;
                }
                self.last_nu = nu;
                self.next_pos = end;
                return d;
            }
        }
        self.re_anchors += 1;
        self.anchor(now_ns, nu, frames, rate, channels, reserve_ns);
        d.event = Event::Reanchor;
        d.reason = reason;
        d.stamp_ns = self.base_stamp_ns;
        d.lateness_ns = now_ns as i64 - d.stamp_ns as i64;
        d
    }
}

#[cfg(test)]
#[path = "vban_receive_timeline_bench.rs"]
mod bench;

#[cfg(test)]
#[path = "vban_receive_timeline_tests.rs"]
mod tests;
