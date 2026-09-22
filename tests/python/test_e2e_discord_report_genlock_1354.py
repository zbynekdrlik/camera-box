"""#1354 scope 3 -- the per-input genlock-conveyor DELTA section of the E2E report composer.

REPORT-ONLY: `_section_genlock_conveyor` appears in the FULL report (`compose_report`) only, NEVER
in the Discord summary (`compose_summary` / `--json-chunks`), which stays byte-identical whether or
not the genlock-audit snapshot is supplied. The section NAMES the ladder victim input so a
delivery-spread failure is diagnosable from the report instead of a bare spread number.
"""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"


def _clean_verdict():
    with open(_FIXTURES / "verdict_clean_pass.json", encoding="utf-8") as f:
        return json.load(f)


_AUDIT = {
    "inputs": {
        "NDI cam1": {"holds": 6, "relocks": 6, "converge_sheds": 0, "dropped_due": 1812},
        "NDI cam4": {"holds": 21, "relocks": 88, "converge_sheds": 275, "dropped_due": 1808},
    },
    "victim": "NDI cam4",
}


def test_section_absent_when_no_snapshot():
    assert edr._section_genlock_conveyor(_clean_verdict(), {"run_id": "1"}) is None
    assert edr._section_genlock_conveyor(_clean_verdict(), {"run_id": "1", "genlock_audit": None}) is None


def test_section_renders_per_input_and_names_the_victim():
    s = edr._section_genlock_conveyor(_clean_verdict(), {"genlock_audit": _AUDIT})
    assert s is not None
    assert "NDI cam1: holds +6, relocks +6, converge_sheds +0" in s
    assert "NDI cam4: holds +21, relocks +88, converge_sheds +275" in s
    # the victim is named AND flagged (⚠️ glyph on its row).
    assert "Najviac postihnutý vstup: NDI cam4" in s
    assert "⚠️ NDI cam4:" in s
    assert "• NDI cam1:" in s  # non-victim gets the plain bullet


def test_no_victim_line_when_no_holds():
    audit = {"inputs": {"NDI cam1": {"holds": 0, "relocks": 0, "converge_sheds": 0, "dropped_due": 9}}, "victim": None}
    s = edr._section_genlock_conveyor(_clean_verdict(), {"genlock_audit": audit})
    assert "bez rebríka" in s
    assert "Najviac postihnutý" not in s


def test_error_field_renders_a_warning():
    s = edr._section_genlock_conveyor(_clean_verdict(), {"genlock_audit": {"error": "no audit tail"}})
    assert "nemeralo sa" in s


def test_full_report_includes_the_section_summary_stays_byte_identical():
    verdict = _clean_verdict()
    meta_with = {"run_id": "1", "event": "CI PR gate", "genlock_audit": _AUDIT}
    meta_without = {"run_id": "1", "event": "CI PR gate"}

    full_with = edr.compose_report(verdict, meta_with)
    full_without = edr.compose_report(verdict, meta_without)
    assert "Genlock dopravník" in full_with
    assert "Genlock dopravník" not in full_without

    # The Discord summary NEVER carries the section, and is byte-identical with/without the snapshot.
    summary_with = edr.compose_summary(verdict, meta_with)
    summary_without = edr.compose_summary(verdict, meta_without)
    assert "Genlock dopravník" not in summary_with
    assert summary_with == summary_without
