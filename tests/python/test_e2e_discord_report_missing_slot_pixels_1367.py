"""Issue 1367 — the pixel proofs of the classified slots the merge could not extract
(`scripts/lib/missing-slot-pixels.sh` merges a `missing_slot_pixels` block into the verdict JSON)
surface in the E2E report.

- The FULL report (`compose_report`) lists every exported PNG path per slot.
- The phone summary (`compose_summary`) carries ONE `🖼` line with the proof directories on a FAIL.
  A PASS stays at most three lines (the owner's hard cap), so it never carries the line.
- No block, or nothing exported: both renderings are byte-identical to before.
"""
import copy
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"
_PASS = "verdict_real_pass_reportonly_1104689227.json"
_FAIL = "verdict_real_fail_cam1_77008829.json"
_META = {"run_id": "77", "event": "CI PR gate", "duration_secs": 300}

_BLOCK = {
    "cap": 12,
    "total_slots": 3,
    "boxes": {"strih": "ok", "stream": "failed"},
    "slots": [
        {
            "node": "cam3",
            "frame_index": 9072,
            "box": "strih",
            "pngs": [
                "/tmp/recording-e2e-77/cam3-missing/frame-9071.png",
                "/tmp/recording-e2e-77/cam3-missing/frame-9072.png",
                "/tmp/recording-e2e-77/cam3-missing/frame-9073.png",
            ],
        },
        {"node": "strih", "frame_index": 400, "box": "stream", "pngs": []},
    ],
    "exported_slots": 1,
    "dirs": ["/tmp/recording-e2e-77/cam3-missing"],
}


def _load(name):
    with open(_FIXTURES / name, encoding="utf-8") as f:
        return json.load(f)


def _with_block(name, block=None):
    v = _load(name)
    v["missing_slot_pixels"] = copy.deepcopy(_BLOCK if block is None else block)
    return v


def _lines(text):
    return [ln for ln in text.splitlines() if ln.strip()]


def test_full_report_lists_every_exported_png_and_the_box_status():
    text = edr.compose_report(_with_block(_FAIL), _META)
    assert "Pixelový dôkaz" in text
    for p in _BLOCK["slots"][0]["pngs"]:
        assert p in text
    assert "cam3" in text and "9072" in text
    assert "strih 400" in text or "strih snímka 400" in text
    assert "stream: zlyhal" in text


def test_fail_summary_carries_one_line_with_the_proof_dir():
    summary = edr.compose_summary(_with_block(_FAIL), _META)
    hits = [ln for ln in _lines(summary) if ln.startswith("🖼")]
    assert len(hits) == 1, summary
    assert "/tmp/recording-e2e-77/cam3-missing" in hits[0]
    assert _lines(summary)[-1].startswith("🔗"), "the link stays last"


def test_pass_summary_keeps_the_three_line_cap():
    base = edr.compose_summary(_load(_PASS), _META)
    summary = edr.compose_summary(_with_block(_PASS), _META)
    assert summary == base
    assert len(_lines(summary)) <= 3


def test_no_block_or_nothing_exported_changes_nothing():
    for name in (_PASS, _FAIL):
        v = _load(name)
        empty = dict(_BLOCK, exported_slots=0, dirs=[], slots=[])
        assert edr.compose_summary(_with_block(name, empty), _META) == edr.compose_summary(v, _META)
        assert edr.compose_report(v, _META) == edr.compose_report(_with_block(name, {}), _META)
