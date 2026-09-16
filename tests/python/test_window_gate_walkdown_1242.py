"""issue 1242 -- unit tests for scripts/window_gate_walkdown.py, the per-window copies/gaps +
cadence-uniformity walk-down mining tool. Pure fixture-driven (no rig, no cargo) -- the
arrival_floor_decompose / audio_lag_decision python-mirror Tier-0 precedent.

Locks the SEGREGATION contract (POST-fix iff the run's genlock bundle is the issue-1320 cure or a
later listed one) and the per-run/per-camera summary the ticket table is built from, so a hand-edit
that drifts the tool off the mined distribution fails here rather than silently mis-reporting."""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import window_gate_walkdown as w  # noqa: E402


def _verdict(overall, wcg, over, fail, undec, worst_beat, worst_der, segs):
    """A minimal verdict-JSON dict: segs = [(cambox, copies, gaps, beat)]."""
    return {
        "overall_pass": overall,
        "all_cambox_continuity": {
            "windows_with_copies_or_gaps": wcg,
            "windows_over_copies_gaps_tolerance": over,
            "windows_failed_report_only": fail,
            "total_undecodable": undec,
            "cadence_uniformity_gate": {
                "worst_uniform_fraction": worst_beat,
                "worst_derived_uniform_fraction": worst_der,
            },
            "segments": [
                {"cambox": c, "copies": cp, "gaps": gp,
                 "presentation_cadence": {"beat_corrected_uniform_fraction": b}}
                for c, cp, gp, b in segs
            ],
        },
    }


# The two live-mined anchors (must match the ticket table): POST-fix clean vs the render-freeze PRE run.
POST_CLEAN = _verdict(True, 0, 0, 0, 0, 0.9988, 0.9574,
                      [("CAM2", 0, 0, 0.9988), ("CAM6", 0, 0, 0.9990)])
PRE_FREEZE = _verdict(False, 2, 1, 2, 0, 0.9481, 0.6297,
                      [("CAM2", 12, 10, 0.9481), ("CAM6", 26, 27, 0.9657)])


def test_summarize_run_level_counts():
    s = w.summarize_verdict(POST_CLEAN)
    assert (s["wcg"], s["w_over_tol"], s["w_fail_strict"], s["undec"]) == (0, 0, 0, 0)
    assert s["worst_beat_unif"] == 0.9988
    assert s["worst_derived_unif"] == 0.9574  # diagnostic field surfaced, never gated


def test_summarize_per_cam_windows():
    s = w.summarize_verdict(PRE_FREEZE)
    assert s["per_cam"]["CAM6"] == [(26, 27, 0.9657)]
    assert w.nonzero_windows(s) == [("CAM2", 12, 10, 0.9481), ("CAM6", 26, 27, 0.9657)]


def test_clean_run_has_no_nonzero_windows():
    assert w.nonzero_windows(w.summarize_verdict(POST_CLEAN)) == []


def test_segregation_by_genlock_bundle():
    # Default post set = the issue-1320 cure bundle only.
    assert w.classify_era("02b53180b", {w.FIX_BUNDLE}) == "POST"
    assert w.classify_era("c4b16074c", {w.FIX_BUNDLE}) == "PRE"  # the pre-cure clean bundle
    assert w.classify_era("3ffe2fbc5", {w.FIX_BUNDLE}) == "PRE"
    # A caller may widen the post set to include a later bundle.
    assert w.classify_era("fac0bce48", {"02b53180b", "fac0bce48"}) == "POST"


def test_distribution_table_renders_both_eras():
    rows = [
        ("25635487", "3ffe2fbc5", "PRE", w.summarize_verdict(PRE_FREEZE)),
        ("180691712", "02b53180b", "POST", w.summarize_verdict(POST_CLEAN)),
    ]
    t = w.distribution_table(rows)
    assert "CAM6 26/27" in t and "| PRE |" in t
    assert "| POST |" in t and "all 0/0" in t
    assert "0.9988" in t and "0.9481" in t


def test_cam_max_present_absent_and_zero():
    # issue 1242 CAM2-override-removal signal: worst max(copies,gaps) for one box.
    pre = w.summarize_verdict(PRE_FREEZE)
    assert w.cam_max(pre, "CAM2") == 12  # max(12,10) over the one CAM2 window
    assert w.cam_max(pre, "CAM6") == 27  # max(26,27)
    # A strict-clean CAM2 (the post-16.9 splitter-fed runs) reads 0, distinct from ABSENT (None).
    assert w.cam_max(w.summarize_verdict(POST_CLEAN), "CAM2") == 0
    assert w.cam_max(pre, "CAM1") is None  # absent box -> None, never a false 0


def test_cam_max_worst_across_multiple_windows():
    v = _verdict(False, 2, 0, 2, 0, 0.99, 0.98,
                 [("CAM2", 1, 0, 0.99), ("CAM2", 3, 5, 0.98)])
    assert w.cam_max(w.summarize_verdict(v), "CAM2") == 5  # max over both windows


def test_distribution_table_cam2_column():
    rows = [
        ("25635487", "3ffe2fbc5", "PRE", w.summarize_verdict(PRE_FREEZE)),
        ("180691712", "02b53180b", "POST", w.summarize_verdict(POST_CLEAN)),
    ]
    t = w.distribution_table(rows)  # default per-cambox col = CAM2
    assert "CAM2 max" in t
    lines = t.splitlines()
    # POST run's CAM2 column reads 0 (strict-clean, the removal precondition); PRE reads 12.
    post_row = next(ln for ln in lines if "180691712" in ln)
    pre_row = next(ln for ln in lines if "25635487" in ln)
    assert post_row.split("|")[9].strip() == "0"
    assert pre_row.split("|")[9].strip() == "12"
