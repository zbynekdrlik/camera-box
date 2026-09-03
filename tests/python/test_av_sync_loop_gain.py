"""#1265 -- unit tests for scripts/av_sync_loop_gain.py, the FIXED loop-gain damping term that
protects the #856 rig-wide A/V controller from oscillating against the measured plant gain ~2.31.

The controller applies `-residual` (loop gain 1); with the collinear plant gain 2.31 (STEP-0:
925->960 slope 2.314, 960->913 slope 2.314) the effective loop gain is 2.31 > 1 and the pin
oscillates with GROWING amplitude (|33.6| -> |47.4| -> |61.4|). This module damps the combined
offset by a fixed gain (default 0.4) BEFORE the existing +/-50 ms/run clamp and BEFORE the #1265
apply guard sees `proposed_offset`, so the effective loop gain is 0.4*2.31 = 0.92 < 1 and it
converges. Pure math, so fully Tier-0 testable off-rig.

Covers:
  a. damped_offset() -- the gain multiply, sign-preserving.
  b. resolve_gain() -- reads AV_SYNC_LOOP_GAIN; validates non-numeric / <=0 / >1 -> default 0.4
     with a loud stderr line; a valid (0, 1] value is used verbatim; unset/empty -> default silently.
  c. the gain * +/-50 clamp INTERACTION + the .616 expected outcome (pin 913, residual ~-61.4,
     gain 0.4 -> damped -24.54 -> required_delay_ms(913, -24.54) == 938 ~ the predicted null 940).
  d. the `damp` CLI -- prints `<damped>\t<gain>` on stdout, the warning on stderr, ALWAYS exits 0.
"""
import os
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_loop_gain  # noqa: E402
import av_sync_calibrate  # noqa: E402


# ---------------------------------------------------------------------------
# (a) damped_offset -- the gain multiply
# ---------------------------------------------------------------------------

class TestDampedOffset:
    def test_scales_a_negative_offset(self):
        assert av_sync_loop_gain.damped_offset(-61.354, 0.4) == pytest.approx(-24.5416)

    def test_scales_a_positive_offset(self):
        assert av_sync_loop_gain.damped_offset(47.41, 0.4) == pytest.approx(18.964)

    def test_gain_one_is_identity(self):
        assert av_sync_loop_gain.damped_offset(-61.354, 1.0) == pytest.approx(-61.354)

    def test_zero_offset_stays_zero(self):
        assert av_sync_loop_gain.damped_offset(0.0, 0.4) == 0.0


# ---------------------------------------------------------------------------
# (b) resolve_gain -- env read + validation
# ---------------------------------------------------------------------------

class TestResolveGain:
    def test_default_when_unset(self):
        assert av_sync_loop_gain.resolve_gain({}) == 0.4

    def test_default_is_04(self):
        assert av_sync_loop_gain.DEFAULT_LOOP_GAIN == 0.4

    def test_empty_string_is_default_silently(self, capsys):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": ""}) == 0.4
        assert capsys.readouterr().err == ""

    def test_valid_value_is_used_verbatim(self):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "0.7"}) == pytest.approx(0.7)

    def test_upper_boundary_one_is_valid(self):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "1.0"}) == pytest.approx(1.0)

    def test_non_numeric_falls_back_with_loud_line(self, capsys):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "abc"}) == 0.4
        err = capsys.readouterr().err
        assert "AV_SYNC_LOOP_GAIN" in err and "0.4" in err

    def test_zero_falls_back_with_loud_line(self, capsys):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "0"}) == 0.4
        assert "AV_SYNC_LOOP_GAIN" in capsys.readouterr().err

    def test_negative_falls_back_with_loud_line(self, capsys):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "-0.5"}) == 0.4
        assert "AV_SYNC_LOOP_GAIN" in capsys.readouterr().err

    def test_above_one_falls_back_with_loud_line(self, capsys):
        assert av_sync_loop_gain.resolve_gain({"AV_SYNC_LOOP_GAIN": "1.5"}) == 0.4
        assert "AV_SYNC_LOOP_GAIN" in capsys.readouterr().err

    def test_defaults_to_real_environ_when_env_none(self, monkeypatch):
        monkeypatch.delenv("AV_SYNC_LOOP_GAIN", raising=False)
        assert av_sync_loop_gain.resolve_gain() == 0.4
        monkeypatch.setenv("AV_SYNC_LOOP_GAIN", "0.6")
        assert av_sync_loop_gain.resolve_gain() == pytest.approx(0.6)


# ---------------------------------------------------------------------------
# (c) gain * +/-50 clamp interaction + the .616 predicted-null scenario
# ---------------------------------------------------------------------------

class TestGainClampInteraction:
    def test_616_scenario_pin_913_residual_minus61_lands_at_938(self):
        # The supervisor's prediction (comment 5517960166): at pin 913 with residual ~-61.4 and
        # gain 0.4, the damped offset is ~-24.54 and the controller moves the pin to ~938 (the
        # predicted null ~940). Encodes the gain -> damped -> +/-50 clamp -> pin pipeline end to end.
        combined = -61.354
        damped = av_sync_loop_gain.damped_offset(combined, 0.4)
        assert damped == pytest.approx(-24.5416, abs=1e-3)
        # required_delay_ms(913, -24.54): raw round(913 + 24.54) = 938, inside +/-50 and [3, 2000].
        assert av_sync_calibrate.required_delay_ms(913, damped) == 938

    def test_undamped_gain1_would_overshoot_past_the_step_clamp(self):
        # Without damping (gain 1) the raw +61.4 correction clamps to the +50 ceiling (pin 963),
        # keeping the oscillation alive -- the whole reason the gain exists.
        assert av_sync_calibrate.required_delay_ms(913, -61.354) == 963  # 913 + 50 (step-clamped)

    def test_damped_correction_stays_within_the_step_clamp(self):
        # 0.4 * -61.4 = -24.5 -> a 24.5ms move, comfortably inside the +/-50/run clamp (no clamp).
        damped = av_sync_loop_gain.damped_offset(-61.354, 0.4)
        assert abs(damped) < av_sync_calibrate.AV_SYNC_MAX_STEP_MS

    def test_large_combined_still_clamped_by_the_step_after_damping(self):
        # A huge combined (+/-200) damped to +/-80 still hits the +/-50/run clamp -- gain reduces
        # but does not replace the clamp (belt and suspenders).
        damped = av_sync_loop_gain.damped_offset(-200.0, 0.4)  # -80
        assert av_sync_calibrate.required_delay_ms(1000, damped) == 1050  # 1000 + 50 (step-clamped)


# ---------------------------------------------------------------------------
# (d) the `damp` CLI -- <damped>\t<gain> on stdout, warning on stderr, always exit 0
# ---------------------------------------------------------------------------

class TestDampCli:
    def test_prints_damped_and_gain_tab_separated(self, monkeypatch, capsys):
        monkeypatch.delenv("AV_SYNC_LOOP_GAIN", raising=False)
        rc = av_sync_loop_gain.main(["damp", "--combined-ms", "-61.354"])
        assert rc == 0
        out = capsys.readouterr().out.strip()
        damped_str, gain_str = out.split("\t")
        assert float(damped_str) == pytest.approx(-24.5416, abs=1e-3)
        assert float(gain_str) == pytest.approx(0.4)

    def test_env_override_flows_into_the_cli(self, monkeypatch, capsys):
        monkeypatch.setenv("AV_SYNC_LOOP_GAIN", "0.5")
        av_sync_loop_gain.main(["damp", "--combined-ms", "-61.354"])
        out = capsys.readouterr().out.strip()
        damped_str, gain_str = out.split("\t")
        assert float(gain_str) == pytest.approx(0.5)
        assert float(damped_str) == pytest.approx(-30.677, abs=1e-3)

    def test_invalid_env_falls_back_and_warns_but_still_exits_zero(self, monkeypatch, capsys):
        monkeypatch.setenv("AV_SYNC_LOOP_GAIN", "nonsense")
        rc = av_sync_loop_gain.main(["damp", "--combined-ms", "-61.354"])
        assert rc == 0
        cap = capsys.readouterr()
        gain_str = cap.out.strip().split("\t")[1]
        assert float(gain_str) == pytest.approx(0.4)
        assert "AV_SYNC_LOOP_GAIN" in cap.err

    def test_unparseable_combined_never_crashes_and_exits_zero(self, monkeypatch, capsys):
        # A guard-safe helper embedded in the cleanup EXIT trap must never abort the run: a garbage
        # combined value prints an EMPTY damped (so the caller skips the apply) and still exits 0.
        monkeypatch.delenv("AV_SYNC_LOOP_GAIN", raising=False)
        rc = av_sync_loop_gain.main(["damp", "--combined-ms", "not-a-number"])
        assert rc == 0
        out = capsys.readouterr().out
        # first tab field (damped) is empty -> the caller's [ -n ... ] skips the apply
        assert out.split("\t")[0] == ""
