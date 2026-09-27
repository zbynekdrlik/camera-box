"""issue 1380 -- the EVENT-mode CONTRACT checks the stream program is back on the production scene.

Development programs the stream OBS's own `Development` scene (the production scene `PRO` nested
inside it); `rig-mode.sh event` switches the stream program back to `PRO`, and the #722 contract
must prove it independently: a stream left on `Development` (or unreadable) is NOT broadcast-clean.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import event_assert as ea  # noqa: E402


def test_the_stream_program_item_is_part_of_the_contract_and_last():
    assert ea.ITEM_ORDER[-1] == "stream_program_production"
    assert "stream_program_production" in ea.ITEM_LABELS_SK


def test_pass_when_the_stream_program_is_the_production_scene():
    assert ea.stream_program_production_ok("PRO", "PRO") is True


def test_fail_when_the_stream_is_left_on_the_development_scene():
    assert ea.stream_program_production_ok("Development", "PRO") is False


def test_fail_closed_when_the_program_scene_is_unreadable():
    assert ea.stream_program_production_ok(None, "PRO") is False
    assert ea.stream_program_production_ok("", "PRO") is False


def test_fail_closed_when_the_expected_scene_is_unknown():
    assert ea.stream_program_production_ok("PRO", None) is False
    assert ea.stream_program_production_ok("PRO", "") is False


def test_compute_item_results_reads_the_two_stream_facts():
    facts = {"stream_program_scene": "PRO", "stream_production_scene": "PRO"}
    assert ea.compute_item_results(facts)["stream_program_production"] is True
    facts["stream_program_scene"] = "Development"
    assert ea.compute_item_results(facts)["stream_program_production"] is False


def test_a_missing_stream_fact_fails_the_item():
    assert ea.compute_item_results({})["stream_program_production"] is False
