"""#1319 P3 — the av-step STEP arm gets the SAME dock-measurement quality gate the BAND arm has.

WHY (16.9.2026, stream box, pin 939 stable): the STEP arm (`classify_av_step`/`analyze`, the path
`handle_box` calls) decided purely on `|recent_med - base_med| > threshold` and NEVER consulted the
dock estimator's scatter facets. On 16.9 it fired two owner pages (13:44 step 1069 ms, 15:09 step
-92 ms) while the medians it compared swung ±1000 ms within 5-min passes — impossible for a real
upstream shift, and rejected in the SAME passes by the BAND arm's `band_quality_ok()` (mad 31 > 15).
This module locks the fix: an untrustworthy dock reading (recent-window median MAD > 15 ms OR min
matched < 30, or a non-finite mad) classifies `LOW_QUALITY` (log-only) on the STEP arm too, never a
STEP page; an ABSENT quality facet (older box) proceeds to today's step judgement; the quality check
sits AFTER REPIN and BEFORE STEP/HEALTHY, mirroring the BAND arm exactly.
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_step_decision as asd  # noqa: E402


def _c(recent, base, pin_stable, age, n_recent, n_base, reachable=1, **kw):
    return asd.classify_av_step(recent, base, pin_stable, age, n_recent, n_base, reachable, **kw)


# ------------------------------------------------------------------ classify_av_step quality gate
def test_1509_low_quality_is_not_a_step():
    # the 15:09 live shape: a huge apparent step at a STABLE pin, plenty of samples, fresh, BUT the
    # dock estimator is noisy (mad 31 > 15) -> LOW_QUALITY, NEVER the STEP that paged the owner.
    v = _c(-494.1, 551.7, "1", 13, 19, 27, recent_mad_ms=31.0, recent_matched_min=10)
    assert v == "LOW_QUALITY", v


def test_1344_class_huge_step_bad_quality_is_low_quality():
    # the 13:44-class page: a ~1000 ms apparent step with an untrustworthy reading (thin cluster,
    # matched 8 < 30) -> LOW_QUALITY, not STEP.
    v = _c(940.0, -129.0, "1", 20, 20, 20, recent_mad_ms=22.0, recent_matched_min=8)
    assert v == "LOW_QUALITY", v


def test_good_quality_step_still_fires():
    # a trustworthy reading (mad 8 <= 15, matched 40 >= 30) with a real |Δ| > 45 STILL pages STEP.
    v = _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=8.0, recent_matched_min=40)
    assert v == "STEP", v


def test_good_quality_small_delta_is_healthy():
    # trustworthy reading, |Δ| within band -> HEALTHY (quality gate does not spuriously fire).
    v = _c(38.0, 68.0, "1", 30, 20, 20, recent_mad_ms=8.0, recent_matched_min=40)
    assert v == "HEALTHY", v


def test_absent_quality_facets_proceed_legacy_step():
    # an older box with no quality facet (mad/matched None) -> band_quality_ok None -> proceed to
    # today's judgement; a real step still pages (backward-compatible legacy path).
    assert _c(8.0, 68.0, "1", 30, 20, 20) == "STEP"                         # both None (defaults)
    assert _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=None,
              recent_matched_min=None) == "STEP"
    # one facet present, the other absent -> still UNJUDGEABLE (None) -> proceed to STEP.
    assert _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=8.0,
              recent_matched_min=None) == "STEP"
    assert _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=None,
              recent_matched_min=40) == "STEP"


def test_nan_mad_is_low_quality():
    # a corrupt (non-finite) mad is untrustworthy, distinct from ABSENT -> False -> LOW_QUALITY.
    assert _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=float("nan"),
              recent_matched_min=40) == "LOW_QUALITY"
    assert _c(8.0, 68.0, "1", 30, 20, 20, recent_mad_ms=float("inf"),
              recent_matched_min=40) == "LOW_QUALITY"


def test_verdict_order_repin_beats_low_quality():
    # a pin move wins over a bad-quality reading (quality sits AFTER REPIN) — mirrors the BAND arm.
    assert _c(8.0, 68.0, "0", 30, 20, 20, recent_mad_ms=31.0, recent_matched_min=10) == "REPIN"


def test_verdict_order_stale_and_unknown_beat_low_quality():
    # STALE / UNKNOWN are decided BEFORE quality, so a bad-quality reading that is also stale/thin
    # reads STALE/UNKNOWN, never LOW_QUALITY (the never-false-page order is preserved).
    assert _c(8.0, 68.0, "1", 999, 20, 20, recent_mad_ms=31.0, recent_matched_min=10) == "STALE"
    assert _c(8.0, 68.0, "1", 30, 5, 20, recent_mad_ms=31.0, recent_matched_min=10) == "UNKNOWN"
    assert _c(None, None, "1", 30, 20, 20, recent_mad_ms=31.0, recent_matched_min=10) == "UNKNOWN"


def test_unreachable_still_skip_first():
    assert _c(8.0, 68.0, "1", 30, 20, 20, reachable=0,
              recent_mad_ms=31.0, recent_matched_min=10) == "SKIP"


# ------------------------------------------------------------------ analyze() reads the facets
_1509_JSON = json.dumps({
    "av_offset_recent_med_ms": "-494.1", "av_offset_base_med_ms": "551.7", "av_offset_pin": "939",
    "av_offset_pin_stable": "1", "av_offset_age_s": "13", "av_offset_n_recent": "19",
    "av_offset_n_base": "27", "av_offset_recent_mad_ms": "31.0", "av_offset_recent_matched_min": "10",
})
_GOODQ_STEP_JSON = json.dumps({
    "av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "68.0", "av_offset_pin": "939",
    "av_offset_pin_stable": "1", "av_offset_age_s": "30", "av_offset_n_recent": "20",
    "av_offset_n_base": "20", "av_offset_recent_mad_ms": "8.0", "av_offset_recent_matched_min": "40",
})
_ABSENTQ_STEP_JSON = json.dumps({
    "av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "68.0", "av_offset_pin": "939",
    "av_offset_pin_stable": "1", "av_offset_age_s": "30", "av_offset_n_recent": "20",
    "av_offset_n_base": "20",  # no quality facets -> legacy STEP
})
_NANQ_JSON = json.dumps({
    "av_offset_recent_med_ms": "8.0", "av_offset_base_med_ms": "68.0", "av_offset_pin": "939",
    "av_offset_pin_stable": "1", "av_offset_age_s": "30", "av_offset_n_recent": "20",
    "av_offset_n_base": "20", "av_offset_recent_mad_ms": "NaN", "av_offset_recent_matched_min": "40",
})


def test_analyze_reads_quality_and_returns_low_quality():
    d = asd.analyze(_1509_JSON, 1)
    assert d["verdict"] == "LOW_QUALITY", d
    # the offset fields the shell logs are still populated on a LOW_QUALITY pass.
    assert d["recent_med_ms"] == -494.1 and d["base_med_ms"] == 551.7 and d["pin"] == 939


def test_analyze_good_quality_still_steps():
    assert asd.analyze(_GOODQ_STEP_JSON, 1)["verdict"] == "STEP"


def test_analyze_absent_quality_is_legacy_step():
    assert asd.analyze(_ABSENTQ_STEP_JSON, 1)["verdict"] == "STEP"


def test_analyze_nan_mad_is_low_quality():
    assert asd.analyze(_NANQ_JSON, 1)["verdict"] == "LOW_QUALITY"


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


def test_cli_low_quality():
    out = _cli(_1509_JSON, 1)
    assert out["verdict"] == "LOW_QUALITY" and out["pin"] == "939"


def test_cli_good_quality_step():
    assert _cli(_GOODQ_STEP_JSON, 1)["verdict"] == "STEP"


# ------------------------------------------------------------------ orchestrator step-arm LOW_QUALITY
_WD = _SCRIPTS / "av-step-alert-watchdog.sh"


def _run_step_arm(tmp_path, body_json, preseed_confirm=None):
    """Source the watchdog, OVERRIDE fetch_bundle_json to serve `body_json` (the existing sourced-
    function seam — the same one test_reference_resolution_rejects_non_finite uses), then call the
    STEP arm `handle_box`. A fake notify records any invocation. Returns (stderr, state_text,
    notified_bool)."""
    body_file = tmp_path / "body.json"
    body_file.write_text(body_json)
    state = tmp_path / "st.state"
    if preseed_confirm is not None:
        state.write_text("confirm_stream=%s\n" % preseed_confirm)
    marker = tmp_path / "notified"
    fake_notify = tmp_path / "fake_notify.py"
    fake_notify.write_text(
        "import pathlib\npathlib.Path(r'%s').write_text('called')\n" % marker)
    script = f'''
      export AV_STEP_ALERT_STATE_FILE="{state}"
      export AV_STEP_DECIDE="{_SCRIPTS / 'av_step_decision.py'}"
      export AIRULESET_NOTIFY="{fake_notify}"
      source "{_WD}"
      fetch_bundle_json() {{ cat "{body_file}"; return 0; }}
      handle_box stream 10.0.0.9
    '''
    p = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    st = state.read_text() if state.exists() else ""
    return p.stderr, st, marker.exists()


def test_orchestrator_step_arm_low_quality_log_only_no_page_no_confirm(tmp_path):
    # A LOW_QUALITY step-arm pass: log-only (BAND-arm wording), NO notify, confirm RESET (never
    # incremented toward a page). Pre-seed confirm=1 to prove the pass resets it to 0 rather than
    # advancing it toward the 2-pass threshold.
    stderr, state, notified = _run_step_arm(tmp_path, _1509_JSON, preseed_confirm=1)
    assert "step not judged, no page" in stderr, stderr
    assert "dock reading not trustworthy" in stderr, stderr
    assert not notified, "LOW_QUALITY must never fire a notification"
    # confirm was reset to 0, not incremented.
    assert "confirm_stream=0" in state, state
    assert "confirm_stream=2" not in state


def test_orchestrator_good_quality_step_still_confirms(tmp_path):
    # control: a trustworthy real step DOES advance the confirm counter (proves the gate only
    # suppresses UNTRUSTWORTHY readings, never a genuine step).
    stderr, state, _notified = _run_step_arm(tmp_path, _GOODQ_STEP_JSON, preseed_confirm=0)
    assert "confirm_stream=1" in state, (stderr, state)


def _run_step_arm_preseed_state(tmp_path, body_json, preseed_lines):
    """Like _run_step_arm but seeds an ARBITRARY set of state lines (e.g. an ALERTED latch)."""
    body_file = tmp_path / "body.json"
    body_file.write_text(body_json)
    state = tmp_path / "st.state"
    state.write_text("\n".join(preseed_lines) + "\n")
    marker = tmp_path / "notified"
    fake_notify = tmp_path / "fake_notify.py"
    fake_notify.write_text(
        "import pathlib\npathlib.Path(r'%s').write_text('called')\n" % marker)
    script = f'''
      export AV_STEP_ALERT_STATE_FILE="{state}"
      export AV_STEP_DECIDE="{_SCRIPTS / 'av_step_decision.py'}"
      export AIRULESET_NOTIFY="{fake_notify}"
      source "{_WD}"
      fetch_bundle_json() {{ cat "{body_file}"; return 0; }}
      handle_box stream 10.0.0.9
    '''
    p = subprocess.run(["bash", "-c", script], capture_output=True, text=True)
    st = state.read_text() if state.exists() else ""
    return p.stderr, st, marker.exists()


def test_orchestrator_low_quality_while_alerted_holds_the_latch_no_false_recovery(tmp_path):
    # The load-bearing incident invariant: a LOW_QUALITY pass while the box is ALREADY ALERTED must
    # NOT clear the recovery latch (alerted_/alert_base_) and must NOT log a RECOVERY — otherwise a
    # noisy pass would falsely claim the upstream step healed. Pre-seed the alerted latch, feed a
    # LOW_QUALITY reading, and assert the latch survives, no recovery, no notify.
    stderr, state, notified = _run_step_arm_preseed_state(
        tmp_path, _1509_JSON,
        ["alerted_stream=1", "alert_base_stream=68.0", "confirm_stream=1",
         "alert_sig_stream=avstep:stream", "alert_passes_stream=3"])
    assert "step not judged, no page" in stderr, stderr
    assert "RECOVERY" not in stderr, stderr           # a noisy pass is never a recovery
    assert not notified
    # the alert latch is untouched (a genuine later recovery can still fire); only confirm reset.
    assert "alerted_stream=1" in state, state
    assert "alert_base_stream=68.0" in state, state
    assert "confirm_stream=0" in state, state
