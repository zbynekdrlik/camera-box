"""issue 905 item 3 — the optical undecodable floor flips from REPORT-ONLY to BLOCKING
(`all_cambox_continuity.undecodable_floor_gates_overall_pass=true`, per-window AND run-wide folds
LIVE). Mirrors the frozen_leg / self_heal / dup_cadence precedent
(test_e2e_discord_report_frozen_selfheal_905.py etc.): a run whose only continuity failure is an
over-floor undecodable count is NAMED in `_blocking_failures` (block 4's floor branch — run-wide
OR per-window), and `_report_only_tripped`'s existing floor branch is guarded
`undecodable_floor_gates_overall_pass is not True` so the classifier auto-follows the flip WITHOUT
double-counting (both blocking AND report-only). Blocking-label assertions anchor on the
floor-specific `nad floor` substring (not the bare `optická čitateľnosť`, which also matches the
imag PRESENCE label)."""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402


def _verdict(*, cont_overall_pass, within_floor, floor_gates, seg_undec=7, total=7):
    # A run whose only continuity signal is the optical undecodable floor: the window is clean on
    # copies/gaps (so block 4 attributes the failure to the floor, not to tolerance).
    return {
        "overall_pass": cont_overall_pass,
        "full_chain": {"zero_loss": True},
        "all_cambox_continuity": {
            "overall_pass": cont_overall_pass,
            "copies_gaps_tolerance": 5,
            "segments": [
                {"cambox": "CAM2", "copies": 0, "gaps": 0, "undecodable": seg_undec,
                 "frames": 850, "pass": False}
            ],
            "total_undecodable": total,
            "run_wide_undecodable_within_floor": within_floor,
            "undecodable_floor_gates_overall_pass": floor_gates,
            "per_window_undecodable_floor": 4,
            "run_undecodable_floor": 6,
        },
    }


def _has(labels, sub):
    return any(sub in label.lower() for label, _ in labels)


# ---- LIVE (post-flip, undecodable_floor_gates_overall_pass=True) -> BLOCKING -------------------

def test_optical_floor_live_run_wide_over_floor_is_blocking_and_not_report_only():
    # Re-gated: the run-wide floor failed -> all_cambox_continuity.overall_pass is False. block 4
    # must NAME it as an optical-readability failure (not the generic "chyba kontinuity"), and it
    # must NOT ALSO appear in _report_only_tripped.
    v = _verdict(cont_overall_pass=False, within_floor=False, floor_gates=True, seg_undec=7, total=7)
    assert _has(edr._blocking_failures(v), "nad floor"), edr._blocking_failures(v)
    assert not any("optická" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    assert "❌" in edr.compose_summary(v, {"run_id": "905"})


def test_optical_floor_live_per_window_over_floor_is_named():
    # A single window over the PER-WINDOW floor (5 > 4) while the run total stays within the run-wide
    # floor (5 <= 6): overall_pass fails via overall_pass_term, and block 4 must still NAME it as a
    # floor red rather than the generic fallback (review 🟡2).
    v = _verdict(cont_overall_pass=False, within_floor=True, floor_gates=True, seg_undec=5, total=5)
    assert _has(edr._blocking_failures(v), "nad floor"), edr._blocking_failures(v)
    assert not any("optická" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)


# ---- pre-flip (undecodable_floor_gates_overall_pass=False) -> stays REPORT-ONLY ----------------

def test_optical_floor_pre_flip_over_floor_stays_report_only():
    # A verdict predating the flip (floor still report-only): the over-floor count is reported as a
    # report-only cross, never as a blocking floor failure, and never double-counted.
    v = _verdict(cont_overall_pass=True, within_floor=False, floor_gates=False, seg_undec=7, total=7)
    assert any("optická" in n.lower() for n in edr._report_only_tripped(v)), \
        edr._report_only_tripped(v)
    assert not _has(edr._blocking_failures(v), "nad floor"), edr._blocking_failures(v)


def test_optical_floor_within_floor_never_flagged_either_way():
    # total within both floors: neither blocking nor report-only, regardless of the gate flag.
    for gates in (True, False):
        v = _verdict(cont_overall_pass=True, within_floor=True, floor_gates=gates,
                     seg_undec=3, total=3)
        v["all_cambox_continuity"]["segments"][0]["pass"] = True
        assert not any("optická" in n.lower() for n in edr._report_only_tripped(v)), gates
        assert not _has(edr._blocking_failures(v), "nad floor"), gates
