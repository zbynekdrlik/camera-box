"""#1316 -- the imag RETIRED verdict renders as a NEUTRAL row on the rig status page.

scripts/rig-health-audit.py emits ONE neutral `[RETIRED] imag ...` row (outside PASS/WARN/FAIL,
like its `[NOTE]` rows) once imag-nb is returned/absent. The RENDERER scripts/rig-status.py must
surface that row as its OWN grey neutral badge -- never green (misleading: the box is gone),
never red/amber (the false-noise the owner complained about), never silently dropped -- counted
in neither fail nor warn, and never flipping the overall page verdict to WARN/FAIL/ERROR on its
own. The verdict TOKEN is single-sourced from the audit's IMAG_RETIRED_VERDICT constant so the
renderer can never drift from what the audit actually emits.
"""
import importlib.util
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load(name, fname):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / fname)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


rs = _load("rig_status_1316", "rig-status.py")
audit = _load("rig_health_audit_1316", "rig-health-audit.py")

RETIRED = audit.IMAG_RETIRED_VERDICT  # the ONE source of truth for the neutral token

# Healthy cams + the neutral retired imag row -> a fleet whose only non-PASS row is imag RETIRED.
_RETIRED_SWEEP = f"""\
[PASS] cam1    svc=active fps=60.0/60.0 chroma=colour
[PASS] cam2    svc=active fps=60.0/60.0 chroma=colour
[{RETIRED}] imag    RETIRED (imag-nb returned to owner 16.9.2026; role returns on a new notebook)
[PASS] strih   obs64=1 render=30.0fps/6.2ms
"""

# The retired row must NOT mask a genuine FAIL elsewhere.
_RETIRED_PLUS_FAIL = f"""\
[PASS] cam1    svc=active fps=60.0/60.0
[{RETIRED}] imag    RETIRED (imag-nb returned to owner 16.9.2026)
[FAIL] stream  obs64=1 render=30.0fps  <<AUDIO-BUF=120ms>>
"""


def _rec(records, node):
    return next((r for r in records if r["node"] == node), None)


def test_retired_row_is_parsed_and_visible():
    recs = rs.parse_audit(_RETIRED_SWEEP)
    imag = _rec(recs, "imag")
    assert imag is not None, "the retired imag row must be surfaced, not dropped"
    assert imag["verdict"] == RETIRED


def test_retired_not_counted_as_a_health_tier():
    recs = rs.parse_audit(_RETIRED_SWEEP)
    s = rs.summarize(recs)
    assert s["fail"] == 0 and s["warn"] == 0
    # cam1, cam2, strih are the ONLY counted rows -- imag (RETIRED) is neutral, never a PASS
    assert s["pass"] == 3


def test_retired_only_nonpass_reads_overall_pass():
    recs = rs.parse_audit(_RETIRED_SWEEP)
    assert rs.overall_state(recs, exit_code=0) == "PASS"


def test_retired_renders_neutral_grey_badge_with_slovak_label():
    recs = rs.parse_audit(_RETIRED_SWEEP)
    page = rs.render_html(recs, "1.7.0-dev.632", "2026-09-16T00:00:00Z", exit_code=0)
    assert "imag" in page
    # a DISTINCT neutral badge/row class -- not the pass/warn/fail ones
    assert "b-RETIRED" in page
    assert "v-RETIRED" in page
    # its CSS rule is present (a grey neutral colour, defined once)
    assert ".b-RETIRED" in page
    # the BADGE shows a neutral Slovak label, not the raw English token
    assert "VRÁTEN" in page


def test_real_fail_still_fails_alongside_a_retired_row():
    recs = rs.parse_audit(_RETIRED_PLUS_FAIL)
    assert _rec(recs, "imag")["verdict"] == RETIRED        # retired still visible
    assert rs.overall_state(recs, exit_code=2) == "FAIL"   # a real FAIL still wins
    s = rs.summarize(recs)
    assert s["fail"] == 1                                   # only stream; imag not counted


def test_neutral_only_set_is_not_a_false_green():
    # a sweep with ZERO real PASS/WARN/FAIL rows is not proof of health -> ERROR, never PASS
    recs = rs.parse_audit(f"[{RETIRED}] imag    RETIRED (box returned)\n")
    assert rs.overall_state(recs, exit_code=0) == "ERROR"


def test_retired_verdict_is_single_sourced_from_the_audit():
    # rig-status must reuse the audit constant, never a retyped literal that can drift
    assert rs.RETIRED_VERDICT == audit.IMAG_RETIRED_VERDICT
