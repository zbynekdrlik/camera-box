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


def test_residual_ceiling_holds_even_with_a_healthy_band_finding6():
    # #1265 supervisor finding: the mbc ts_lag flap does NOT explain the residual -- a run AFTER the
    # stream-OBS restart, with a FLAT (HEALTHY) band, still measured residual -111.5ms (a real
    # oscillating upstream-audio-latency STEP). Condition 2 must HOLD REGARDLESS of the band, or that
    # real case walks the pin. Scoping condition 2 to a non-healthy band (a rejected review idea)
    # would let this straight through.
    r = g.hold_reason(residual_median_ms=-111.5, residual_spread_ms=27.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=None, proposed_offset_ms=-283.0)
    assert r != "", "a bad residual with a HEALTHY/flat band must still HOLD"
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


# ------------------------------------------------------------------ SUSTAINED two-run confirmation (supervisor 2026-09-02)
def test_sustained_step_proceeds_a_confirmed_real_offset():
    # a REAL upstream step confirmed by a 2nd consistent run: residual -111 now, prev -111 (fresh) ->
    # SUSTAINED -> conditions 2/3 STAND DOWN, the apply proceeds (the #856 +/-50 clamp bounds it).
    assert g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,
                         prev_residual_ms=-111.0, prev_residual_age_s=3600) == ""


def test_first_off_baseline_run_holds_outlier_protection():
    # first anomalous run (prev None) -> HOLD, awaiting a 2nd consistent run.
    r = g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,
                      prev_residual_ms=None, prev_residual_age_s=None)
    assert r != "" and "2nd consistent run" in r and "no prior run" in r


def test_disagreeing_prev_holds():
    # prev -60 vs now -111 differ by 51ms > 33 tol -> NOT sustained -> HOLD (an outlier/oscillation).
    r = g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,
                      prev_residual_ms=-60.0, prev_residual_age_s=3600)
    assert r != "" and "disagrees" in r


def test_stale_prev_holds():
    # prev agrees in value but is >24h old -> not a valid confirmation basis -> HOLD.
    r = g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,
                      prev_residual_ms=-111.0, prev_residual_age_s=90000)
    assert r != "" and "stale" in r


def test_band_drifting_holds_even_when_sustained():
    # condition 1 is independent of SUSTAINED: never tune during a flapping timeline, even a confirmed one.
    r = g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="DRIFTING",
                      last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,
                      prev_residual_ms=-111.0, prev_residual_age_s=3600)
    assert r != "" and ("DRIFTING" in r or "band" in r.lower())


def test_sustained_bypasses_the_jump_condition_too():
    # a confirmed step also stands down the jump condition (proposed swings far from last-applied).
    assert g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=-283.0, proposed_offset_ms=-111.0,  # 172ms jump
                         prev_residual_ms=-111.0, prev_residual_age_s=3600) == ""


def test_the_real_2026_09_01_series_converges_not_holds_forever():
    # -77 -> -126 -> -111 across three runs (the supervisor's real upstream step). The first two
    # disagree > tol (HOLD, outlier protection), the third agrees with the second within tol
    # (SUSTAINED -> PROCEED, so #856 applies instead of the rig staying ~90ms mis-aligned forever).
    common = dict(residual_spread_ms=25.0, band_verdict="HEALTHY",
                  last_applied_offset_ms=-283.0, proposed_offset_ms=-283.0)
    assert g.hold_reason(residual_median_ms=-77.0, prev_residual_ms=-20.0, prev_residual_age_s=3600, **common) != ""
    assert g.hold_reason(residual_median_ms=-126.0, prev_residual_ms=-77.0, prev_residual_age_s=3600, **common) != ""
    assert g.hold_reason(residual_median_ms=-111.0, prev_residual_ms=-126.0, prev_residual_age_s=3600, **common) == ""


def test_sustained_tol_is_configurable():
    # a 40ms run-to-run delta is sustained under a wider tol, held under the default.
    assert g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=None, proposed_offset_ms=-111.0,
                         prev_residual_ms=-71.0, prev_residual_age_s=3600, sustained_tol_ms=50.0) == ""
    assert g.hold_reason(residual_median_ms=-111.0, residual_spread_ms=25.0, band_verdict="HEALTHY",
                         last_applied_offset_ms=None, proposed_offset_ms=-111.0,
                         prev_residual_ms=-71.0, prev_residual_age_s=3600, sustained_tol_ms=33.0) != ""


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


def test_cli_decide_sustained_proceeds():
    # a confirmed step via the CLI (prev args) -> empty hold_reason (proceed).
    out = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "-111.0",
         "--residual-spread-ms", "25.0", "--band-verdict", "HEALTHY",
         "--last-applied-offset-ms", "-283.0", "--proposed-offset-ms", "-111.0",
         "--prev-residual-ms", "-111.0", "--prev-residual-age-s", "3600"],
        capture_output=True, text=True, check=True,
    ).stdout
    assert "hold_reason=\n" in out or out.strip().endswith("hold_reason=")


def test_cli_decide_first_offbaseline_holds():
    out = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "-111.0",
         "--band-verdict", "HEALTHY", "--proposed-offset-ms", "-111.0"],
        capture_output=True, text=True, check=True,
    ).stdout
    line = next(l for l in out.splitlines() if l.startswith("hold_reason="))
    assert line != "hold_reason=" and "2nd consistent run" in line


def test_cli_always_exits_zero_even_on_garbage():
    r = subprocess.run(
        [sys.executable, str(_GUARD), "decide", "--residual-median-ms", "x",
         "--proposed-offset-ms", "y", "--band-verdict", "", "--last-applied-offset-ms", ""],
        capture_output=True, text=True,
    )
    assert r.returncode == 0
