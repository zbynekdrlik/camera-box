"""issue 1439 (ROZHODNUTE 6359382787) -- the E2E Discord report names the BLOCKING program-audio
continuity gate (`program_audio_continuity` in the verdict JSON, merged by
scripts/lib/program-audio-continuity.sh):

  * a FAIL or UNKNOWN block makes the run RED (`❌ E2E TEST ZLYHAL`) even when the merge
    recording-verdict passed: the E2E's exit is RED, the report must not say PASS;
  * the summary line names every counter with its delta and calls concealed slots LOST;
  * UNKNOWN reads "neoverená" and FAILS -- never a pass;
  * a PASS block keeps the 3-line PASS summary byte-identical; no block = byte-identical to before.

The verdict is the REAL pass fixture (run 1104689227), the block is the real decision over the
REAL stream-snv log excerpt (tests/fixtures/program_audio_continuity_1439/).
"""
import copy
import json
import pathlib
import sys

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402
import program_audio_continuity as pac  # noqa: E402

_FIX = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"
_LOG = _ROOT / "tests/fixtures/program_audio_continuity_1439/stream-snv-2026-10-23-from-41400.txt"
FIRST = 41400


def _pass_verdict():
    return json.loads((_FIX / "verdict_real_pass_reportonly_1104689227.json").read_text())


def _block(start, end):
    lines = _LOG.read_text().splitlines()
    return pac.decide(lines, "mbc", start - FIRST + 1, end - FIRST + 1, True)


def _with(block):
    v = _pass_verdict()
    v["program_audio_continuity"] = block
    return v


META = {"run_id": "42", "run_url": "https://example.invalid/run/42"}


def test_a_lost_program_audio_window_turns_a_passing_merge_red_and_names_the_lost_slots():
    v = _with(_block(41620, 41710))
    assert v["overall_pass"] is True  # the merge passed: only the audio gate is red
    text = edr.compose_summary(v, META)
    assert text.splitlines()[0].startswith("❌ E2E TEST ZLYHAL"), text
    line = next(l for l in text.splitlines() if "zvuk" in l.lower())
    assert "7 stratených" in line and "3 zamaskované" in line and "4 ticho" in line, line
    assert "zamaskované = STRATENÉ" in line, line
    assert "✅" not in text


def test_a_concealed_only_loss_is_red_too():
    text = edr.compose_summary(_with(_block(41900, 41930)), META)
    assert text.startswith("❌ E2E TEST ZLYHAL")
    assert "2 stratených" in text and "2 zamaskované" in text


def test_an_unknown_window_is_red_and_says_so():
    text = edr.compose_summary(_with(pac.unknown_block("no window START mark", "mbc")), META)
    assert text.startswith("❌ E2E TEST ZLYHAL"), text
    assert "NEOVERENÁ" in text and "no window START mark" in text, text


def test_a_pass_block_keeps_the_three_line_pass():
    base = edr.compose_summary(_pass_verdict(), META)
    text = edr.compose_summary(_with(_block(42000, 42100)), META)
    assert text == base and len(text.splitlines()) == 3, text


def test_no_block_is_byte_identical_and_report_only_never_gates():
    v = _pass_verdict()
    assert edr.compose_summary(copy.deepcopy(v), META) == edr.compose_summary(v, META)
    blk = _block(41620, 41710)
    blk["gates_overall_pass"] = False  # a report-only block (never shipped) would not gate
    assert edr.compose_summary(_with(blk), META).startswith("✅")


def test_the_full_report_has_the_program_audio_section():
    text = edr.compose_report(_with(_block(41620, 41710)), {"run_id": "42"})
    assert "Kontinuita zvuku programu" in text
    assert "❌ RED" in text and "missing 4 packet(s)" in text, text
    assert "lost +7" in text
    text = edr.compose_report(_with(_block(42000, 42100)), {"run_id": "42"})
    assert "Kontinuita zvuku programu" in text and "✅" in text
    assert "Kontinuita zvuku programu" not in edr.compose_report(_pass_verdict(), {"run_id": "42"})


def test_the_full_report_names_what_an_older_build_did_not_grade():
    blk = _block(42000, 42100)
    blk["not_graded"] = ["stale", "emit_missed"]
    text = edr.compose_report(_with(blk), {"run_id": "42"})
    assert "neoverené (starší build): stale, emit_missed" in text, text
