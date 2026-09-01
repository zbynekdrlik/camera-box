"""issue 1196 — the projection-tap scanout-TEAR gate is now LIVE (promoted from report-only). Its
`all_cambox_continuity.tear` block carries `gates_overall_pass=true`; an Observed window whose
`tear_gate_pass` is False must render as a `❌` BLOCKING failure (the imag projection is tearing on
the CAM2 leg). The `gates_overall_pass is True` guard makes the classifier auto-follow a future
one-line disarm (route to nothing) — the delivery-spread / own_burn_absent flip pattern."""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402


def _verdict_with_tear(gates_overall_pass, window_pass):
    """Minimal verdict carrying a tear block. `window_pass` is the CAM2 window's tear_gate_pass."""
    return {
        "overall_pass": not (gates_overall_pass and window_pass is False),
        "all_cambox_continuity": {
            "tear": {
                "gates_overall_pass": gates_overall_pass,
                "tear_fraction_ceiling": 0.005,
                "windows": [
                    {"cambox": "CAM2", "tear_gate_pass": window_pass, "viability": "observed"},
                    {"cambox": "CAM1", "tear_gate_pass": True, "viability": "unproven"},
                ],
            }
        },
    }


def test_live_tear_over_ceiling_is_a_blocking_failure():
    v = _verdict_with_tear(gates_overall_pass=True, window_pass=False)
    labels = [label for label, _ in edr._blocking_failures(v)]
    assert any("scanout tear" in label for label in labels), (
        f"a LIVE tear over the ceiling must be a blocking failure, got {labels!r}"
    )
    # The failing CAM2 leg is named.
    assert any("CAM2" in label for label in labels if "scanout" in label)


def test_green_tear_window_is_not_a_blocking_failure():
    v = _verdict_with_tear(gates_overall_pass=True, window_pass=True)
    labels = [label for label, _ in edr._blocking_failures(v)]
    assert not any("scanout" in label for label in labels), (
        f"a passing tear window must not be a blocking failure, got {labels!r}"
    )


def test_disarmed_tear_seam_never_blocks():
    # If the seam is ever flipped back to report-only (gates_overall_pass=false), a failing window
    # must NOT be a blocking failure — the guard makes the classifier auto-follow the disarm.
    v = _verdict_with_tear(gates_overall_pass=False, window_pass=False)
    labels = [label for label, _ in edr._blocking_failures(v)]
    assert not any("scanout" in label for label in labels), (
        f"a disarmed tear seam must never block, got {labels!r}"
    )


def test_missing_tear_block_does_not_crash():
    assert isinstance(edr._blocking_failures({}), list)
    assert isinstance(edr._blocking_failures({"all_cambox_continuity": {}}), list)


def test_non_operable_tear_signal_is_report_only_not_blocking():
    # issue 1196 review-hardening: signal_operable=False (aux collapsed) surfaces as a report-only
    # info line, NEVER a blocking failure (a genuinely aux-free run is not a failure).
    v = {
        "overall_pass": True,
        "all_cambox_continuity": {
            "tear": {
                "gates_overall_pass": True,
                "signal_operable": False,
                "windows": [{"cambox": "CAM2", "tear_gate_pass": True}],
            }
        },
    }
    names = edr._report_only_tripped(v)
    assert any("slep" in n for n in names), (
        f"a non-operable tear signal must surface report-only, got {names!r}"
    )
    labels = [label for label, _ in edr._blocking_failures(v)]
    assert not any("slep" in label or "tear" in label.lower() for label in labels), (
        f"non-operable must never be a blocking failure, got {labels!r}"
    )


def test_operable_tear_signal_is_not_surfaced():
    v = {
        "all_cambox_continuity": {
            "tear": {
                "gates_overall_pass": True,
                "signal_operable": True,
                "windows": [{"cambox": "CAM2", "tear_gate_pass": True}],
            }
        }
    }
    assert not any("slep" in n for n in edr._report_only_tripped(v))
