"""#1300 -- the rig-health-audit.py CG-chain wiring is REPORT-ONLY.

`check_cg_chain()` emits exactly ONE row with the `CG_CHAIN_REPORT_VERDICT` verdict, which must be
"NOTE" so main()'s PASS/WARN/FAIL counting ignores it -> the CG-chain row can NEVER change the rig
audit's exit code. `cg_chain_detail_from_output` is the pure fold of cg-chain-verify.sh's table
output into that row's detail; it is fixture-driven (no ssh/WS), loaded via the hyphenated-filename
importlib pattern the rig-status tests use.
"""
import importlib.util
import pathlib

_AUDIT = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "rig-health-audit.py"


def _load():
    spec = importlib.util.spec_from_file_location("rig_health_audit_1300", _AUDIT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_report_verdict_is_note():
    # Report-only invariant: the row verdict must be NOTE (main counts only PASS/WARN/FAIL).
    assert _load().CG_CHAIN_REPORT_VERDICT == "NOTE"


def test_note_is_never_counted_as_a_real_verdict():
    mod = _load()
    assert mod.CG_CHAIN_REPORT_VERDICT not in ("PASS", "WARN", "FAIL")


def test_detail_counts_sources_and_echoes_overall():
    mod = _load()
    out = (
        "HOP SOURCE LOCK SAMP ...\n"
        "cg-obs sp-1_video yes 3 8 0 0 0 0 0 7.62 PASS\n"
        "cg-obs sp-2_video no 2 9 0 0 0 0 0 -18.40 FAIL\n"
        "         reason: asrc residual out of band\n"
        "strih cg yes 3 7 0 0 0 0 0 8.01 PASS\n"
        "stream NDI obs hudba yes 2 4 0 0 0 0 0 7.20 PASS\n"
        "OVERALL: FAIL\n"
    )
    detail = mod.cg_chain_detail_from_output(out)
    assert "overall=FAIL" in detail
    assert "sources_pass=3" in detail
    assert "sources_fail=1" in detail
    # the detail advertises its report-only nature + the preserved #787 exemption.
    assert "report-only #1300" in detail
    assert "#787" in detail


def test_detail_all_pass():
    mod = _load()
    out = (
        "cg-obs sp-1_video yes 3 8 0 0 0 0 0 7.62 PASS\n"
        "strih cg yes 3 7 0 0 0 0 0 8.01 PASS\n"
        "OVERALL: PASS\n"
    )
    detail = mod.cg_chain_detail_from_output(out)
    assert "overall=PASS" in detail
    assert "sources_pass=2" in detail
    assert "sources_fail=0" in detail


def test_empty_output_is_safe():
    mod = _load()
    detail = mod.cg_chain_detail_from_output("")
    assert "overall=?" in detail
    assert "sources_pass=0" in detail
    assert "sources_fail=0" in detail
