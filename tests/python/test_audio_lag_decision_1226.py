"""#1226 — PURE decision core for the dev1 audio-lag alert watchdog (scripts/audio_lag_decision.py).

The #1199 strih-nic-selfheal / #1203 ndi-halving python-mirror precedent: the decision RED->GREENs
LOCALLY under Tier-0 (#557 kills cargo), with no ssh/OBS/rig. The watchdog curls
`:8899/bundle-state.json` from strih+stream (the audio_ts_lag_ms facet #1226 adds to the gather) and
pages when a box's audio timeline sits sustained > threshold behind realtime.
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


# ------------------------------------------------------------------ extract_audio_lag
def test_extract_reads_lag_and_source():
    body = json.dumps({"audio_ts_lag_ms": "1672741", "audio_ts_lag_src": "mbc", "obs_version": "32.1.2"})
    assert ald.extract_audio_lag(body) == (1672741, "mbc")


def test_extract_missing_key_is_none():
    assert ald.extract_audio_lag(json.dumps({"obs_version": "32.1.2"})) == (None, None)


def test_extract_empty_value_is_none():
    assert ald.extract_audio_lag(json.dumps({"audio_ts_lag_ms": "", "audio_ts_lag_src": ""})) == (None, None)


def test_extract_bad_json_is_none():
    assert ald.extract_audio_lag("not json at all") == (None, None)
    assert ald.extract_audio_lag("") == (None, None)
    assert ald.extract_audio_lag(None) == (None, None)


def test_extract_non_integer_value_is_none():
    assert ald.extract_audio_lag(json.dumps({"audio_ts_lag_ms": "abc"})) == (None, None)


def test_extract_json_array_is_none():
    # A non-object top-level JSON must never crash / mis-read.
    assert ald.extract_audio_lag("[1,2,3]") == (None, None)


# ------------------------------------------------------------------ classify
def test_classify_box_unreachable_is_skip():
    # box_reachable != 1 -> SKIP: the box/:8899-down page is #732/#1001's job, not this watchdog's.
    assert ald.classify(1672741, box_reachable=0, threshold_ms=5000) == "SKIP"


def test_classify_absent_facet_is_unknown():
    assert ald.classify(None, box_reachable=1, threshold_ms=5000) == "UNKNOWN"


def test_classify_above_threshold_is_lagging():
    assert ald.classify(5001, box_reachable=1, threshold_ms=5000) == "LAGGING"
    assert ald.classify(1672741, box_reachable=1, threshold_ms=5000) == "LAGGING"


def test_classify_at_or_below_threshold_is_healthy():
    assert ald.classify(5000, box_reachable=1, threshold_ms=5000) == "HEALTHY"
    assert ald.classify(107, box_reachable=1, threshold_ms=5000) == "HEALTHY"
    assert ald.classify(0, box_reachable=1, threshold_ms=5000) == "HEALTHY"


# ------------------------------------------------------------------ analyze
def test_analyze_lagging_composes_verdict_and_values():
    body = json.dumps({"audio_ts_lag_ms": "1672741", "audio_ts_lag_src": "mbc"})
    res = ald.analyze(body, box_reachable=1, threshold_ms=5000)
    assert res == {"verdict": "LAGGING", "lag_ms": 1672741, "src": "mbc"}


def test_analyze_healthy():
    body = json.dumps({"audio_ts_lag_ms": "107", "audio_ts_lag_src": "mbc"})
    res = ald.analyze(body, box_reachable=1, threshold_ms=5000)
    assert res == {"verdict": "HEALTHY", "lag_ms": 107, "src": "mbc"}


def test_analyze_facet_absent_is_unknown():
    body = json.dumps({"obs_version": "32.1.2"})
    res = ald.analyze(body, box_reachable=1, threshold_ms=5000)
    assert res == {"verdict": "UNKNOWN", "lag_ms": None, "src": None}


def test_analyze_box_unreachable_skips_without_parsing():
    res = ald.analyze("", box_reachable=0, threshold_ms=5000)
    assert res == {"verdict": "SKIP", "lag_ms": None, "src": None}


# ------------------------------------------------------------------ CLI contract (shell reads these)
def test_cli_analyze_lagging():
    body = json.dumps({"audio_ts_lag_ms": "1672741", "audio_ts_lag_src": "mbc"})
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "analyze", "--box-reachable", "1", "--threshold-ms", "5000"],
        input=body, capture_output=True, text=True, check=True,
    ).stdout
    fields = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert fields["verdict"] == "LAGGING"
    assert fields["lag_ms"] == "1672741"
    assert fields["src"] == "mbc"


def test_cli_analyze_box_unreachable_skip():
    out = subprocess.run(
        [sys.executable, str(_DECIDE), "analyze", "--box-reachable", "0", "--threshold-ms", "5000"],
        input="", capture_output=True, text=True, check=True,
    ).stdout
    fields = dict(line.split("=", 1) for line in out.splitlines() if "=" in line)
    assert fields["verdict"] == "SKIP"
    assert fields["lag_ms"] == ""
    assert fields["src"] == ""
