#!/usr/bin/env python3
"""Issue 1372 part C -- measure every VBAN stream in a packet capture: rate by FRAME COUNTER + loss.

WHY: the owner's contract is that audio travelling BETWEEN PCs (Dante natively, VBAN by virtue of
every PC ticking on the dantesync-disciplined clock) runs in the Dante tick. Nothing measured it.
The FOH clicks were found by hand with pktmon, and the first reading was WRONG: the stream rate was
derived from the PACKET COUNT, so 5 lost packets in 90 s read as -132.9 ppm when the stream really
ran +18.1 ppm (#1367 comment 5832526338). A count mixes two independent faults (clock rate and
packet loss) into one number. This module separates them:

  * RATE comes from the VBAN frame COUNTER (the 32-bit `nuFrame` in every header): the sample index
    of a packet is nuFrame x samples-per-frame, and the stream's sample clock is the least-squares
    slope of that index against the capture time. A lost packet leaves a hole in the counter but
    moves no point off the line, so loss never biases the rate. Network and sender delay only ever
    make an arrival LATER, so a plain fit over every arrival is biased by one stall-then-burst
    (+65.7 ppm for one 500 ms stall in 60 s, review round 1): the fit is ONE-SIDED TRIMMED -- points
    arriving later than the line by more than LATE_TRIM_MADS robust deviations (and at least
    LATE_TRIM_FLOOR_S) are dropped and the line refitted, a few times. Symmetric jitter is bounded,
    so it keeps every point (full least-squares precision); a stalled burst is dropped.
  * LOSS is the hole count: per counter segment, (max - min + 1) - unique frames. A duplicate (pktmon
    captures one packet at several components) or a reorder is therefore never counted as loss.
  * A JUMP is a counter discontinuity the elapsed time cannot explain (a sender restart resets the
    counter). The fit pools the segments (a common slope, one intercept per segment), so a restart
    neither breaks the rate nor turns into fake loss.
  * The max inter-packet gap and the fit residual (the arrival jitter, obs-vban sends in bursts)
    are reported alongside.

The capture's timestamps must come from a DISCIPLINED clock for the rate to mean "vs the Dante
tick": strih-lx (Linux, adjtimex slews CLOCK_REALTIME and CLOCK_MONOTONIC) is the reference
receiver the dev1 watchdog captures on. A capture needs snaplen >= 96 so the 28-byte VBAN header
survives the link + IPv4 + UDP headers.

Formats: classic pcap (either byte order, usec or nsec) with linktype LINUX_SLL2 276 (`tcpdump -i
any` on a current kernel), LINUX_SLL 113 (an older tcpdump), EN10MB 1 (one NIC, incl. 802.1Q) or
raw IPv4 (101/228); and pcapng (pktmon `etl2pcap`, any if_tsresol). IPv4/UDP only; fragments and
non-VBAN datagrams are skipped.

PURE: no I/O outside `main`. Tier-0 tests: tests/python/test_vban_rate_1372.py.

Usage:
  vban_rate.py analyze CAPTURE [--dst IP ...] [--ppm-bound N] [--loss-ceiling X]
                               [--min-span-s S] [--json | --tsv]
Exit: 0 = analysed (the verdict is in the output, report-only), 2 = unreadable capture / usage.
"""
from __future__ import annotations

import argparse
import json
import math
import struct
import sys
from dataclasses import asdict, dataclass, field

# VBAN sample-rate index table (VBAN specification, the 5 low bits of header byte 4).
SR_TABLE = [6000, 12000, 24000, 48000, 96000, 192000, 384000, 8000, 16000, 32000, 64000, 128000,
            256000, 512000, 11025, 22050, 44100, 88200, 176400, 352800, 705600]
VBAN_MAGIC = b"VBAN"
VBAN_HEADER_LEN = 28
VBAN_SUBPROTOCOL_AUDIO = 0
MIN_SNAPLEN = 96

# A backwards counter step larger than this many frames is a sender restart, not a reorder.
JUMP_BACK_FRAMES = 64
# A step back to a counter ALREADY seen more than this long ago is a sender restart, not a network
# duplicate (a pktmon/network duplicate arrives within microseconds to a few ms of the original).
DUP_WINDOW_S = 0.05
# How far the restart-vs-straggler lookahead may skip over counters the segment already holds.
LOOKAHEAD_MAX = 256
# Packets arriving closer together than this were released as one burst: one timing observation.
CLUSTER_GAP_S = 0.001
# The one-sided trim: drop points LATER than the line by more than max(LATE_TRIM_MADS robust
# standard deviations, LATE_TRIM_FLOOR_S) above the median lateness; refit up to LATE_TRIM_ROUNDS.
LATE_TRIM_MADS = 4.0
LATE_TRIM_FLOOR_S = 0.002
LATE_TRIM_ROUNDS = 4
# A rate is only a FAULT when it clears the bound by this many standard errors of the fit.
RATE_STDERR_MARGIN = 2.0
# A forward step is a jump when it exceeds twice the frames the elapsed time can explain plus this
# slack (a real network outage advances the counter by ~the elapsed frames: that is LOSS).
JUMP_FWD_SLACK_FRAMES = 64

DEFAULT_PPM_BOUND = 20.0      # provisional -- calibrate from data once part A (the Windows OBS
                              # media clock) is live; before it Windows senders sit ~10-20 ppm off
DEFAULT_LOSS_CEILING = 1e-4   # provisional; the fohabl VBAN OUT loss measured on 25.9. at strih-lx
                              # was 8.8e-4..1.05e-3 (a click source) -- ~10x over this ceiling
DEFAULT_MIN_SPAN_S = 20.0     # a shorter stream cannot resolve a few ppm through the arrival jitter
MIN_FRAMES = 50


class CaptureError(ValueError):
    """The bytes are not a capture this module can read."""


@dataclass(frozen=True)
class VbanHeader:
    name: str
    sr_index: int
    sample_rate: int
    samples_per_frame: int
    channels: int
    frame: int


@dataclass(frozen=True)
class Packet:
    ts_ns: int
    src: str
    sport: int
    dst: str
    dport: int
    payload: bytes  # the CAPTURED prefix of the UDP payload (snaplen may cut it)


@dataclass
class StreamStats:
    key: str
    name: str
    src: str
    sport: int
    dst: str
    dport: int
    sample_rate: int
    samples_per_frame: int
    channels: int
    packets: int
    unique_frames: int
    duplicates: int
    reordered: int
    lost: int
    loss_ratio: float
    jumps: int
    segments: int
    span_s: float
    rate_ppm: float | None
    rate_stderr_ppm: float | None
    max_gap_ms: float
    resid_rms_ms: float | None
    resid_max_ms: float | None


@dataclass
class CaptureResult:
    linktype: int
    packets: int
    streams: list = field(default_factory=list)
    truncated_vban: int = 0


@dataclass(frozen=True)
class Grading:
    ppm_bound: float = DEFAULT_PPM_BOUND
    loss_ceiling: float = DEFAULT_LOSS_CEILING
    min_span_s: float = DEFAULT_MIN_SPAN_S


# ---------------------------------------------------------------------------------------------
# VBAN
# ---------------------------------------------------------------------------------------------

def parse_vban(payload: bytes) -> VbanHeader | None:
    """The VBAN AUDIO header of a UDP payload, or None (not VBAN, another sub-protocol, cut short)."""
    if len(payload) < VBAN_HEADER_LEN or payload[:4] != VBAN_MAGIC:
        return None
    sr_byte, nbs, nbc = payload[4], payload[5], payload[6]
    if (sr_byte >> 5) != VBAN_SUBPROTOCOL_AUDIO:
        return None
    sr_index = sr_byte & 0x1F
    if sr_index >= len(SR_TABLE):
        return None
    name = payload[8:24].split(b"\x00", 1)[0].decode("ascii", errors="replace")
    (frame,) = struct.unpack_from("<I", payload, 24)
    return VbanHeader(name=name, sr_index=sr_index, sample_rate=SR_TABLE[sr_index],
                      samples_per_frame=nbs + 1, channels=nbc + 1, frame=frame)


# ---------------------------------------------------------------------------------------------
# capture parsing
# ---------------------------------------------------------------------------------------------

def _ip(b: bytes) -> str:
    return ".".join(str(x) for x in b)


def _link_to_ipv4(linktype: int, frame: bytes) -> bytes | None:
    """Strip the link header; the IPv4 packet (possibly truncated) or None for anything else."""
    if linktype == 1:  # EN10MB
        off, proto = 12, None
        while off + 2 <= len(frame):
            proto = struct.unpack_from("!H", frame, off)[0]
            if proto in (0x8100, 0x88A8):
                off += 4
                continue
            off += 2
            break
        else:
            return None
    elif linktype == 113:  # LINUX_SLL
        if len(frame) < 16:
            return None
        proto, off = struct.unpack_from("!H", frame, 14)[0], 16
    elif linktype == 276:  # LINUX_SLL2
        if len(frame) < 20:
            return None
        proto, off = struct.unpack_from("!H", frame, 0)[0], 20
    elif linktype in (101, 228):  # RAW / IPV4
        proto, off = 0x0800, 0
    else:
        return None
    if proto == 0x8100 and linktype != 1 and off + 4 <= len(frame):
        proto = struct.unpack_from("!H", frame, off + 2)[0]
        off += 4
    if proto != 0x0800:
        return None
    return frame[off:]


def _ipv4_udp(ip: bytes, ts_ns: int) -> Packet | None:
    if len(ip) < 20 or (ip[0] >> 4) != 4:
        return None
    ihl = (ip[0] & 0x0F) * 4
    if ihl < 20 or ip[9] != 17:
        return None
    flags_frag = struct.unpack_from("!H", ip, 6)[0]
    if flags_frag & 0x1FFF:  # a non-first fragment carries no UDP header
        return None
    if len(ip) < ihl + 8:
        return None
    sport, dport = struct.unpack_from("!HH", ip, ihl)
    return Packet(ts_ns=ts_ns, src=_ip(ip[12:16]), sport=sport, dst=_ip(ip[16:20]), dport=dport,
                  payload=ip[ihl + 8:])


def _iter_pcap(data: bytes):
    magic_le = struct.unpack_from("<I", data, 0)[0]
    if magic_le in (0xA1B2C3D4, 0xA1B23C4D):
        e = "<"
    elif struct.unpack_from(">I", data, 0)[0] in (0xA1B2C3D4, 0xA1B23C4D):
        e = ">"
    else:
        raise CaptureError("not a pcap")
    if len(data) < 24:
        raise CaptureError("pcap global header cut short")
    magic = struct.unpack_from(e + "I", data, 0)[0]
    nsec = magic == 0xA1B23C4D
    linktype = struct.unpack_from(e + "I", data, 20)[0] & 0x0FFFFFFF
    off = 24
    while off + 16 <= len(data):
        sec, frac, incl, _orig = struct.unpack_from(e + "IIII", data, off)
        off += 16
        frame = data[off:off + incl]
        off += incl
        ts_ns = sec * 1_000_000_000 + (frac if nsec else frac * 1000)
        yield linktype, ts_ns, frame


def _iter_pcapng(data: bytes):
    off = 0
    e = "<"
    ifaces: list[tuple[int, int, int]] = []  # (linktype, units-per-second numerator, is_pow2)
    while off + 12 <= len(data):
        btype = struct.unpack_from(e + "I", data, off)[0]  # SHB's type is a byte palindrome
        if btype == 0x0A0D0D0A:
            bom = struct.unpack_from("<I", data, off + 8)[0]
            e = "<" if bom == 0x1A2B3C4D else ">"
            ifaces = []
        total = struct.unpack_from(e + "I", data, off + 4)[0]
        if total < 12 or off + total > len(data):
            break
        body = data[off + 8:off + total - 4]
        if btype == 1 and len(body) >= 8:  # IDB
            linktype = struct.unpack_from(e + "H", body, 0)[0]
            resol = (10, 6)
            o = 8
            while o + 4 <= len(body):
                code, ln = struct.unpack_from(e + "HH", body, o)
                o += 4
                if code == 0:
                    break
                if code == 9 and ln >= 1:
                    v = body[o]
                    resol = (2, v & 0x7F) if v & 0x80 else (10, v)
                o += ln + ((4 - ln % 4) % 4)
            ifaces.append((linktype, resol[0], resol[1]))
        elif btype == 6 and len(body) >= 20:  # EPB
            iface, ts_hi, ts_lo, caplen, _orig = struct.unpack_from(e + "IIIII", body, 0)
            if iface < len(ifaces):
                linktype, base, exp = ifaces[iface]
                ticks = (ts_hi << 32) | ts_lo
                ts_ns = ticks * 1_000_000_000 // (base ** exp)
                yield linktype, ts_ns, body[20:20 + caplen]
        off += total


def iter_packets(data: bytes):
    """(linktype, [Packet]) for every IPv4/UDP datagram in a pcap or pcapng capture."""
    if len(data) < 12:
        raise CaptureError("capture too short")
    if struct.unpack_from("<I", data, 0)[0] == 0x0A0D0D0A:
        frames = _iter_pcapng(data)
    else:
        frames = _iter_pcap(data)
    linktype = -1
    packets = []
    for lt, ts_ns, frame in frames:
        linktype = lt
        ip = _link_to_ipv4(lt, frame)
        if ip is None:
            continue
        p = _ipv4_udp(ip, ts_ns)
        if p is not None:
            packets.append(p)
    return linktype, packets


# ---------------------------------------------------------------------------------------------
# the counter-based measurement
# ---------------------------------------------------------------------------------------------

def _signed32(d: int) -> int:
    return ((d + (1 << 31)) % (1 << 32)) - (1 << 31)


def measure_stream(times_ns: list, frames: list, samples_per_frame: int, sample_rate: int) -> dict:
    """Rate / loss / jumps of ONE stream from its (capture time, counter) pairs in capture order."""
    n = len(times_ns)
    frames_per_s = sample_rate / samples_per_frame
    segments: list[list[tuple[int, int]]] = []   # per segment: (t_ns, unwrapped counter)
    jumps = reordered = duplicates = 0
    # head_u = the highest unwrapped counter of the current segment: every step is measured from it,
    # so a reordered straggler (a step back) never moves the reference the next packet is read from.
    # head_t = the arrival of the last ACCEPTED packet: a duplicate or a dropped straggler must never
    # shrink the elapsed time the step limits are computed from (review round 3)
    head_u = head_t = seg_min = seg_t0 = None
    seen: dict = {}   # unwrapped counter -> first arrival (ns) in the current segment
    for i, (t, f) in enumerate(zip(times_ns, frames)):
        straggler = False
        if head_u is None:
            u, d = f, 1
            segments.append([])
            seg_t0 = t
        else:
            d = _signed32(f - (head_u & 0xFFFFFFFF))
            dt_s = max(0.0, (t - head_t) / 1e9)
            fwd_limit = 2.0 * dt_s * frames_per_s + JUMP_FWD_SLACK_FRAMES
            u = head_u + d
            settled = (t - seg_t0) / 1e9 > DUP_WINDOW_S  # a reorder at a segment's start is normal
            candidate = (d < -JUMP_BACK_FRAMES or d > fwd_limit
                         or (d < 0 and settled and u < seg_min - 1)
                         or (d < 0 and u in seen and (t - seen[u]) / 1e9 > DUP_WINDOW_S))
            if candidate:
                # One packet of lookahead decides: the NEXT packet continuing the OLD sequence makes
                # this one a straggler/late duplicate; continuing from THIS packet makes it a restart.
                verdict = _next_continues(times_ns, frames, i, head_u, f, head_t, frames_per_s, seen)
                if verdict == "old":
                    straggler = True
                elif verdict == "new" or d < -JUMP_BACK_FRAMES or d > fwd_limit:
                    jumps += 1
                    u, d = f, 1
                    segments.append([])
                    seen = {}
                    head_u = seg_min = None
                    seg_t0 = t
                else:
                    straggler = True
        if u in seen:
            duplicates += 1
            continue
        if straggler and (d > 0 or u < seg_min - 1):
            # a lone far-ahead counter or a very late packet from before everything this segment has
            # seen: dropped -- adding it would stretch the span into phantom loss
            reordered += 1
            continue
        if d < 0:
            reordered += 1
        seen[u] = t
        segments[-1].append((t, u))
        head_t = t
        head_u = u if head_u is None or u > head_u else head_u
        seg_min = u if seg_min is None or u < seg_min else seg_min

    unique = sum(len(s) for s in segments)
    lost = 0
    for s in segments:
        us = [u for _, u in s]
        lost += (max(us) - min(us) + 1) - len(us)

    t0 = times_ns[0] if n else 0
    points = [[((t - t0) / 1e9, u * samples_per_frame) for t, u in s] for s in segments]
    rate_ppm = stderr_ppm = resid_rms_ms = resid_max_ms = None
    first = _pooled_fit(points)
    if first is not None:
        # arrival jitter of ALL packets around the preliminary line (samples -> seconds via slope)
        slope0, _, _, _, fitted0 = first
        sq = [((y - (my + slope0 * (x - mx))) / slope0) for xs_ys, mx, my in fitted0 for x, y in xs_ys]
        resid_rms_ms = math.sqrt(sum(r * r for r in sq) / len(sq)) * 1e3
        resid_max_ms = max(abs(r) for r in sq) * 1e3
        # the rate: a one-sided trimmed fit -- a late burst (stall) cannot bias it
        slope, sse, sxx, npts, fitted = _late_trimmed_fit(points, first)
        rate_ppm = (slope / sample_rate - 1.0) * 1e6
        se = _cluster_stderr(slope, sxx, fitted)
        if se is not None:
            stderr_ppm = se / sample_rate * 1e6

    max_gap_ms = 0.0
    for a, b in zip(times_ns, times_ns[1:]):
        max_gap_ms = max(max_gap_ms, (b - a) / 1e6)
    span_s = (times_ns[-1] - times_ns[0]) / 1e9 if n > 1 else 0.0
    expected = unique + lost
    return {
        "packets": n, "unique_frames": unique, "duplicates": duplicates, "reordered": reordered,
        "lost": lost, "loss_ratio": (lost / expected) if expected else 0.0, "jumps": jumps,
        "segments": len(segments), "span_s": span_s, "rate_ppm": rate_ppm,
        "rate_stderr_ppm": stderr_ppm, "max_gap_ms": max_gap_ms, "resid_rms_ms": resid_rms_ms,
        "resid_max_ms": resid_max_ms,
    }


def _next_continues(times_ns: list, frames: list, i: int, head_u: int, f: int, head_t: int,
                    frames_per_s: float, seen: dict) -> str:
    """Decide the jump candidate at index I (counter F; current segment head HEAD_U, arrived HEAD_T;
    the segment's SEEN counters) by what follows it, looking at most LOOKAHEAD_MAX packets ahead.
    Skipped on the way: counters the segment already holds (the rest of a late-duplicate burst, or a
    restart re-sending counters this segment saw) and counters still in flight around the head
    (-JUMP_BACK_FRAMES <= step <= 0: an ordinary reorder). The first counter ABOVE the head decides:
      "old" -- it continues the old head within the step limit: the old stream never stopped, so the
              candidate is a straggler or a late duplicate. Only when the skipped counters (held
              or in flight) cover the whole gap back to F (a restart re-sending them would do exactly
              that) must it also arrive ON TIME for its step (within DUP_WINDOW_S of step / rate
              after HEAD_T) -- a restart's continuation arrives late for the old head;
      "new" -- it continues from F instead: a restart;
      ""    -- it continues neither. If the lookahead window is used up by already-held counters
              and F itself is held, the candidate is "old" (a very long replay of old counters)."""
    t_cand = times_ns[i]
    u_cand = head_u + _signed32(f - (head_u & 0xFFFFFFFF))
    skipped_seen = skipped = 0
    for j in range(i + 1, min(len(frames), i + 1 + LOOKAHEAD_MAX)):
        t_next, f_next = times_ns[j], frames[j]
        d_old = _signed32(f_next - (head_u & 0xFFFFFFFF))
        if head_u + d_old in seen:
            skipped_seen += 1
            skipped += 1
            continue
        if -JUMP_BACK_FRAMES <= d_old <= 0:
            skipped += 1
            continue  # still in flight around the head -- an ordinary reorder, not a decision
        elapsed_old = max(0.0, (t_next - head_t) / 1e9)
        limit_old = 2.0 * elapsed_old * frames_per_s + JUMP_FWD_SLACK_FRAMES
        # a restart re-sending counters covers the whole gap back to F (some may still be in flight)
        replay = skipped_seen > 0 and skipped >= head_u - u_cand
        on_time = elapsed_old <= d_old / frames_per_s + DUP_WINDOW_S
        if 0 < d_old <= limit_old and (on_time or not replay):
            return "old"
        d_new = _signed32(f_next - f)
        limit_new = 2.0 * max(0.0, (t_next - t_cand) / 1e9) * frames_per_s + JUMP_FWD_SLACK_FRAMES
        if 0 < d_new <= limit_new:
            return "new"
        return ""
    return "old" if u_cand in seen else ""


def _cluster_stderr(slope: float, sxx: float, fitted: list):
    """The slope's standard error with every ARRIVAL CLUSTER as one observation: packets arriving
    less than CLUSTER_GAP_S apart were released together (obs-vban sends in bursts) and share one
    timing error, so a per-packet stderr is ~sqrt(burst size) too small (review round 4: a true
    0 ppm clock graded FAULT on 2/40 bursty seeds). Cluster-robust sandwich
    Var(b) = G/(G-1) x sum_c (sum_{i in c} (x_i - mx) e_i)^2 / Sxx^2 over G clusters."""
    if sxx <= 0:
        return None
    total = 0.0
    clusters = 0
    for xy, mx, my in fitted:
        acc, last_x = 0.0, None
        for x, y in xy:
            if last_x is not None and x - last_x >= CLUSTER_GAP_S:
                total += acc * acc
                clusters += 1
                acc = 0.0
            acc += (x - mx) * (y - (my + slope * (x - mx)))
            last_x = x
        if last_x is not None:
            total += acc * acc
            clusters += 1
    if clusters < 3:
        return None
    return math.sqrt(clusters / (clusters - 1) * total) / sxx


def _pooled_fit(segments_xy: list):
    """Least-squares slope of y on x pooled over segments (one intercept each). Returns
    (slope, sse, sxx, npts, fitted) or None when there is no spread to fit."""
    sxx = sxy = 0.0
    fitted = []
    for xy in segments_xy:
        if len(xy) < 2:
            continue
        mx = sum(x for x, _ in xy) / len(xy)
        my = sum(y for _, y in xy) / len(xy)
        sxx += sum((x - mx) ** 2 for x, _ in xy)
        sxy += sum((x - mx) * (y - my) for x, y in xy)
        fitted.append((xy, mx, my))
    if sxx <= 0:
        return None
    slope = sxy / sxx
    sse = sum((y - (my + slope * (x - mx))) ** 2 for xy, mx, my in fitted for x, y in xy)
    npts = sum(len(xy) for xy, _, _ in fitted)
    return slope, sse, sxx, npts, fitted


def _late_trimmed_fit(points: list, fit: tuple) -> tuple:
    """Refit without the points that arrived LATER than the line by more than the robust bound
    (median lateness + max(LATE_TRIM_MADS x 1.4826 x MAD, LATE_TRIM_FLOOR_S)). Delay is one-sided:
    only late points are dropped, never early ones. Stops when nothing more is dropped, after
    LATE_TRIM_ROUNDS, or when too few points would remain; returns the last valid fit."""
    for _ in range(LATE_TRIM_ROUNDS):
        slope, _, _, _, fitted = fit
        lines = {id(xy): (mx, my) for xy, mx, my in fitted}
        late = []
        for seg in points:
            if id(seg) not in lines:
                continue
            mx, my = lines[id(seg)]
            late.extend(-(y - (my + slope * (x - mx))) / slope for x, y in seg)
        if not late:
            return fit
        ordered = sorted(late)
        med = ordered[len(ordered) // 2]
        mad = sorted(abs(v - med) for v in late)[len(late) // 2] * 1.4826
        bound = med + max(LATE_TRIM_MADS * mad, LATE_TRIM_FLOOR_S)
        kept, dropped = [], 0
        for seg in points:
            if id(seg) not in lines:
                kept.append(seg)
                continue
            mx, my = lines[id(seg)]
            keep = [(x, y) for x, y in seg if -(y - (my + slope * (x - mx))) / slope <= bound]
            dropped += len(seg) - len(keep)
            kept.append(keep)
        if dropped == 0:
            return fit
        refit = _pooled_fit(kept)
        if refit is None or refit[3] < 3:
            return fit
        points, fit = kept, refit
    return fit


def analyze_capture(data: bytes, only_dst: tuple = ()) -> CaptureResult:
    """Parse a capture and measure every VBAN audio stream in it (ordered by stream key).
    `only_dst` (IPv4 strings) keeps only the streams ARRIVING at those addresses -- a capture on
    strih-lx also sees the hub's own outgoing streams, which say nothing about a peer's clock."""
    linktype, packets = iter_packets(data)
    groups: dict = {}
    truncated = 0
    for p in packets:
        if only_dst and p.dst not in only_dst:
            continue
        if len(p.payload) < VBAN_HEADER_LEN:
            if p.payload[:4] == VBAN_MAGIC or (p.payload and VBAN_MAGIC.startswith(p.payload[:4])):
                truncated += 1
            continue
        h = parse_vban(p.payload)
        if h is None:
            continue
        key = f"{h.name}@{p.src}:{p.sport}>{p.dst}:{p.dport}"
        g = groups.setdefault(key, {"p": p, "h": h, "t": [], "f": []})
        g["t"].append(p.ts_ns)
        g["f"].append(h.frame)
    streams = []
    for key in sorted(groups):
        g = groups[key]
        p, h = g["p"], g["h"]
        m = measure_stream(g["t"], g["f"], h.samples_per_frame, h.sample_rate)
        streams.append(StreamStats(key=key, name=h.name, src=p.src, sport=p.sport, dst=p.dst,
                                   dport=p.dport, sample_rate=h.sample_rate,
                                   samples_per_frame=h.samples_per_frame, channels=h.channels, **m))
    return CaptureResult(linktype=linktype, packets=len(packets), streams=streams,
                         truncated_vban=truncated)


# ---------------------------------------------------------------------------------------------
# grading
# ---------------------------------------------------------------------------------------------

def grade(s: StreamStats, g: Grading) -> tuple:
    """(verdict, reasons): OK | FAULT (|rate| - 2 stderr outside +-ppm_bound, and/or loss over the
    ceiling) | UNCERTAIN (the rate's 2-stderr interval straddles the bound; loss is still graded) |
    SHORT (too little data to grade). Neither SHORT nor UNCERTAIN ever pages or recovers."""
    if s.span_s < g.min_span_s or s.unique_frames < MIN_FRAMES or s.rate_ppm is None:
        return "SHORT", [f"span {s.span_s:.1f}s / {s.unique_frames} frames is too short to grade"]
    why = []
    margin = RATE_STDERR_MARGIN * (s.rate_stderr_ppm or 0.0)
    # the 2-stderr interval decides: wholly outside the bound = FAULT, wholly inside = OK, straddling
    # it = UNCERTAIN (review round 3: a gross fault is a FAULT however noisy the fit)
    uncertain = False
    if abs(s.rate_ppm) - margin > g.ppm_bound:
        why.append(f"rate {s.rate_ppm:+.2f} ppm is outside +-{g.ppm_bound:g} ppm of nominal")
    elif abs(s.rate_ppm) + margin > g.ppm_bound:
        uncertain = True
    if s.loss_ratio > g.loss_ceiling:
        why.append(f"loss {s.lost} frames ({s.loss_ratio:.2e}) is over the {g.loss_ceiling:.0e} ceiling")
    if why:
        return "FAULT", why
    if uncertain:
        return "UNCERTAIN", [f"rate {s.rate_ppm:+.2f} +- {margin:.1f} ppm (2 stderr) straddles "
                             f"the +-{g.ppm_bound:g} ppm bound"]
    return "OK", []


def overall_verdict(res: CaptureResult, grades: list) -> str:
    """CAPTURE_TRUNCATED (snaplen cut every VBAN header) | NO_STREAMS | FAULT | OK | UNKNOWN (every
    stream SHORT or UNCERTAIN)."""
    if not res.streams and res.truncated_vban:
        return "CAPTURE_TRUNCATED"
    if not grades:
        return "NO_STREAMS"
    verdicts = [v for v, _ in grades]
    if "FAULT" in verdicts:
        return "FAULT"
    if "OK" in verdicts:
        return "OK"
    return "UNKNOWN"


def _fmt(v, spec):
    return "na" if v is None else format(v, spec)


def render_text(res: CaptureResult, grades: list, g: Grading) -> str:
    lines = [f"overall={overall_verdict(res, grades)} streams={len(res.streams)} "
             f"linktype={res.linktype} udp_packets={res.packets} truncated_vban={res.truncated_vban} "
             f"ppm_bound={g.ppm_bound:g} loss_ceiling={g.loss_ceiling:g}"]
    for s, (v, why) in zip(res.streams, grades):
        lines.append(
            f"stream={s.name} src={s.src}:{s.sport} dst={s.dst}:{s.dport} verdict={v} "
            f"rate_ppm={_fmt(s.rate_ppm, '+.2f')} stderr_ppm={_fmt(s.rate_stderr_ppm, '.2f')} "
            f"lost={s.lost} loss_ratio={s.loss_ratio:.2e} jumps={s.jumps} dup={s.duplicates} "
            f"reordered={s.reordered} max_gap_ms={s.max_gap_ms:.1f} "
            f"jitter_rms_ms={_fmt(s.resid_rms_ms, '.2f')} jitter_max_ms={_fmt(s.resid_max_ms, '.2f')} "
            f"span_s={s.span_s:.1f} sr={s.sample_rate} spf={s.samples_per_frame} ch={s.channels}"
            + (f" why={'; '.join(why)}" if why else ""))
    return "\n".join(lines)


def render_json(res: CaptureResult, grades: list, g: Grading) -> str:
    streams = []
    for s, (v, why) in zip(res.streams, grades):
        d = asdict(s)
        d["verdict"] = v
        d["why"] = why
        streams.append(d)
    return json.dumps({"overall": overall_verdict(res, grades), "linktype": res.linktype,
                       "udp_packets": res.packets, "truncated_vban": res.truncated_vban,
                       "grading": asdict(g), "streams": streams}, indent=2)


def render_tsv(res: CaptureResult, grades: list) -> str:
    """The shell-facing form (the dev1 watchdog): `overall<TAB><verdict>` then one line per stream:
    `stream<TAB>key<TAB>name<TAB>src<TAB>verdict<TAB>rate_ppm<TAB>lost<TAB>loss_ratio<TAB>why`.
    Tabs/newlines never occur inside a field (a VBAN name is <= 16 printable bytes)."""
    def clean(v):
        v = str(v).replace("\t", " ").replace("\n", " ")
        return v if v else "-"  # bash `read` merges EMPTY tab fields -- never emit one
    lines = [f"overall\t{overall_verdict(res, grades)}"]
    for s, (v, why) in zip(res.streams, grades):
        lines.append("\t".join(clean(x) for x in (
            "stream", s.key, s.name, s.src, v, _fmt(s.rate_ppm, "+.2f"), s.lost,
            f"{s.loss_ratio:.2e}", "; ".join(why) or "-")))
    return "\n".join(lines)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="vban_rate.py", description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("analyze", help="measure every VBAN stream in a pcap/pcapng capture")
    a.add_argument("capture")
    a.add_argument("--ppm-bound", type=float, default=DEFAULT_PPM_BOUND)
    a.add_argument("--loss-ceiling", type=float, default=DEFAULT_LOSS_CEILING)
    a.add_argument("--min-span-s", type=float, default=DEFAULT_MIN_SPAN_S)
    a.add_argument("--dst", action="append", default=[],
                   help="keep only streams arriving at this IPv4 (repeatable; default: all)")
    a.add_argument("--json", action="store_true")
    a.add_argument("--tsv", action="store_true", help="tab-separated lines for the dev1 watchdog")
    ns = ap.parse_args(argv)
    g = Grading(ppm_bound=ns.ppm_bound, loss_ceiling=ns.loss_ceiling, min_span_s=ns.min_span_s)
    try:
        with open(ns.capture, "rb") as fh:
            data = fh.read()
        res = analyze_capture(data, only_dst=tuple(ns.dst))
    except (OSError, CaptureError, struct.error) as exc:
        print(f"vban_rate: cannot read capture {ns.capture}: {exc}", file=sys.stderr)
        return 2
    grades = [grade(s, g) for s in res.streams]
    if ns.tsv:
        print(render_tsv(res, grades))
    elif ns.json:
        print(render_json(res, grades, g))
    else:
        print(render_text(res, grades, g))
    return 0


if __name__ == "__main__":
    sys.exit(main())
