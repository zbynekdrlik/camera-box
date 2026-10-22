#!/usr/bin/env python3
"""Issue 1460 slice 1 -- the PURE packet planner of the lv1 VBAN emulator (scripts/vban_emulator.py).

WHY: the issue-1439 receive source (fixed reserve R, following the sender's clock, the genlock ASRC,
a lost packet = silence for exactly its slot) must be proven against a sender like the PP console
lv1: a VBAN stream on its OWN clock that arrives in bursts with gaps of up to ~30 ms (the strih hub
measured the largest 10-min gap of lv1 at 26.3-29.1 ms, issue 1439 design 6338127636). lv1 is off
at PP, so the proof runs at SNV against a sender this module schedules (design 6338685774).

WHAT IT PLANS, per packet n (256 frames, PCM16 stereo, 48 kHz):
  * the SENDER-CLOCK slot: `deadline_ns(n) = n x 256 / (48000 x (1 + ppm))` s, computed in integer
    nanoseconds from n (never accumulated), so the average rate is exactly 48000 x (1 + ppm) frames
    per second however long the run is. ppm > 0 = the sender's clock runs FAST (more samples per
    real second than nominal), the case the ASRC must slow down;
  * the RELEASE instant: the slot plus a delay-only arrival jitter (|N(0, sd)|, a network never
    delivers early), held while a STALL runs, never before the previous release (order kept). A
    stall is sized as the SEND GAP it opens (the hub measured lv1's inter-arrival gaps): nothing
    leaves until `gap` after the previous release, then every packet whose slot fell inside leaves
    as one burst;
  * the VBAN frame counter `nuFrame` = start_frame + n (mod 2^32), counted for EVERY packet, a lost
    one included -- that gap is how a receiver knows a packet is missing;
  * scheduled LOSSES: packets that are generated (their samples advance the pilot's phase
    accumulator, their counter is used) but never sent. A correct receiver plays silence for exactly
    those slots and the pilot continues after them as if they had played.

PROFILES (`PROFILES`): `clean` (no jitter, no stall, no loss, 0 ppm), `lv1` (+1.4 ppm -- the hub servo's
measured lv1 offset --, 0.3 ms jitter, a send gap of 8-29 ms on average every 20 s, three losses)
and `stress` (+50 ppm, 1 ms jitter, a send gap of 15-35 ms on average every 6 s, four losses up to 8
packets). Any profile takes a `--ppm` override (the bench runs `stress:-50` for the other sign).

The pilot is a continuous sine from a phase accumulator at the NOMINAL 48 kHz: sample k of the
stream (k = n x 256 + i) carries sin(2 pi f k / 48000). Both channels carry it. A receiver that
drops or repeats even one sample shows a phase step in the recording (scripts/vban_sine_continuity.py).

The VBAN header is the ONE layout of scripts/vban_rate.py (magic, rate index | sub-protocol, nbs-1,
nbc-1, format, 16-byte name, nuFrame LE); `vban_header` builds it and the tests read it back through
`vban_rate.parse_vban`.

The rig rule "only the QPSK marker may sound" (owner): the pilot goes only into an OBS TEST input
routed to a dedicated recording track (scripts/vban-receiver-bench.sh). `port_refusal` refuses the
ports a real receiver on the rig listens on, so the emulator can never feed the program path.

PURE: no I/O, no clock, no socket. numpy only for the pilot samples. Tier-0 tests:
tests/python/test_vban_emulator_plan_1460.py.
"""
from __future__ import annotations

import math
import random
import struct
from dataclasses import dataclass

import numpy as np

from vban_rate import SR_TABLE, VBAN_HEADER_LEN, VBAN_MAGIC

SAMPLE_RATE = 48000
FRAMES_PER_PACKET = 256
CHANNELS = 2
# VBAN data format byte: bit resolution index 1 = 16-bit signed integer, codec 0 = PCM.
VBAN_FMT_PCM16 = 0x01
SR_INDEX = SR_TABLE.index(SAMPLE_RATE)
STREAM_NAME_MAX = 16
NS_PER_S = 1_000_000_000
# ppm is planned in units of 0.001 ppm so the slot formula stays in exact integers.
PPM_SCALE = 1000
PPM_LIMIT = 2000.0
# A scheduled loss must leave this much pilot before and after it inside the run (the analysis
# needs pilot on both sides of a silence slot to check the timeline across it).
LOSS_EDGE_S = 1.0
DEFAULT_PILOT_HZ = 997.0   # prime, so 256 samples (one packet) is never a whole number of periods
DEFAULT_LEVEL_DBFS = -12.0

# The ports a real VBAN receiver on the rig listens on (the strih intercom hub + the stream-role
# program input, the strih-role program input of the issue-1439 design, the hub's Janus RTP leg).
RESERVED_PORTS = {
    6980: "the intercom hub / the stream-role OBS program input",
    6981: "the strih-role OBS program input (issue 1439 design 6338457001)",
    6990: "the intercom hub's Janus RTP leg",
}


@dataclass(frozen=True)
class Profile:
    name: str
    ppm: float               # the sender clock offset
    jitter_sd_ms: float      # delay-only arrival jitter, |N(0, sd)|
    stall_every_s: float     # mean time between stalls (exponential), 0 = never
    stall_min_ms: float
    stall_max_ms: float      # a stall's send gap is uniform in [min, max] (min > one slot, 5.33 ms)
    losses: tuple            # default losses: (fraction of the run, consecutive packets)


PROFILES = {
    "clean": Profile("clean", 0.0, 0.0, 0.0, 0.0, 0.0, ()),
    "lv1": Profile("lv1", 1.4, 0.3, 20.0, 8.0, 29.0, ((0.25, 1), (0.5, 2), (0.75, 3))),
    "stress": Profile("stress", 50.0, 1.0, 6.0, 15.0, 35.0, ((0.2, 1), (0.4, 2), (0.6, 4), (0.8, 8))),
}


@dataclass(frozen=True)
class PlannedPacket:
    index: int        # n, from 0
    nu_frame: int     # the VBAN frame counter
    deadline_ns: int  # the sender-clock slot, from the run start
    release_ns: int   # when the emulator sends it, from the run start (>= the previous release)
    lost: bool        # scheduled loss: never sent
    stalled: bool     # held by a stall (left in its burst)


@dataclass(frozen=True)
class Loss:
    packet: int
    count: int

    @property
    def sample(self):
        return self.packet * FRAMES_PER_PACKET


def ppm_units(ppm):
    """ppm -> integer units of 0.001 ppm. Raises ValueError outside +-PPM_LIMIT or when not finite."""
    if not isinstance(ppm, (int, float)) or not math.isfinite(ppm):
        raise ValueError("ppm must be a finite number")
    if abs(ppm) > PPM_LIMIT:
        raise ValueError("ppm %.3f is outside +-%.0f" % (ppm, PPM_LIMIT))
    return int(round(ppm * PPM_SCALE))


def deadline_ns(n, ppm_u):
    """The sender-clock slot of packet n in ns from the run start: n x 256 / (48000 x (1 + ppm)) s,
    floored to whole ns from n alone (never a running sum, so no drift accumulates)."""
    return (n * FRAMES_PER_PACKET * NS_PER_S * NS_PER_S) // (SAMPLE_RATE * (NS_PER_S + ppm_u))


def packet_count(duration_s, ppm_u):
    """The packets whose slot falls inside [0, duration_s)."""
    if duration_s <= 0:
        raise ValueError("duration must be positive")
    dur_ns = int(round(duration_s * NS_PER_S))
    # the first n with deadline_ns(n) >= dur_ns
    n = (dur_ns * SAMPLE_RATE * (NS_PER_S + ppm_u)) // (FRAMES_PER_PACKET * NS_PER_S * NS_PER_S)
    while deadline_ns(n, ppm_u) < dur_ns:
        n += 1
    while n > 0 and deadline_ns(n - 1, ppm_u) >= dur_ns:
        n -= 1
    return n


def first_packet_at(t_s, ppm_u):
    """The first packet whose slot is at or after t_s."""
    return packet_count(t_s, ppm_u) if t_s > 0 else 0


def parse_losses(spec):
    """'T:N,T:N' (T seconds into the run, N consecutive packets) -> [(T, N)]; 'none' / '' -> [].
    Raises ValueError on anything else."""
    spec = (spec or "").strip()
    if spec in ("", "none"):
        return []
    out = []
    for tok in spec.split(","):
        t, sep, n = tok.strip().partition(":")
        if not sep:
            raise ValueError("loss %r is not T:N" % tok)
        t_s, count = float(t), int(n)
        if not math.isfinite(t_s) or count < 1:
            raise ValueError("loss %r needs a time and a count >= 1" % tok)
        out.append((t_s, count))
    return out


def default_losses(profile, duration_s):
    """The profile's losses at their fractions of the run, as (T, N)."""
    return [(frac * duration_s, count) for frac, count in profile.losses]


def plan_losses(loss_times, duration_s, ppm_u):
    """[(T, N)] -> sorted [Loss]. Raises ValueError for a loss within LOSS_EDGE_S of either end of
    the run, past its last packet, or overlapping (or adjacent to) another loss."""
    n_packets = packet_count(duration_s, ppm_u)
    losses = []
    for t_s, count in loss_times:
        if t_s < LOSS_EDGE_S or t_s > duration_s - LOSS_EDGE_S:
            raise ValueError("loss at %.3f s is within %.1f s of the run's ends" % (t_s, LOSS_EDGE_S))
        p = first_packet_at(t_s, ppm_u)
        if p + count > n_packets:
            raise ValueError("loss at %.3f s (%d packets) runs past the last packet" % (t_s, count))
        losses.append(Loss(p, count))
    losses.sort(key=lambda lo: lo.packet)
    for a, b in zip(losses, losses[1:]):
        if b.packet <= a.packet + a.count:
            raise ValueError("losses at packets %d and %d overlap or touch" % (a.packet, b.packet))
    end_edge = packet_count(duration_s - LOSS_EDGE_S, ppm_u)
    for lo in losses:
        if lo.packet + lo.count > end_edge:
            raise ValueError("loss at packet %d ends within %.1f s of the run's end" % (lo.packet, LOSS_EDGE_S))
    return losses


def plan(profile, duration_s, seed=0, ppm=None, losses=None, start_frame=0):
    """Yield every PlannedPacket of one run, in order.

    profile: a Profile; ppm overrides its offset; losses: [Loss] (None = the profile's defaults);
    start_frame: the first nuFrame. Deterministic for a given seed."""
    ppm_u = ppm_units(profile.ppm if ppm is None else ppm)
    if not 0 <= start_frame < 2 ** 32:
        raise ValueError("start_frame must be a 32-bit counter")
    if losses is None:
        losses = plan_losses(default_losses(profile, duration_s), duration_s, ppm_u)
    lost = set()
    for lo in losses:
        lost.update(range(lo.packet, lo.packet + lo.count))
    rng = random.Random(seed)
    sd_ns = profile.jitter_sd_ms * 1e6
    stall_start = stall_end = None
    next_stall = _next_stall_at(rng, profile, 0)
    prev_release = 0
    for n in range(packet_count(duration_s, ppm_u)):
        slot = deadline_ns(n, ppm_u)
        if next_stall is not None and slot >= next_stall:
            # the stall is sized as the SEND GAP it opens (the hub measured lv1's gaps, not its
            # hold windows): nothing leaves until `gap` after the previous release, then a burst
            gap = rng.uniform(profile.stall_min_ms, profile.stall_max_ms) * 1e6
            stall_start, stall_end = slot, prev_release + int(gap)
            next_stall = _next_stall_at(rng, profile, stall_end)
        jitter = int(abs(rng.gauss(0.0, sd_ns))) if sd_ns > 0 else 0
        release = slot + jitter
        stalled = stall_start is not None and stall_start <= slot < stall_end
        if stalled:
            release = max(release, stall_end)
        release = max(release, prev_release)
        prev_release = release
        yield PlannedPacket(n, (start_frame + n) % 2 ** 32, slot, release, n in lost, stalled)


def _next_stall_at(rng, profile, after_ns):
    if profile.stall_every_s <= 0 or profile.stall_max_ms <= 0:
        return None
    return after_ns + int(rng.expovariate(1.0 / profile.stall_every_s) * NS_PER_S)


def summarize(packets):
    """One dict describing a planned run: counts, the effective rate, the stalls / bursts, the
    largest gap between two SENT packets and the largest release delay."""
    n = sent = lost = 0
    last_slot = 0
    prev_sent = None
    max_gap = 0
    max_late = 0
    burst = max_burst = 0
    stalls = 0
    prev_stalled = False
    for p in packets:
        n += 1
        last_slot = p.deadline_ns
        max_late = max(max_late, p.release_ns - p.deadline_ns)
        if p.stalled and not prev_stalled:
            stalls += 1
        prev_stalled = p.stalled
        if p.lost:
            lost += 1
            continue
        sent += 1
        if prev_sent is not None:
            gap = p.release_ns - prev_sent
            max_gap = max(max_gap, gap)
            burst = burst + 1 if gap == 0 else 1
        else:
            burst = 1
        max_burst = max(max_burst, burst)
        prev_sent = p.release_ns
    rate = (n - 1) * FRAMES_PER_PACKET * NS_PER_S / last_slot if n > 1 and last_slot else None
    return {
        "packets": n, "sent": sent, "lost": lost, "stalls": stalls,
        "rate_hz": rate,
        "rate_ppm": (rate / SAMPLE_RATE - 1.0) * 1e6 if rate else None,
        "max_send_gap_ms": max_gap / 1e6, "max_burst_packets": max_burst,
        "max_release_delay_ms": max_late / 1e6,
    }


def amplitude(level_dbfs):
    """dBFS (peak, full scale = 1.0) -> the linear peak. Refuses a level above -1 dBFS."""
    if not math.isfinite(level_dbfs) or level_dbfs > -1.0:
        raise ValueError("pilot level must be a finite dBFS <= -1")
    return 10.0 ** (level_dbfs / 20.0)


def pilot_block(first_sample, frames, freq_hz, amp):
    """The pilot for stream samples [first_sample, first_sample + frames): little-endian PCM16,
    stereo interleaved, both channels equal. The phase is 2 pi x frac(f k / 48000) from the absolute
    sample index k, so a run's phase never accumulates rounding."""
    if not 0 < freq_hz < SAMPLE_RATE / 2:
        raise ValueError("pilot frequency must be inside (0, 24000) Hz")
    k = np.arange(first_sample, first_sample + frames, dtype=np.float64)
    cycles = freq_hz * k / SAMPLE_RATE
    phase = 2.0 * np.pi * (cycles - np.floor(cycles))
    mono = np.round(np.sin(phase) * amp * 32767.0).astype("<i2")
    return np.repeat(mono, CHANNELS).tobytes()


def stream_name_ok(name):
    """VBAN stream name: 1..16 printable ASCII characters."""
    return (isinstance(name, str) and 0 < len(name) <= STREAM_NAME_MAX
            and all(32 < ord(c) < 127 for c in name))


def vban_header(stream_name, nu_frame, frames=FRAMES_PER_PACKET, channels=CHANNELS):
    """The 28-byte VBAN audio header (scripts/vban_rate.py layout)."""
    if not stream_name_ok(stream_name):
        raise ValueError("stream name must be 1..16 printable ASCII characters")
    if not 1 <= frames <= 256 or not 1 <= channels <= 256:
        raise ValueError("frames and channels must be 1..256")
    name = stream_name.encode("ascii").ljust(STREAM_NAME_MAX, b"\x00")
    hdr = VBAN_MAGIC + struct.pack("<BBBB", SR_INDEX, frames - 1, channels - 1, VBAN_FMT_PCM16)
    hdr += name + struct.pack("<I", nu_frame % 2 ** 32)
    assert len(hdr) == VBAN_HEADER_LEN
    return hdr


def packet_bytes(stream_name, packet, freq_hz, amp):
    """The datagram of one planned packet."""
    return vban_header(stream_name, packet.nu_frame) + pilot_block(
        packet.index * FRAMES_PER_PACKET, FRAMES_PER_PACKET, freq_hz, amp)


def port_refusal(port):
    """None for a usable UDP port, else why it is refused."""
    if not isinstance(port, int) or not 1024 <= port <= 65535:
        return "port %r is not a UDP port in 1024..65535" % (port,)
    if port in RESERVED_PORTS:
        return "port %d belongs to %s -- the pilot must never reach a real program/intercom input" % (
            port, RESERVED_PORTS[port])
    return None


def schedule_document(profile, ppm, duration_s, seed, losses, start_frame, stream_name, pilot_hz,
                      level_dbfs, summary):
    """The schedule JSON the analysis grades against (scripts/vban_sine_continuity.py --schedule)."""
    ppm_u = ppm_units(ppm)
    return {
        "schema": 1,
        "profile": profile.name,
        "ppm": ppm_u / PPM_SCALE,
        "duration_s": duration_s,
        "seed": seed,
        "sample_rate": SAMPLE_RATE,
        "frames_per_packet": FRAMES_PER_PACKET,
        "channels": CHANNELS,
        "start_frame": start_frame,
        "stream_name": stream_name,
        "pilot_hz": pilot_hz,
        "level_dbfs": level_dbfs,
        "losses": [{"packet": lo.packet, "count": lo.count, "sample": lo.sample,
                    "time_s": deadline_ns(lo.packet, ppm_u) / NS_PER_S} for lo in losses],
        "summary": summary,
    }
