"""issue 1367 — a cambox window whose captured content is MULTI-SOURCE (cam2 filming the strih-lx
multiview) is judged by its node burn: recording-verdict.rs tags it
`segments[].multi_source` and folds its copies/gaps, cadence and frozen_leg REPORT-ONLY. Both
report renderings must name the window `multi-source (report-only by #1367 decision)` with its
fraction, never render it as a `❌`, and a PASS keeps the 3-line cap.

The fixture is the REAL release-PR-1373 run 2059624745 verdict re-folded by the decision
(`verdict_multi_source_pass_2059624745_1367.json`: the two CAM2 windows carry the tag, the cadence
worsts and frozen_leg exclude them, overall_pass=true — the shape recording-verdict.rs emits)."""
import copy
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIX = (
    pathlib.Path(__file__).resolve().parent
    / "fixtures"
    / "e2e_discord_report"
    / "verdict_multi_source_pass_2059624745_1367.json"
)
TAG = "multi-source (report-only by #1367 decision)"


def _load():
    return json.loads(_FIX.read_text())


def _with_cam3_failure(v):
    """The same run, but a SINGLE-source CAM3 window carries copies/gaps -> a real blocking red."""
    v = copy.deepcopy(v)
    ac = v["all_cambox_continuity"]
    cam3 = next(s for s in ac["segments"] if s["cambox"] == "CAM3")
    cam3["copies"] = 9
    cam3["gaps"] = 9
    cam3["pass"] = False
    cam3["relaxed_pass"] = False
    ac["overall_pass"] = False
    ac["windows_over_copies_gaps_tolerance"] = 1
    v["overall_pass"] = False
    return v


def test_fixture_is_the_refolded_real_run_1367():
    v = _load()
    ac = v["all_cambox_continuity"]
    tagged = [s for s in ac["segments"] if s.get("multi_source")]
    assert [s["cambox"] for s in tagged] == ["CAM2", "CAM2"]
    assert [round(s["multi_source"]["multi_path_suspect_fraction"], 2) for s in tagged] == [
        0.43,
        0.6,
    ]
    assert all(s["copies"] > 0 for s in tagged), "the copies stay computed (report-only)"
    assert v["overall_pass"] is True
    assert v["frozen_leg"]["frozen"] == []
    assert len(v["frozen_leg"]["multi_source_report_only"]) == 2


def test_pass_summary_keeps_three_lines_and_names_the_multi_source_windows_1367():
    text = edr.compose_summary(_load(), {"run_id": "2059624745"})
    lines = text.splitlines()
    assert len(lines) == 3, text
    assert lines[0].startswith("✅ E2E TEST PREŠIEL"), text
    assert f"CAM2 {TAG}, 0.43, 0.60" in lines[1], text
    assert "❌" not in text


def test_multi_source_window_is_never_a_blocking_failure_1367():
    labels = [label for label, _ in edr._blocking_failures(_load())]
    assert labels == [], labels


def test_fail_summary_blames_only_the_single_source_window_1367():
    v = _with_cam3_failure(_load())
    labels = [label for label, _ in edr._blocking_failures(v)]
    cont = [label for label in labels if "Plynulosť/kontinuita" in label]
    assert len(cont) == 1, labels
    assert "CAM3" in cont[0] and "CAM2" not in cont[0], cont
    text = edr.compose_summary(v, {"run_id": "2059624745"})
    info = [line for line in text.splitlines() if line.startswith("ℹ️")]
    assert len(info) == 1, text
    assert f"CAM2 {TAG}, 0.43, 0.60" in info[0], text


def test_full_report_names_each_multi_source_window_with_its_fraction_1367():
    text = edr.compose_report(_load(), {"run_id": "2059624745"})
    assert f"CAM2 {TAG}, 0.43" in text, text
    assert f"CAM2 {TAG}, 0.60" in text, text
    assert "✅ PASS" in text


def test_a_verdict_without_multi_source_windows_renders_unchanged_1367():
    v = _load()
    for s in v["all_cambox_continuity"]["segments"]:
        s.pop("multi_source", None)
    v["frozen_leg"].pop("multi_source_report_only", None)
    text = edr.compose_summary(v, {"run_id": "2059624745"})
    assert TAG not in text
    assert "multi-source" not in edr.compose_report(v, {"run_id": "2059624745"})
