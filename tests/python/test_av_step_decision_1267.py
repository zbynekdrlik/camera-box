"""#1267 — unit tests for the PURE dev1 upstream-audio-latency STEP decision core
(scripts/av_step_decision.py).

The 2026-09-01 incident was an UPSTREAM audio-chain latency STEP (the mastered Dante feed into the
stream box's DVS `mbc` source got ≈ −60…−90 ms later) at a CONSTANT genlock pin, flagged ~3 h before
the E2E A/V gate failed. The dev1 watchdog reads the `av_offset_*` facets bundle_state_gather (#1267)
exposes on :8899 and decides when to page a report-only ⚠️. This module is that decision, pure and
exhaustively unit-testable (the audio_lag #1226 / #1199 python-mirror precedent, so it RED->GREENs
LOCALLY under Tier-0 — pytest runs freely, cargo is banned).

The genlock pin is a COVARIATE, NEVER subtracted (a live pin jump 976->1024 left the raw offset
~unchanged, so `offset - pin` reads a phantom step): a pin move in the analyzed span -> REPIN
(report-only), so a STEP is only ever judged across a constant-pin window.
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_step_decision as asd  # noqa: E402


# ------------------------------------------------------------------ classify_av_step
def _c(recent, base, pin_stable, age, n_recent, n_base, reachable=1, **kw):
    return asd.classify_av_step(recent, base, pin_stable, age, n_recent, n_base, reachable, **kw)


def test_constant_pin_sustained_step_pages():
    # the 2026-09-01 shape: baseline +68, recent +8, constant pin, plenty of samples, fresh.
    assert _c(8.0, 68.0, "1", 30, 20, 20) == "STEP"


def test_within_band_is_healthy():
    # normal 10-min medians wander ±30; a 30 ms difference at threshold 45 is NOT a step.
    assert _c(38.0, 68.0, "1", 30, 20, 20) == "HEALTHY"


def test_threshold_is_strict_greater_than():
    # exactly at the 45 ms threshold is NOT a step (strict >).
    assert _c(23.0, 68.0, "1", 30, 20, 20) == "HEALTHY"
    assert _c(22.9, 68.0, "1", 30, 20, 20) == "STEP"


def test_step_direction_agnostic():
    # a POSITIVE upstream shift (recent higher than baseline) steps too.
    assert _c(120.0, 60.0, "1", 30, 20, 20) == "STEP"


def test_pin_move_is_repin_never_a_step_even_with_a_big_raw_diff():
    # the covariate: a pin change during the window makes a big raw offset diff a REPIN, not a STEP —
    # exactly the false page the pin covariate must prevent (offset - pin would read a phantom step).
    assert _c(8.0, 54.0, "0", 30, 20, 20) == "REPIN"


def test_missing_pin_stable_flag_never_masks_a_step_as_healthy():
    # a None/absent flag is NOT "1", so it holds (REPIN), never silently pages OR silently passes.
    assert _c(8.0, 68.0, None, 30, 20, 20) == "REPIN"


def test_stale_series_is_never_a_step_page():
    # dock stopped while the log advanced: decided BEFORE the step check, so a stale series never
    # false-pages even with a huge apparent step.
    assert _c(8.0, 68.0, "1", 999, 20, 20) == "STALE"
    # age exactly at the threshold is NOT stale (strict >).
    assert _c(8.0, 68.0, "1", 300, 20, 20) == "STEP"
    assert _c(8.0, 68.0, "1", 301, 20, 20) == "STALE"


def test_age_none_skips_stale_branch():
    # an old box with no freshness facet (age None) must not be treated as stale.
    assert _c(8.0, 68.0, "1", None, 20, 20) == "STEP"


def test_too_few_samples_is_unknown_never_a_false_step():
    assert _c(8.0, 68.0, "1", 30, 5, 20) == "UNKNOWN"     # thin recent window
    assert _c(8.0, 68.0, "1", 30, 20, 5) == "UNKNOWN"     # thin baseline window
    assert _c(8.0, 68.0, "1", 30, 6, 6) == "STEP"         # exactly at min_samples is enough


def test_absent_facet_is_unknown():
    assert _c(None, None, None, None, None, None) == "UNKNOWN"
    assert _c(8.0, None, "1", 30, 20, 20) == "UNKNOWN"    # baseline missing (thin/no baseline window)


def test_unreachable_is_skip_before_anything_else():
    # a dev1-side outage can only ever produce SKIP -> never a false page (no reference-anchor needed).
    assert _c(8.0, 68.0, "1", 30, 20, 20, reachable=0) == "SKIP"
    assert _c(None, None, None, None, None, None, reachable=0) == "SKIP"


def test_env_overridable_thresholds():
    # a tighter threshold catches a smaller step; a looser one tolerates it.
    assert _c(50.0, 68.0, "1", 30, 20, 20, step_threshold_ms=10) == "STEP"
    assert _c(8.0, 68.0, "1", 30, 20, 20, step_threshold_ms=100) == "HEALTHY"


# ------------------------------------------------------------------ extract_av_step / analyze
_STEP_JSON = json.dumps({
    "av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "68.0", "av_offset_pin": "926",
    "av_offset_pin_stable": "1", "av_offset_age_s": "30", "av_offset_n_recent": "20",
    "av_offset_n_base": "20",
})


def test_extract_reads_every_field():
    recent, base, pin, ps, age, nr, nb = asd.extract_av_step(_STEP_JSON)
    assert (recent, base, pin, ps, age, nr, nb) == (8.0, 68.0, 926, "1", 30, 20, 20)


def test_extract_absent_all_none():
    assert asd.extract_av_step("{}") == (None, None, None, None, None, None, None)
    assert asd.extract_av_step("not json") == (None, None, None, None, None, None, None)
    assert asd.extract_av_step("") == (None, None, None, None, None, None, None)


def test_extract_empty_string_fields_are_none():
    body = json.dumps({"av_offset_recent_med_ms": "", "av_offset_pin_stable": ""})
    recent, _b, _p, ps, *_ = asd.extract_av_step(body)
    assert recent is None and ps is None


def test_analyze_step_dict():
    d = asd.analyze(_STEP_JSON, 1)
    assert d["verdict"] == "STEP"
    assert d["recent_med_ms"] == 8.0 and d["base_med_ms"] == 68.0
    assert d["pin"] == 926 and d["step_ms"] == -60.0
    # the full dict carries the fields the shell reads; recovered is None without a recovery_base.
    assert d["age_s"] == 30 and d["pin_stable"] == "1"
    assert d["n_recent"] == 20 and d["n_base"] == 20 and d["recovered"] is None


def test_analyze_skip_does_not_parse_body():
    d = asd.analyze("garbage-not-json", 0)
    assert d == {"verdict": "SKIP", "recent_med_ms": None, "base_med_ms": None, "pin": None,
                 "step_ms": None, "age_s": None, "pin_stable": None, "n_recent": None,
                 "n_base": None, "recovered": None}


# ------------------------------------------------------------------ frozen-baseline recovery (#1267 🟡)
def test_recovered_to_baseline():
    # the offset physically returned to the frozen pre-step baseline -> recovered.
    assert asd.recovered_to_baseline(65.0, 68.0) is True
    assert asd.recovered_to_baseline(68.0, 68.0) is True
    # still far from the pre-step baseline (a persistent step absorbed into the rolling base) -> not.
    assert asd.recovered_to_baseline(8.0, 68.0) is False
    # exactly at the threshold counts as recovered (<=).
    assert asd.recovered_to_baseline(68.0 - 45, 68.0) is True
    assert asd.recovered_to_baseline(68.0 - 45.1, 68.0) is False
    # no judgement possible when either is absent.
    assert asd.recovered_to_baseline(None, 68.0) is None
    assert asd.recovered_to_baseline(8.0, None) is None


def test_analyze_recovered_flag_only_with_a_recovery_base():
    # a persistent step that the rolling baseline absorbed (recent==base at the stepped level) reads
    # HEALTHY, but recovered=0 against the FROZEN pre-step base -> the watchdog HOLDs the alert.
    absorbed = json.dumps({"av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "8.0",
                           "av_offset_pin": "926", "av_offset_pin_stable": "1", "av_offset_age_s": "30",
                           "av_offset_n_recent": "20", "av_offset_n_base": "20"})
    assert asd.analyze(absorbed, 1)["verdict"] == "HEALTHY"
    assert asd.analyze(absorbed, 1, recovery_base=68.0)["recovered"] == 0     # NOT back to pre-step
    # a genuine physical recovery: recent returned to the frozen pre-step base -> recovered=1.
    healed = json.dumps({"av_offset_recent_med_ms": "66.0", "av_offset_base_med_ms": "66.0",
                         "av_offset_pin": "926", "av_offset_pin_stable": "1", "av_offset_age_s": "30",
                         "av_offset_n_recent": "20", "av_offset_n_base": "20"})
    assert asd.analyze(healed, 1, recovery_base=68.0)["recovered"] == 1
    # without a recovery_base the field stays None (not alerted -> nothing to judge).
    assert asd.analyze(absorbed, 1)["recovered"] is None


# ------------------------------------------------------------------ CLI
def _cli(body, reachable, *extra):
    p = subprocess.run(
        [sys.executable, str(_SCRIPTS / "av_step_decision.py"), "analyze",
         "--box-reachable", str(reachable), *extra],
        input=body.encode(), capture_output=True)
    assert p.returncode == 0, p.stderr.decode()
    out = {}
    for line in p.stdout.decode().splitlines():
        k, _, v = line.partition("=")
        out[k] = v
    return out


def test_cli_step():
    out = _cli(_STEP_JSON, 1)
    assert out["verdict"] == "STEP" and out["step_ms"] == "-60.0" and out["pin"] == "926"
    assert out["pin_stable"] == "1" and out["age_s"] == "30"


def test_cli_repin():
    body = json.dumps({"av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "54.0",
                       "av_offset_pin": "1024", "av_offset_pin_stable": "0",
                       "av_offset_age_s": "30", "av_offset_n_recent": "20", "av_offset_n_base": "20"})
    out = _cli(body, 1)
    assert out["verdict"] == "REPIN"


def test_cli_skip_needs_no_stdin():
    out = _cli("", 0)
    assert out["verdict"] == "SKIP" and out["recent_med_ms"] == "" and out["pin"] == ""
    assert out["recovered"] == ""


def test_cli_recovery_base_reports_recovered():
    absorbed = json.dumps({"av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "8.0",
                           "av_offset_pin": "926", "av_offset_pin_stable": "1", "av_offset_age_s": "30",
                           "av_offset_n_recent": "20", "av_offset_n_base": "20"})
    # absorbed step, still off the frozen pre-step base -> recovered=0 (the watchdog holds the alert).
    out = _cli(absorbed, 1, "--recovery-base", "68.0")
    assert out["verdict"] == "HEALTHY" and out["recovered"] == "0"
    # physical recovery -> recovered=1.
    healed = json.dumps({"av_offset_recent_med_ms": "66.0", "av_offset_base_med_ms": "66.0",
                         "av_offset_pin": "926", "av_offset_pin_stable": "1", "av_offset_age_s": "30",
                         "av_offset_n_recent": "20", "av_offset_n_base": "20"})
    out2 = _cli(healed, 1, "--recovery-base", "68.0")
    assert out2["recovered"] == "1"
