"""#722 -- unit tests for scripts/event_assert.py, the PURE decision + aggregation layer for
the EVENT-mode CONTRACT (rig-mode.sh event's 8-item machine-checkable assert phase).

Trigger: the 2026-07-12 live incident (#721) -- rig-mode event + a manual supervisor checklist
BOTH said "clean" while a QR was live on air; the user caught it by eye minutes before
broadcast. "EVENT mode" must never again depend on someone remembering what to check, or on any
single step's own exit code being trusted blindly -- every item gets its OWN pure decision
function here, fed by facts gathered elsewhere (ssh fleet checks, OBS-WS reads), and the
aggregation is itself unit-tested: any ONE item failing must fail the whole contract, and the
Slovak summary must NAME every failing item.

These tests exercise event_assert.py directly (no subprocess, no network) -- pure functions
operating on already-gathered fact dictionaries, exactly as they will be called by the CLI after
rig-mode.sh / a small python gather step assembles the facts JSON.
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import event_assert as ea  # noqa: E402

ALL_SIX_BOXES = ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"]


# ---------------------------------------------------------------------------
# Item 1 — no paint processes fleet-wide.
# ---------------------------------------------------------------------------


def test_paint_processes_ok_when_every_box_has_zero():
    facts = {b: 0 for b in ALL_SIX_BOXES}
    assert ea.paint_processes_ok(facts) is True


def test_paint_processes_fails_when_any_box_has_a_painter():
    facts = {b: 0 for b in ALL_SIX_BOXES}
    facts["cam2"] = 1
    assert ea.paint_processes_ok(facts) is False


# ---------------------------------------------------------------------------
# Item 2 — pixel proof (QR decode must find nothing on any camera scene).
# ---------------------------------------------------------------------------


def test_pixel_proof_ok_when_no_qr_found_anywhere():
    facts = {"Cam 1": [], "Cam 2": [], "Cam 3": [], "Cam 4": []}
    assert ea.pixel_proof_ok(facts) is True


def test_pixel_proof_fails_when_any_scene_decodes_a_qr():
    # The EXACT 2026-07-12 incident: a QR was live on air.
    facts = {"Cam 1": [], "Cam 2": ["RUNID=911002;FRAME=88213"], "Cam 3": [], "Cam 4": []}
    assert ea.pixel_proof_ok(facts) is False


def test_pixel_proof_fails_closed_on_no_scenes_gathered():
    # An empty facts dict must NEVER be treated as "nothing found -> pass" -- that would silently
    # pass when the gather step itself failed to screenshot anything.
    assert ea.pixel_proof_ok({}) is False


# ---------------------------------------------------------------------------
# Item 3 — burns off.
# ---------------------------------------------------------------------------


def test_burns_off_ok_when_every_target_is_false():
    facts = {"strih:NDI cam5": False, "stream:NDI 2ME PGM": False, "imag:NDI CAM1": False}
    assert ea.burns_off_ok(facts) is True


def test_burns_off_fails_when_any_target_is_still_on():
    facts = {"strih:NDI cam5": False, "stream:NDI 2ME PGM": True, "imag:NDI CAM1": False}
    assert ea.burns_off_ok(facts) is False


def test_burns_off_fails_closed_on_empty_facts():
    assert ea.burns_off_ok({}) is False


# ---------------------------------------------------------------------------
# Item 4 — no active recordings/streams.
# ---------------------------------------------------------------------------


def test_no_recordings_ok_when_nothing_active():
    facts = {
        "strih:record": False, "strih:stream": False,
        "stream:record": False, "stream:stream": False,
    }
    assert ea.no_recordings_ok(facts) is True


def test_no_recordings_fails_on_a_stray_recording():
    facts = {
        "strih:record": True, "strih:stream": False,
        "stream:record": False, "stream:stream": False,
    }
    assert ea.no_recordings_ok(facts) is False


# ---------------------------------------------------------------------------
# Item 5 — services healthy 6/6, no stray test units.
# ---------------------------------------------------------------------------


def test_services_healthy_ok_when_all_active_and_no_stray_units():
    active = {b: True for b in ALL_SIX_BOXES}
    stray = {b: [] for b in ALL_SIX_BOXES}
    assert ea.services_healthy_ok(active, stray) is True


def test_services_healthy_fails_when_a_box_is_inactive():
    active = {b: True for b in ALL_SIX_BOXES}
    active["cam4"] = False
    stray = {b: [] for b in ALL_SIX_BOXES}
    assert ea.services_healthy_ok(active, stray) is False


def test_services_healthy_fails_on_a_stray_test_unit():
    active = {b: True for b in ALL_SIX_BOXES}
    stray = {b: [] for b in ALL_SIX_BOXES}
    stray["cam2"] = ["camera-box-burn-911002.service"]
    assert ea.services_healthy_ok(active, stray) is False


# ---------------------------------------------------------------------------
# Item 6 — latency == calibrated.
# ---------------------------------------------------------------------------


def test_latency_calibrated_ok_when_equal():
    assert ea.latency_calibrated_ok(925, 925) is True


def test_latency_calibrated_fails_when_different():
    assert ea.latency_calibrated_ok(1000, 925) is False


def test_latency_calibrated_fails_closed_when_no_calibrated_value_known():
    # Never PASS on "we don't know the calibrated value" -- that's exactly the #691 stomp risk.
    assert ea.latency_calibrated_ok(925, None) is False


# ---------------------------------------------------------------------------
# Item 7 — NDI mapping correct.
# ---------------------------------------------------------------------------


def test_ndi_mapping_ok_when_no_mismatches():
    assert ea.ndi_mapping_ok([]) is True


def test_ndi_mapping_fails_on_any_mismatch():
    assert ea.ndi_mapping_ok([("NDI cam5", "wrong-sender", "CAM1 (usb)")]) is False


# ---------------------------------------------------------------------------
# Item 8 — test artifacts cleared.
# ---------------------------------------------------------------------------


def test_artifacts_cleared_ok_when_nothing_exists():
    assert ea.artifacts_cleared_ok([]) is True


def test_artifacts_cleared_fails_when_something_remains():
    assert ea.artifacts_cleared_ok(["/run/rig-painter.pid"]) is False


# ---------------------------------------------------------------------------
# Aggregation — ANY single item failing fails the WHOLE contract, and the failing item is named.
# ---------------------------------------------------------------------------


def _all_pass_item_results():
    return {name: True for name in ea.ITEM_ORDER}


def test_aggregate_passes_when_every_item_passes():
    overall, failed = ea.aggregate(_all_pass_item_results())
    assert overall is True
    assert failed == []


def test_aggregate_fails_when_one_item_fails_and_names_it():
    results = _all_pass_item_results()
    results["pixel_proof"] = False
    overall, failed = ea.aggregate(results)
    assert overall is False
    assert failed == ["pixel_proof"]


def test_aggregate_names_every_failing_item_in_stable_order():
    results = _all_pass_item_results()
    results["artifacts_cleared"] = False
    results["paint_processes"] = False
    overall, failed = ea.aggregate(results)
    assert overall is False
    # Stable ITEM_ORDER, not insertion/dict order -- paint_processes is item 1, artifacts_cleared
    # is item 8.
    assert failed == ["paint_processes", "artifacts_cleared"]


def test_aggregate_treats_a_missing_item_as_failed_never_as_passed():
    results = _all_pass_item_results()
    del results["ndi_mapping"]
    overall, failed = ea.aggregate(results)
    assert overall is False
    assert "ndi_mapping" in failed


# ---------------------------------------------------------------------------
# Slovak summary — every item named, PASS/FAIL clearly marked, human-readable.
# ---------------------------------------------------------------------------


def test_summary_all_pass_mentions_every_item_as_ok():
    results = _all_pass_item_results()
    summary = ea.format_summary_sk(True, results)
    for name in ea.ITEM_ORDER:
        assert ea.ITEM_LABELS_SK[name] in summary
    assert "OK" in summary


def test_summary_on_failure_names_the_failing_item():
    results = _all_pass_item_results()
    results["burns_off"] = False
    summary = ea.format_summary_sk(False, results)
    assert ea.ITEM_LABELS_SK["burns_off"] in summary
    assert "CHYBA" in summary
