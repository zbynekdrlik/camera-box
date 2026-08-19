"""#1003 -- Tier-0 unit tests for the measurement-window equalization resolver
(scripts/e2e_measurement_pins.py). Pure functions only: no OBS WebSocket, no rig, no cargo.

Proves the design consult's "derive, don't hard-code" + coherence invariants, the
baseline-anchored leftover detection (the trap the 2026-08-19 revert hit), and the report-only
staleness decision -- AND that the checked-in scripts/e2e-measurement-pins.json is coherent."""
import json
import os
import sys

import pytest

_HERE = os.path.dirname(os.path.abspath(__file__))
_SCRIPTS = os.path.abspath(os.path.join(_HERE, "..", "..", "scripts"))
if _SCRIPTS not in sys.path:
    sys.path.insert(0, _SCRIPTS)

import e2e_measurement_pins as mp  # noqa: E402


def _profile():
    """The seeded 2026-08-19 measured inputs (a fresh dict per call -- tests never share state)."""
    return {
        "target_delivery_ms": 207,
        "min_deep_pin_ms": 80,
        "leftover_slack_ms": 40,
        "staleness_frames": 1.5,
        "av_expected_ms": 0,
        "cameras": {
            "NDI cam1": {"production_pin_ms": 3, "production_delivery_p50_ms": 120.0, "production_av_offset_ms": 95.2},
            "NDI cam2": {"production_pin_ms": 6, "production_delivery_p50_ms": 44.5, "production_av_offset_ms": 24.1},
            "NDI cam3": {"production_pin_ms": 20, "production_delivery_p50_ms": 42.6, "production_av_offset_ms": 15.4},
        },
        "stream": {"source": "NDI 2ME PGM", "production_hold_ms": 971},
    }


class TestResolvePins:
    def test_derives_the_delivery_equalized_deep_pins(self):
        # transports 117/38.5/22.6 -> pins target(207) - transport, all deep (>=80).
        assert mp.resolve_pins(_profile()) == {"NDI cam1": 90, "NDI cam2": 168, "NDI cam3": 184}

    def test_every_derived_pin_is_deep(self):
        pins = mp.resolve_pins(_profile())
        assert all(p >= 80 for p in pins.values())

    def test_pin_plus_transport_equalizes_delivery_to_target(self):
        prof = _profile()
        pins = mp.resolve_pins(prof)
        for src, cam in prof["cameras"].items():
            assert abs(pins[src] + mp.transport_ms(cam) - prof["target_delivery_ms"]) <= 1.0

    def test_a_faster_camera_gets_a_deeper_pin(self):
        # cam3 (fastest transport 22.6) must get the DEEPEST pin; cam1 (slowest 117) the shallowest.
        pins = mp.resolve_pins(_profile())
        assert pins["NDI cam3"] > pins["NDI cam2"] > pins["NDI cam1"]


class TestResolveHold:
    def test_rebalances_the_hold_to_re_zero_the_common_level(self):
        # audio_ref ~= 24.13, common level 207-24.13 ~= 182.87 -> hold 971-182.87 ~= 788.
        assert mp.resolve_hold(_profile()) == 788

    def test_hold_is_below_production_hold(self):
        prof = _profile()
        assert mp.resolve_hold(prof) < prof["stream"]["production_hold_ms"]


class TestResolveAvExpected:
    def test_is_the_profile_value_not_a_blind_zero(self):
        prof = _profile()
        prof["av_expected_ms"] = 12
        assert mp.resolve_av_expected(prof) == 12

    def test_a_nonzero_av_expected_shifts_the_hold_coherently(self):
        base = mp.resolve_hold(_profile())
        prof = _profile()
        prof["av_expected_ms"] = 20
        # wanting the common level to LAND at +20 instead of 0 means holding 20ms MORE.
        assert mp.resolve_hold(prof) == base + 20


class TestResolvePlan:
    def test_carries_production_references_for_leftover_detection(self):
        plan = mp.resolve_plan(_profile())
        assert plan["production"]["strih_pins"] == {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 20}
        assert plan["production"]["stream_hold_ms"] == 971
        assert plan["stream_source"] == "NDI 2ME PGM"
        assert plan["av_expected_ms"] == 0


class TestCoherenceCheck:
    def test_the_seeded_profile_is_coherent(self):
        assert mp.coherence_check(_profile()) == []

    def test_a_shallow_target_flags_a_non_deep_pin(self):
        prof = _profile()
        prof["target_delivery_ms"] = 130  # cam1 pin -> 13, below min_deep_pin_ms 80
        problems = mp.coherence_check(prof)
        assert any("not in the deep-phase regime" in p for p in problems)

    def test_inconsistent_per_camera_audio_refs_flag_the_av_triple(self):
        # The cameras share ONE physical audio path, so delivery - av_offset (the audio ref)
        # should be ~equal across cameras; no single hold can re-zero a wide spread. A profile
        # whose measured A/V offset for one camera disagrees with its delivery -> the A/V triple
        # (invariant #3) fires. Here cam1's av_offset is edited far off (audio ref 80 vs ~24).
        prof = _profile()
        prof["cameras"]["NDI cam1"]["production_av_offset_ms"] = 40.0
        problems = mp.coherence_check(prof)
        assert any("predicted equalized A/V" in p for p in problems)

    def test_av_triple_stays_coherent_for_any_av_expected(self):
        # resolve_hold shifts WITH av_expected, so a nonzero dialed expectation is still coherent
        # (the pins<->hold<->av_expected triple self-consistent) -- proving av_expected is a real
        # coherent input, not a blind constant.
        prof = _profile()
        prof["av_expected_ms"] = 20
        assert mp.coherence_check(prof) == []


class TestClassifyLeftover:
    def test_live_matching_production_is_snapshotted(self):
        assert mp.classify_leftover(3, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "snapshot"

    def test_live_equal_to_the_test_value_is_leftover(self):
        assert mp.classify_leftover(90, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "leftover-test"

    def test_live_far_from_production_is_leftover_even_if_not_the_test_value(self):
        # a prior run left it at 500 (neither prod 3 nor test 90) -> beyond slack -> leftover.
        assert mp.classify_leftover(500, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "leftover-test"

    def test_live_within_slack_of_production_is_snapshotted(self):
        assert mp.classify_leftover(35, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "snapshot"

    def test_unreadable_live_is_unknown(self):
        assert mp.classify_leftover(None, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "unknown"

    def test_the_stream_hold_leftover_789_is_caught_against_prod_971(self):
        # the exact 2026-08-19 revert incident: a leftover 789 must NOT be adopted as prod 971.
        assert mp.classify_leftover(789, production_ref_ms=971, test_value_ms=789, slack_ms=40) == "leftover-test"


class TestStalenessReport:
    def test_not_stale_when_observed_delivery_is_at_target(self):
        prof = _profile()
        observed = {"NDI cam1": 207.0, "NDI cam2": 205.0, "NDI cam3": 209.0}
        rep = mp.staleness_report(prof, observed, staleness_frames=1.5)
        assert rep["stale"] is False

    def test_stale_when_a_camera_drifts_beyond_the_frame_threshold(self):
        prof = _profile()
        # 1.5 frames ~= 50ms; cam1 at 270 is 63ms off target 207 -> stale.
        observed = {"NDI cam1": 270.0, "NDI cam2": 205.0, "NDI cam3": 209.0}
        rep = mp.staleness_report(prof, observed, staleness_frames=1.5)
        assert rep["stale"] is True
        assert rep["cameras"]["NDI cam1"]["stale"] is True
        assert rep["cameras"]["NDI cam2"]["stale"] is False

    def test_a_missing_camera_is_skipped_not_treated_as_stale(self):
        prof = _profile()
        observed = {"NDI cam1": 207.0}  # cam2/cam3 absent from a partial verdict
        rep = mp.staleness_report(prof, observed, staleness_frames=1.5)
        assert rep["stale"] is False
        assert set(rep["cameras"]) == {"NDI cam1"}

    def test_threshold_is_reported_in_ms(self):
        rep = mp.staleness_report(_profile(), {}, staleness_frames=1.5)
        assert rep["threshold_ms"] == pytest.approx(50.0, abs=0.1)


class TestLoadProfile:
    def test_loads_the_checked_in_config_and_it_is_coherent(self):
        path = os.path.join(_SCRIPTS, "e2e-measurement-pins.json")
        prof = mp.load_profile(path)
        assert mp.coherence_check(prof) == [], "the checked-in profile must always be coherent"
        assert mp.resolve_pins(prof) == {"NDI cam1": 90, "NDI cam2": 168, "NDI cam3": 184}
        assert mp.resolve_hold(prof) == 788

    def test_missing_file_fails_loud(self, tmp_path):
        with pytest.raises(SystemExit):
            mp.load_profile(str(tmp_path / "nope.json"))

    def test_malformed_json_fails_loud(self, tmp_path):
        p = tmp_path / "bad.json"
        p.write_text("{not json")
        with pytest.raises(SystemExit):
            mp.load_profile(str(p))

    def test_missing_required_key_fails_loud(self, tmp_path):
        prof = _profile()
        del prof["target_delivery_ms"]
        p = tmp_path / "partial.json"
        p.write_text(json.dumps(prof))
        with pytest.raises(SystemExit):
            mp.load_profile(str(p))

    def test_camera_missing_a_measured_field_fails_loud(self, tmp_path):
        prof = _profile()
        del prof["cameras"]["NDI cam2"]["production_delivery_p50_ms"]
        p = tmp_path / "partial.json"
        p.write_text(json.dumps(prof))
        with pytest.raises(SystemExit):
            mp.load_profile(str(p))
