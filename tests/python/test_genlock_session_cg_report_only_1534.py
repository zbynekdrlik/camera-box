"""issue 1534 ROZHODNUTE (owner, 20.11.2026): the strih `CG-obs` input -- the cg feed from the
resolume cg OBS, minimal priority -- is graded and reported by the whole-session video gate but
never gates. Every camera and stream input stays blocking.

Live case: E2E 44663427882 attempt 3 passed every block except `CG-obs` underruns 11 -> 99 after
the settle, the resolume sender delivering fewer frames while SongPlayer played on it."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "scripts"))
import e2e_report_session_continuity as rep  # noqa: E402
import genlock_session_continuity as gsc  # noqa: E402
from test_genlock_session_continuity_1534 import _inp, decide, raw_read, session  # noqa: E402

NAMES = ("JXS cam3 src", "CG-obs")


def test_cg_obs_underruns_are_graded_but_never_gate():
    res = decide(raw_read(session(names=NAMES, edits={("audit", "CG-obs", 300): {"u": 88}})))
    cg = _inp(res, "CG-obs")
    assert cg["verdict"] == "FAIL", cg
    assert cg["report_only"] is True
    assert "minimal priority" in cg["note"]
    assert res["verdict"] == "PASS", res["failures"]
    assert not any("CG-obs" in f for f in res["failures"])


def test_a_camera_input_still_gates_next_to_a_report_only_cg():
    res = decide(raw_read(session(names=NAMES, edits={("audit", "CG-obs", 300): {"u": 88},
                                                      ("audit", "JXS cam3 src", 300): {"u": 2}})))
    assert res["verdict"] == "FAIL"
    assert any("'JXS cam3 src'" in f for f in res["failures"])
    assert not any("CG-obs" in f for f in res["failures"])


def test_only_the_exact_cg_obs_name_is_report_only():
    for name in ("CG-obs 2", "cg-obs", "NDI cam1", "Zaloha kamera"):
        res = decide(raw_read(session(names=("JXS cam3 src", name), edits={("audit", name, 300): {"u": 4}})))
        assert res["verdict"] == "FAIL", name
        assert _inp(res, name)["report_only"] is False, name


def test_the_combined_block_passes_and_the_report_says_report_only():
    res = decide(raw_read(session(names=NAMES, edits={("audit", "CG-obs", 300): {"u": 88}})))
    blk = gsc.combine([res])
    assert blk["pass"] is True and blk["gates_overall_pass"] is True
    v = {"genlock_session_continuity": blk}
    assert not rep.blocks(v)
    text = rep.section(v)
    line = next(l for l in text.split("\n") if "'CG-obs'" in l)
    assert "len informatívne" in line and "minimálna priorita" in line, line
    assert "kvitovaná offline" not in line
    assert "Zlyhalo" not in text
