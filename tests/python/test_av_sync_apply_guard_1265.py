"""#1265 task 3 — the PURE refusal predicate protecting the #856 rig-wide A/V controller from
walking the prod pin when THIS run's audio timeline was unstable (scripts/av_sync_apply_guard.py).

The #856 combiner (`av_sync_combine_offsets.py`) + `av_sync_calibrate.py --apply` in cleanup() walk
`NDI 2ME PGM`'s genlock latency toward the median of this run's measured per-camera offsets, guarded
only by <2-measured-cams / >100 ms-spread. A run whose `mbc` audio timeline was flapping (issue 1265)
still passes both, so it calibrated the pin 926->976 against noise. This predicate HOLDs the apply on
any of THREE independent signals: the run's stream `mbc` ts_lag band was DRIFTING (task-2 verdict),
the residual median is beyond a sanity ceiling (green series was within +/-33 ms; the bad runs were
-77/-126), or the proposed correction jumps far from the last-applied value. Pure, pytest Tier-0.
"""
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_apply_guard as g  # noqa: E402

_GUARD = _SCRIPTS / "av_sync_apply_guard.py"


# ------------------------------------------------------------------ proceed (empty reason)
def test_stable_run_proceeds():
    # green-series-shaped: small residual, HEALTHY band, proposed near last-applied -> no hold.
    r = g.hold_reason(residual_median_ms=16.9, residual_spread_ms=20.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-285.0)
    assert r == ""


def test_no_signals_at_all_proceeds():
    # everything empty/unknown (pre-deploy: no band facet, no last-applied) + a small residual -> proceed.
    r = g.hold_reason(residual_median_ms=-8.6, residual_spread_ms=None, band_verdict="",
                      last_applied_offset_ms=None, proposed_offset_ms=-283.0)
    assert r == ""


# ------------------------------------------------------------------ (1) band DRIFTING
def test_band_drifting_holds():
    r = g.hold_reason(residual_median_ms=16.0, residual_spread_ms=20.0, band_verdict="DRIFTING",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-285.0)
    assert r != ""
    assert "DRIFTING" in r or "band" in r.lower()


def test_band_unknown_does_not_hold_by_itself():
    # UNKNOWN/SKIP band (pre-deploy, or box unreachable) is NOT a hold on its own.
    for v in ("UNKNOWN", "SKIP", ""):
        assert g.hold_reason(residual_median_ms=10.0, residual_spread_ms=5.0, band_verdict=v,
                             last_applied_offset_ms=None, proposed_offset_ms=-283.0) == ""


# ------------------------------------------------------------------ (2) residual beyond the ceiling
def test_residual_beyond_ceiling_holds_both_incident_runs():
    for resid in (-77.2, -126.0):
        r = g.hold_reason(residual_median_ms=resid, residual_spread_ms=30.0, band_verdict="",
                          last_applied_offset_ms=None, proposed_offset_ms=-283.0)
        assert r != "", f"residual {resid} must hold"
        assert "residual" in r.lower()


def test_green_series_residuals_within_ceiling_proceed():
    for resid in (-8.6, 6.5, 32.9, -18.6, 16.9, -20.0):
        assert g.hold_reason(residual_median_ms=resid, residual_spread_ms=30.0, band_verdict="HEALTHY",
                             last_applied_offset_ms=None, proposed_offset_ms=-283.0) == "", \
            f"green-series residual {resid} must proceed"


def test_residual_ceiling_is_configurable():
    assert g.hold_reason(residual_median_ms=50.0, residual_spread_ms=10.0, band_verdict="",
                         last_applied_offset_ms=None, proposed_offset_ms=0.0,
                         residual_ceiling_ms=40.0) != ""
    assert g.hold_reason(residual_median_ms=50.0, residual_spread_ms=10.0, band_verdict="",
                         last_applied_offset_ms=None, proposed_offset_ms=0.0,
                         residual_ceiling_ms=90.0) == ""


# ------------------------------------------------------------------ (3) jump vs last-applied
def test_big_jump_from_last_applied_holds():
    r = g.hold_reason(residual_median_ms=10.0, residual_spread_ms=5.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-150.0)  # 133 ms jump
    assert r != ""
    assert "jump" in r.lower() or "last applied" in r.lower()


def test_small_drift_from_last_applied_proceeds():
    # a steady small drift the controller SHOULD track is not a jump.
    assert g.hold_reason(residual_median_ms=10.0, residual_spread_ms=5.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=-283.0, proposed_offset_ms=-330.0) == ""  # 47 ms < 90


def test_absent_last_applied_skips_jump_condition():
    # no ~/.camera-box/av-sync-last.json yet -> condition 3 dormant, not a hold.
    assert g.hold_reason(residual_median_ms=10.0, residual_spread_ms=5.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=None, proposed_offset_ms=-999.0) == ""


# ------------------------------------------------------------------ fail-safe numeric parsing
def test_unparseable_numerics_do_not_crash_and_skip_their_condition():
    # a garbage residual just skips condition 2 (never a crash, never a false hold from it).
    assert g.hold_reason(residual_median_ms="n/a", residual_spread_ms="", band_verdict="HEALTHY",
                         last_applied_offset_ms="", proposed_offset_ms="-283") == ""


def test_string_inputs_parse():
    # the shell passes everything as strings.
    assert g.hold_reason(residual_median_ms="-126.0", residual_spread_ms="20.8", band_verdict="",
                         last_applied_offset_ms="", proposed_offset_ms="-283.0") != ""


# ------------------------------------------------------------------ CLI contract (the shell reads this)
def test_cli_decide_hold():
    out = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "-126.0",
         "--residual-spread-ms", "20.8", "--band-verdict", "DRIFTING",
         "--last-applied-offset-ms", "", "--proposed-offset-ms", "-283.0"],
        capture_output=True, text=True, check=True,
    ).stdout
    line = next(l for l in out.splitlines() if l.startswith("hold_reason="))
    assert line != "hold_reason="        # non-empty reason


def test_cli_decide_proceed():
    out = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "16.9",
         "--residual-spread-ms", "20.0", "--band-verdict", "HEALTHY",
         "--last-applied-offset-ms", "-283.0", "--proposed-offset-ms", "-285.0"],
        capture_output=True, text=True, check=True,
    ).stdout
    assert "hold_reason=\n" in out or out.strip().endswith("hold_reason=")


def test_cli_always_exits_zero_even_on_garbage():
    r = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "x",
         "--proposed-offset-ms", "y", "--band-verdict", "", "--last-applied-offset-ms", ""],
        capture_output=True, text=True,
    )
    assert r.returncode == 0
