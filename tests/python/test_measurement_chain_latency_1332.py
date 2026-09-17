"""#1332 — alias-aware pairing for the measurement-chain-latency kernel
(`scripts/measurement_chain_latency.py`).

REGRESSION: since issue 1318 the cam2 painter emits the QPSK marker every 0.5 s
(`--audio-marker-cadence-ticks 30`). The old `pair_latencies` pairs each onset with the LATEST emit
at or before it, assuming the pairing window (`DEFAULT_MAX_PAIR_MS = 2000`) is < half the cadence.
At a 0.5 s cadence the ~1.13 s real chain latency has FOUR candidate emits in the 2 s window, and
"latest prior emit" picks the alias `1132 − 2×500 ≈ 132 ms` → `classify` reads DRIFTED vs the
1140.7 ms baseline → the handover check (item 14) reports a FALSE "zabudol si: meracia zvukova cesta".

FIX (design-by main): alias-aware pairing with the persisted baseline as a PRIOR —
`pair_latencies(emits, onsets, max_pair_ns, prior_ns=…)` picks the candidate whose latency is closest
to the prior; without a prior and an ambiguous (emit-cadence < window) spacing it does NOT pair and
surfaces `ambiguous=True` → `classify` → UNKNOWN reason `ambiguous-cadence`, never DRIFTED. `--baseline`
refuses to persist an aliased value on an ambiguous cadence unless `--expected-ms <ms>` gives the prior.

Tier-0 (pytest, #557) — pure kernel only, no rig / no cargo.
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

# --- fixtures -----------------------------------------------------------------------------------
WALL0 = 1_757_800_000_000_000_000  # a plausible UNIX-epoch ns anchor (a wall-clock emit_ts ~1.7e18)
CAD_500 = 500_000_000              # 0.5 s marker cadence (30 painter ticks @ 60 Hz — the #1318 regime)
CAD_5S = 5_000_000_000            # the old 5 s cadence (300 ticks) — unambiguous vs the 2 s window
LAT_1132 = 1_132_000_000          # the real ~1.13 s chain latency
PRIOR_1140 = 1_140_700_000        # baseline 1140.7 ms in ns
MAX_PAIR = 2_000_000_000          # the DEFAULT_MAX_PAIR_MS = 2000 window in ns
DT_NS = 50_000_000                # ~50 ms InputVolumeMeters cadence
LOUD = -8.0
SILENT = -90.0
THRESH = -60.0


def marker_csv(emit_ts_list):
    lines = ["# qpsk-params sr=48000 carrier=6000 c=2 q=2 vr=1001/60000", "index,frame_id,emit_ts_ns"]
    for i, ts in enumerate(emit_ts_list):
        lines.append(f"{i % 256},{1000 + i * 300},{ts}")
    return "\n".join(lines) + "\n"


def meter_text(onset_ts_list, burst_len=3, dt_ns=DT_NS):
    """A continuous `sample <t_ns> <db>` stream: a SILENT floor with a short LOUD burst at each onset.
    `start` is placed so every onset lands exactly on the dt grid (the onsets are a multiple of dt
    apart in these fixtures), so `detect_onsets` returns the onset times without quantization noise."""
    start = onset_ts_list[0] - 10 * dt_ns
    end = onset_ts_list[-1] + (burst_len + 10) * dt_ns
    loud = set()
    for on in onset_ts_list:
        for k in range(burst_len):
            loud.add(round((on + k * dt_ns - start) / dt_ns))
    n = round((end - start) / dt_ns)
    return "\n".join(f"sample {start + i * dt_ns} {LOUD if i in loud else SILENT}"
                     for i in range(n + 1)) + "\n"


def emits(n, base=WALL0, cad=CAD_500):
    return [base + i * cad for i in range(n)]


def onsets_from(emit_list, lat_ns=LAT_1132, first=4, last_off=4):
    """Onsets at `lat_ns` after a middle slice of the emits (so each onset has both earlier and later
    candidate emits within the window — the ambiguity the fix must resolve)."""
    return [emit_list[i] + lat_ns for i in range(first, len(emit_list) - last_off)]


# --- new pure helpers (RED today: absent) -------------------------------------------------------
def test_emit_cadence_ns_is_median_inter_emit_gap():
    assert m.emit_cadence_ns(emits(8, cad=CAD_500)) == CAD_500
    assert m.emit_cadence_ns(emits(8, cad=CAD_5S)) == CAD_5S
    # a single jitter gap does not move the median
    e = [0, 500, 1000, 1400, 1900]  # gaps 500,500,400,500 → median 500
    assert m.emit_cadence_ns(e) == 500
    assert m.emit_cadence_ns([123]) is None   # < 2 emits → no cadence
    assert m.emit_cadence_ns([]) is None


def test_pairing_is_ambiguous():
    # cadence < window → more than one candidate can fall in the window → ambiguous
    assert m.pairing_is_ambiguous(CAD_500, MAX_PAIR) is True
    # cadence > window → at most one candidate → unambiguous (the old 5 s regime)
    assert m.pairing_is_ambiguous(CAD_5S, MAX_PAIR) is False
    assert m.pairing_is_ambiguous(None, MAX_PAIR) is False  # no cadence known → never ambiguous


# --- pairing with a prior (RED today: no prior_ns param) ----------------------------------------
def test_pair_latencies_prior_resolves_alias():
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    # WITHOUT a prior the legacy "latest prior emit" aliases to ~132 ms
    assert m.median_ms(m.pair_latencies(e, o, MAX_PAIR)) == 132.0
    # WITH the baseline as prior every onset pairs to the emit ~1132 ms before it
    with_prior = m.pair_latencies(e, o, MAX_PAIR, prior_ns=PRIOR_1140)
    assert m.median_ms(with_prior) == 1132.0
    assert len(with_prior) == len(o)


def test_pair_latencies_no_prior_legacy_byte_identical():
    # the exact shape the existing 1312 tests pin — no-prior path must stay latest-prior-emit
    e = [1000, 6000, 11000]
    o = [1120, 6100, 11150]
    assert m.pair_latencies(e, o, max_pair_ns=2000) == [120, 100, 150]
    assert m.pair_latencies(e, o, max_pair_ns=2000, prior_ns=None) == [120, 100, 150]


# --- measure (RED today: no prior_ns / no ambiguous key) ----------------------------------------
def test_measure_with_prior_resolves_alias():
    # (a) 0.5 s emits, onsets +1132 ms, prior 1140 → median ≈ 1132 (fails today: 132)
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    res = m.measure(marker_csv(e), meter_text(o), THRESH, MAX_PAIR, prior_ns=PRIOR_1140)
    assert res["wall_clock_ok"] is True
    assert res["ambiguous"] is False
    assert res["paired"] >= 3
    assert abs(res["latency_ms"] - 1132.0) < 5.0


def test_measure_without_prior_is_ambiguous():
    # (b) same, WITHOUT a prior → do not pair, surface ambiguous → reason ambiguous-cadence
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    res = m.measure(marker_csv(e), meter_text(o), THRESH, MAX_PAIR, prior_ns=None)
    assert res["ambiguous"] is True
    assert res["paired"] == 0
    assert res["latency_ms"] is None
    assert m.reason(m.UNKNOWN, res["markers"], res["wall_clock_ok"], res["paired"], 3,
                    chain_silent=res["chain_silent"], ambiguous=res["ambiguous"]) == "ambiguous-cadence"


def test_measure_5s_cadence_no_prior_unchanged():
    # (c) a 5 s (unambiguous) cadence with no prior keeps the old behaviour: NOT ambiguous, pairs
    e = emits(8, cad=CAD_5S)
    o = [x + 120_000_000 for x in e]
    res = m.measure(marker_csv(e), meter_text(o), THRESH, MAX_PAIR, prior_ns=None)
    assert res["ambiguous"] is False
    assert res["paired"] == len(o)
    assert abs(res["latency_ms"] - 120.0) < 5.0


# --- classify / reason gain `ambiguous` (RED today: no ambiguous kwarg) --------------------------
def test_classify_ambiguous_is_unknown_before_baseline():
    # ambiguous wins over the NO-BASELINE branch — never a false DRIFTED, never NO-BASELINE
    assert m.classify(None, None, 0, 1, 6, True, 90.0, 3, ambiguous=True) == m.UNKNOWN
    # a resolved (non-ambiguous) chain still classifies normally
    assert m.classify(1132.0, 1140.7, 6, 1, 6, True, 90.0, 3, ambiguous=False) == m.ALIGNED


def test_reason_ambiguous_cadence_token():
    assert m.reason(m.UNKNOWN, 6, True, 0, 3, chain_silent=False, ambiguous=True) == "ambiguous-cadence"
    # chain-silent still wins over ambiguous (a dead chain is the more important problem)
    assert m.reason(m.UNKNOWN, 6, True, 0, 3, chain_silent=True, ambiguous=True) == "chain-silent"


# --- CLI -----------------------------------------------------------------------------------------
def _run_cli(args):
    return subprocess.run([sys.executable, str(_SCRIPTS / "measurement_chain_latency.py")] + args,
                          capture_output=True, text=True)


def _kv(out):
    d = {}
    for ln in out.splitlines():
        if "=" in ln:
            k, v = ln.split("=", 1)
            d[k] = v
    return d


def _write_fixture(tmp_path, e, o):
    mk = tmp_path / "markers.txt"
    mt = tmp_path / "meter.txt"
    mk.write_text(marker_csv(e))
    mt.write_text(meter_text(o))
    return mk, mt


def test_cli_alias_incident_aligned_with_baseline(tmp_path):
    # (e) THE incident end-to-end: 0.5 s emits, +1132 ms onsets, baseline 1140.7 seeded →
    # verdict ALIGNED (fails today: DRIFTED, because the aliased 132 is compared to 1140.7).
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    mk, mt = _write_fixture(tmp_path, e, o)
    bl = tmp_path / "baseline.json"
    bl.write_text(json.dumps({"baseline_ms": 1140.7}))
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl)])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.ALIGNED
    assert abs(float(kv["latency_ms"]) - 1132.0) < 5.0


def test_cli_ambiguous_no_baseline_reason(tmp_path):
    # (f) 0.5 s emits, no baseline → UNKNOWN reason ambiguous-cadence (fails today: NO-BASELINE)
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    mk, mt = _write_fixture(tmp_path, e, o)
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60",
                  "--baseline-file", str(tmp_path / "baseline.json")])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == m.UNKNOWN
    assert kv["reason"] == "ambiguous-cadence"


def test_cli_baseline_refuses_ambiguous_without_expected(tmp_path):
    # (d) --write-baseline on an ambiguous cadence with NO prior must FAIL LOUD (non-zero) and NOT
    # persist an aliased value (fails today: exit 0, writes the 132 ms alias as the baseline).
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    mk, mt = _write_fixture(tmp_path, e, o)
    bl = tmp_path / "baseline.json"
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl),
                  "--write-baseline"])
    assert r.returncode != 0
    assert not bl.exists()  # nothing persisted
    assert "expected-ms" in r.stderr


def test_cli_baseline_writes_with_expected(tmp_path):
    # (d) with --expected-ms the prior disambiguates and the REAL measured value is persisted
    e = emits(24, cad=CAD_500)
    o = onsets_from(e)
    mk, mt = _write_fixture(tmp_path, e, o)
    bl = tmp_path / "baseline.json"
    r = _run_cli(["classify", "--marker-file", str(mk), "--meter-file", str(mt),
                  "--box-reachable", "1", "--threshold-db", "-60", "--baseline-file", str(bl),
                  "--write-baseline", "--expected-ms", "1140.7"])
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == "BASELINE-WRITTEN"
    assert bl.exists()
    assert abs(m.read_baseline(str(bl)) - 1132.0) < 5.0
