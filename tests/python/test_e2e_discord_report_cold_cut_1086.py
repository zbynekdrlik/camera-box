"""issue 1086 — the cold-cut onset seam (`all_cambox_continuity.cold_cut_onset`) is now LIVE. A
GENUINE cold-cut miss on a post-flip verdict (its node ships `gates_overall_pass=true`) must render
as a `❌` blocking failure in the summary and appear in `_blocking_failures`, NEVER as a report-only
`ℹ️` line — the delivery-spread / own-burn-absent flip pattern (`e2e-discord-report.md`). A PRE-flip
verdict (`gates_overall_pass=false`, e.g. the historical `verdict_real_fail_cam1_77008829.json`
fixture) must stay report-only so the two classifiers never double-count across the flip."""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"


def _cold_cut_verdict(gates_overall_pass, any_genuine_cold_cut_miss):
    """A minimal verdict whose cold-cut node carries the two fields the classifier keys on. The
    rest is a clean PASSING run so cold-cut is the only thing that can trip either classifier."""
    return {
        "overall_pass": not any_genuine_cold_cut_miss,
        "full_chain": {"zero_loss": True},
        "all_cambox_continuity": {
            "overall_pass": not any_genuine_cold_cut_miss,
            "cold_cut_onset": {
                "cold_transitions_found": 3,
                "any_wakeup_over_max": False,
                "any_wakeup_missing": any_genuine_cold_cut_miss,
                "any_onset_undecodable": any_genuine_cold_cut_miss,
                "any_receive_degraded": False,
                "any_miss_possibly_segfault": False,
                "any_genuine_cold_cut_miss": any_genuine_cold_cut_miss,
                "pass": not any_genuine_cold_cut_miss,
                "gates_overall_pass": gates_overall_pass,
            },
        },
    }


# --- LIVE (post-flip) verdict: a genuine miss is a BLOCKING failure --------------------------

def test_live_genuine_miss_is_a_blocking_failure():
    v = _cold_cut_verdict(gates_overall_pass=True, any_genuine_cold_cut_miss=True)
    failures = edr._blocking_failures(v)
    assert any("cold-cut" in label.lower() for label, _ in failures), (
        f"a LIVE genuine cold-cut miss must be a blocking failure, got {failures!r}"
    )


def test_live_genuine_miss_is_not_double_counted_report_only():
    v = _cold_cut_verdict(gates_overall_pass=True, any_genuine_cold_cut_miss=True)
    names = edr._report_only_tripped(v)
    assert "cold-cut" not in names, (
        f"a LIVE genuine miss must NOT ALSO appear in report-only (double-count), got {names!r}"
    )


def test_live_genuine_miss_renders_a_fail_summary():
    v = _cold_cut_verdict(gates_overall_pass=True, any_genuine_cold_cut_miss=True)
    summary = edr.compose_summary(v, {"run_id": "1086live"})
    assert "❌" in summary, f"a LIVE genuine cold-cut miss must render a ❌ FAIL, got:\n{summary}"


# --- PRE-flip verdict: a genuine miss stays report-only (no double count) --------------------

def test_preflip_genuine_miss_stays_report_only():
    v = _cold_cut_verdict(gates_overall_pass=False, any_genuine_cold_cut_miss=True)
    names = edr._report_only_tripped(v)
    assert "cold-cut" in names, (
        f"a PRE-flip genuine miss must stay report-only, got {names!r}"
    )
    failures = edr._blocking_failures(v)
    assert all("cold-cut" not in label.lower() for label, _ in failures), (
        f"a PRE-flip miss must NOT be a blocking failure, got {failures!r}"
    )


def test_historical_fixture_preflip_stays_report_only():
    """The retained `verdict_real_fail_cam1_77008829.json` carries
    cold_cut_onset.gates_overall_pass=false + any_genuine_cold_cut_miss=true — it must keep meaning
    report-only for cold-cut (its overall FAIL is the CAM1 continuity gate, not cold-cut)."""
    v = json.loads((_FIXTURES / "verdict_real_fail_cam1_77008829.json").read_text())
    cc = v["all_cambox_continuity"]["cold_cut_onset"]
    assert cc["gates_overall_pass"] is False and cc["any_genuine_cold_cut_miss"] is True
    assert "cold-cut" in edr._report_only_tripped(v)
    assert all("cold-cut" not in label.lower() for label, _ in edr._blocking_failures(v))


# --- Clean cold cut: neither classifier trips ------------------------------------------------

def test_clean_cold_cut_trips_neither_classifier():
    v = _cold_cut_verdict(gates_overall_pass=True, any_genuine_cold_cut_miss=False)
    assert all("cold-cut" not in label.lower() for label, _ in edr._blocking_failures(v))
    assert "cold-cut" not in edr._report_only_tripped(v)
    assert "❌" not in edr.compose_summary(v, {"run_id": "1086clean"})
