"""issue 1247 — the per-camera "own digital burn absent" REPORT-ONLY seam
(`full_chain.own_burn_absent_gate`) must render as a report-only `ℹ️ sledované` line and NEVER as a
`❌` blocking failure, mirroring the e2e-discord-report.md convention (#1127). The seam also carries
`gates_overall_pass` so a future one-line LIVE flip auto-routes it to a blocking failure instead of
double-counting (the delivery-spread pattern)."""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402


def _verdict_own_burn_absent_tripped_report_only():
    """A PASSING run (overall_pass=true, as run 1635844760 really was) whose ONLY flagged thing is
    the report-only own-burn-absent gate on cam2 — the exact durable-artifact-overstates case."""
    return {
        "overall_pass": True,
        "full_chain": {
            "zero_loss": True,
            "own_burn_absent_gate": {
                "scheduled_cams": ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6", "cam7"],
                "absent_cams": ["cam2"],
                "per_cam": {"cam1": False, "cam2": True, "cam3": False},
                "pass": False,
                "gates_overall_pass": False,
            },
        },
    }


def test_tripped_report_only_gate_is_listed_as_report_only():
    v = _verdict_own_burn_absent_tripped_report_only()
    names = edr._report_only_tripped(v)
    assert any("burn" in n.lower() for n in names), (
        f"own-burn-absent must appear in the report-only list, got {names!r}"
    )


def test_tripped_report_only_gate_is_never_a_blocking_failure():
    v = _verdict_own_burn_absent_tripped_report_only()
    failures = edr._blocking_failures(v)
    assert all("burn" not in label.lower() or "zero-loss" in label.lower()
               for label, _ in failures), (
        f"own-burn-absent (report-only) must NOT be a blocking failure, got {failures!r}"
    )
    # And the summary of this PASSING run must not carry a ❌.
    summary = edr.compose_summary(v, {"run_id": "1635844760"})
    assert "❌" not in summary, f"a passing run must never render a ❌, got:\n{summary}"


def test_clean_gate_is_not_listed():
    v = {
        "overall_pass": True,
        "full_chain": {
            "zero_loss": True,
            "own_burn_absent_gate": {
                "scheduled_cams": ["cam1", "cam2"],
                "absent_cams": [],
                "per_cam": {"cam1": False, "cam2": False},
                "pass": True,
                "gates_overall_pass": False,
            },
        },
    }
    names = edr._report_only_tripped(v)
    assert not any("burn" in n.lower() for n in names), (
        f"a clean own-burn gate must not be listed, got {names!r}"
    )


def test_future_live_flip_routes_to_blocking_not_report_only():
    # If the seam is ever flipped LIVE (gates_overall_pass=true) and it fails, it must be a BLOCKING
    # failure (auto-follows the seam flip) and NOT double-counted in report-only.
    v = {
        "overall_pass": False,
        "full_chain": {
            "zero_loss": True,
            "own_burn_absent_gate": {
                "scheduled_cams": ["cam1", "cam2"],
                "absent_cams": ["cam2"],
                "per_cam": {"cam1": False, "cam2": True},
                "pass": False,
                "gates_overall_pass": True,
            },
        },
    }
    assert any("burn" in label.lower() and "zero-loss" not in label.lower()
               for label, _ in edr._blocking_failures(v)), (
        "a LIVE-flipped tripped own-burn gate must be a blocking failure"
    )
    assert not any("burn" in n.lower() for n in edr._report_only_tripped(v)), (
        "a LIVE-flipped gate must not also appear in report-only (no double-count)"
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
