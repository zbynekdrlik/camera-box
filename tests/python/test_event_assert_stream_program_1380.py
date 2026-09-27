"""issue 1380 -- the EVENT-mode CONTRACT checks the stream program is no longer the development scene.

Development programs the stream OBS's own `Development` scene (the production scene `PRO` nested
inside it). ROZHODNUTIE 27.9.2026 (main): `rig-mode.sh event` switches the stream program to `PRO`
ONLY when the live program is `Development` -- an operator on `PRE`/`POST`/`PRO`/anything else is
left alone -- and the #722 contract item passes when the program scene is READABLE and is NOT the
development scene. Unreadable stays fail-closed.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import event_assert as ea  # noqa: E402

ITEM = "stream_program_not_development"


def test_the_stream_program_item_is_part_of_the_contract_and_last():
    assert ea.ITEM_ORDER[-1] == ITEM
    assert ITEM in ea.ITEM_LABELS_SK
    assert "stream_program_production" not in ea.ITEM_ORDER


def test_pass_on_the_production_scene():
    assert ea.stream_program_not_development_ok("PRO", "Development") is True


def test_pass_on_any_operator_scene():
    for scene in ("PRE", "POST", "zaloha"):
        assert ea.stream_program_not_development_ok(scene, "Development") is True


def test_fail_when_the_stream_is_left_on_the_development_scene():
    assert ea.stream_program_not_development_ok("Development", "Development") is False


def test_fail_closed_when_the_program_scene_is_unreadable():
    assert ea.stream_program_not_development_ok(None, "Development") is False
    assert ea.stream_program_not_development_ok("", "Development") is False


def test_fail_closed_when_the_development_scene_name_is_unknown():
    assert ea.stream_program_not_development_ok("PRO", None) is False
    assert ea.stream_program_not_development_ok("PRO", "") is False


def test_compute_item_results_reads_the_two_stream_facts():
    facts = {"stream_program_scene": "PRE", "stream_dev_scene": "Development"}
    assert ea.compute_item_results(facts)[ITEM] is True
    facts["stream_program_scene"] = "Development"
    assert ea.compute_item_results(facts)[ITEM] is False


def test_a_missing_stream_fact_fails_the_item():
    assert ea.compute_item_results({})[ITEM] is False
