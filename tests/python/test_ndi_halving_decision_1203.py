"""#1203 -- tests for the PURE decision core of the NDI per-connection rate-halving auto-heal
watchdog (`scripts/ndi_halving_decision.py`).

Layer 1 (this file, RED->GREEN, local + CI): the pure decision matrix -- parse the recv-timing
#797 tap, measure the WITHIN-PASS rate + cap_avg, classify HALVED/HEALTHY/BORDERLINE/UNKNOWN/SKIP,
and the cure-vs-page escalation (cooldown). No I/O, no ssh, no OBS -- the strih-nic-selfheal #1199
python-mirror precedent, chosen so the decision RED->GREENs LOCALLY under Tier-0 #557 (cargo,
even --no-run, cannot run; the family `tests/harness_*.rs` are CI-only).

The bash orchestrator's confirm/throttle/cure GLUE has its own family-standard harness
(`tests/harness_ndi_halving_watchdog_1203.rs`); this file owns the pure matrix.
"""

import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import ndi_halving_decision as d


# The real DistroAV recv-timing line shape (vendor/distroav/src/ndi-source.cpp:1477 +
# obs_log's `[distroav]` prefix + OBS's `HH:MM:SS.mmm:` log-time prefix).
def line(ts, source, n, cap_avg, cap_max=33.1, out_avg=0.2, out_max=1.1):
    return (
        f"{ts}: [distroav] recv-timing #797 '{source}': "
        f"n={n} cap_avg={cap_avg:.2f}ms cap_max={cap_max:.2f}ms "
        f"out_avg={out_avg:.2f}ms out_max={out_max:.2f}ms"
    )


# A healthy 30 fps 2ME-PGM history: ~150 frames per ~5.0 s window, cap_avg ~12.6 ms.
def healthy_log(source="NDI 2ME PGM", base_ss=0, n=150, cap=12.60, count=6):
    out = []
    for i in range(count):
        ss = base_ss + i * 5
        mm, s = divmod(ss, 60)
        out.append(line(f"14:{mm:02d}:{s:02d}.017", source, n, cap))
    return "\n".join(out)


# The live-degraded 2ME-PGM history: n~75 per ~5.0 s (15,0/s) + cap_avg ~65,9 ms.
def halved_log(source="NDI 2ME PGM", base_ss=0, n=75, cap=65.90, count=6):
    out = []
    for i in range(count):
        ss = base_ss + i * 5
        mm, s = divmod(ss, 60)
        out.append(line(f"14:{mm:02d}:{s:02d}.017", source, n, cap))
    return "\n".join(out)


# =================================================================================================
# ts_to_seconds
# =================================================================================================

def test_ts_to_seconds_parses_obs_prefix_with_ms():
    assert d.ts_to_seconds("14:00:05.017") == 14 * 3600 + 5 + 0.017

def test_ts_to_seconds_strips_trailing_colon():
    assert d.ts_to_seconds("14:00:05.017:") == 14 * 3600 + 5 + 0.017

def test_ts_to_seconds_rejects_garbage():
    assert d.ts_to_seconds("not-a-time") is None
    assert d.ts_to_seconds("") is None
    assert d.ts_to_seconds("25:00:00") is None  # not a real clock hour


# =================================================================================================
# parse_recv_timing
# =================================================================================================

def test_parse_extracts_ts_n_capavg_for_the_named_source():
    text = halved_log(count=3)
    rows = d.parse_recv_timing(text, "NDI 2ME PGM")
    assert len(rows) == 3
    ts, n, cap = rows[-1]
    assert n == 75
    assert abs(cap - 65.90) < 1e-6
    assert ts is not None

def test_parse_source_name_is_exact_never_a_prefix_match():
    # 'NDI 2ME PGM' must NOT match 'NDI 2ME PGM (mv)' (the trailing ': anchor).
    text = "\n".join([
        line("14:00:05.017", "NDI 2ME PGM (mv)", 300, 8.0),
        line("14:00:10.017", "NDI 2ME PGM", 150, 12.6),
    ])
    rows = d.parse_recv_timing(text, "NDI 2ME PGM")
    assert len(rows) == 1
    assert rows[0][1] == 150

def test_parse_returns_empty_for_a_source_with_no_lines():
    assert d.parse_recv_timing(healthy_log(), "NDI cam9") == []


# =================================================================================================
# measure -- WITHIN-PASS rate from the last two lines (n is per-interval, reset-on-read)
# =================================================================================================

def test_measure_healthy_30fps_reads_about_30():
    m = d.measure(healthy_log(), "NDI 2ME PGM")
    assert m["samples"] >= 2
    assert abs(m["fps"] - 30.0) < 0.2   # 150 / ~5.0 s
    assert abs(m["window_s"] - 5.0) < 0.1
    assert abs(m["cap_avg"] - 12.60) < 1e-6

def test_measure_degraded_reads_about_15():
    m = d.measure(halved_log(), "NDI 2ME PGM")
    assert abs(m["fps"] - 15.0) < 0.2   # 75 / ~5.0 s
    assert abs(m["cap_avg"] - 65.90) < 1e-6

def test_measure_one_line_is_unmeasurable_but_tap_alive():
    text = line("14:00:05.017", "NDI 2ME PGM", 150, 12.6)
    m = d.measure(text, "NDI 2ME PGM")
    assert m["samples"] == 1
    assert m["fps"] is None      # need TWO lines to measure a per-interval rate
    assert m["window_s"] is None

def test_measure_missing_log_is_unmeasurable_and_blind():
    m = d.measure("", "NDI 2ME PGM")
    assert m["samples"] == 0
    assert m["fps"] is None

def test_measure_rejects_a_pair_straddling_a_gap_over_the_window_cap():
    # A freeze-recovery pair: prev at :00, curr 40 s later with a partial n -> window 40 s > cap.
    text = "\n".join([
        line("14:00:05.017", "NDI 2ME PGM", 150, 12.6),
        line("14:00:45.017", "NDI 2ME PGM", 30, 20.0),
    ])
    m = d.measure(text, "NDI 2ME PGM", max_window_s=15.0)
    assert m["fps"] is None       # unmeasurable (straddles a gap) -> reseed, never a false HALVED
    assert m["samples"] == 2      # the tap IS alive (2 lines parsed)

def test_measure_handles_midnight_wrap_in_the_date_less_log():
    text = "\n".join([
        line("23:59:58.000", "NDI 2ME PGM", 150, 12.6),
        line("00:00:03.000", "NDI 2ME PGM", 150, 12.6),
    ])
    m = d.measure(text, "NDI 2ME PGM")
    assert abs(m["window_s"] - 5.0) < 0.01
    assert abs(m["fps"] - 30.0) < 0.2


# =================================================================================================
# recency anchor -- a source that STOPPED emitting must not yield a stale verdict off old tail lines
# =================================================================================================

def test_newest_recv_timing_ts_is_the_max_across_all_sources():
    text = "\n".join([
        line("14:00:05.017", "NDI 2ME PGM", 75, 65.9),
        line("14:00:40.017", "NDI cam1", 300, 16.3),   # a later, still-flowing sibling
    ])
    assert d.newest_recv_timing_ts(text) == d.ts_to_seconds("14:00:40.017")

def test_newest_recv_timing_ts_none_on_empty():
    assert d.newest_recv_timing_ts("") is None

def test_measure_stale_pair_far_behind_log_now_is_unmeasurable():
    # The source's two lines are at :00/:05, but the log's newest line (any source) is 40 s later.
    m = d.measure(halved_log(), "NDI 2ME PGM",
                  log_now=d.ts_to_seconds("14:00:55.017"), stale_after_s=12.0)
    assert m["fps"] is None       # stopped emitting -> stale -> reseed, never a false verdict
    assert m["samples"] >= 2      # the lines ARE in the tail (tap alive), just old

def test_measure_fresh_pair_within_recency_is_measured():
    m = d.measure(halved_log(base_ss=0, count=6), "NDI 2ME PGM",
                  log_now=d.ts_to_seconds("14:00:26.017"), stale_after_s=12.0)
    assert abs(m["fps"] - 15.0) < 0.2   # newest line at :25, log_now :26 -> not stale

def test_measure_negative_gap_is_never_stale():
    # This source IS the newest (log_now == curr_ts) -> gap 0 -> measured normally.
    m = d.measure(halved_log(), "NDI 2ME PGM", log_now=d.ts_to_seconds("14:00:00.000"))
    assert m["fps"] is not None

def test_analyze_stale_input_reads_unknown_while_a_sibling_flows():
    # 2ME PGM stopped at :15; cam1 still flowing at :50 -> 2ME PGM is stale -> UNKNOWN (not HALVED).
    text = halved_log("NDI 2ME PGM", base_ss=0, count=4) + "\n" + \
        "\n".join(line(f"14:00:{45 + i:02d}.017", "NDI cam1", 300, 16.3) for i in range(3))
    a = d.analyze(text, "NDI 2ME PGM", 30, 1, 1)
    assert a["verdict"] == "UNKNOWN"
    assert a["samples"] >= 2     # tap alive (lines present), but stale -> never a false HALVED


# =================================================================================================
# classify -- the decision bands
# =================================================================================================

def test_classify_healthy_30fps():
    assert d.classify(30.0, 12.6, 30, 5.0, 1, 1) == "HEALTHY"

def test_classify_halved_by_rate():
    # 15 fps <= 0.6*30 = 18 -> HALVED even though cap 65.9 is just under 2*33.3.
    assert d.classify(15.0, 65.9, 30, 5.0, 1, 1) == "HALVED"

def test_classify_halved_by_cap_avg_even_when_rate_ok():
    # cap 70 ms >= 2*33.3 -> HALVED on the cap term alone.
    assert d.classify(29.0, 70.0, 30, 5.0, 1, 1) == "HALVED"

def test_classify_borderline_between_the_bands():
    # 22 fps: > 0.6*30 (18) but < 0.85*30 (25.5); cap 40 ms: <2x but >1.5x interval -> BORDERLINE.
    assert d.classify(22.0, 40.0, 30, 5.0, 1, 1) == "BORDERLINE"

def test_classify_unknown_when_unmeasurable():
    assert d.classify(None, None, 30, None, 1, 1) == "UNKNOWN"

def test_classify_unknown_when_window_too_short():
    assert d.classify(30.0, 12.6, 30, 1.0, 1, 1, min_window_s=3.0) == "UNKNOWN"

def test_classify_skip_when_box_unreachable():
    assert d.classify(15.0, 65.9, 30, 5.0, 0, 1) == "SKIP"

def test_classify_skip_when_not_expected_live():
    assert d.classify(15.0, 65.9, 30, 5.0, 1, 0) == "SKIP"

def test_classify_60fps_sibling_healthy_uses_its_own_interval():
    # A 60 fps input: healthy at 60/s, cap 16.3 ms (interval 16.67 ms).
    assert d.classify(60.0, 16.3, 60, 5.0, 1, 1) == "HEALTHY"

def test_classify_60fps_halved_reads_30():
    assert d.classify(30.0, 40.0, 60, 5.0, 1, 1) == "HALVED"


# =================================================================================================
# analyze -- parse + measure + classify end to end from a raw log
# =================================================================================================

def test_analyze_halved_log_end_to_end():
    a = d.analyze(halved_log(), "NDI 2ME PGM", 30, 1, 1)
    assert a["verdict"] == "HALVED"
    assert abs(a["fps"] - 15.0) < 0.2
    assert a["samples"] >= 2

def test_analyze_healthy_log_end_to_end():
    a = d.analyze(healthy_log(), "NDI 2ME PGM", 30, 1, 1)
    assert a["verdict"] == "HEALTHY"

def test_analyze_skip_when_unreachable_never_reads_the_log():
    a = d.analyze(halved_log(), "NDI 2ME PGM", 30, 0, 1)
    assert a["verdict"] == "SKIP"
    assert a["samples"] == 0

def test_analyze_missing_line_is_unknown_and_blind():
    a = d.analyze(healthy_log(source="NDI cam1"), "NDI 2ME PGM", 30, 1, 1)
    assert a["verdict"] == "UNKNOWN"
    assert a["samples"] == 0


# =================================================================================================
# cooldown + cure-decision
# =================================================================================================

def test_cooldown_elapsed_when_never_cured():
    assert d.cooldown_elapsed("", 1000, 600) is True
    assert d.cooldown_elapsed(None, 1000, 600) is True

def test_cooldown_not_elapsed_within_window():
    assert d.cooldown_elapsed("1000", 1300, 600) is False   # only 300 s since last cure

def test_cooldown_elapsed_past_window():
    assert d.cooldown_elapsed("1000", 1700, 600) is True    # 700 s >= 600

def test_cure_decision_page_when_cure_disabled():
    assert d.cure_decision(False, True) == "page"     # report-only: always alert, never cure

def test_cure_decision_cure_when_enabled_and_cooldown_ok():
    assert d.cure_decision(True, True) == "cure"

def test_cure_decision_page_when_enabled_but_within_cooldown():
    # already cured this episode + still halved -> page, never reattach-spam.
    assert d.cure_decision(True, False) == "page"


# =================================================================================================
# CLI (the seam the bash orchestrator actually calls)
# =================================================================================================

def _cli(args, stdin=""):
    return subprocess.run(
        [sys.executable, str(_SCRIPTS / "ndi_halving_decision.py"), *args],
        input=stdin, capture_output=True, text=True, check=False,
    )

def _kv(out):
    d2 = {}
    for ln in out.splitlines():
        if "=" in ln:
            k, v = ln.split("=", 1)
            d2[k] = v
    return d2

def test_cli_analyze_halved():
    r = _cli(
        ["analyze", "--source", "NDI 2ME PGM", "--expected-fps", "30",
         "--box-reachable", "1", "--expected-live", "1"],
        stdin=halved_log(),
    )
    assert r.returncode == 0, r.stderr
    kv = _kv(r.stdout)
    assert kv["verdict"] == "HALVED"
    assert abs(float(kv["fps"]) - 15.0) < 0.2
    assert int(kv["samples"]) >= 2

def test_cli_cure_decision():
    r = _cli(["cure-decision", "--cure-enabled", "1", "--last-cure-ts", "1000",
              "--now", "1700", "--cooldown-s", "600"])
    assert r.returncode == 0, r.stderr
    assert _kv(r.stdout)["action"] == "cure"

    r2 = _cli(["cure-decision", "--cure-enabled", "0", "--last-cure-ts", "",
               "--now", "1700", "--cooldown-s", "600"])
    assert _kv(r2.stdout)["action"] == "page"
