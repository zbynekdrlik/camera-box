"""issue 1387 slice 2 (design 6558387590) -- the E2E Discord report names the BLOCKING JPEG XS link
continuity gate (`jxs_link_continuity` in the verdict JSON, merged by
scripts/lib/jxs-link-continuity.sh, rendered by scripts/e2e_report_jxs_link.py):

  * a FAIL or UNKNOWN block makes the run RED (`❌ E2E TEST ZLYHAL`) even when the merge
    recording-verdict passed: the E2E's exit is RED, the report must not say PASS;
  * the summary line names the failing link and calls a concealed slice LOST;
  * UNKNOWN reads "NEOVERENÉ" and FAILS -- never a pass;
  * a PASS block keeps the 3-line PASS summary byte-identical; no block = byte-identical to before;
  * the full report has one line per link.

The verdict is the REAL pass fixture (run 1104689227); the block is the real decision over lines
rendered from the shipped C format strings (jxs_link_fakes_1387).
"""
import copy
import json
import pathlib
import sys

_HERE = pathlib.Path(__file__).resolve().parent
_ROOT = _HERE.parents[1]
for p in (_ROOT / "scripts", _HERE):
    if str(p) not in sys.path:
        sys.path.insert(0, str(p))

import e2e_discord_report as edr  # noqa: E402
import jxs_link_continuity as jlc  # noqa: E402
from jxs_link_fakes_1387 import CAMS, PGM, recording, run_box  # noqa: E402

_FIX = _HERE / "fixtures" / "e2e_discord_report"
META = {"run_id": "42", "run_url": "https://example.invalid/run/42"}


def _pass_verdict():
    return json.loads((_FIX / "verdict_real_pass_reportonly_1104689227.json").read_text())


def _block(src=None, out=None, pgm=None, stream_unknown=None):
    log, start, stop, st, et = recording(src=src, out=out)
    strih = run_box(log, start, stop, start_time=st, stop_time=et)
    if stream_unknown:
        stream = jlc.unknown_box("stream", stream_unknown, [PGM], False)
    else:
        log, start, stop, st, et = recording(names=[PGM], output=False, src=pgm)
        stream = run_box(log, start, stop, names=[PGM], program_output=False, start_time=st, stop_time=et,
                         role="stream")
    return jlc.combine([strih, stream])


def _with(block):
    v = _pass_verdict()
    v["jxs_link_continuity"] = block
    return v


def test_a_lost_slice_turns_a_passing_merge_red_and_names_the_link():
    v = _with(_block(src={2: {"JXS cam3 src": {"lost_slices": 3, "concealed": 3}}}))
    assert v["overall_pass"] is True  # the merge passed: only the JPEG XS gate is red
    text = edr.compose_summary(v, META)
    assert text.splitlines()[0].startswith("❌ E2E TEST ZLYHAL"), text
    line = next(l for l in text.splitlines() if "JPEG XS" in l)
    assert "strih vstup 'JXS cam3 src'" in line and "lost_slices +3" in line, line
    assert "concealed +3 (zamaskované = STRATENÉ)" in line, line
    assert "✅" not in text


def test_an_output_drop_is_named_as_the_output():
    text = edr.compose_summary(_with(_block(out={1: {"not_taken": 2}})), META)
    assert text.startswith("❌ E2E TEST ZLYHAL")
    assert "strih výstup 'camera-box JPEG XS program': not_taken +2" in text, text


def test_an_unknown_block_is_red_and_says_so():
    text = edr.compose_summary(_with(_block(stream_unknown="no window START mark")), META)
    assert text.startswith("❌ E2E TEST ZLYHAL"), text
    assert "NEOVERENÉ = ZLYHANIE" in text and "no window START mark" in text, text


def test_a_pass_block_keeps_the_three_line_pass():
    base = edr.compose_summary(_pass_verdict(), META)
    text = edr.compose_summary(_with(_block()), META)
    assert text == base and len(text.splitlines()) == 3, text


def test_no_block_is_byte_identical_and_a_report_only_block_never_gates():
    v = _pass_verdict()
    assert edr.compose_summary(copy.deepcopy(v), META) == edr.compose_summary(v, META)
    blk = _block(src={1: {"JXS cam5 src": {"foreign": 1}}})
    blk["gates_overall_pass"] = False  # a report-only block (never shipped) would not gate
    assert edr.compose_summary(_with(blk), META).startswith("✅")


def test_the_full_report_has_one_line_per_link():
    text = edr.compose_report(_with(_block(src={2: {"JXS cam3 src": {"concealed": 1}}})), {"run_id": "42"})
    assert "Kontinuita JPEG XS liniek (okno nahrávky): ❌ FAIL" in text
    assert "❌ RED" in text
    assert "Strata na JPEG XS linke" in text  # the overall section's blocker line
    sec = text[text.index("Kontinuita JPEG XS liniek"):].split("\n\n")[0].splitlines()
    for name in CAMS + [PGM, "camera-box JPEG XS program"]:
        assert sum(1 for l in sec if l.startswith("  • ") and f"'{name}'" in l) == 1, name
    cam3 = next(l for l in sec if l.startswith("  • ") and "'JXS cam3 src'" in l)
    assert "concealed 1" in cam3 and cam3.endswith("❌"), cam3
    text = edr.compose_report(_with(_block(src={1: {n: {"fec_repaired": 4} for n in CAMS}})), {"run_id": "42"})
    assert "Kontinuita JPEG XS liniek (okno nahrávky): ✅ PASS" in text
    assert "informatívne: fec_repaired 4" in text
    assert "Kontinuita JPEG XS liniek" not in edr.compose_report(_pass_verdict(), {"run_id": "42"})
