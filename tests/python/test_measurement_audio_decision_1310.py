"""#1310 — PURE decision core for the dev1 measurement-audio presence watchdog.

The between-run gap the watchdog closes: the mbc measurement-audio chain (cam2 HDMI speaker → mic →
mbc Ableton → Dante → stream OBS `mbc`) reads DIGITAL SILENCE after a production and NOTHING pages it
— only the next full ~300 s E2E cycle discovers it (`recording-e2e.sh [4b2/8]` #748,
`max_volume -91.0 dB`, the 2026-07-12 week-long-muted-mic incident). This module is the pure kernel
of the watchdog that samples the mbc peak level out-of-band and decides when to page. No I/O, no WS,
no OBS — exhaustively unit-testable (pytest Tier-0, #557 kills local cargo), the #1199 python-mirror
pattern shared with `audio_lag_decision` / `dantesync_clock_decision`.

Verdicts (classify):
  SKIP    — stream OBS not reachable this pass (WS connect failed → the probe exited non-zero). That
            page is #1001 (network-reach) / #732 (bundle-state) territory, never this watchdog's, so
            paging requires a successfully-fetched positive reading and a dev1-side outage can only
            produce SKIP (never a false silent page).
  UNKNOWN — WS reachable but the `mbc` input never appeared in the meter stream this window (a
            renamed/removed input, or the InputVolumeMeters event unavailable on this build) → no
            reading to judge, held, never a fabricated page.
  SILENT  — reachable, `mbc` meter present, peak_db < threshold → page after a 2-pass confirm.
  PRESENT — reachable, `mbc` meter present, peak_db >= threshold → healthy.

The threshold is the SAME -60 dB bar as scripts/lib/audio-presence-preflight.sh
(`audio_preflight_is_silent`, #748); the orchestrator sources that lib and passes it in, so it is
NEVER retyped here (classify/analyze take it as a required argument — there is no hardcoded default).
SILENT uses strict `<` — byte-identical to `audio_preflight_is_silent`'s convention (exactly at the
threshold is PRESENT, not SILENT).
"""
import importlib.util
import pathlib
import subprocess
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_MODULE_PATH = _ROOT / "scripts" / "measurement_audio_decision.py"


def _load():
    spec = importlib.util.spec_from_file_location("measurement_audio_decision", _MODULE_PATH)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


mad = _load()

THRESH = -60.0


# ---- classify ------------------------------------------------------------------------------------

def test_classify_unreachable_is_skip():
    assert mad.classify(None, 0, 0, THRESH) == "SKIP"
    # box_reachable anything other than 1 is SKIP, even with a plausible reading
    assert mad.classify(-4.0, 0, 1, THRESH) == "SKIP"


def test_classify_meter_absent_is_unknown():
    # reachable but the mbc input never appeared in the meter stream
    assert mad.classify(None, 1, 0, THRESH) == "UNKNOWN"


def test_classify_meter_present_but_no_number_is_unknown():
    # defensive: meter_present=1 but no numeric level parsed -> UNKNOWN, never a false HEALTHY
    assert mad.classify(None, 1, 1, THRESH) == "UNKNOWN"


def test_classify_digital_silence_is_silent():
    # the -91 dB (#748) / clamped -100 dB digital-silence case
    assert mad.classify(-91.0, 1, 1, THRESH) == "SILENT"
    assert mad.classify(-100.0, 1, 1, THRESH) == "SILENT"


def test_classify_live_marker_is_present():
    # a live QPSK marker reads ~-5 dB
    assert mad.classify(-5.0, 1, 1, THRESH) == "PRESENT"


def test_classify_strict_boundary_matches_audio_preflight_is_silent():
    # exactly at the threshold is PRESENT (strict <), byte-identical to audio_preflight_is_silent
    assert mad.classify(-60.0, 1, 1, THRESH) == "PRESENT"
    # just below is SILENT
    assert mad.classify(-60.1, 1, 1, THRESH) == "SILENT"
    # just above is PRESENT
    assert mad.classify(-59.9, 1, 1, THRESH) == "PRESENT"


def test_classify_threshold_is_a_parameter_not_hardcoded():
    # a caller-supplied threshold is honored (proves -60 is not baked in)
    assert mad.classify(-70.0, 1, 1, -80.0) == "PRESENT"
    assert mad.classify(-85.0, 1, 1, -80.0) == "SILENT"


# ---- extract_probe -------------------------------------------------------------------------------

def test_extract_probe_present_reading():
    peak, present = mad.extract_probe("meter_present=1\npeak_db=-4.7\n")
    assert peak == -4.7
    assert present == 1


def test_extract_probe_silent_reading():
    peak, present = mad.extract_probe("meter_present=1\npeak_db=-100.0\n")
    assert peak == -100.0
    assert present == 1


def test_extract_probe_meter_absent():
    peak, present = mad.extract_probe("meter_present=0\npeak_db=\n")
    assert peak is None
    assert present == 0


def test_extract_probe_missing_meter_line_defaults_absent():
    # a truncated/garbled probe with no meter_present line is treated as absent (UNKNOWN), never present
    peak, present = mad.extract_probe("garbage\n")
    assert present == 0


def test_extract_probe_empty_input():
    peak, present = mad.extract_probe("")
    assert peak is None
    assert present == 0


def test_extract_probe_unparseable_peak_with_present_meter():
    peak, present = mad.extract_probe("meter_present=1\npeak_db=notanumber\n")
    assert peak is None
    assert present == 1


# ---- analyze -------------------------------------------------------------------------------------

def test_analyze_unreachable_skips_without_parsing():
    r = mad.analyze("meter_present=1\npeak_db=-4.0\n", 0, THRESH)
    assert r["verdict"] == "SKIP"
    assert r["peak_db"] is None
    assert r["meter_present"] == 0


def test_analyze_silent():
    r = mad.analyze("meter_present=1\npeak_db=-91.0\n", 1, THRESH)
    assert r["verdict"] == "SILENT"
    assert r["peak_db"] == -91.0
    assert r["meter_present"] == 1


def test_analyze_present():
    r = mad.analyze("meter_present=1\npeak_db=-5.4\n", 1, THRESH)
    assert r["verdict"] == "PRESENT"
    assert r["peak_db"] == -5.4


def test_analyze_meter_absent_unknown():
    r = mad.analyze("meter_present=0\npeak_db=\n", 1, THRESH)
    assert r["verdict"] == "UNKNOWN"


# ---- CLI -----------------------------------------------------------------------------------------

def _run_cli(args, stdin_text):
    return subprocess.run(
        [sys.executable, str(_MODULE_PATH)] + args,
        input=stdin_text, capture_output=True, text=True,
    )


def test_cli_analyze_silent_emits_key_value_lines():
    p = _run_cli(["analyze", "--box-reachable", "1", "--threshold-db", "-60"],
                 "meter_present=1\npeak_db=-91.0\n")
    assert p.returncode == 0, p.stderr
    out = dict(l.split("=", 1) for l in p.stdout.splitlines() if "=" in l)
    assert out["verdict"] == "SILENT"
    assert out["peak_db"] == "-91.0"
    assert out["meter_present"] == "1"


def test_cli_analyze_present():
    p = _run_cli(["analyze", "--box-reachable", "1", "--threshold-db", "-60"],
                 "meter_present=1\npeak_db=-4.2\n")
    assert p.returncode == 0, p.stderr
    out = dict(l.split("=", 1) for l in p.stdout.splitlines() if "=" in l)
    assert out["verdict"] == "PRESENT"


def test_cli_analyze_unreachable_needs_no_stdin():
    p = _run_cli(["analyze", "--box-reachable", "0", "--threshold-db", "-60"], "")
    assert p.returncode == 0, p.stderr
    out = dict(l.split("=", 1) for l in p.stdout.splitlines() if "=" in l)
    assert out["verdict"] == "SKIP"


def test_cli_threshold_is_required():
    # production must always source + pass the threshold; there is no hardcoded -60 fallback
    p = _run_cli(["analyze", "--box-reachable", "1"], "meter_present=1\npeak_db=-4.0\n")
    assert p.returncode != 0
