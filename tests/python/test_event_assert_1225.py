"""#1225 -- regression test: scripts/event_assert.py's decision functions (and
compute_item_results, the entry point event_assert.py's own CLI ("decide") calls) must NEVER
crash with a TypeError when a facet's value is None -- a per-scene screenshot/decode failure
(scripts/qr_screenshot_check.py's own "could not check this scene" signal) is a LEGITIMATE
outcome, not a programming error.

Live incident (2026-08-30, ~09:55): `rig-mode.sh event`'s final aggregate assert crashed with

    File "scripts/event_assert.py", line 79, in pixel_proof_ok
        return all(len(v) == 0 for v in qr_findings.values())
    TypeError: object of type 'NoneType' has no len()

printing a FALSE "RESULT: EVENT mode -- #722 CONTRACT FAILED" on a rig the supervisor had
manually verified clean (sweep-check burns OFF everywhere, cam2-painter inactive+disabled, no
frame-probe, program screenshots without a QR, production camera-box active on all 5 boxes). The
CRASH, not a dirty rig, produced the false FAIL -- exactly the kind of failure #722's own design
doc says must never happen ("EVENT mode must never again depend on someone remembering what to
check").

This file reproduces the EXACT crash shape from the traceback (one qr_findings scene's own value
is None) and proves it now fails CLOSED (a named, honest FAIL) instead of raising -- both via the
narrow function directly and via compute_item_results, the real call path event_assert.py's CLI
uses. It also covers the "neighbouring consumers of facts.get" the ticket calls out (#1225).
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import event_assert as ea  # noqa: E402


def _clean_facts():
    """A facts dict that would otherwise PASS every item -- the shared base for each test below,
    so injecting a single None only ever exercises exactly ONE facet's unreadability."""
    return {
        "fleet_paint_process_counts": {"cam1": 0, "cam2": 0},
        "qr_findings": {"Cam 1": [], "Cam 2": [], "Cam 3": [], "Cam 4": []},
        "burn_states": {"strih:NDI cam5": False},
        "recording_states": {"strih:record": False, "strih:stream": False},
        "fleet_service_active": {"cam1": True, "cam2": True},
        "fleet_stray_units": {"cam1": [], "cam2": []},
        "latency_current_ms": 925,
        "latency_calibrated_ms": 925,
        "ndi_mismatches": [],
        "artifacts_existing": [],
    }


def test_baseline_facts_pass_every_item():
    # Sanity check: the shared clean fixture actually passes on its own, so every None-injection
    # test below is exercising exactly the ONE facet under test, not some unrelated bug.
    results = ea.compute_item_results(_clean_facts())
    overall, failed = ea.aggregate(results)
    assert overall is True, failed


# ---------------------------------------------------------------------------
# The exact live-incident shape: one qr_findings SCENE's own value is None.
# ---------------------------------------------------------------------------


def test_pixel_proof_ok_never_raises_when_one_scene_is_none():
    qr_findings = {"Cam 1": [], "Cam 2": None, "Cam 3": [], "Cam 4": []}
    assert ea.pixel_proof_ok(qr_findings) is False  # fail CLOSED, never raise


def test_compute_item_results_never_raises_when_a_qr_scene_is_none():
    facts = _clean_facts()
    facts["qr_findings"] = {"Cam 1": [], "Cam 2": None, "Cam 3": [], "Cam 4": []}
    results = ea.compute_item_results(facts)  # must NOT raise TypeError
    assert results["pixel_proof"] is False
    overall, failed = ea.aggregate(results)
    assert overall is False
    assert "pixel_proof" in failed


def test_compute_item_results_never_raises_when_the_whole_qr_facet_is_none():
    # A facts JSON can also carry an explicit `"qr_findings": null` (the WHOLE facet unreadable,
    # not just one scene) -- json.load() turns that into a bare None just like the per-scene
    # case, and it must fail closed too, never raise.
    facts = _clean_facts()
    facts["qr_findings"] = None
    results = ea.compute_item_results(facts)
    assert results["pixel_proof"] is False


def test_summary_names_pixel_proof_as_failing_with_a_reason_when_a_scene_is_unreadable():
    facts = _clean_facts()
    facts["qr_findings"] = {"Cam 1": [], "Cam 2": None, "Cam 3": [], "Cam 4": []}
    results = ea.compute_item_results(facts)
    overall, _ = ea.aggregate(results)
    detail = ea.pixel_proof_detail(facts["qr_findings"])
    summary = ea.format_summary_sk(overall, results, {"pixel_proof": detail} if detail else {})
    assert ea.ITEM_LABELS_SK["pixel_proof"] in summary
    assert "CHYBA" in summary
    assert "facet unreadable" in summary
    assert "Cam 2" in summary


def test_pixel_proof_detail_empty_when_all_scenes_clean():
    assert ea.pixel_proof_detail({"Cam 1": [], "Cam 2": []}) == ""


def test_pixel_proof_detail_names_a_scene_with_a_live_qr_too():
    detail = ea.pixel_proof_detail({"Cam 1": [], "Cam 2": ["RUNID=911002;FRAME=1"]})
    assert "Cam 2" in detail
    assert "najdeny" in detail


# ---------------------------------------------------------------------------
# Neighbouring facts.get consumers -- same crash CLASS, guarded preemptively (#1225).
# ---------------------------------------------------------------------------


def test_paint_processes_ok_never_raises_when_a_box_value_is_none():
    assert ea.paint_processes_ok({"cam1": 0, "cam2": None}) is False


def test_paint_processes_ok_never_raises_when_the_whole_facet_is_none():
    facts = _clean_facts()
    facts["fleet_paint_process_counts"] = None
    results = ea.compute_item_results(facts)
    assert results["paint_processes"] is False


def test_services_healthy_ok_never_raises_when_a_stray_units_value_is_none():
    active = {"cam1": True, "cam2": True}
    stray = {"cam1": [], "cam2": None}
    assert ea.services_healthy_ok(active, stray) is False


def test_services_healthy_ok_never_raises_when_stray_units_facet_is_none():
    active = {"cam1": True, "cam2": True}
    assert ea.services_healthy_ok(active, None) is False


def test_no_recordings_ok_never_raises_when_the_whole_facet_is_none():
    assert ea.no_recordings_ok(None) is False


def test_ndi_mapping_ok_never_raises_on_none():
    assert ea.ndi_mapping_ok(None) is False


def test_artifacts_cleared_ok_never_raises_on_none():
    assert ea.artifacts_cleared_ok(None) is False


def test_compute_item_results_never_raises_when_ndi_or_artifacts_facets_are_none():
    facts = _clean_facts()
    facts["ndi_mismatches"] = None
    facts["artifacts_existing"] = None
    results = ea.compute_item_results(facts)
    assert results["ndi_mapping"] is False
    assert results["artifacts_cleared"] is False
