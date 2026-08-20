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
    def test_derives_the_delivery_equalized_deep_then_phase_snapped_pins(self):
        # transports 117/38.5/22.6 -> equalized 90/168.5/184.4; cam2 168 (frac 0.04) is
        # limit-cycle-prone -> phase-snapped to 160 (frac 0.80). cam1 90 (0.70) + cam3 184 (0.52)
        # are already phase-safe and unchanged.
        assert mp.resolve_pins(_profile()) == {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}

    def test_every_derived_pin_is_deep(self):
        pins = mp.resolve_pins(_profile())
        assert all(p >= 80 for p in pins.values())

    def test_the_UNsnapped_equalized_pin_exactly_equalizes_delivery(self):
        # the PRIMARY objective, before the phase snap: equalized_pin + transport == target exactly.
        prof = _profile()
        for cam in prof["cameras"].values():
            assert abs(mp.equalized_pin_ms(prof, cam) + mp.transport_ms(cam)
                       - prof["target_delivery_ms"]) < 1e-9

    def test_every_resolved_pin_is_phase_safe(self):
        # the whole point: no shipped pin may sit in the limit-cycle-prone band (frac < 0.5).
        pins = mp.resolve_pins(_profile())
        assert all(not mp._phase_is_prone(p) for p in pins.values())

    def test_a_faster_camera_gets_a_deeper_pin(self):
        # cam3 (fastest transport 22.6) must get the DEEPEST pin; cam1 (slowest 117) the shallowest.
        pins = mp.resolve_pins(_profile())
        assert pins["NDI cam3"] > pins["NDI cam2"] > pins["NDI cam1"]


class TestPhaseSnap:
    def test_snaps_a_prone_pin_to_the_nearest_safe_lower_value(self):
        # equalized 168.5 -> round 168 (frac 0.04, prone) -> nearest safe centre-band value is 160
        # (frac 0.80, dist 8), preferred over 176 (frac 0.28, would be prone anyway) — lower wins.
        assert mp.phase_snap_pin(168.5) == 160
        assert mp.PHASE_SAFE_LO_FRAC <= mp._phase_frac(160) <= mp.PHASE_SAFE_HI_FRAC

    def test_leaves_an_already_phase_safe_pin_alone(self):
        assert mp.phase_snap_pin(90.0) == 90       # frac 0.70, safe
        assert mp.phase_snap_pin(184.4) == 184     # frac 0.52, safe (>= 0.5, round-up overshoot)

    def test_a_prone_pin_is_moved_out_of_the_band(self):
        # 200 -> frac(200/33.333)=0.0 (prone). Must land phase-safe.
        snapped = mp.phase_snap_pin(200.0)
        assert not mp._phase_is_prone(snapped)
        assert abs(snapped - 200) <= mp.PHASE_SNAP_MAX_COST_MS


class TestResolveHold:
    def test_rebalances_the_hold_to_re_zero_the_MEAN_snapped_delivery(self):
        # snapped deliveries 207/198.5/206.6 -> mean 204.03; audio_ref ~= 24.13; common level
        # 179.9 -> hold 971-179.9 ~= 791 (the mean-snapped-delivery centring, not target 207).
        assert mp.resolve_hold(_profile()) == 791

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

    def test_flags_a_prone_pin_that_cannot_be_snapped_within_budget(self, monkeypatch):
        # shrink the snap budget to 0 so a prone equalized pin (cam2 168, frac 0.04) has NO safe pin
        # in range -> phase_snap returns it still-prone -> the phase-safety invariant FIRES (a
        # genuine "cannot equalize AND phase-fix within budget" state, never silently shipped).
        monkeypatch.setattr(mp, "PHASE_SNAP_MAX_COST_MS", 0)
        problems = mp.coherence_check(_profile())
        assert any("limit-cycle-prone" in p for p in problems)


class TestClassifyLeftover:
    def test_live_matching_production_is_snapshotted(self):
        assert mp.classify_leftover(3, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "snapshot"

    def test_live_equal_to_the_test_value_is_leftover(self):
        assert mp.classify_leftover(90, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "leftover-test"

    def test_live_far_from_production_and_not_the_test_value_is_stale(self):
        # a prior run left it at 500 (neither prod 3 nor test 90) -> beyond slack, NOT the test
        # value -> STALE (the caller must fail loud, never auto-write a constant over the live rig).
        assert mp.classify_leftover(500, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "stale"

    def test_a_legitimate_operator_retune_of_the_stream_hold_is_stale_not_stomped(self):
        # the 2026-08-19 revert class: prod hold legitimately re-tuned to 915 (its drift baseline),
        # beyond slack 40 of the profile's 971 ref, NOT the test value 788 -> STALE -> fail loud,
        # never silently restore 971 over it.
        assert mp.classify_leftover(915, production_ref_ms=971, test_value_ms=788, slack_ms=40) == "stale"

    def test_live_within_slack_of_production_is_snapshotted(self):
        assert mp.classify_leftover(35, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "snapshot"

    def test_unreadable_live_is_unknown(self):
        assert mp.classify_leftover(None, production_ref_ms=3, test_value_ms=90, slack_ms=40) == "unknown"

    def test_the_stream_hold_leftover_789_is_caught_against_prod_971(self):
        # the exact 2026-08-19 revert incident: a leftover 789 must NOT be adopted as prod 971.
        assert mp.classify_leftover(789, production_ref_ms=971, test_value_ms=789, slack_ms=40) == "leftover-test"


class TestStalenessReport:
    def test_not_stale_when_observed_delivery_is_near_the_per_camera_snapped_expected(self):
        # expected (snapped) deliveries: cam1 207, cam2 198.5, cam3 206.6. Observed near each.
        prof = _profile()
        observed = {"NDI cam1": 207.0, "NDI cam2": 200.0, "NDI cam3": 205.0}
        rep = mp.staleness_report(prof, observed, staleness_frames=1.5)
        assert rep["stale"] is False
        assert rep["cameras"]["NDI cam2"]["expected_ms"] == 198.5  # NOT the 207 target

    def test_stale_when_a_camera_drifts_beyond_the_frame_threshold(self):
        prof = _profile()
        # 1.5 frames ~= 50ms; cam1 at 270 is 63ms off its expected 207 -> stale.
        observed = {"NDI cam1": 270.0, "NDI cam2": 200.0, "NDI cam3": 205.0}
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
        assert mp.resolve_pins(prof) == {"NDI cam1": 90, "NDI cam2": 160, "NDI cam3": 184}
        assert mp.resolve_hold(prof) == 791

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


# --------------------------------------------------------------------------- #
# #1124 -- report-only diagnostics wired on top of the #1003 profile: the
# staleness-from-verdict key mapping (item 1) and the edge-oscillation FIFO
# classifier (item 2). Both PURE + Tier-0; the harness wiring lives in
# scripts/lib/measurement-eq.sh + recording-e2e.sh.
# --------------------------------------------------------------------------- #

class TestObservedDeliveryFromVerdict:
    """item 1: map the verdict's all_cambox_delivery_latency (keys cam1/cam2/...,
    each {p50_ms} or null) onto the profile's camera keys (NDI cam1/...) so
    staleness_report can consume it directly."""

    def _verdict(self, cam1=169.8, cam2=None, cam3=175.6):
        cams = {}
        for name, p50 in (("cam1", cam1), ("cam2", cam2), ("cam3", cam3)):
            cams[name] = None if p50 is None else {"p50_ms": p50, "mean_ms": p50}
        return {"all_cambox_delivery_latency": cams}

    def test_maps_camN_to_profile_NDI_camN_keys(self):
        obs = mp.observed_delivery_from_verdict(self._verdict(), _profile())
        assert obs == {"NDI cam1": 169.8, "NDI cam3": 175.6}  # cam2 null -> skipped

    def test_a_null_camera_is_skipped_not_zeroed(self):
        obs = mp.observed_delivery_from_verdict(self._verdict(cam2=None), _profile())
        assert "NDI cam2" not in obs

    def test_a_camera_absent_from_the_verdict_block_is_skipped(self):
        v = {"all_cambox_delivery_latency": {"cam1": {"p50_ms": 200.0}}}
        obs = mp.observed_delivery_from_verdict(v, _profile())
        assert obs == {"NDI cam1": 200.0}

    def test_a_camera_not_in_the_profile_is_ignored(self):
        # cam7 delivers but the profile only covers cam1..cam3 -> never appears.
        v = {"all_cambox_delivery_latency": {"cam1": {"p50_ms": 200.0}, "cam7": {"p50_ms": 50.0}}}
        obs = mp.observed_delivery_from_verdict(v, _profile())
        assert set(obs) == {"NDI cam1"}

    def test_a_missing_delivery_block_yields_empty_not_error(self):
        assert mp.observed_delivery_from_verdict({}, _profile()) == {}
        assert mp.observed_delivery_from_verdict({"all_cambox_delivery_latency": None}, _profile()) == {}

    def test_non_numeric_scalar_summary_keys_are_ignored(self):
        # the block carries cross_camera_spread_ms / gates_overall_pass scalars alongside cams.
        v = {"all_cambox_delivery_latency": {
            "cam1": {"p50_ms": 210.0}, "cross_camera_spread_ms": 5.7, "gates_overall_pass": True}}
        obs = mp.observed_delivery_from_verdict(v, _profile())
        assert obs == {"NDI cam1": 210.0}

    def test_feeds_staleness_report_end_to_end(self):
        # the whole point of item 1: build observed from the verdict, then judge staleness.
        v = self._verdict(cam1=270.0, cam2=None, cam3=205.0)  # cam1 63ms off its 207 expected
        obs = mp.observed_delivery_from_verdict(v, _profile())
        rep = mp.staleness_report(_profile(), obs, staleness_frames=1.5)
        assert rep["stale"] is True
        assert rep["cameras"]["NDI cam1"]["stale"] is True


class TestEdgeOscillationReport:
    """item 2: detect the uniform copies-approx-gaps FIFO limit-cycle signature per cambox
    from the verdict's all_cambox_continuity.segments. DATA-CALIBRATED from the 19 local
    verdict JSONs: only the genuine FIFO run (1804432786 CAM2) is a suspect."""

    def _verdict(self, segs):
        return {"all_cambox_continuity": {"segments": [
            {"cambox": cb, "copies": c, "gaps": g} for (cb, c, g) in segs]}}

    def test_the_live_fifo_validation_signature_is_a_suspect(self):
        # verdict 1804432786: CAM2 pin 168 (frac 0.04) churned 5/4, 7/7, 5/4 per segment.
        v = self._verdict([
            ("CAM1", 0, 0), ("CAM2", 5, 4), ("CAM3", 0, 0),
            ("CAM1", 1, 3), ("CAM2", 7, 7), ("CAM3", 0, 0),
            ("CAM1", 2, 2), ("CAM2", 5, 4), ("CAM3", 0, 0),
            ("CAM1", 1, 0)])
        rep = mp.edge_oscillation_report(v)
        assert rep["suspect"] is True
        assert rep["camboxes"]["CAM2"]["suspect"] is True
        assert rep["camboxes"]["CAM2"]["oscillating_windows"] == 3
        assert "CAM2" in rep["suspect_camboxes"]

    def test_the_post_snap_healthy_meq_run_is_not_a_suspect(self):
        # verdict 66065064 (spread 5.78, post cam2 168->160 snap): the churn is GONE.
        v = self._verdict([
            ("CAM1", 2, 3), ("CAM2", 0, 0), ("CAM3", 0, 0),
            ("CAM1", 1, 0), ("CAM2", 1, 6), ("CAM3", 0, 0),
            ("CAM1", 0, 1), ("CAM2", 1, 1), ("CAM3", 0, 0),
            ("CAM1", 1, 0)])
        rep = mp.edge_oscillation_report(v)
        assert rep["suspect"] is False
        assert rep["suspect_camboxes"] == []

    def test_a_frozen_camera_storm_is_not_an_edge_oscillation(self):
        # verdict 547108056 CAM1: 3/3, 98/1, 845/0, 30/29 -- a DEAD camera, not a FIFO edge.
        # The storm windows (98/1, 845/0) must EXCLUDE it (a frozen leg is a different class).
        v = self._verdict([
            ("CAM1", 3, 3), ("CAM1", 98, 1), ("CAM1", 845, 0), ("CAM1", 30, 29)])
        rep = mp.edge_oscillation_report(v)
        assert rep["suspect"] is False
        assert rep["camboxes"]["CAM1"]["suspect"] is False
        assert rep["camboxes"]["CAM1"]["storm_windows"] >= 1

    def test_a_single_oscillating_window_is_not_sustained_enough(self):
        # one 17/17 window alone (66065064 CAM2 had exactly one) is a singleton, not the pattern.
        v = self._verdict([("CAM2", 17, 17), ("CAM2", 0, 0), ("CAM2", 1, 1)])
        rep = mp.edge_oscillation_report(v)
        assert rep["suspect"] is False

    def test_a_gap_heavy_asymmetric_window_is_not_balanced(self):
        # copies=1 gaps=6 fails the both>=3 test AND the balance test -> not oscillating.
        v = self._verdict([("CAM2", 1, 6), ("CAM2", 1, 6)])
        rep = mp.edge_oscillation_report(v)
        assert rep["suspect"] is False

    def test_missing_continuity_or_segments_is_not_a_suspect_not_an_error(self):
        assert mp.edge_oscillation_report({})["suspect"] is False
        assert mp.edge_oscillation_report({"all_cambox_continuity": {}})["suspect"] is False
        assert mp.edge_oscillation_report(
            {"all_cambox_continuity": {"segments": []}})["suspect"] is False

    def test_report_carries_the_calibrated_thresholds(self):
        rep = mp.edge_oscillation_report(self._verdict([("CAM2", 5, 4), ("CAM2", 7, 7)]))
        assert rep["threshold"]["min_both"] == mp.EDGE_OSC_MIN_BOTH
        assert rep["threshold"]["max_magnitude"] == mp.EDGE_OSC_MAX_MAGNITUDE
        assert rep["threshold"]["min_windows"] == mp.EDGE_OSC_MIN_WINDOWS


class TestStalenessFromVerdictCLI:
    def test_reads_a_verdict_and_reports_report_only_exit_0(self, tmp_path, capsys):
        prof_p = os.path.join(_SCRIPTS, "e2e-measurement-pins.json")
        v = {"all_cambox_delivery_latency": {"cam1": {"p50_ms": 270.0}, "cam3": {"p50_ms": 205.0}}}
        vp = tmp_path / "verdict.json"
        vp.write_text(json.dumps(v))
        rc = mp.main(["staleness-from-verdict", "--profile", prof_p, "--verdict", str(vp)])
        out = capsys.readouterr()
        assert rc == 0  # report-only never fails the caller
        rep = json.loads(out.out)
        assert rep["stale"] is True  # cam1 270 vs expected 207

    def test_missing_delivery_block_is_report_only_not_error(self, tmp_path, capsys):
        prof_p = os.path.join(_SCRIPTS, "e2e-measurement-pins.json")
        vp = tmp_path / "verdict.json"
        vp.write_text(json.dumps({"nodes": {}}))
        rc = mp.main(["staleness-from-verdict", "--profile", prof_p, "--verdict", str(vp)])
        assert rc == 0


class TestEdgeOscillationCLI:
    def test_reports_suspect_report_only_exit_0(self, tmp_path, capsys):
        v = {"all_cambox_continuity": {"segments": [
            {"cambox": "CAM2", "copies": 5, "gaps": 4},
            {"cambox": "CAM2", "copies": 7, "gaps": 7}]}}
        vp = tmp_path / "verdict.json"
        vp.write_text(json.dumps(v))
        rc = mp.main(["edge-oscillation", "--verdict", str(vp)])
        out = capsys.readouterr()
        assert rc == 0
        rep = json.loads(out.out)
        assert rep["suspect"] is True

    def test_clean_run_reports_not_suspect(self, tmp_path, capsys):
        v = {"all_cambox_continuity": {"segments": [
            {"cambox": "CAM2", "copies": 0, "gaps": 0}]}}
        vp = tmp_path / "verdict.json"
        vp.write_text(json.dumps(v))
        rc = mp.main(["edge-oscillation", "--verdict", str(vp)])
        out = capsys.readouterr()
        assert rc == 0
        assert json.loads(out.out)["suspect"] is False
