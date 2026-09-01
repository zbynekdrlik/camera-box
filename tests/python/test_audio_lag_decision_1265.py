"""#1265 — the BAND decision added to scripts/audio_lag_decision.py (`extract_ref_band`,
`classify_band`, `analyze_band`, the `band` CLI subcommand): the dev1 half of the tens-of-ms
per-REFERENCE-source ts_lag band watch.

The box exposes `audio_ref_lag_{src,base_ms,high_ms,low_ms,duty_pct,n}` (bundle_state_gather.
audio_ref_band_from_log); this grades the band SHAPE at tens-of-ms resolution — DRIFTING iff the
high mode sits a threshold above the flat-start baseline (or the tail low, if no baseline) AND a
meaningful FRACTION of the recent window is up there (a genuine bimodal flap, not one spike). Pure,
pytest Tier-0. The existing #1226/#1231 lag decision is UNTOUCHED (a separate dimension).
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import audio_lag_decision as ald  # noqa: E402

_DECIDE = _SCRIPTS / "audio_lag_decision.py"


def _band_body(**kw):
    base = {
        "audio_ref_lag_src": "mbc", "audio_ref_lag_base_ms": "107",
        "audio_ref_lag_high_ms": "181", "audio_ref_lag_low_ms": "107",
        "audio_ref_lag_duty_pct": "50", "audio_ref_lag_n": "14",
    }
    base.update({k: str(v) for k, v in kw.items()})
    return json.dumps(base)


# ------------------------------------------------------------------ extract_ref_band
def test_extract_ref_band_reads_all_fields():
    band = ald.extract_ref_band(_band_body())
    assert band == {"base_ms": 107, "high_ms": 181, "low_ms": 107, "duty_pct": 50, "n": 14, "src": "mbc"}


def test_extract_ref_band_absent_is_none_fields():
    band = ald.extract_ref_band(json.dumps({"obs_version": "32.2.0"}))
    assert band == {"base_ms": None, "high_ms": None, "low_ms": None, "duty_pct": None, "n": None, "src": None}


def test_extract_ref_band_empty_base_is_none_but_others_present():
    # base_ms "" (a small whole log with no startup region) is None; high/low/n still parse.
    band = ald.extract_ref_band(_band_body(audio_ref_lag_base_ms=""))
    assert band["base_ms"] is None
    assert band["high_ms"] == 181 and band["low_ms"] == 107 and band["n"] == 14


def test_extract_ref_band_bad_json_is_all_none():
    for bad in ("not json", "", None, "[1,2,3]"):
        band = ald.extract_ref_band(bad)
        assert band["high_ms"] is None and band["n"] is None


# ------------------------------------------------------------------ classify_band
def test_classify_band_unreachable_is_skip():
    assert ald.classify_band(107, 181, 107, 50, 14, box_reachable=0) == "SKIP"


def test_classify_band_absent_is_unknown():
    assert ald.classify_band(None, None, None, None, None, box_reachable=1) == "UNKNOWN"


def test_classify_band_too_few_samples_is_unknown():
    # n below min_samples -> not enough to characterize a band -> UNKNOWN, never a false page.
    assert ald.classify_band(107, 181, 107, 50, 5, box_reachable=1, min_samples=8) == "UNKNOWN"


def test_classify_band_default_min_samples_is_10_finding2():
    # #1265 review finding 2: at n<=9 the p90 nearest-rank index IS the max, so a lone startup spike
    # would read DRIFTING. The default min_samples=10 makes n=8/9 UNKNOWN (not judged) -- a single
    # spike among 8 (duty 12% > 10%, high=the spike) must NOT page.
    assert ald.classify_band(107, 300, 107, 12, 8, box_reachable=1) == "UNKNOWN"
    assert ald.classify_band(107, 300, 107, 11, 9, box_reachable=1) == "UNKNOWN"
    # at n>=10 a genuine bimodal flap is still DRIFTING (the default now bites at 10, not 8).
    assert ald.classify_band(107, 181, 107, 50, 10, box_reachable=1) == "DRIFTING"


def test_classify_band_head_elevated_uses_min_baseline_finding3():
    # #1265 review finding 3: a head that is ITSELF in the high mode (a restart straight into the bad
    # state) must not mask the drift. base=180 (elevated head median) but low=107 (tail low mode) ->
    # baseline=min(180,107)=107 -> deviation=181-107=74>40 -> DRIFTING. The OLD base-only rule would
    # have given deviation=181-180=1 -> a false HEALTHY.
    assert ald.classify_band(180, 181, 107, 50, 14, box_reachable=1) == "DRIFTING"


def test_classify_band_bimodal_flap_is_drifting():
    # high 181 vs baseline 107 = +74 > 40 dev threshold, duty 50% >= 10% -> DRIFTING.
    assert ald.classify_band(107, 181, 107, 50, 14, box_reachable=1) == "DRIFTING"


def test_classify_band_flat_is_healthy():
    assert ald.classify_band(107, 108, 107, 0, 14, box_reachable=1) == "HEALTHY"


def test_classify_band_high_deviation_but_low_duty_is_healthy():
    # a single spike: high deviation but only a few % of the window up there -> NOT a sustained band.
    assert ald.classify_band(107, 300, 107, 5, 14, box_reachable=1) == "HEALTHY"


def test_classify_band_no_baseline_uses_tail_low():
    # base None (small whole log) -> deviation measured against the tail low (107); still DRIFTING.
    assert ald.classify_band(None, 181, 107, 50, 14, box_reachable=1) == "DRIFTING"


def test_classify_band_deviation_at_threshold_is_not_drifting():
    # strictly greater-than (matches the lag-threshold convention): dev == threshold is HEALTHY.
    assert ald.classify_band(107, 147, 107, 50, 14, box_reachable=1, dev_threshold_ms=40) == "HEALTHY"


def test_classify_band_creeping_band_both_modes_up_still_drifting_via_baseline():
    # both modes crept up (low 150 / high 190) but base (flat start) is 107 -> +83 dev -> DRIFTING,
    # even though the within-window spread (190-150=40) alone would not trip.
    assert ald.classify_band(107, 190, 150, 60, 14, box_reachable=1) == "DRIFTING"


# ------------------------------------------------------------------ analyze_band
def test_analyze_band_drifting_dict():
    res = ald.analyze_band(_band_body(), box_reachable=1)
    assert res == {"verdict": "DRIFTING", "high_ms": 181, "base_ms": 107,
                   "low_ms": 107, "duty_pct": 50, "n": 14, "src": "mbc"}


def test_analyze_band_unreachable_skips_without_parsing():
    res = ald.analyze_band("", box_reachable=0)
    assert res["verdict"] == "SKIP"


# ------------------------------------------------------------------ CLI band contract (shell reads these)
def test_cli_band_drifting():
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "band", "--box-reachable", "1"],
        input=_band_body(), capture_output=True, text=True, check=True,
    ).stdout
    f = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert f["band_verdict"] == "DRIFTING"
    assert f["band_high_ms"] == "181"
    assert f["band_base_ms"] == "107"
    assert f["band_low_ms"] == "107"
    assert f["band_duty_pct"] == "50"
    assert f["band_n"] == "14"
    assert f["band_src"] == "mbc"


def test_cli_band_unreachable_skip():
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "band", "--box-reachable", "0"],
        input="", capture_output=True, text=True, check=True,
    ).stdout
    f = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert f["band_verdict"] == "SKIP"


# ------------------------------------------------------------------ the #1226/#1231 lag arm still works
def test_lag_arm_untouched():
    body = json.dumps({"audio_ts_lag_ms": "1672741", "audio_ts_lag_src": "mbc"})
    assert ald.analyze(body, box_reachable=1, threshold_ms=5000) == {
        "verdict": "LAGGING", "lag_ms": 1672741, "src": "mbc"}
