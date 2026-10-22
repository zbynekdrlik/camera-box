"""Issue 1460 slice 1 -- the lv1 VBAN emulator: pure planner + the sender CLI.

scripts/vban_emulator_plan.py plans every packet of a run (the sender-clock slot from a ppm offset,
the release with jitter / stalls, the scheduled losses, the VBAN counter, the pilot sine);
scripts/vban_emulator.py sends it. These tests pin the planner's contract and drive the sender
against a local UDP socket (never the rig).

Tier-0: numpy + the stdlib, no rig, no cargo.
"""
import json
import pathlib
import socket
import subprocess
import sys
import threading

import numpy as np
import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import vban_emulator_plan as vp  # noqa: E402
import vban_rate  # noqa: E402

EMULATOR = SCRIPTS / "vban_emulator.py"
NS = 1_000_000_000


def _plan(profile="lv1", duration_s=60.0, **kw):
    return list(vp.plan(vp.PROFILES[profile], duration_s, **kw))


# --- the clock offset: the average rate is exact -------------------------------------------------

@pytest.mark.parametrize("ppm", [0.0, 1.4, 50.0, -50.0, 500.0])
def test_the_slot_rate_is_exactly_the_injected_offset(ppm):
    u = vp.ppm_units(ppm)
    n = 2_000_000                                   # ~3 h of packets
    rate = n * vp.FRAMES_PER_PACKET * NS / vp.deadline_ns(n, u)
    assert rate / vp.SAMPLE_RATE - 1.0 == pytest.approx(ppm * 1e-6, abs=1e-12)


def test_slots_come_from_the_index_never_a_running_sum():
    u = vp.ppm_units(50.0)
    # each slot is the floor of the exact rational value, so no drift can accumulate
    for n in (1, 7, 12345, 987654):
        exact = n * vp.FRAMES_PER_PACKET * NS * NS // (vp.SAMPLE_RATE * (NS + u))
        assert vp.deadline_ns(n, u) == exact
    steps = np.diff([vp.deadline_ns(n, u) for n in range(1000)])
    assert set(steps.tolist()) <= {5333066, 5333067}   # 256 / (48000 x 1.00005) s, +-1 ns rounding


def test_packet_count_covers_exactly_the_run():
    for ppm in (0.0, 50.0, -50.0):
        u = vp.ppm_units(ppm)
        n = vp.packet_count(10.0, u)
        assert vp.deadline_ns(n - 1, u) < 10 * NS <= vp.deadline_ns(n, u)


def test_a_fast_sender_sends_more_packets_in_the_same_time():
    fast = vp.packet_count(600.0, vp.ppm_units(50.0))
    nominal = vp.packet_count(600.0, vp.ppm_units(0.0))
    slow = vp.packet_count(600.0, vp.ppm_units(-50.0))
    # 600 s x 48000 x 50e-6 / 256 = 5.6 packets either way
    assert 5 <= fast - nominal <= 6 and 5 <= nominal - slow <= 6


def test_ppm_outside_the_limit_is_refused():
    for bad in (2500.0, -2001.0, float("nan"), float("inf")):
        with pytest.raises(ValueError):
            vp.ppm_units(bad)


# --- the release: jitter, stalls, bursts ---------------------------------------------------------

def test_clean_sends_every_packet_on_its_slot():
    for p in _plan("clean", 10.0):
        assert p.release_ns == p.deadline_ns and not p.lost and not p.stalled


@pytest.mark.parametrize("profile", ["lv1", "stress"])
def test_releases_are_ordered_never_early(profile):
    prev = 0
    for p in _plan(profile, 300.0):
        assert p.release_ns >= p.deadline_ns
        assert p.release_ns >= prev
        prev = p.release_ns


def test_lv1_gaps_are_shaped_like_the_measured_lv1_feed():
    s = vp.summarize(_plan("lv1", 600.0, losses=[]))
    # the hub measured lv1's largest 10-min gap at 26.3-29.1 ms: the stalls reach ~29 ms, never 35
    assert 20.0 <= s["max_send_gap_ms"] <= 31.0
    assert s["stalls"] >= 10
    assert s["max_burst_packets"] >= 3


def test_stress_gaps_reach_35_ms():
    s = vp.summarize(_plan("stress", 600.0, losses=[]))
    assert 30.0 <= s["max_send_gap_ms"] <= 41.0
    assert s["stalls"] >= 50


def test_a_stall_holds_its_packets_and_releases_them_as_one_burst():
    pk = _plan("stress", 120.0, losses=[])
    stalled = [p for p in pk if p.stalled]
    assert stalled
    first = stalled[0]
    run = [p for p in stalled if p.release_ns == first.release_ns]
    assert len(run) >= 2
    assert all(p.release_ns > p.deadline_ns for p in run)


def test_the_plan_is_deterministic_per_seed():
    a = _plan("lv1", 60.0, seed=7)
    b = _plan("lv1", 60.0, seed=7)
    c = _plan("lv1", 60.0, seed=8)
    assert a == b and a != c


# --- losses + the VBAN counter -------------------------------------------------------------------

def test_a_lost_packet_still_takes_its_counter_and_its_samples():
    losses = vp.plan_losses([(10.0, 3)], 60.0, vp.ppm_units(0.0))
    pk = _plan("clean", 60.0, losses=losses)
    lost = [p for p in pk if p.lost]
    assert [p.index for p in lost] == [losses[0].packet + i for i in range(3)]
    assert [p.nu_frame for p in pk] == list(range(len(pk)))
    assert losses[0].sample == losses[0].packet * 256


def test_the_counter_wraps_at_32_bits():
    pk = _plan("clean", 3.0, start_frame=2 ** 32 - 2, losses=[])
    assert [p.nu_frame for p in pk[:4]] == [2 ** 32 - 2, 2 ** 32 - 1, 0, 1]


def test_the_default_losses_sit_at_the_profiles_fractions():
    u = vp.ppm_units(vp.PROFILES["stress"].ppm)
    losses = vp.plan_losses(vp.default_losses(vp.PROFILES["stress"], 600.0), 600.0, u)
    assert [lo.count for lo in losses] == [1, 2, 4, 8]
    assert [round(vp.deadline_ns(lo.packet, u) / NS) for lo in losses] == [120, 240, 360, 480]


@pytest.mark.parametrize("bad", [[(0.5, 1)], [(59.5, 1)], [(10.0, 2), (10.004, 1)], [(10.0, 9999)]])
def test_bad_losses_are_refused(bad):
    with pytest.raises(ValueError):
        vp.plan_losses(bad, 60.0, vp.ppm_units(0.0))


def test_loss_spec_parsing():
    assert vp.parse_losses("none") == []
    assert vp.parse_losses("30:1, 60.5:4") == [(30.0, 1), (60.5, 4)]
    for bad in ("30", "30:0", "x:1"):
        with pytest.raises(ValueError):
            vp.parse_losses(bad)


# --- the wire: header + pilot --------------------------------------------------------------------

def test_the_header_reads_back_through_the_one_vban_parser():
    hdr = vp.vban_header("bench1460", 123456789)
    h = vban_rate.parse_vban(hdr + b"\x00" * 1024)
    assert (h.name, h.sample_rate, h.samples_per_frame, h.channels, h.frame) == (
        "bench1460", 48000, 256, 2, 123456789)
    assert hdr[7] == vp.VBAN_FMT_PCM16


@pytest.mark.parametrize("name", ["", "x" * 17, "bad name", "čau"])
def test_a_bad_stream_name_is_refused(name):
    with pytest.raises(ValueError):
        vp.vban_header(name, 0)


def test_the_pilot_is_continuous_across_packets_and_both_channels_carry_it():
    amp = vp.amplitude(-12.0)
    parts = b"".join(vp.pilot_block(n * 256, 256, 997.0, amp) for n in range(40))
    whole = vp.pilot_block(0, 40 * 256, 997.0, amp)
    assert parts == whole
    st = np.frombuffer(whole, dtype="<i2").reshape(-1, 2)
    assert np.array_equal(st[:, 0], st[:, 1])
    k = np.arange(st.shape[0])
    want = np.round(np.sin(2 * np.pi * 997.0 * k / 48000) * amp * 32767)
    assert np.max(np.abs(st[:, 0] - want)) <= 1
    assert 20 * np.log10(np.max(np.abs(st[:, 0])) / 32767) == pytest.approx(-12.0, abs=0.05)


def test_a_packet_carries_the_pilot_at_its_own_sample_index():
    amp = vp.amplitude(-12.0)
    p = vp.PlannedPacket(10, 10, 0, 0, False, False)
    after = vp.packet_bytes("x", p, 997.0, amp)[vban_rate.VBAN_HEADER_LEN:]
    assert after == vp.pilot_block(10 * 256, 256, 997.0, amp)


@pytest.mark.parametrize("port", [6980, 6981, 6990, 80, 70000])
def test_the_ports_of_real_receivers_are_refused(port):
    assert vp.port_refusal(port)


def test_a_free_port_is_accepted():
    assert vp.port_refusal(6995) is None


def test_the_summary_reports_the_planned_rate():
    s = vp.summarize(_plan("stress", 120.0, ppm=-50.0))
    assert s["rate_ppm"] == pytest.approx(-50.0, abs=0.01)
    assert s["lost"] == 15


# --- the sender CLI ------------------------------------------------------------------------------

def _run(args, **kw):
    return subprocess.run([sys.executable, str(EMULATOR)] + args, capture_output=True, text=True, **kw)


def test_dry_run_prints_the_schedule_and_writes_it(tmp_path):
    sched = tmp_path / "s.json"
    r = _run(["--dry-run", "--profile", "stress", "--ppm", "-50", "--duration-s", "60",
              "--schedule-out", str(sched), "--print-packets", "2"])
    assert r.returncode == 0, r.stderr
    assert "nothing is sent" in r.stdout and "loss: packet" in r.stdout and "stall:" in r.stdout
    assert r.stdout.count("\npacket ") == 2
    doc = json.loads(sched.read_text())
    assert doc["ppm"] == -50.0 and doc["sample_rate"] == 48000 and len(doc["losses"]) == 4
    assert doc["summary"]["rate_ppm"] == pytest.approx(-50.0, abs=0.01)


@pytest.mark.parametrize("args", [["--dry-run", "--port", "6980"], ["--dry-run", "--stream-name", "x" * 20],
                                  ["--profile", "lv1"], ["--dry-run", "--losses", "1:0"],
                                  ["--dry-run", "--level-dbfs", "0"]])
def test_bad_arguments_are_a_usage_error(args):
    r = _run(args)
    assert r.returncode == 3, r.stdout + r.stderr


def _receive_until_quiet(rx, got):
    while True:
        try:
            got.append(rx.recvfrom(2048)[0])
        except socket.timeout:
            return   # 5 s without a packet: the sender has finished


def test_the_sender_delivers_the_planned_stream_to_a_local_socket(tmp_path):
    rx = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    rx.bind(("127.0.0.1", 0))
    rx.settimeout(5.0)
    port = rx.getsockname()[1]
    got = []
    t = threading.Thread(target=_receive_until_quiet, args=(rx, got))
    t.start()
    stats = tmp_path / "stats.json"
    sched = tmp_path / "sched.json"
    r = _run(["--target", "127.0.0.1", "--port", str(port), "--profile", "lv1", "--duration-s", "2.5",
              "--losses", "1.2:2", "--stats-out", str(stats), "--schedule-out", str(sched)])
    t.join()
    rx.close()
    assert r.returncode == 0, r.stderr
    st = json.loads(stats.read_text())
    doc = json.loads(sched.read_text())
    n_planned = doc["summary"]["packets"]
    assert st["sent"] == len(got) == n_planned - 2 and st["lost"] == 2
    frames = [vban_rate.parse_vban(g).frame for g in got]
    lost = doc["losses"][0]["packet"]
    assert frames == [f for f in range(n_planned) if f not in (lost, lost + 1)]
    # the payload of each packet is the pilot at its own sample index
    amp = vp.amplitude(-12.0)
    for g in got[::50]:
        f = vban_rate.parse_vban(g).frame
        assert g[vban_rate.VBAN_HEADER_LEN:] == vp.pilot_block(f * 256, 256, 997.0, amp)
