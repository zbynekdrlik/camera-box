"""#1231 — the STALE verdict + age extraction added to scripts/audio_lag_decision.py (the freshness
follow-up to the #1226 audio-lag watchdog decision core).

The gather now ships `audio_ts_lag_age_s` = the in-log age (seconds) of the freshest `#800` line
behind the OBS log's newest line. When that age exceeds a few emit periods, telemetry has stalled
while the log kept advancing (concern b) — a DISTINCT state the dev1 watchdog surfaces (machine-channel
log, NO phone page, per #1206), never a false HEALTHY and never a LAGGING page off a stale reading.

Pure decision, pytest Tier-0 (#557 kills cargo). The existing #1226 tests stay green: `classify`
gains age params with backward-compatible defaults, `analyze` keeps its `{verdict,lag_ms,src}` dict
(the age drives the verdict internally), and the CLI gains an ADDITIONAL `age_s=` line.
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


# ------------------------------------------------------------------ extract_audio_age
def test_extract_age_reads_facet():
    body = json.dumps({"audio_ts_lag_age_s": "600", "obs_version": "32.1.2"})
    assert ald.extract_audio_age(body) == 600


def test_extract_age_zero_is_zero_not_none():
    # A fresh box ships age "0" — a real value (telemetry alive), NOT absent.
    assert ald.extract_audio_age(json.dumps({"audio_ts_lag_age_s": "0"})) == 0


def test_extract_age_absent_is_none():
    assert ald.extract_audio_age(json.dumps({"obs_version": "32.1.2"})) is None
    assert ald.extract_audio_age(json.dumps({"audio_ts_lag_age_s": ""})) is None


def test_extract_age_bad_inputs_are_none():
    assert ald.extract_audio_age("not json") is None
    assert ald.extract_audio_age("") is None
    assert ald.extract_audio_age(None) is None
    assert ald.extract_audio_age(json.dumps({"audio_ts_lag_age_s": "abc"})) is None
    assert ald.extract_audio_age("[1,2,3]") is None


# ------------------------------------------------------------------ classify STALE
def test_classify_stale_age_over_threshold():
    # telemetry present but stale (stopped while the log advanced), lag empty -> STALE, not UNKNOWN.
    assert ald.classify(None, box_reachable=1, threshold_ms=5000,
                        age_s=600, stale_threshold_s=180) == "STALE"


def test_classify_fresh_age_is_not_stale():
    assert ald.classify(107, box_reachable=1, threshold_ms=5000,
                        age_s=0, stale_threshold_s=180) == "HEALTHY"


def test_classify_age_at_threshold_is_not_stale():
    # strictly greater-than: age == threshold is NOT yet stale (matches the lag threshold convention).
    assert ald.classify(None, box_reachable=1, threshold_ms=5000,
                        age_s=180, stale_threshold_s=180) == "UNKNOWN"


def test_classify_stale_beats_lag():
    # A stale reading must never become a LAGGING page — STALE is decided BEFORE the lag threshold.
    assert ald.classify(999999, box_reachable=1, threshold_ms=5000,
                        age_s=600, stale_threshold_s=180) == "STALE"


def test_classify_stale_defers_to_skip_when_unreachable():
    assert ald.classify(None, box_reachable=0, threshold_ms=5000,
                        age_s=600, stale_threshold_s=180) == "SKIP"


def test_classify_no_age_is_backward_compatible():
    # An old box (pre-#1231) ships no age facet -> age_s None -> pure #1226 lag decision.
    assert ald.classify(9999, box_reachable=1, threshold_ms=5000) == "LAGGING"
    assert ald.classify(107, box_reachable=1, threshold_ms=5000) == "HEALTHY"
    assert ald.classify(None, box_reachable=1, threshold_ms=5000) == "UNKNOWN"


# ------------------------------------------------------------------ analyze STALE
def test_analyze_stale_body():
    body = json.dumps({"audio_ts_lag_age_s": "600"})  # age present + large, no lag facet
    res = ald.analyze(body, box_reachable=1, threshold_ms=5000, stale_threshold_s=180)
    assert res == {"verdict": "STALE", "lag_ms": None, "src": None}


def test_analyze_fresh_lagging_still_lagging_with_age():
    body = json.dumps({"audio_ts_lag_ms": "1672741", "audio_ts_lag_src": "mbc",
                       "audio_ts_lag_age_s": "0"})
    res = ald.analyze(body, box_reachable=1, threshold_ms=5000, stale_threshold_s=180)
    assert res == {"verdict": "LAGGING", "lag_ms": 1672741, "src": "mbc"}


# ------------------------------------------------------------------ CLI contract (shell reads these)
def test_cli_emits_age_line_and_stale_verdict():
    body = json.dumps({"audio_ts_lag_age_s": "600"})
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "analyze", "--box-reachable", "1",
         "--threshold-ms", "5000", "--stale-threshold-s", "180"],
        input=body, capture_output=True, text=True, check=True,
    ).stdout
    fields = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert fields["verdict"] == "STALE"
    assert fields["age_s"] == "600"


def test_cli_age_line_empty_when_absent():
    body = json.dumps({"audio_ts_lag_ms": "107", "audio_ts_lag_src": "mbc"})
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "analyze", "--box-reachable", "1", "--threshold-ms", "5000"],
        input=body, capture_output=True, text=True, check=True,
    ).stdout
    fields = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert fields["verdict"] == "HEALTHY"
    assert fields["age_s"] == ""
