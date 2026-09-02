"""issue 1166 promote — the duplication-masked-cadence gate (`all_cambox_continuity.
duplication_masked_cadence`) flips from REPORT-ONLY to LIVE (`gates_overall_pass=true`). Mirrors the
`own_burn_absent`/`tear` precedent (test_e2e_discord_report_own_burn_absent_1247.py /
test_e2e_discord_report_tear_1196.py): `_blocking_failures` gets a new branch guarded by the node's
own `gates_overall_pass` field, and `_report_only_tripped`'s existing branch is guarded
`gates_overall_pass is not True` so the classifier auto-follows the flip without double-counting
(the delivery-spread pattern the module docstring already documents)."""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402


def _verdict_dup_cadence_masked(gates_overall_pass):
    """A run with a positive duplication_masked_cadence trip (masked_windows=1), the node's own
    `gates_overall_pass` set as given — pre-flip verdicts carry `false`, post-flip verdicts (this
    ticket) carry `true`."""
    return {
        "overall_pass": not gates_overall_pass,  # a LIVE trip fails the run; report-only doesn't
        "full_chain": {"zero_loss": True},
        "all_cambox_continuity": {
            "overall_pass": True,
            "duplication_masked_cadence": {
                "masked_windows": 1,
                "worst_masked_duplicate_fraction": 0.503,
                "worst_raw_duplicate_fraction": 0.503,
                "bound_duplicate_fraction": 0.10,
                "pass": False,
                "gates_overall_pass": gates_overall_pass,
                "signal_viability": "viable",
                "signal_promotable": True,
            },
        },
    }


def _verdict_dup_cadence_clean(gates_overall_pass):
    return {
        "overall_pass": True,
        "full_chain": {"zero_loss": True},
        "all_cambox_continuity": {
            "overall_pass": True,
            "duplication_masked_cadence": {
                "masked_windows": 0,
                "worst_masked_duplicate_fraction": None,
                "worst_raw_duplicate_fraction": 0.007,
                "bound_duplicate_fraction": 0.10,
                "pass": True,
                "gates_overall_pass": gates_overall_pass,
                "signal_viability": "viable",
                "signal_promotable": True,
            },
        },
    }


def test_live_flip_masked_window_is_a_blocking_failure():
    v = _verdict_dup_cadence_masked(gates_overall_pass=True)
    failures = edr._blocking_failures(v)
    assert any(
        "duplik" in label.lower() and "zero-loss" not in label.lower()
        for label, _ in failures
    ), f"a LIVE-flipped masked dup-cadence window must be a blocking failure, got {failures!r}"


def test_live_flip_masked_window_is_not_also_report_only():
    v = _verdict_dup_cadence_masked(gates_overall_pass=True)
    names = edr._report_only_tripped(v)
    assert not any("duplik" in n.lower() for n in names), (
        f"a LIVE-flipped gate must not also appear in report-only (no double-count), got {names!r}"
    )


def test_live_flip_masked_window_renders_a_cross_in_summary():
    v = _verdict_dup_cadence_masked(gates_overall_pass=True)
    summary = edr.compose_summary(v, {"run_id": "1717119205"})
    assert "❌" in summary, f"a LIVE-flipped FAIL must render a cross, got:\n{summary}"


def test_pre_flip_verdict_still_routes_report_only():
    # An OLD verdict rendered before the flip (gates_overall_pass=false on its own node — the shape
    # every retained pre-#1166-promote verdict JSON carries) must keep its historical report-only
    # routing: never retroactively promoted to a blocking failure by a naive "field present" check.
    v = _verdict_dup_cadence_masked(gates_overall_pass=False)
    names = edr._report_only_tripped(v)
    assert any("duplik" in n.lower() for n in names), (
        f"a pre-flip (gates_overall_pass=false) masked window must stay report-only, got {names!r}"
    )
    failures = edr._blocking_failures(v)
    assert not any("duplik" in label.lower() for label, _ in failures), (
        f"a pre-flip masked window must NOT be a blocking failure, got {failures!r}"
    )
    summary = edr.compose_summary(v, {"run_id": "1104689227"})
    assert "❌" not in summary, f"a pre-flip report-only trip must never render a ❌, got:\n{summary}"


def test_clean_gate_is_never_listed_either_way():
    for gop in (True, False):
        v = _verdict_dup_cadence_clean(gates_overall_pass=gop)
        assert not any("duplik" in n.lower() for n in edr._report_only_tripped(v)), (
            f"a clean (masked_windows=0) gate must not be report-only-listed, gates_overall_pass={gop}"
        )
        assert not any("duplik" in label.lower() for label, _ in edr._blocking_failures(v)), (
            f"a clean (masked_windows=0) gate must not be a blocking failure, gates_overall_pass={gop}"
        )


if __name__ == "__main__":
    # Allow direct execution (Tier-0: pytest is available, but a bare run is a quick smoke).
    import traceback
    fns = [v for k, v in sorted(globals().items()) if k.startswith("test_") and callable(v)]
    failed = 0
    for fn in fns:
        try:
            fn()
            print("PASS", fn.__name__)
        except Exception:
            failed += 1
            print("FAIL", fn.__name__)
            traceback.print_exc()
    sys.exit(1 if failed else 0)
