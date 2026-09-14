"""#1312 — tests for the PURE measurement-chain-latency kernel
(`scripts/measurement_chain_latency.py`): the between-productions read-only check that the mbc
measurement-audio chain (cam2 painter QPSK marker → speaker → mic → mbc Ableton → Dante → stream OBS
`mbc`) is still ALIGNED with the video within the E2E's ±90 ms gate, NOT merely audible (#1310's job).

The signal is a short PAIRED measurement: the cam2 marker log's emit timestamps
(`/run/rig-qpsk-markers.csv`, rows `index,frame_id,emit_ts_ns`) vs the stream `mbc` audio burst
ONSETS timestamped off the OBS-WS `InputVolumeMeters` peak on dev1. Per-marker latency = onset − emit;
median over ≥ 3 paired markers is compared to a persisted baseline, |now − baseline| > 90 ms → drift.

ALL logic (onset detection at the SAME −60 dB bar, pairing, median, the wall-clock-shape guard, the
classify decision + baseline r/w) lives in the pure module and is exhaustively pytest-tested here
(Tier-0 #557: no cargo, no live box). The OBS-WS sampler + the ssh marker read are the thin I/O half
(`scripts/measurement_chain_latency_probe.py` / `scripts/measurement-chain-latency.sh`).
"""

import json
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import measurement_chain_latency as m

# --- fixture builders ---------------------------------------------------------------------------
# a plausible UNIX-epoch ns anchor (2026-09-14-ish) — a wall-clock emit_ts is ~1.7e18 ns.
WALL0 = 1_757_800_000_000_000_000
CADENCE_NS = 5_000_000_000  # ~5 s (300 painter ticks @ 60 Hz — the permanent-unit default)
LOUD = -8.0
SILENT = -90.0
THRESH = -60.0


def marker_csv(emit_ts_list):
    """A realistic marker CSV: the `#` params comment, the header row, then `index,frame_id,emit_ts_ns`
    data rows (index/frame_id values are irrelevant to the latency — only the 3rd field matters)."""
    lines = ["# qpsk-params sr=48000 carrier=6000 c=2 q=2 vr=1001/60000", "index,frame_id,emit_ts_ns"]
    for i, ts in enumerate(emit_ts_list):
        lines.append(f"{i % 256},{1000 + i * 300},{ts}")
    return "\n".join(lines) + "\n"


def meter_text(onset_ts_list, latency_ns=None, burst_len=3, dt_ns=50_000_000):
    """Build an `InputVolumeMeters` sample stream (`sample <t_ns> <db>` rows): a leading SILENT sample
    (so the detector is armed), then for each onset a short LOUD burst preceded by a SILENT gap.
    `onset_ts_list` are the burst-start wall times on dev1's clock."""
    rows = [f"sample {onset_ts_list[0] - 10 * dt_ns} {SILENT}"]  # leading silence → arm the detector
    for on in onset_ts_list:
        rows.append(f"sample {on - dt_ns} {SILENT}")             # gap right before the burst
        for k in range(burst_len):
            rows.append(f"sample {on + k * dt_ns} {LOUD}")       # the burst (first sample IS the onset)
        rows.append(f"sample {on + burst_len * dt_ns} {SILENT}")  # burst ends
    return "\n".join(rows) + "\n"


def emits(n, base=WALL0):
    return [base + i * CADENCE_NS for i in range(n)]


# --- low-level parsers --------------------------------------------------------------------------
def test_parse_marker_csv_skips_header_comment_and_malformed():
    text = ("# qpsk-params sr=48000\n"
            "index,frame_id,emit_ts_ns\n"
            "3,1300,1757800000000000000\n"
            "\n"
            "garbage line\n"
            "4,1600,1757805000000000000\n"
            "5,notanint\n")  # too few / malformed fields skipped
    assert m.parse_marker_csv(text) == [1757800000000000000, 1757805000000000000]
    assert m.parse_marker_csv("") == []
    assert m.parse_marker_csv("index,frame_id,emit_ts_ns\n") == []


def test_parse_meter_samples():
    text = "sample 1757800000000000000 -8.0\nnot a sample\nsample 1757800050000000000 -90\n"
    assert m.parse_meter_samples(text) == [
        (1757800000000000000, -8.0),
        (1757800050000000000, -90.0),
    ]
    assert m.parse_meter_samples("") == []


def test_wall_clock_shape_guard():
    assert m.looks_like_wall_clock_ns(WALL0) is True
    assert m.looks_like_wall_clock_ns(320_000_000_000) is False   # ~5 min monotonic elapsed
    assert m.emits_are_wall_clock(emits(6)) is True
    assert m.emits_are_wall_clock([320_000_000_000, 640_000_000_000]) is False  # monotonic painter
    assert m.emits_are_wall_clock([]) is False


# --- onset detection ----------------------------------------------------------------------------
def test_detect_onsets_rising_edge_with_debounce():
    # one clean burst → exactly one onset (the first sample at/above the bar after silence)
    samples = [(0, SILENT), (10, LOUD), (20, LOUD), (30, SILENT), (40, LOUD)]
    assert m.detect_onsets(samples, THRESH) == [10, 40]
    # starting ABOVE the bar (mid-burst) does NOT count until it has dropped below then risen again
    samples2 = [(0, LOUD), (10, LOUD), (20, SILENT), (30, LOUD)]
    assert m.detect_onsets(samples2, THRESH) == [30]
    # exactly at the bar counts as present (>= threshold, matching audio_preflight strict-< silence)
    assert m.detect_onsets([(0, SILENT), (10, THRESH)], THRESH) == [10]
    assert m.detect_onsets([], THRESH) == []


# --- pairing ------------------------------------------------------------------------------------
def test_pair_latencies_pairs_onset_to_latest_prior_emit_within_window():
    e = [1000, 6000, 11000]
    o = [1120, 6100, 11150]  # each onset ~120-150 after its emit
    assert m.pair_latencies(e, o, max_pair_ns=2000) == [120, 100, 150]


def test_pair_latencies_tolerates_a_missed_marker():
    # only 2 of 3 markers produced a detectable onset → 2 latencies, no crash
    e = [1000, 6000, 11000]
    o = [1120, 11150]
    assert m.pair_latencies(e, o, max_pair_ns=2000) == [120, 150]


def test_pair_latencies_rejects_out_of_window_and_negative():
    e = [1000, 6000]
    # an onset far past its emit (> window) is not a real pairing; an onset before any emit is dropped
    o = [500, 9000]  # 500 has no prior emit; 9000-6000=3000 > 2000 window
    assert m.pair_latencies(e, o, max_pair_ns=2000) == []


def test_median_ms():
    assert m.median_ms([100_000_000, 120_000_000, 140_000_000]) == 120.0  # odd
    assert m.median_ms([100_000_000, 140_000_000]) == 120.0               # even → mean
    assert m.median_ms([]) is None


# --- measure (end to end over fixtures) ---------------------------------------------------------
def test_measure_wall_clock_healthy_chain():
    lat = 120_000_000  # 120 ms audio-chain latency
    e = emits(6)
    onsets = [x + lat for x in e]
    res = m.measure(marker_csv(e), meter_text(onsets), THRESH, max_pair_ns=2_000_000_000)
    assert res["markers"] == 6
    assert res["onsets"] == 6
    assert res["paired"] == 6
    assert res["wall_clock_ok"] is True
    assert abs(res["latency_ms"] - 120.0) < 1.0


def test_measure_monotonic_emit_ts_is_not_paired():
    # the PERMANENT painter emits monotonic-since-start emit_ts (no --wall-clock): not comparable to
    # the dev1 onset wall clock → never paired, latency None, wall_clock_ok False (→ UNKNOWN, never a
    # false drift). This is the #1312 STEP-0 finding guarded in code.
    mono = [320_000_000_000 + i * CADENCE_NS for i in range(6)]  # elapsed-since-start ns
    onsets = [WALL0 + i * CADENCE_NS + 120_000_000 for i in range(6)]
    res = m.measure(marker_csv(mono), meter_text(onsets), THRESH, max_pair_ns=2_000_000_000)
    assert res["markers"] == 6
    assert res["wall_clock_ok"] is False
    assert res["paired"] == 0
    assert res["latency_ms"] is None


# --- classify (the decision table) --------------------------------------------------------------
def _classify(latency_ms=120.0, baseline_ms=118.0, paired=6, box_reachable=1, markers=6,
              wall_clock_ok=True, tolerance_ms=90.0, min_paired=3):
    return m.classify(latency_ms, baseline_ms, paired, box_reachable, markers, wall_clock_ok,
                      tolerance_ms, min_paired)


def test_classify_decision_table():
    assert _classify(latency_ms=120.0, baseline_ms=118.0) == m.ALIGNED       # |120-118|=2 <= 90
    assert _classify(latency_ms=120.0, baseline_ms=200.0) == m.ALIGNED       # |120-200|=80 <= 90
    assert _classify(latency_ms=-20.0, baseline_ms=120.0) == m.DRIFTED       # |−20−120|=140 > 90
    assert _classify(box_reachable=0) == m.SKIP                              # stream OBS down
    assert _classify(markers=0, wall_clock_ok=False) == m.UNKNOWN            # cam2 down / no log
    assert _classify(wall_clock_ok=False) == m.UNKNOWN                       # monotonic emit_ts
    assert _classify(baseline_ms=None) == m.NO_BASELINE                      # not seeded yet
    assert _classify(paired=2) == m.UNKNOWN                                  # < min_paired onsets
    assert _classify(latency_ms=None, paired=6) == m.UNKNOWN                 # no median


def test_classify_order_skip_beats_everything():
    # box unreachable is SKIP even if everything else looks bad — dev1-side outage is #1001/#732
    assert _classify(box_reachable=0, markers=0, wall_clock_ok=False, baseline_ms=None) == m.SKIP


# --- baseline r/w -------------------------------------------------------------------------------
def test_baseline_roundtrip(tmp_path):
    p = str(tmp_path / "sub" / "measurement-chain-latency-baseline.json")
    assert m.read_baseline(p) is None                 # missing file → None (NO-BASELINE)
    m.write_baseline(p, 123.4)
    assert m.read_baseline(p) == 123.4
    # a corrupt / non-numeric file reads None, never a fabricated number
    with open(p, "w") as fh:
        fh.write("not json")
    assert m.read_baseline(p) is None


# --- CLI (subprocess, the exact shape the bash orchestrator invokes) ------------------------------
def _run_cli(args, cwd=None):
    return subprocess.run([sys.executable, str(_SCRIPTS / "measurement_chain_latency.py")] + args,
                          capture_output=True, text=True, cwd=cwd)


def _kv(out):
    d = {}
    for ln in out.splitlines():
        if "=" in ln:
            k, v = ln.split("=", 1)
            d[k] = v
    return d


def test_cli_classify_drifted(tmp_path):
    e = emits(6)
    lat = 250_000_000  # 250 ms — a big DC step vs a 5 ms baseline (the −140 ms class of incident)
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    bl = tmp_path / "baseline.json"
    mk.write_text(marker_csv(e))
    mt.write_text(meter_text([x + lat for x in e]))
    bl.write_text(json.dumps({"baseline_ms": 5.0}))
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl)])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.DRIFTED
    assert kv["box_reachable"] == "1"
    assert abs(float(kv["latency_ms"]) - 250.0) < 2.0


def test_cli_write_baseline(tmp_path):
    e = emits(6)
    lat = 130_000_000
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    bl = tmp_path / "baseline.json"
    mk.write_text(marker_csv(e))
    mt.write_text(meter_text([x + lat for x in e]))
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl),
                  "--write-baseline"])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == "BASELINE-WRITTEN"
    assert bl.exists()
    assert abs(m.read_baseline(str(bl)) - 130.0) < 2.0


def test_cli_write_baseline_refuses_monotonic(tmp_path):
    # --write-baseline must NEVER persist a monotonic-emit measurement (it is not comparable to the
    # onset wall clock) — the file stays absent and the verdict stays UNKNOWN (reason monotonic-emit).
    mono = [320_000_000_000 + i * CADENCE_NS for i in range(6)]
    onsets = [WALL0 + i * CADENCE_NS + 120_000_000 for i in range(6)]
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    bl = tmp_path / "baseline.json"
    mk.write_text(marker_csv(mono))
    mt.write_text(meter_text(onsets))
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl),
                  "--write-baseline"])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.UNKNOWN
    assert kv["reason"] == "monotonic-emit"
    assert not bl.exists()  # nothing written


def test_cli_write_baseline_refuses_too_few_onsets(tmp_path):
    # only 2 wall-clock markers -> paired < min_paired (3): --write-baseline must NOT overwrite an
    # existing baseline, and the verdict is UNKNOWN (too-few), never a persisted bad value.
    e = emits(2)
    onsets = [x + 120_000_000 for x in e]
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    bl = tmp_path / "baseline.json"
    mk.write_text(marker_csv(e))
    mt.write_text(meter_text(onsets))
    bl.write_text(json.dumps({"baseline_ms": 42.0}))  # a pre-existing baseline that must survive
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl),
                  "--write-baseline"])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.UNKNOWN
    assert m.read_baseline(str(bl)) == 42.0  # NOT overwritten by the too-few measurement


def test_cli_skip_when_box_unreachable(tmp_path):
    bl = tmp_path / "baseline.json"
    bl.write_text(json.dumps({"baseline_ms": 5.0}))
    # box unreachable: no meter/marker content needed — the CLI must not require it to report SKIP
    r = _run_cli(["classify", "--marker-file", "/nonexistent", "--meter-file", "/nonexistent",
                  "--box-reachable", "0", "--threshold-db", "-60", "--baseline-file", str(bl)])
    assert r.returncode == 0, r.stderr
    assert _kv(r.stdout)["verdict"] == m.SKIP


def test_cli_monotonic_emit_is_unknown(tmp_path):
    mono = [320_000_000_000 + i * CADENCE_NS for i in range(6)]
    onsets = [WALL0 + i * CADENCE_NS + 120_000_000 for i in range(6)]
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    bl = tmp_path / "baseline.json"
    mk.write_text(marker_csv(mono))
    mt.write_text(meter_text(onsets))
    bl.write_text(json.dumps({"baseline_ms": 120.0}))
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl)])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.UNKNOWN
    assert kv["reason"] == "monotonic-emit"
