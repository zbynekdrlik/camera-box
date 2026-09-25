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
    s = only_stream(pcap_bytes(276, stream_records(n, gap_at=5000, gap_s=3.0)))
    assert s.jumps == 0
    assert s.lost == int(3.0 * NOMINAL / SPF)
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
