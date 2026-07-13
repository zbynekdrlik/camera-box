"""#738 — unit tests for the pure calibration math in scripts/obs_colour_correction_calibrate.py
(chroma-cast metric, grey-world gain computation, color_multiply int packing). No OBS WebSocket
is opened here -- pure functions only, matching this repo's established pure-module test pattern.
"""
import sys
from pathlib import Path

_SCRIPTS = Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import obs_colour_correction_calibrate as occ  # noqa: E402


# ---------------------------------------------------------------------------
# chroma_cast_bt601
# ---------------------------------------------------------------------------

class TestChromaCastBt601:
    def test_neutral_grey_has_zero_cast(self):
        cb, cr, mag = occ.chroma_cast_bt601(100.0, 100.0, 100.0)
        assert abs(cb) < 1e-9
        assert abs(cr) < 1e-9
        assert mag == 0.0

    def test_real_measured_cam1_cast_matches_the_live_rig_reading(self):
        # Real numbers sampled live on strih, 2026-07-13 (NDI cam5 input -> physical CAM1,
        # Elgato 4K S, V4L2 correction already applied): meanRGB=(69.21, 43.20, 62.76). cb/cr are
        # linear in the channel means so they match the live per-pixel-averaged reading exactly;
        # `mag` here is computed FROM the mean triple (magnitude-of-the-mean-cast, what the
        # grey-world calibration actually acts on) -- NOT the same number as a per-pixel
        # magnitude averaged over the frame (that reading, 13.59, differs because sqrt(x^2+y^2)
        # is not linear, so mean-of-magnitudes != magnitude-of-means whenever the cast varies
        # across the frame).
        cb, cr, mag = occ.chroma_cast_bt601(69.21413194, 43.19590278, 62.76480903)
        assert abs(cb - 5.39) < 0.1
        assert abs(cr - 11.42) < 0.1
        assert abs(mag - 12.63) < 0.1

    def test_pure_red_casts_toward_positive_cr(self):
        _cb, cr, _mag = occ.chroma_cast_bt601(255.0, 0.0, 0.0)
        assert cr > 0

    def test_pure_blue_casts_toward_positive_cb(self):
        cb, _cr, _mag = occ.chroma_cast_bt601(0.0, 0.0, 255.0)
        assert cb > 0


# ---------------------------------------------------------------------------
# grey_world_gains
# ---------------------------------------------------------------------------

class TestGreyWorldGains:
    def test_neutral_input_yields_unity_gains(self):
        assert occ.grey_world_gains(100.0, 100.0, 100.0, damping=1.0) == (1.0, 1.0, 1.0)

    def test_zero_damping_is_always_a_no_op(self):
        assert occ.grey_world_gains(200.0, 50.0, 150.0, damping=0.0) == (1.0, 1.0, 1.0)

    def test_full_damping_fully_neutralizes_the_cast_anchored_on_the_min_channel(self):
        # mean_r=150, mean_g=100, mean_b=125 -> target=min(150,100,125)=100 (the G channel, so
        # its own gain is exactly 1.0 -- a no-op, never a boost). Every channel's ideal gain
        # (0.667/1.0/0.8) stays inside [GAIN_MIN, GAIN_MAX] so the clamp never interferes --
        # applying the full correction must land every channel on the SAME (darkest) target.
        gains = occ.grey_world_gains(150.0, 100.0, 125.0, damping=1.0)
        assert gains[1] == 1.0, "the min channel's own gain must be a no-op, never a boost"
        corrected = (150.0 * gains[0], 100.0 * gains[1], 125.0 * gains[2])
        target = 100.0
        for c in corrected:
            assert abs(c - target) < 1e-6

    def test_gain_never_exceeds_one_since_color_multiply_cannot_boost(self):
        # #738 GOTCHA (live incident, 2026-07-13): anchoring on the MEAN would routinely need a
        # gain > 1.0 on a below-average channel -- pack_color_multiply silently clamps that to a
        # no-op (byte 255 = gain 1.0), so a "boost" channel never actually moves. Anchoring on
        # the min channel guarantees every gain stays <= 1.0 -- representable, never a silent
        # no-op -- across a range of asymmetric inputs, not just one hand-picked triple.
        for mean in [
            (150.0, 100.0, 125.0),
            (69.44, 43.63, 64.31),  # the real live cam1 reading (round 0)
            (200.0, 60.0, 90.0),
            (10.0, 250.0, 40.0),
        ]:
            gains = occ.grey_world_gains(*mean, damping=1.0)
            assert all(g <= 1.0 + 1e-9 for g in gains), f"{mean} -> {gains}"

    def test_partial_damping_is_between_identity_and_full_correction(self):
        full = occ.grey_world_gains(150.0, 100.0, 125.0, damping=1.0)
        half = occ.grey_world_gains(150.0, 100.0, 125.0, damping=0.5)
        for g_full, g_half in zip(full, half):
            # halfway between 1.0 (no-op) and the full-damping gain.
            assert abs(g_half - (1.0 + 0.5 * (g_full - 1.0))) < 1e-9

    def test_gains_are_clamped_to_a_safe_range(self):
        # An extreme cast (one channel far dimmer than the others) would otherwise demand an
        # extreme gain on the OTHER channels to match it -- must clamp.
        gains = occ.grey_world_gains(200.0, 1.0, 200.0, damping=1.0)
        assert all(occ.GAIN_MIN <= g <= occ.GAIN_MAX for g in gains)

    def test_degenerate_all_channels_zero_returns_identity(self):
        assert occ.grey_world_gains(0.0, 0.0, 0.0) == (1.0, 1.0, 1.0)

    def test_degenerate_one_channel_zero_returns_identity(self):
        # A fully black channel (no real signal) must never trigger a division by zero or an
        # extreme gain -- the whole reading is unreliable, so no-op.
        assert occ.grey_world_gains(50.0, 0.0, 20.0) == (1.0, 1.0, 1.0)

    def test_real_cam1_reading_produces_gains_that_reduce_the_cast(self):
        # The SAME live reading as the chroma-cast test above -- applying the computed gains
        # (at the calibration script's own default damping=0.6) must measurably shrink the cast
        # magnitude versus doing nothing.
        mean = (69.21413194, 43.19590278, 62.76480903)
        gains = occ.grey_world_gains(*mean, damping=0.6)
        corrected = tuple(mean[i] * gains[i] for i in range(3))
        _cb0, _cr0, mag_before = occ.chroma_cast_bt601(*mean)
        _cb1, _cr1, mag_after = occ.chroma_cast_bt601(*corrected)
        assert mag_after < mag_before


# ---------------------------------------------------------------------------
# compose_gains
# ---------------------------------------------------------------------------

class TestComposeGains:
    def test_composing_with_identity_is_a_no_op(self):
        assert occ.compose_gains((0.8, 1.2, 0.9), (1.0, 1.0, 1.0)) == (0.8, 1.2, 0.9)

    def test_composes_by_elementwise_multiplication(self):
        composed = occ.compose_gains((0.8, 1.0, 1.2), (0.9, 1.1, 0.8))
        assert abs(composed[0] - 0.72) < 1e-9
        assert abs(composed[1] - 1.1) < 1e-9
        assert abs(composed[2] - 0.96) < 1e-9

    def test_composed_result_is_clamped(self):
        composed = occ.compose_gains((1.8, 1.0, 0.4), (1.8, 1.0, 0.4))
        assert composed[0] == occ.GAIN_MAX  # 3.24 clamps down
        assert composed[2] == occ.GAIN_MIN  # 0.16 clamps up


# ---------------------------------------------------------------------------
# pack_color_multiply / unpack_color_multiply
# ---------------------------------------------------------------------------

class TestColorMultiplyPacking:
    def test_identity_gains_pack_to_the_live_confirmed_obs_default(self):
        assert occ.pack_color_multiply(1.0, 1.0, 1.0) == occ.IDENTITY_COLOR_MULTIPLY

    def test_round_trips_through_pack_and_unpack(self):
        packed = occ.pack_color_multiply(0.8, 1.0, 0.6)
        r, g, b = occ.unpack_color_multiply(packed)
        assert abs(r - 0.8) < 1 / 255
        assert abs(g - 1.0) < 1 / 255
        assert abs(b - 0.6) < 1 / 255

    def test_matches_the_live_round_trip_confirmed_on_strih_2026_07_13(self):
        # Live-confirmed byte layout: SetSourceFilterSettings(color_multiply=0x0099FFCC) ->
        # GetSourceFilter returned literally 10092492 (== 0x0099FFCC) -- packing
        # gain_r=204/255, gain_g=255/255, gain_b=153/255 must reproduce that exact int.
        packed = occ.pack_color_multiply(204 / 255, 255 / 255, 153 / 255)
        assert packed == 10092492

    def test_out_of_range_gains_clamp_to_valid_bytes(self):
        packed = occ.pack_color_multiply(-1.0, 5.0, 1.0)
        r, g, b = occ.unpack_color_multiply(packed)
        assert r == 0.0
        assert g == 1.0  # 5.0 * 255 clamps to byte 255 -> unpacked 1.0
        assert abs(b - 1.0) < 1e-9

    def test_alpha_byte_is_always_zero_matching_the_observed_obs_default(self):
        packed = occ.pack_color_multiply(1.0, 1.0, 1.0)
        assert (packed >> 24) & 0xFF == 0
