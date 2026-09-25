"""Issue 1372 part C -- the inter-PC audio rate gate: pure tests for scripts/vban_rate.py.

WHY: nothing measured whether the AUDIO travelling between PCs (VBAN) runs in the Dante tick. The
FOH clicks were found by hand with pktmon, and the first reading was WRONG: it derived the stream
rate from the PACKET COUNT, so 5 lost packets in 90 s read as -132.9 ppm when the stream was really
+18.1 ppm (#1367 comment 5832526338). scripts/vban_rate.py is the pure kernel: it parses a capture
(classic pcap LINUX_SLL2 276 / LINUX_SLL 113 / EN10MB 1, and a pktmon pcapng), then measures each
VBAN stream's rate by the FRAME COUNTER (nuFrame x samples-per-frame, a least-squares slope against
capture time), plus loss (counter gaps), jumps (a sender restart) and the max inter-packet gap.

Tier-0: every fixture is synthesised here in bytes -- no rig, no cargo.
"""
import importlib.util
import json
import pathlib
import struct
import sys

import pytest

_MOD = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "vban_rate.py"
_spec = importlib.util.spec_from_file_location("vban_rate", _MOD)
vr = importlib.util.module_from_spec(_spec)
sys.modules["vban_rate"] = vr
_spec.loader.exec_module(vr)

NOMINAL = 48000
SPF = 256  # samples per frame (nbs + 1)


# ---------------------------------------------------------------------------------------------
# fixture builders (bytes on the wire)
# ---------------------------------------------------------------------------------------------

def vban_payload(name="fohabl-strih", frame=0, sr_index=3, nbs=SPF - 1, nbc=1, fmt=1,
                 data_len=64, sub=0):
    hdr = (b"VBAN" + bytes([(sub << 5) | (sr_index & 0x1F), nbs, nbc, fmt])
           + name.encode().ljust(16, b"\0") + struct.pack("<I", frame & 0xFFFFFFFF))
    return hdr + b"\x00" * data_len


def udp_ipv4(src="10.77.7.30", dst="10.77.9.202", sport=6980, dport=6980, payload=b"",
             frag_off=0, more_frags=False, proto=17):
    udp = struct.pack("!HHHH", sport, dport, 8 + len(payload), 0) + payload
    flags_frag = (0x2000 if more_frags else 0) | (frag_off & 0x1FFF)
    total = 20 + len(udp)
    ip = struct.pack("!BBHHHBBH4s4s", 0x45, 0, total, 0, flags_frag, 64, proto, 0,
                     bytes(int(x) for x in src.split(".")), bytes(int(x) for x in dst.split(".")))
    return ip + udp


def eth(ipb, vlan=None):
    head = b"\x00\x11\x22\x33\x44\x55" + b"\x66\x77\x88\x99\xaa\xbb"
    if vlan is not None:
        head += struct.pack("!HH", 0x8100, vlan)
    return head + struct.pack("!H", 0x0800) + ipb


def sll(ipb):
    return struct.pack("!HHH", 0, 1, 6) + b"\x00" * 8 + struct.pack("!H", 0x0800) + ipb


def sll2(ipb):
    return struct.pack("!HHIHBB", 0x0800, 0, 2, 1, 0, 6) + b"\x00" * 8 + ipb


LINK = {1: eth, 113: sll, 276: sll2}


def pcap_bytes(linktype, records, nsec=False, big=False, snaplen=65535):
    """records: [(ts_ns, frame_bytes)] -> a classic pcap. Frames longer than snaplen are cut, exactly
    like tcpdump -s."""
    e = ">" if big else "<"
    magic = 0xA1B23C4D if nsec else 0xA1B2C3D4
    out = struct.pack(e + "IHHiIII", magic, 2, 4, 0, 0, snaplen, linktype)
    for ts_ns, frame in records:
        sec, rem = divmod(ts_ns, 1_000_000_000)
        frac = rem if nsec else rem // 1000
        cap = frame[:snaplen]
        out += struct.pack(e + "IIII", sec, frac, len(cap), len(frame)) + cap
    return out


def _pad4(b):
    return b + b"\x00" * ((4 - len(b) % 4) % 4)


def _block(btype, body):
    total = 12 + len(body)
    return struct.pack("<II", btype, total) + body + struct.pack("<I", total)


def pcapng_bytes(linktype, records, tsresol=None, snaplen=0):
    """A minimal little-endian pcapng (SHB + one IDB + EPBs), the pktmon etl2pcap shape."""
    shb = _block(0x0A0D0D0A, struct.pack("<IHHq", 0x1A2B3C4D, 1, 0, -1))
    opts = b""
    if tsresol is not None:
        opts += struct.pack("<HH", 9, 1) + _pad4(bytes([tsresol]))
        opts += struct.pack("<HH", 0, 0)
    idb = _block(1, struct.pack("<HHI", linktype, 0, snaplen) + opts)
    unit = 1_000_000 if tsresol is None else 10 ** tsresol
    out = shb + idb
    for ts_ns, frame in records:
        ts = ts_ns * unit // 1_000_000_000
        body = struct.pack("<IIIII", 0, ts >> 32, ts & 0xFFFFFFFF, len(frame), len(frame))
        out += _block(6, body + _pad4(frame))
    return out


def stream_records(n, ppm=0.0, start_frame=1000, t0_ns=1_790_000_000_000_000_000, link=276,
                   drop=(), dup_every=0, jitter_ms=0.0, name="fohabl-strih", src="10.77.7.30",
                   restart_at=None, gap_at=None, gap_s=0.0, swap_at=None):
    """A VBAN stream whose TRUE sample clock runs `ppm` off nominal, captured at the receiver."""
    period_ns = SPF / (NOMINAL * (1.0 + ppm * 1e-6)) * 1e9
    recs = []
    frame = start_frame
    extra_ns = 0.0
    for i in range(n):
        if restart_at is not None and i == restart_at:
            frame = 0
        if gap_at is not None and i == gap_at:
            extra_ns += gap_s * 1e9
        t = t0_ns + int(i * period_ns + extra_ns)
        if jitter_ms:
            # deterministic pseudo-random arrival jitter in [-jitter_ms, +jitter_ms]
            j = ((i * 7919) % 2001 - 1000) / 1000.0 * jitter_ms
            t += int(j * 1e6)
        if i not in drop:
            ipb = udp_ipv4(src=src, payload=vban_payload(name=name, frame=frame))
            recs.append((t, LINK[link](ipb)))
            if dup_every and i % dup_every == 0:
                recs.append((t + 20_000, LINK[link](ipb)))
        frame = (frame + 1) & 0xFFFFFFFF
        if gap_at is not None and i + 1 == gap_at:
            # the SENDER keeps counting through the network outage: its counter jumps by the
            # frames that elapsed, the capture sees nothing
            frame = (frame + int(gap_s * NOMINAL / SPF)) & 0xFFFFFFFF
    if swap_at is not None:
        recs[swap_at], recs[swap_at + 1] = recs[swap_at + 1], recs[swap_at]
    return recs


def only_stream(data, **kw):
    res = vr.analyze_capture(data, **kw)
    assert len(res.streams) == 1, res.streams
    return res.streams[0]


# ---------------------------------------------------------------------------------------------
# header parsing
# ---------------------------------------------------------------------------------------------

def test_vban_header_fields():
    h = vr.parse_vban(vban_payload(name="cg", frame=77, sr_index=4, nbs=127, nbc=1))
    assert h.name == "cg"
    assert h.sample_rate == 96000
    assert h.samples_per_frame == 128
    assert h.channels == 2
    assert h.frame == 77


def test_vban_non_audio_subprotocol_and_non_vban_are_ignored():
    assert vr.parse_vban(vban_payload(sub=2)) is None          # VBAN text/service, not audio
    assert vr.parse_vban(b"NOPE" + b"\x00" * 40) is None
    assert vr.parse_vban(vban_payload()[:20]) is None           # header cut short


def test_sample_rate_table_matches_the_vban_spec():
    assert vr.SR_TABLE[:7] == [6000, 12000, 24000, 48000, 96000, 192000, 384000]
    assert vr.SR_TABLE[16] == 44100
    assert vr.SR_TABLE[20] == 705600


# ---------------------------------------------------------------------------------------------
# the counter-based rate -- the heart of the tool
# ---------------------------------------------------------------------------------------------

def test_rate_is_read_from_the_frame_counter_not_the_packet_count():
    """The 25.9. mistake: 5 lost packets in 90 s. The COUNTER reads the true rate (~0 ppm here);
    the packet COUNT would read ~-296 ppm. Loss is reported separately, never folded into rate."""
    n = int(90 * NOMINAL / SPF)
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=0.0, drop={100, 900, 5000, 9000, 15000})))
    assert abs(s.rate_ppm) < 0.2, s.rate_ppm
    assert s.lost == 5
    assert s.jumps == 0
    # the WRONG method, for the record: packets over span vs nominal
    count_rate = (s.packets / s.span_s) / (NOMINAL / SPF)
    assert abs((count_rate - 1.0) * 1e6) > 100


@pytest.mark.parametrize("ppm", [18.1, -8.9, 6.4, 0.8])
def test_rate_recovers_the_true_clock_offset(ppm):
    n = int(60 * NOMINAL / SPF)
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=ppm)))
    assert abs(s.rate_ppm - ppm) < 0.05, (s.rate_ppm, ppm)
    assert s.lost == 0


def test_rate_survives_bursty_arrival_jitter():
    """obs-vban sends in bursts (p99 21 ms, max 32 ms, #1372 finding); a 60 s least-squares fit
    still reads the clock within a few ppm and reports the jitter."""
    n = int(60 * NOMINAL / SPF)
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=12.0, jitter_ms=20.0)))
    assert abs(s.rate_ppm - 12.0) < 3.0, s.rate_ppm
    assert s.resid_max_ms > 10.0
    assert s.rate_stderr_ppm > 0.0


def test_counter_wraps_at_2_pow_32_without_a_jump():
    n = 4000
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=5.0, start_frame=2**32 - 100)))
    assert s.jumps == 0
    assert s.lost == 0
    assert abs(s.rate_ppm - 5.0) < 0.5


def test_sender_restart_is_a_jump_not_loss_and_the_rate_holds():
    n = 8000
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=-3.0, start_frame=50_000, restart_at=4000)))
    assert s.jumps == 1
    assert s.segments == 2
    assert s.lost == 0
    assert abs(s.rate_ppm + 3.0) < 0.5


def test_network_outage_counts_as_loss_and_max_gap():
    """The sender kept counting through a 3 s outage: the counter gap is LOSS (not a jump) and the
    inter-packet gap is reported."""
    n = 10000
    gap_frames = 562  # ~3 s; the outage length is a WHOLE number of frames so the sender's clock
    #                   stays on one line (a fractional-frame fixture would inject a phase step)
    s = only_stream(pcap_bytes(276, stream_records(n, gap_at=5000, gap_s=gap_frames * SPF / NOMINAL)))
    assert s.jumps == 0
    assert s.lost == gap_frames
    assert s.max_gap_ms > 2900
    assert abs(s.rate_ppm) < 0.5


def test_pktmon_duplicates_are_not_loss_and_not_rate():
    n = 6000
    s = only_stream(pcap_bytes(1, stream_records(n, ppm=2.0, dup_every=3, link=1), nsec=True))
    assert s.duplicates == len(range(0, n, 3))
    assert s.lost == 0
    assert abs(s.rate_ppm - 2.0) < 0.5


def test_reorder_is_counted_and_is_not_loss():
    n = 3000
    s = only_stream(pcap_bytes(276, stream_records(n, swap_at=1000)))
    assert s.reordered == 1
    assert s.lost == 0


# ---------------------------------------------------------------------------------------------
# capture formats
# ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("link", [276, 113, 1])
@pytest.mark.parametrize("nsec", [False, True])
@pytest.mark.parametrize("big", [False, True])
def test_classic_pcap_every_linktype_and_byte_order(link, nsec, big):
    n = 3000
    s = only_stream(pcap_bytes(link, stream_records(n, ppm=7.0, link=link), nsec=nsec, big=big))
    assert s.name == "fohabl-strih"
    assert s.src == "10.77.7.30"
    assert s.dport == 6980
    assert s.sample_rate == NOMINAL
    assert s.unique_frames == n
    assert abs(s.rate_ppm - 7.0) < 1.0  # usec timestamps quantise a 16 s run to ~0.1 ppm


def test_ethernet_vlan_tag_is_skipped():
    recs = [(t, eth(udp_ipv4(payload=vban_payload(frame=i)), vlan=7))
            for i, t in enumerate(range(0, 3000 * 5_333_333, 5_333_333))]
    s = only_stream(pcap_bytes(1, recs))
    assert s.unique_frames == 3000


@pytest.mark.parametrize("tsresol", [None, 9])
def test_pktmon_pcapng(tsresol):
    n = 3000
    s = only_stream(pcapng_bytes(1, stream_records(n, ppm=-11.7, link=1), tsresol=tsresol))
    assert s.unique_frames == n
    assert abs(s.rate_ppm + 11.7) < 1.0


def test_snaplen_96_keeps_the_header_and_reports_the_real_udp_length():
    n = 2000
    recs = [(t, sll2(udp_ipv4(payload=vban_payload(frame=i, data_len=1024))))
            for i, t in enumerate(range(0, n * 5_333_333, 5_333_333))]
    res = vr.analyze_capture(pcap_bytes(276, recs, snaplen=96))
    assert len(res.streams) == 1
    assert res.truncated_vban == 0


def test_snaplen_too_small_is_a_loud_error_not_an_empty_ok():
    recs = [(t, sll2(udp_ipv4(payload=vban_payload(frame=i))))
            for i, t in enumerate(range(0, 200 * 5_333_333, 5_333_333))]
    res = vr.analyze_capture(pcap_bytes(276, recs, snaplen=56))
    assert res.streams == []
    assert res.truncated_vban == 200
    assert vr.overall_verdict(res, []) == "CAPTURE_TRUNCATED"


def test_non_vban_udp_fragments_and_tcp_are_ignored():
    recs = []
    for i in range(500):
        t = i * 5_333_333
        recs.append((t, sll2(udp_ipv4(payload=vban_payload(frame=i)))))
        recs.append((t + 1, sll2(udp_ipv4(payload=b"NDI-junk" * 10, dport=5961))))
        recs.append((t + 2, sll2(udp_ipv4(payload=vban_payload(frame=i), frag_off=100))))
        recs.append((t + 3, sll2(udp_ipv4(payload=vban_payload(frame=i), proto=6))))
    s = only_stream(pcap_bytes(276, recs))
    assert s.unique_frames == 500
    assert s.duplicates == 0


def test_streams_are_separated_by_sender_and_name():
    a = stream_records(3000, ppm=1.0, name="fohabl-strih", src="10.77.7.30")
    b = stream_records(3000, ppm=-9.0, name="cg", src="10.77.9.201")
    res = vr.analyze_capture(pcap_bytes(276, sorted(a + b)))
    by = {s.name: s for s in res.streams}
    assert set(by) == {"fohabl-strih", "cg"}
    assert abs(by["cg"].rate_ppm + 9.0) < 1.0


def test_garbage_capture_raises():
    with pytest.raises(vr.CaptureError):
        vr.analyze_capture(b"\x00" * 64)


# ---------------------------------------------------------------------------------------------
# grading + CLI
# ---------------------------------------------------------------------------------------------

def _stats(**over):
    base = dict(key="k", name="n", src="10.77.7.30", sport=6980, dst="10.77.9.202", dport=6980,
                sample_rate=48000, samples_per_frame=256, channels=2, packets=11000,
                unique_frames=11000, duplicates=0, reordered=0, lost=0, loss_ratio=0.0, jumps=0,
                segments=1, span_s=60.0, rate_ppm=1.0, rate_stderr_ppm=0.3, max_gap_ms=30.0,
                resid_rms_ms=3.0, resid_max_ms=20.0)
    base.update(over)
    return vr.StreamStats(**base)


def test_grade_ok_rate_loss_short():
    g = vr.Grading(ppm_bound=20.0, loss_ceiling=1e-4, min_span_s=20.0)
    assert vr.grade(_stats(), g) == ("OK", [])
    v, why = vr.grade(_stats(rate_ppm=-25.0), g)
    assert v == "FAULT" and any("rate" in w for w in why)
    v, why = vr.grade(_stats(lost=5, loss_ratio=4.5e-4), g)
    assert v == "FAULT" and any("loss" in w for w in why)
    v, why = vr.grade(_stats(rate_ppm=30.0, lost=5, loss_ratio=4.5e-4), g)
    assert v == "FAULT" and len(why) == 2
    assert vr.grade(_stats(span_s=5.0), g)[0] == "SHORT"


def test_overall_verdict():
    g = vr.Grading(ppm_bound=20.0, loss_ceiling=1e-4, min_span_s=20.0)
    res = vr.CaptureResult(linktype=276, packets=0, streams=[], truncated_vban=0)
    assert vr.overall_verdict(res, []) == "NO_STREAMS"
    assert vr.overall_verdict(res, [("OK", [])]) == "OK"
    assert vr.overall_verdict(res, [("OK", []), ("SHORT", [])]) == "OK"
    assert vr.overall_verdict(res, [("SHORT", [])]) == "UNKNOWN"
    assert vr.overall_verdict(res, [("OK", []), ("FAULT", ["x"])]) == "FAULT"
    del g


def test_cli_text_and_json(tmp_path, capsys):
    n = int(30 * NOMINAL / SPF)
    p = tmp_path / "cap.pcap"
    p.write_bytes(pcap_bytes(276, stream_records(n, ppm=40.0, drop={10, 20})))
    rc = vr.main(["analyze", str(p), "--ppm-bound", "20"])
    out = capsys.readouterr().out
    assert rc == 0
    assert "overall=FAULT" in out
    assert "stream=fohabl-strih" in out and "verdict=FAULT" in out
    rc = vr.main(["analyze", str(p), "--json"])
    doc = json.loads(capsys.readouterr().out)
    assert rc == 0
    assert doc["overall"] == "FAULT"
    st = doc["streams"][0]
    assert st["lost"] == 2
    assert abs(st["rate_ppm"] - 40.0) < 0.5


def test_cli_unreadable_capture_exits_2(tmp_path, capsys):
    p = tmp_path / "bad.pcap"
    p.write_bytes(b"not a capture at all, just text")
    assert vr.main(["analyze", str(p)]) == 2
    assert "vban_rate:" in capsys.readouterr().err


def test_dst_filter_keeps_only_streams_arriving_at_the_receiver(tmp_path, capsys):
    """A capture on strih-lx also carries the hub's OWN outgoing streams (strih-lx -> camN); the
    watchdog grades only what ARRIVES there (live capture 25.9.: 7 hub streams + fohabl-strih)."""
    t0 = 1_790_000_000_000_000_000
    inbound = stream_records(3000, name="fohabl-strih", src="10.77.7.30", t0_ns=t0)
    outbound = [(t0 + i * 5_333_333 + 7,
                 sll2(udp_ipv4(src="10.77.9.202", dst="10.77.9.61",
                               payload=vban_payload(name="cam1", frame=i))))
                for i in range(3000)]
    data = pcap_bytes(276, sorted(inbound + outbound))
    assert {s.name for s in vr.analyze_capture(data).streams} == {"fohabl-strih", "cam1"}
    kept = vr.analyze_capture(data, only_dst=("10.77.9.202",)).streams
    assert [s.name for s in kept] == ["fohabl-strih"]
    p = tmp_path / "mixed.pcap"
    p.write_bytes(data)
    assert vr.main(["analyze", str(p), "--dst", "10.77.9.202", "--min-span-s", "5"]) == 0
    out = capsys.readouterr().out
    assert "stream=fohabl-strih" in out and "stream=cam1" not in out


# ---------------------------------------------------------------------------------------------
# review round 1 (issue 1372): arrival delay is one-sided, restarts near the start, BE pcapng,
# an empty VBAN name in the shell-facing output, the stderr margin in grading
# ---------------------------------------------------------------------------------------------

def _stall(recs, at_s, stall_s):
    """Every packet that would arrive inside [at, at+stall) is held and released as a burst at the
    stall end -- network/sender delay only ever makes an arrival LATER, never earlier."""
    t0 = recs[0][0]
    lo, hi = t0 + int(at_s * 1e9), t0 + int((at_s + stall_s) * 1e9)
    out, k = [], 0
    for t, fr in recs:
        if lo <= t < hi:
            out.append((hi + k * 20_000, fr))
            k += 1
        else:
            out.append((t, fr))
    return out


def test_a_single_stall_then_burst_does_not_bias_the_rate():
    """Review round 1: an OLS fit of counter vs ARRIVAL read +65.7 ppm for one 500 ms stall 10 s off
    centre in a clean 60 s capture. Delay is one-sided, so the fit drops points that arrive far
    LATER than the line (a one-sided trim) and refits."""
    n = int(60 * NOMINAL / SPF)
    s = only_stream(pcap_bytes(276, _stall(stream_records(n, ppm=0.0), at_s=20.0, stall_s=0.5)))
    assert abs(s.rate_ppm) < 1.0, s.rate_ppm
    assert s.lost == 0 and s.jumps == 0
    assert s.max_gap_ms > 490


def test_the_trimmed_fit_still_reads_a_true_offset_through_stalls_and_jitter():
    n = int(60 * NOMINAL / SPF)
    recs = stream_records(n, ppm=-12.0, jitter_ms=5.0)
    for at in (8.0, 31.0, 47.0):
        recs = _stall(recs, at_s=at, stall_s=0.3)
    s = only_stream(pcap_bytes(276, sorted(recs)))
    assert abs(s.rate_ppm + 12.0) < 1.5, s.rate_ppm


def test_a_restart_near_the_start_is_a_jump_not_duplicates():
    """Review round 1: a sender that started < 64 frames before restarting to 0 was read as a
    reorder -- 40 'duplicates' and -2156 ppm. A step back to a counter already seen long ago (or
    below everything seen) is a jump."""
    n = 6000
    s = only_stream(pcap_bytes(276, stream_records(n, ppm=4.0, start_frame=0, restart_at=40)))
    assert s.jumps == 1, (s.jumps, s.duplicates, s.reordered)
    assert s.duplicates == 0
    assert abs(s.rate_ppm - 4.0) < 0.5, s.rate_ppm


def _pcapng_be(linktype, records):
    def blk(btype, body):
        total = 12 + len(body)
        return struct.pack(">II", btype, total) + body + struct.pack(">I", total)
    out = blk(0x0A0D0D0A, struct.pack(">IHHq", 0x1A2B3C4D, 1, 0, -1))
    out += blk(1, struct.pack(">HHI", linktype, 0, 0))
    for ts_ns, frame in records:
        ts = ts_ns // 1000
        body = struct.pack(">IIIII", 0, ts >> 32, ts & 0xFFFFFFFF, len(frame), len(frame))
        out += blk(6, body + _pad4(frame))
    return out


def test_big_endian_pcapng_is_read_not_silently_empty():
    s = only_stream(_pcapng_be(1, stream_records(3000, ppm=3.0, link=1)))
    assert s.unique_frames == 3000


def test_tsv_keeps_its_columns_for_an_empty_stream_name():
    """An empty VBAN name must not collapse a tab column (bash `read` merges empty tab fields)."""
    recs = [(t, sll2(udp_ipv4(payload=vban_payload(name="", frame=i))))
            for i, t in enumerate(range(0, 3000 * 5_333_333, 5_333_333))]
    res = vr.analyze_capture(pcap_bytes(276, recs))
    g = vr.Grading(min_span_s=5.0)
    line = vr.render_tsv(res, [vr.grade(s, g) for s in res.streams]).splitlines()[1]
    fields = line.split("\t")
    assert len(fields) == 9 and all(fields), fields
    assert fields[2] == "-" and fields[4] in ("OK", "FAULT")


def test_rate_fault_needs_the_bound_cleared_by_two_stderr():
    g = vr.Grading(ppm_bound=20.0, loss_ceiling=1e-4, min_span_s=20.0)
    assert vr.grade(_stats(rate_ppm=21.0, rate_stderr_ppm=1.0), g)[0] == "OK"
    assert vr.grade(_stats(rate_ppm=23.0, rate_stderr_ppm=1.0), g)[0] == "FAULT"


# ---------------------------------------------------------------------------------------------
# review round 2 (issue 1372): no phantom restart from a start reorder or a late duplicate;
# a stream too noisy to resolve the bound is UNCERTAIN, never OK
# ---------------------------------------------------------------------------------------------

def test_a_reorder_right_at_the_segment_start_is_not_a_restart():
    recs = stream_records(3000, start_frame=0)
    frames_in_arrival_order = [recs[2][1], recs[0][1], recs[1][1]]  # arrival order 2, 0, 1, 3, ...
    recs[0:3] = [(recs[i][0], f) for i, f in enumerate(frames_in_arrival_order)]
    s = only_stream(pcap_bytes(276, recs))
    assert s.jumps == 0 and s.lost == 0, (s.jumps, s.lost)


def test_a_duplicate_arriving_late_is_a_duplicate_not_a_restart():
    recs = stream_records(6000)
    late = []
    for k in range(10):
        i = 500 + k * 500
        t, fr = recs[i]
        late.append((t + 60_000_000, fr))  # the same packet again, 60 ms later
    s = only_stream(pcap_bytes(276, sorted(recs + late)))
    assert s.jumps == 0, s.jumps
    assert s.lost == 0 and s.duplicates == 10, (s.lost, s.duplicates)


def test_a_very_late_straggler_is_not_a_restart_and_books_no_phantom_loss():
    recs = stream_records(6000)
    t, fr = recs[3000]
    recs = recs[:3000] + recs[3001:4000] + [(recs[3999][0] + 1000, fr)] + recs[4000:]
    s = only_stream(pcap_bytes(276, recs))
    assert s.jumps == 0 and s.lost == 0, (s.jumps, s.lost)


def test_a_genuine_restart_is_still_a_jump():
    s = only_stream(pcap_bytes(276, stream_records(6000, start_frame=50_000, restart_at=3000)))
    assert s.jumps == 1 and s.lost == 0


def test_a_stream_too_noisy_to_resolve_the_bound_is_uncertain_not_ok():
    g = vr.Grading(ppm_bound=20.0, loss_ceiling=1e-4, min_span_s=20.0)
    assert vr.grade(_stats(rate_ppm=30.0, rate_stderr_ppm=10.8), g)[0] == "UNCERTAIN"
    assert vr.grade(_stats(rate_ppm=1.0, rate_stderr_ppm=10.8), g)[0] == "UNCERTAIN"
    # loss is graded independently of the rate's precision
    assert vr.grade(_stats(rate_ppm=1.0, rate_stderr_ppm=10.8, lost=9, loss_ratio=8e-4), g)[0] == "FAULT"
    res = vr.CaptureResult(linktype=276, packets=0, streams=[], truncated_vban=0)
    assert vr.overall_verdict(res, [("UNCERTAIN", ["x"])]) == "UNKNOWN"
