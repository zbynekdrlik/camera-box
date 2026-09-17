"""issue 1333 (owner ruling 17.9.2026 „av tolerancia ma byt 30ms!!!") — pins the ONE A/V
tolerance = 30 ms across every python consumer of the A/V verdict, and pins that the Discord
report classifies the A/V gate from the verdict's OWN recorded gate_pass / gate_tolerance_ms,
never re-deriving pass from the tolerance.

Mechanical half (plan items 1 + 3) only. The crate-root Rust authority
(`AV_OFFSET_GATE_TOLERANCE_MS = 30.0`) is pinned by `src/av_window.rs`'s own unit test
`tolerance_is_owner_ruling_30ms_1333` (CI-run). Here we pin the python side:

  - the live offset ALERT default (`avsync_lineup.OFFSET_ALARM_MS_DEFAULT`, issue 1331) = 30,
  - the dev1 watchdog env default (`AVSYNC_LINEUP_OFFSET_ALARM_MS:-30` in
    scripts/avsync-lineup-alert-watchdog.sh) = 30,
  - the operator measurement default (`av_sync_measure.py --threshold-ms default`) = 30,
    read from SOURCE TEXT because importing av_sync_measure.py pulls torch/obs_phase2 (absent on
    dev1) — the same reason avsync_lineup.py cross-references, never imports, that value,
  - all three thresholds are EQUAL (one tolerance everywhere), and
  - e2e_discord_report.py renders the tolerance + gate glyph straight from the verdict block,
    so a HISTORICAL captured verdict (recorded gate_tolerance_ms=90.0) still reports ±90 with its
    OWN recorded gate_pass, while a synthetic 30.0 verdict reports ±30 — the report never
    re-derives pass from the tolerance.
"""
import pathlib
import re
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import avsync_lineup as al  # noqa: E402
import e2e_discord_report as edr  # noqa: E402

OWNER_RULING_TOLERANCE_MS = 30


def _measure_threshold_default():
    """The argparse default of av_sync_measure.py's --threshold-ms, read from source text
    (the module cannot be imported on dev1 — it pulls torch/obs_phase2)."""
    src = (_SCRIPTS / "av_sync_measure.py").read_text(encoding="utf-8")
    m = re.search(r'add_argument\(\s*"--threshold-ms"\s*,\s*type=int\s*,\s*default=(\d+)\s*\)', src)
    assert m, "could not find the --threshold-ms argparse default in av_sync_measure.py"
    return int(m.group(1))


def _watchdog_env_default():
    """The dev1 watchdog's env default: OFFSET_ALARM_MS="${AVSYNC_LINEUP_OFFSET_ALARM_MS:-NN}"."""
    src = (_SCRIPTS / "avsync-lineup-alert-watchdog.sh").read_text(encoding="utf-8")
    m = re.search(r'OFFSET_ALARM_MS="\$\{AVSYNC_LINEUP_OFFSET_ALARM_MS:-(\d+)\}"', src)
    assert m, "could not find the AVSYNC_LINEUP_OFFSET_ALARM_MS env default in the watchdog"
    return int(m.group(1))


# --- the single tolerance = 30 ms across every live-alert consumer -----------------------------


def test_lineup_offset_alarm_default_is_owner_ruling_30ms():
    assert al.OFFSET_ALARM_MS_DEFAULT == OWNER_RULING_TOLERANCE_MS


def test_measure_threshold_default_is_owner_ruling_30ms():
    assert _measure_threshold_default() == OWNER_RULING_TOLERANCE_MS


def test_watchdog_env_default_is_owner_ruling_30ms():
    assert _watchdog_env_default() == OWNER_RULING_TOLERANCE_MS


def test_all_three_live_thresholds_are_equal_one_tolerance_everywhere():
    # avsync_lineup.py cannot import av_sync_measure.py on dev1, so equality is pinned by the
    # source text — the design's explicit single-source contract for the live thresholds.
    assert (
        al.OFFSET_ALARM_MS_DEFAULT
        == _measure_threshold_default()
        == _watchdog_env_default()
        == OWNER_RULING_TOLERANCE_MS
    )


# --- the report classifies from the verdict's OWN gate_pass / gate_tolerance_ms ----------------


def _av_verdict(tolerance_ms, cam_offset_ms, cam_gate_pass, block_gate_pass):
    return {
        "all_cambox_av_sync": {
            "cam1": {
                "node": "cam1",
                "verdict": "measured",
                "av_offset_ms": cam_offset_ms,
                "mad_ms": 2.0,
                "candidates": 900,
                "cluster_samples": 120,
                "gate_pass": cam_gate_pass,
            },
            "gate_tolerance_ms": tolerance_ms,
            "gate_pass": block_gate_pass,
        }
    }


def test_report_shows_historical_90ms_verdict_with_its_own_recorded_pass_never_re_derived():
    # A historical captured verdict recorded gate_tolerance_ms=90.0 and gate_pass=True for a cam
    # whose +50ms offset would FAIL the new ±30 gate. The report must show the RECORDED ±90 and
    # the RECORDED pass glyph, never recompute pass from the tolerance.
    out = edr._section_av_sync(_av_verdict(90.0, 50.0, True, True))
    assert "±90.0ms" in out, out
    assert "±30.0ms" not in out, out
    # per-camera glyph is the node's recorded gate_pass (✅), not a re-derivation against ±30.
    assert "✅ cam1" in out, out
    assert "❌ cam1" not in out, out


def test_report_shows_synthetic_30ms_verdict_as_pm30():
    out = edr._section_av_sync(_av_verdict(30.0, 5.0, True, True))
    assert "±30.0ms" in out, out
    assert "±90.0ms" not in out, out


def test_report_fail_glyph_comes_from_recorded_gate_pass_false_not_tolerance():
    # Recorded gate_pass=False even though the cam offset (+2ms) is trivially inside ±30 — the
    # report must trust the recorded FAIL, proving it never re-derives pass from the tolerance.
    out = edr._section_av_sync(_av_verdict(30.0, 2.0, False, False))
    assert "❌ cam1" in out, out
    assert "±30.0ms" in out, out
