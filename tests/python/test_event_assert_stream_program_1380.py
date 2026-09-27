"""issue 1380 -- the EVENT-mode CONTRACT never grades the stream program scene.

Owner hard rule, 27.9.2026, verbatim: "nemas ti nikdy v stream obs davat do programu scenu PRO!!!!!"
EVENT mode does not switch the stream program at all -- the owner cuts to `PRO` himself -- so the #722
contract has no stream-program item: whatever scene is on program (Development included) is never a
FAIL. rig-mode.sh prints the current program scene as a report-only line.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import event_assert as ea  # noqa: E402


def test_the_contract_has_no_stream_program_item():
    assert not any("stream_program" in name for name in ea.ITEM_ORDER)
    assert len(ea.ITEM_ORDER) == 8


def test_the_stream_program_scene_never_fails_the_contract():
    clean = {
        "fleet_paint_process_counts": {"cam1": 0},
        "qr_findings": {"Cam 1": []},
        "burn_states": {"strih:NDI cam1": False},
        "recording_states": {},
        "fleet_service_active": {"cam1": True},
        "fleet_stray_units": {"cam1": []},
        "latency_current_ms": 925,
        "latency_calibrated_ms": 925,
        "ndi_mismatches": [],
        "artifacts_existing": [],
    }
    for scene in ("Development", "PRO", "PRE", None):
        facts = dict(clean, stream_program_scene=scene)
        overall, failed = ea.aggregate(ea.compute_item_results(facts))
        assert overall is True, (scene, failed)
