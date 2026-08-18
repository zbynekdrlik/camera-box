"""issue 1108 -- unit tests for the dantesync NTP step-rate observability facet in
scripts/rig-health-audit.py (the issue-787 status-page sweep).

The facet surfaces, per node, how often dantesync STEPPED its clock in the last hour -- the
signal behind the fleet-wide QR/burn-ball skips of issue 1108 (a step-storm on the strih NTP
master jumps every box's genlock timecode -> FIFO underruns -> visible skips). It is pure
OBSERVABILITY for the status page; it never touches the E2E gate or dantesync-gate.sh.

These pin the PURE pieces (no ssh / no WS / no HTTP):
  * NTP_STEP_COUNT_AWK    -- the REAL Linux-node counter (the awk program embedded verbatim in the
                            check_cam/check_imag ssh commands); tested by running it against
                            synthetic journals, so the PRODUCTION counter itself is covered -- no
                            second Python copy to drift (the awk analogue of the cadence kernel).
  * parse_ntp_status()   -- pull (ntp_steps_last_hour, ntp_step_storm) from the dantesync
                            :8898 JSON (Windows nodes); ADDITIVE fields -> (None, None) when a
                            body lacks them (dantesync < 1.8.45) or is unparseable.
  * grade_ntp_steprate() -- OK/WARN/FAIL/UNKNOWN + a soft/hard `problems` entry, thresholds
                            mirroring dantesync's own 120/h step-storm boundary.
  * box_verdict()        -- the shared three-tier fold (reused, already tested elsewhere too).
"""
import importlib.util
import re
import subprocess
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "rig_health_audit", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


# --------------------------------------------------------------- NTP_STEP_COUNT_AWK (the real counter)
def _journal(n_steps, extra_lines=0):
    lines = []
    for i in range(n_steps):
        lines.append(f"Aug 18 09:{i%60:02d}:00 CAM1 dantesync[123]: [NTP] Stepped +{1000+i}us")
    for i in range(extra_lines):
        lines.append(f"Aug 18 09:{i%60:02d}:30 CAM1 dantesync[123]: offset:+{50+i}us drift ok")
    return "\n".join(lines)


def _count_via_real_awk(journal_text):
    """Run the ACTUAL awk program the feeder ships in its ssh command against `journal_text`, and
    parse the `ntp_steps_1h=<N>` line exactly as check_cam/check_imag do. This covers the REAL
    production Linux counter, not a Python re-implementation of it."""
    out = subprocess.run(["awk", _mod.NTP_STEP_COUNT_AWK], input=journal_text,
                         capture_output=True, text=True)
    assert out.returncode == 0, out.stderr        # END always prints -> awk must exit 0
    m = re.search(r"ntp_steps_1h=(\d+)", out.stdout)
    assert m, f"awk emitted no ntp_steps_1h= line: {out.stdout!r}"
    return int(m.group(1))


def test_real_awk_counts_only_stepped_lines():
    assert _count_via_real_awk(_journal(28, extra_lines=40)) == 28


def test_real_awk_zero_on_no_steps():
    assert _count_via_real_awk(_journal(0, extra_lines=12)) == 0


def test_real_awk_empty_blob_prints_zero():
    # END runs even on empty input -> `ntp_steps_1h=0`, so a readable-but-idle journal is a real 0,
    # not an absent line (the off_us proxy is what turns an UNREADABLE journal into UNKNOWN).
    assert _count_via_real_awk("") == 0


def test_real_awk_does_not_match_bare_stepped_word():
    # a decoy line mentioning "Stepped" without the "[NTP] " prefix must NOT be counted.
    log = "Aug 18 09:00:00 CAM1 other[9]: Stepped over a config value\n" + _journal(3)
    assert _count_via_real_awk(log) == 3


# --------------------------------------------------------------- parse_ntp_status
def test_parse_ntp_status_reads_present_fields():
    body = '{"is_locked":true,"ntp_offset_us":514,"ntp_steps_last_hour":147,"ntp_step_storm":true}'
    steps, storm = _mod.parse_ntp_status(body)
    assert steps == 147
    assert storm is True


def test_parse_ntp_status_storm_false_is_false_not_none():
    body = '{"ntp_steps_last_hour":12,"ntp_step_storm":false}'
    steps, storm = _mod.parse_ntp_status(body)
    assert steps == 12
    assert storm is False


def test_parse_ntp_status_absent_fields_are_none():
    # the LIVE reality today: strih/stream serve dantesync < 1.8.45, which lacks both additive
    # fields -> UNKNOWN, never a false alarm.
    body = ('{"offset_ns":639726024,"is_locked":true,"ntp_offset_us":514,'
            '"ntp_spread_us":94,"ntp_sample_count":3,"mode":"NANO"}')
    assert _mod.parse_ntp_status(body) == (None, None)


def test_parse_ntp_status_unparseable_is_none():
    assert _mod.parse_ntp_status("") == (None, None)
    assert _mod.parse_ntp_status("not json") == (None, None)
    assert _mod.parse_ntp_status("[1,2,3]") == (None, None)   # valid json, wrong shape


def test_parse_ntp_status_ignores_bool_masquerading_as_count():
    # a bool must not be read as a step count (True -> 1).
    body = '{"ntp_steps_last_hour":true}'
    steps, _ = _mod.parse_ntp_status(body)
    assert steps is None


# --------------------------------------------------------------- grade_ntp_steprate
def test_grade_unknown_when_steps_none():
    v, disp, probs = _mod.grade_ntp_steprate(None)
    assert v == "UNKNOWN"
    assert disp == "n/a"
    assert probs == []                       # UNKNOWN never pages / never a false alarm


def test_grade_ok_at_and_below_healthy_ceiling():
    for n in (0, 28, 36, 72):                # baseline ~30-36/h; healthy ceiling 72/h
        v, disp, probs = _mod.grade_ntp_steprate(n)
        assert v == "OK", n
        assert disp == f"{n}/h"
        assert probs == []


def test_grade_warn_above_ceiling_below_storm():
    for n in (73, 100, 119):
        v, disp, probs = _mod.grade_ntp_steprate(n)
        assert v == "WARN", n
        assert disp == f"{n}/h"
        assert len(probs) == 1
        assert probs[0].startswith("warn:")   # SOFT -> box_verdict WARN, does not page
        assert str(n) in probs[0]


def test_grade_fail_at_and_above_storm_boundary():
    # mirrors dantesync's own 120/h step-storm boundary (issue-1108 storm floor 129/h; strih 147-180/h)
    for n in (120, 129, 147, 180):
        v, disp, probs = _mod.grade_ntp_steprate(n)
        assert v == "FAIL", n
        assert disp == f"{n}/h"
        assert len(probs) == 1
        assert not probs[0].startswith("warn:")   # HARD -> box_verdict FAIL, pages
        assert str(n) in probs[0]


def test_grade_storm_flag_forces_fail_even_at_low_count():
    # the Windows :8898 step-storm flag is authoritative even if the count reads low.
    v, disp, probs = _mod.grade_ntp_steprate(5, storm=True)
    assert v == "FAIL"
    assert len(probs) == 1 and not probs[0].startswith("warn:")


def test_grade_storm_flag_true_without_count():
    v, disp, probs = _mod.grade_ntp_steprate(None, storm=True)
    assert v == "FAIL"
    assert probs and not probs[0].startswith("warn:")


def test_grade_storm_flag_false_does_not_force_fail():
    v, _, probs = _mod.grade_ntp_steprate(30, storm=False)
    assert v == "OK"
    assert probs == []


def test_grade_storm_flag_low_count_names_the_flag_not_a_false_boundary_crossing():
    # issue-1108 review: a storm FLAG with a sub-120 count must NOT annotate the count as (>=120/h)
    # (that read as self-contradictory on the page). Show the honest count, name the flag as trigger.
    v, disp, probs = _mod.grade_ntp_steprate(5, storm=True)
    assert v == "FAIL"
    assert disp == "5/h"                          # the real observed count is still shown, honestly
    assert len(probs) == 1
    assert "(>=" not in probs[0]                  # no misleading "crossed the storm bound" claim
    assert "flag" in probs[0]                     # the dantesync :8898 storm flag is named as trigger


def test_grade_count_at_boundary_still_cites_the_count_not_the_flag():
    # the count-triggered FAIL keeps its explicit (>=120/h) annotation (that IS a boundary crossing).
    _, disp, probs = _mod.grade_ntp_steprate(120)
    assert disp == "120/h"
    assert "(>=120/h)" in probs[0] and "flag" not in probs[0]


# --------------------------------------------------------------- integration fold
# The exact fold each check_* performs: `problems += steprate_problems; box_verdict(problems)`.
def test_fold_storm_drives_node_to_fail():
    _, _, probs = _mod.grade_ntp_steprate(147)
    problems = []
    problems += probs
    assert _mod.box_verdict(problems) == "FAIL"


def test_fold_elevated_rate_drives_node_to_warn():
    _, _, probs = _mod.grade_ntp_steprate(90)
    problems = []
    problems += probs
    assert _mod.box_verdict(problems) == "WARN"


def test_fold_healthy_rate_stays_pass():
    _, _, probs = _mod.grade_ntp_steprate(28)
    assert _mod.box_verdict([] + probs) == "PASS"


def test_fold_unknown_does_not_page():
    _, _, probs = _mod.grade_ntp_steprate(None)
    assert _mod.box_verdict([] + probs) == "PASS"   # a node otherwise clean stays PASS on UNKNOWN
