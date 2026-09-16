"""#1320 — unit tests for the PURE dev1 render-freeze / relock-storm pager decision core
(scripts/render_freeze_decision.py).

The watchdog reads two bundle-state facets off each box's :8899 and pages on a RECURRENCE of the
issue-1320 defect (a scene-switch reattach freezing the PROGRAM render, which starves the 2ME PGM
NDI output → a receiver relock storm):

  * program_render_lagged (+ _age_s)  — a `lagged>0` window == a PROGRAM render-thread freeze. A
    RELAUNCH legitimately lags a little (observed relaunch band prl 1/2/11 vs the 228 real freeze),
    so the RENDER_FREEZE arm gates on a MAGNITUDE FLOOR (default 30, above the relaunch band, below
    the smallest genuine freeze) AND a freshness bound (a freeze that scrolled deep into the tail /
    an old relaunch lag is stale, no page).
  * relock_bursts (+ _age_s)  — issue 1318's summarize_relock_bursts: ≥8 relocks within 1 s on an
    input == a FIFO overshoot storm. The RELOCK_STORM arm pages on bursts>=1 fresh.

No I/O — exhaustively pytest-able under Tier-0 (which kills cargo). The audio_lag_decision.py /
ndi_halving_decision.py #1199 python-mirror precedent.
"""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import render_freeze_decision as rfd  # noqa: E402


def _bundle(**facets):
    """A /bundle-state.json body carrying exactly the named facets (omit-when-absent modelled by
    simply not passing the key)."""
    return json.dumps(facets)


# ── RENDER arm ────────────────────────────────────────────────────────────────────────────────
def test_fresh_severe_lagged_is_render_freeze():
    # The 18:27 freeze signature: lagged=228, age 5 s (recent). Above the 30 floor, fresh → page.
    v = rfd.classify_render(228, 5, box_reachable=1)
    assert v == "RENDER_FREEZE", v


def test_relaunch_window_small_lagged_is_healthy():
    # A relaunch startup-lag (prl 1/2/11) is BELOW the magnitude floor → not a freeze, never a page.
    for lagged in (1, 2, 11, 29):
        assert rfd.classify_render(lagged, 5, box_reachable=1) == "HEALTHY", lagged


def test_stale_severe_lagged_is_healthy():
    # stream's live prl=2 age=4464 s, and a 228 freeze that scrolled deep into the tail — both are
    # older than the freshness bound → stale → no page (it is not a RECENT freeze).
    assert rfd.classify_render(2, 4464, box_reachable=1) == "HEALTHY"
    assert rfd.classify_render(228, 4464, box_reachable=1) == "HEALTHY"


def test_healthy_zero_lagged_is_healthy():
    # "render telemetry live, no freeze" — lagged=0 present, fresh.
    assert rfd.classify_render(0, 4, box_reachable=1) == "HEALTHY"


def test_render_absent_facet_is_unknown():
    # No program_render_lagged facet (a stock OBS / no audit line yet) → no reading to judge.
    assert rfd.classify_render(None, None, box_reachable=1) == "UNKNOWN"


def test_render_unreachable_is_skip():
    # :8899 not fetchable → SKIP (deferred to issue 732 / issue 1001), never a render page.
    assert rfd.classify_render(228, 5, box_reachable=0) == "SKIP"


# ── RELOCK arm ────────────────────────────────────────────────────────────────────────────────
def test_fresh_burst_is_relock_storm():
    assert rfd.classify_relock(1, 8, box_reachable=1) == "RELOCK_STORM"
    assert rfd.classify_relock(2, 8, box_reachable=1) == "RELOCK_STORM"


def test_zero_bursts_present_is_healthy():
    # relock lines present but no cluster reached the ≥8-in-1s threshold → relocks live, no storm.
    assert rfd.classify_relock(0, 8, box_reachable=1) == "HEALTHY"


def test_stale_burst_is_healthy():
    # A storm that aged out of the freshness window → no re-page.
    assert rfd.classify_relock(1, 4464, box_reachable=1) == "HEALTHY"


def test_relock_absent_facet_is_unknown():
    # No relock_bursts facet at all (steady state — relock lines only appear during a storm).
    assert rfd.classify_relock(None, None, box_reachable=1) == "UNKNOWN"


def test_relock_unreachable_is_skip():
    assert rfd.classify_relock(1, 8, box_reachable=0) == "SKIP"


# ── analyze (both arms from one fetched body) ───────────────────────────────────────────────────
def test_analyze_render_freeze_and_relock_storm_together():
    body = _bundle(program_render_lagged="228", program_render_lagged_age_s="5",
                   relock_bursts="1", relock_bursts_age_s="8")
    out = rfd.analyze(body, box_reachable=1)
    assert out["render_verdict"] == "RENDER_FREEZE"
    assert out["lagged"] == 228 and out["lagged_age_s"] == 5
    assert out["relock_verdict"] == "RELOCK_STORM"
    assert out["bursts"] == 1 and out["bursts_age_s"] == 8


def test_analyze_absent_facets_are_unknown():
    out = rfd.analyze(_bundle(), box_reachable=1)
    assert out["render_verdict"] == "UNKNOWN"
    assert out["relock_verdict"] == "UNKNOWN"


def test_analyze_unreachable_skips_without_parsing():
    out = rfd.analyze("", box_reachable=0)
    assert out["render_verdict"] == "SKIP"
    assert out["relock_verdict"] == "SKIP"
    assert out["lagged"] is None and out["bursts"] is None


def test_analyze_healthy_live_box():
    # The current live strih reading: prl=0 fresh, no relock facet.
    out = rfd.analyze(_bundle(program_render_lagged="0", program_render_lagged_age_s="4"),
                      box_reachable=1)
    assert out["render_verdict"] == "HEALTHY"
    assert out["relock_verdict"] == "UNKNOWN"  # no relock_bursts facet


def test_analyze_tolerates_non_json_body():
    out = rfd.analyze("not json at all", box_reachable=1)
    assert out["render_verdict"] == "UNKNOWN"
    assert out["relock_verdict"] == "UNKNOWN"
