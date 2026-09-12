"""#1296 — unit tests for grade_resolume_bundle() in scripts/rig-health-audit.py.

RESOLUME-SNV is surfaced on the issue-787 status page as a REPORT-ONLY, rate-EXEMPT (#787) node:
its genlock build + OBS identity are rendered when it is serving :8899, and it is OMITTED entirely
when the traveling box is away. The grader must NEVER produce FAIL/WARN (it is not a gated node),
and it must never crash on a missing/partial/malformed state. These pin that PURE grader (no HTTP).
"""
import importlib.util
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
grade = _mod.grade_resolume_bundle


def test_present_state_renders_pass_with_all_facets():
    verdict, detail = grade({
        "genlock_build_sha": "abc1234feedface",
        "obs_process_count": "1",
        "port4455_owner_version": "31.0.0",
    })
    assert verdict == "PASS"
    assert "genlock_build_sha=abc1234feedface" in detail
    assert "obs64=1" in detail
    assert "obs_version=31.0.0" in detail
    # #787: the row must advertise that it is report-only / rate-exempt, never a gated verdict.
    assert "report-only" in detail


def test_away_box_is_omitted_never_failed():
    # None (not serving / unreachable) -> verdict None -> check_resolume omits the row entirely.
    assert grade(None) == (None, None)
    # an empty dict (served but empty body) is likewise "nothing to show", never a FAIL.
    assert grade({}) == (None, None)


def test_malformed_state_is_omitted_never_crashes():
    # a non-dict body (a JSON array / string) must be treated as away, not crash.
    assert grade([]) == (None, None)
    assert grade("not-a-dict") == (None, None)


def test_partial_state_fills_missing_facets_with_na_still_pass():
    verdict, detail = grade({"genlock_build_sha": "deadbeef"})
    assert verdict == "PASS"
    assert "genlock_build_sha=deadbeef" in detail
    assert "obs64=n/a" in detail
    assert "obs_version=n/a" in detail


def test_never_returns_fail_or_warn_for_any_input():
    # The #787 exemption contract: this grader is report-only, it can only PASS or be omitted.
    for state in (None, {}, {"genlock_build_sha": "x"},
                  {"genlock_build_sha": "x", "obs_process_count": "2", "port4455_owner_version": "30"}):
        verdict, _ = grade(state)
        assert verdict in (None, "PASS"), f"report-only grader must never FAIL/WARN: {verdict!r}"
