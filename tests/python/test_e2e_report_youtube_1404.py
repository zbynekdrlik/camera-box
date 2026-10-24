"""Issue 1404 -- the E2E report names its END POINT (owner 24.10.2026): the Discord summary never
claims the full path until the YouTube stage passed, a red YouTube block makes the run RED, and the
per-point table (job summary + PR comment) shows cam -> strih -> stream recording -> YouTube."""
import copy
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "scripts"))
import e2e_discord_report as rep  # noqa: E402
import e2e_report_youtube as yt  # noqa: E402

FIX = REPO / "tests" / "python" / "fixtures" / "e2e_discord_report"


def _pass_verdict():
    return json.loads((FIX / "verdict_real_pass_reportonly_1104689227.json").read_text())


def _yt_block(verdict="PASS", reasons=()):
    return {"schema": 1, "verdict": verdict, "pass": verdict == "PASS", "gates_overall_pass": True,
            "reasons": list(reasons), "vod_id": "AbCdEfGhIjK", "windows_measured": 2, "windows_dropped": [],
            "criteria": {"av": verdict, "dupskip": verdict, "publish": "PASS", "audio": verdict, "coverage": "PASS"},
            "windows": [{"name": "w01-CAM5", "errors": [], "dup": 0, "skip": 0, "audio_lag_jumps": 0,
                         "audio_low_corr": 0, "audio_silent": 0, "av_delta_ms": 11.5},
                        {"name": "w02-CAM3", "errors": [], "dup": 0, "skip": 0, "audio_lag_jumps": 0,
                         "audio_low_corr": 0, "audio_silent": 0, "av_delta_ms": 14.0}]}


def test_a_pass_without_youtube_never_claims_the_full_path():
    v = _pass_verdict()
    s = rep.compose_summary(v, {"run_id": "1"})
    assert "celá cesta" not in s and "YouTube sa v tomto behu nemeral" in s
    assert s.startswith("✅")  # the merge passed; it only names its end point honestly


def test_a_youtube_pass_names_the_full_path_to_youtube():
    v = _pass_verdict()
    v["youtube_leg"] = _yt_block()
    s = rep.compose_summary(v, {"run_id": "1"})
    assert s.startswith("✅") and "celá cesta kamera → YouTube" in s
    assert len(s.splitlines()) == 3  # the owner's 3-line PASS cap holds


def test_a_red_youtube_block_makes_the_run_red_and_is_named():
    for verdict, word in (("FAIL", "ZLYHAL"), ("UNKNOWN", "NEOVERENÝ = ZLYHANIE")):
        v = _pass_verdict()
        v["youtube_leg"] = _yt_block(verdict, ["restreamer delivery refused: HTTP 503 not_provisioned"])
        assert rep._run_passed(v) is False
        s = rep.compose_summary(v, {"run_id": "1"})
        assert s.startswith("❌") and word in s and "not_provisioned" in s
        full = rep.compose_report(v, {"run_id": "1"})
        assert "📺 YouTube" in full and "❌ RED" in full


def test_the_points_table_states_the_end_point_and_each_point():
    v = _pass_verdict()
    md = yt.points_markdown(v, "https://example/run/1")
    assert "Koncový bod overenia: stream nahrávka" in md and "NIE JE overenie celej cesty" in md
    for row in ("| strih program nahrávka |", "| stream nahrávka |", "| YouTube VOD"):
        assert row in md
    assert "strih nemá merací zvuk" in md
    assert "https://example/run/1" in md
    v["youtube_leg"] = _yt_block()
    md = yt.points_markdown(v)
    assert "Koncový bod: YouTube" in md and "NIE JE" not in md
    assert "VOD−nahrávka +11.5..+14.0 ms" in md


def test_an_unknown_youtube_criterion_is_never_shown_green():
    v = _pass_verdict()
    b = _yt_block("UNKNOWN", ["w01-CAM5: dup/skip not measured"])
    v["youtube_leg"] = b
    yt_row = [line for line in yt.points_markdown(v).splitlines() if line.startswith("| YouTube VOD")][0]
    assert "✅" not in yt_row


def test_the_cli_writes_a_page_even_without_a_verdict(tmp_path):
    out = tmp_path / "points.md"
    r = subprocess.run([sys.executable, str(REPO / "scripts" / "e2e_report_youtube.py"), "--verdict",
                        str(tmp_path / "missing.json"), "--out", str(out)], capture_output=True, text=True)
    assert r.returncode == 0 and "Verdikt JSON neexistuje" in out.read_text()


def test_a_verdict_without_the_block_renders_the_report_as_before_apart_from_the_end_point():
    v = _pass_verdict()
    before = rep.compose_report(copy.deepcopy(v), {"run_id": "1"})
    assert "📺 YouTube" not in before


def test_a_stage_that_started_but_never_folded_is_red_not_unmeasured(tmp_path):
    v = _pass_verdict()
    md = yt.points_markdown(v, stage_attempted=True)
    row = [line for line in md.splitlines() if line.startswith("| YouTube VOD")][0]
    assert "nemeraný" not in row and "✅" not in row
    assert "no verdict was folded" in md and "NIE JE" in md
    vj = tmp_path / "v.json"
    vj.write_text(json.dumps(v))
    stage = tmp_path / "youtube-leg"
    stage.mkdir()
    (stage / "state.json").write_text("{}")
    out = tmp_path / "p.md"
    subprocess.run([sys.executable, str(REPO / "scripts" / "e2e_report_youtube.py"), "--verdict", str(vj),
                    "--stage-dir", str(stage), "--out", str(out)], check=True)
    assert "no verdict was folded" in out.read_text()
