"""#711 — unit tests for scripts/e2e_discord_report.py, the pure Slovak Discord-report composer
for the full-path E2E verdict JSON (recording-verdict --merge-partials --json output).

Fixtures live in tests/python/fixtures/e2e_discord_report/:
  - verdict_all_cambox_red_709.json   REAL run (CI run 29180680684, commit 6ca6f97c3 — the
    #709-era gate: continuity FAIL, imag optical judder, cam2 A/V measured -44.5ms outside
    tolerance, most cameras' A/V UNKNOWN with thin candidate pools). Anchors the composer
    against genuine field shapes, not an invented approximation.
  - verdict_clean_pass.json           synthetic, everything PASS.
  - verdict_silent_audio.json         synthetic, continuity/loss PASS but EVERY camera's A/V
    candidates=0 — must compose the literal "tichá stopa" phrase issue #711 requires, never a
    fabricated number.
  - verdict_legacy_single_camera.json synthetic, no all_cambox_* blocks at all (a non-ALL_CAMBOX
    single-camera run) — the composer must degrade gracefully to N/A lines, never crash.
  - verdict_derived_av_714.json       synthetic, cam2 measured + 5 cameras "derived" (#714:
    sample-starved on their own, but soundly estimated from cam2's own offset + their own #286
    delivery-latency delta) — must compose "ODVODENÉ <value>", never a bare "UNKNOWN", for every
    derived camera; one camera's derived offset is deliberately outside tolerance to prove the
    gate still fails closed on a bad DERIVED estimate too.
  - verdict_offline_ack_excluded_855.json synthetic, cam1-4 measured/PASS, cam5/cam6/cam7
    operator-acked offline (CAMBOX_OFFLINE_ACK / rig-fleet.txt, #855) -- must compose "VYNECHANÉ"
    with the ack reason, never a bare "UNKNOWN", and the excluded boxes must never drag the
    overall verdict down (gate_pass stays true).
"""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"


def _load(name):
    with open(_FIXTURES / name, encoding="utf-8") as f:
        return json.load(f)


# ---------------------------------------------------------------------------
# Real #709-era red fixture
# ---------------------------------------------------------------------------

class TestRealRedFixture:
    def setup_method(self):
        self.verdict = _load("verdict_all_cambox_red_709.json")
        self.meta = {
            "run_id": "819997241",
            "event": "CI PR gate",
            "duration_secs": 300,
            "gate_exit": 1,
        }
        self.report = edr.compose_report(self.verdict, self.meta)

    def test_headline_is_red(self):
        assert "❌ RED" in self.report
        assert "✅ PASS" not in self.report.split("5️⃣")[1]

    def test_reports_cam2_measured_av_offset(self):
        # cam2 av_offset_ms = -44.531250000027, gate_pass=false, 24 cluster samples.
        assert "cam2" in self.report
        assert "-44.5ms" in self.report
        assert "zhody=24" in self.report

    def test_reports_thin_candidate_cams_as_unknown_with_a_reason(self):
        # cam1/cam3/cam4/cam5/cam6 all have candidates>0 but cluster_samples=0 -> UNKNOWN with
        # "nedostatok konzistentných vzoriek", never a bare number and never silently omitted.
        assert "cam1: UNKNOWN — nedostatok konzistentných vzoriek" in self.report

    def test_never_claims_all_silent_when_cam2_has_candidates(self):
        # cam2 genuinely measured something — the "tichá stopa" whole-run line must NOT fire.
        assert "žiadna kamera nezachytila zvukovú značku" not in self.report

    def test_reports_continuity_and_imag_judder_blockers(self):
        assert "Kontinuita medzi kamerami (stream): FAIL" in self.report
        assert "Kontinuita/plynulosť na IMAG: FAIL" in self.report

    def test_reports_per_camera_zero_loss_stream_numbers(self):
        # cam1: present_count=1694, expected_count=1694, real_drops=0, burn_unreadable=0, zero_loss=True
        assert "✅ cam1: 1694/1694 snímok, 0 stratených, 0 nečitateľných" in self.report

    def test_reports_imag_path_per_camera_from_segments(self):
        # imag.segments CAM4 pass=True aggregated; CAM1 pass=False (two segments, both False).
        assert "❌ cam1:" in self.report  # imag leg cam1 fails in both its segments
        assert "✅ cam4:" in self.report  # imag leg cam4's single segment passes

    def test_reports_delivery_latency_spread_section(self):
        assert "Video sync NDI kamier v strih OBS" in self.report
        assert "Rozptyl medzi kamerami: 11.0ms" in self.report

    def test_reports_source_latency_stability_and_imag_floor(self):
        assert "Latencia — stabilita" in self.report
        assert "Minimálna nameraná latencia smerom k IMAG" in self.report
        # cam5 has the smallest min_ms (53.984543) among the 5 measured (no cam2).
        assert "54.0ms" in self.report

    def test_gate_exit_code_surfaced(self):
        assert "exit code: 1" in self.report

    def test_camera_list_includes_all_six(self):
        header = self.report.split("\n\n")[0]
        for n in range(1, 7):
            assert f"CAM{n}" in header


# ---------------------------------------------------------------------------
# Clean pass fixture
# ---------------------------------------------------------------------------

class TestCleanPassFixture:
    def setup_method(self):
        self.verdict = _load("verdict_clean_pass.json")
        self.report = edr.compose_report(self.verdict, {"run_id": "1", "event": "manuálny beh"})

    def test_headline_is_pass(self):
        assert "✅ PASS" in self.report

    def test_no_blockers_listed(self):
        assert "Všetky brány prešli" in self.report
        assert "Blokujúce brány" not in self.report

    def test_every_camera_shows_pass_glyph_in_stream_loss(self):
        for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"]:
            assert f"✅ {cam}:" in self.report

    def test_av_sync_all_measured_and_passing(self):
        assert "UNKNOWN" not in self.report.split("4️⃣")[1].split("5️⃣")[0]


# ---------------------------------------------------------------------------
# Silent-audio fixture -- the exact required phrase
# ---------------------------------------------------------------------------

class TestSilentAudioFixture:
    def setup_method(self):
        self.verdict = _load("verdict_silent_audio.json")
        self.report = edr.compose_report(self.verdict, {"run_id": "2", "event": "CI PR gate"})

    def test_composes_the_literal_required_phrase(self):
        # Issue #711 body, verbatim: "a silent-audio verdict must compose 'A/V: UNKNOWN — tichá
        # stopa' not a number."
        assert "A/V: UNKNOWN — tichá stopa" in self.report

    def test_never_fabricates_an_offset_number_for_a_silent_camera(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "ms, MAD" not in av_section  # no per-camera "offset Xms, MAD Yms" line printed

    def test_per_camera_lines_also_state_the_silent_reason(self):
        assert "cam1: UNKNOWN — tichá stopa" in self.report
        assert "cam2: UNKNOWN — tichá stopa" in self.report

    def test_overall_is_red_because_of_av_gate(self):
        assert "❌ RED" in self.report
        assert "A/V synchronizácia: FAIL" in self.report

    def test_continuity_sections_still_report_pass(self):
        # This fixture's loss/continuity are clean -- only A/V is red. The report must not
        # conflate the two: continuity blocker line must be ABSENT.
        assert "Kontinuita medzi kamerami (stream): FAIL" not in self.report


# ---------------------------------------------------------------------------
# #714 -- a sample-starved camera gets a DERIVED estimate, never a bare UNKNOWN
# ---------------------------------------------------------------------------

class TestDerivedAvFixture:
    def setup_method(self):
        self.verdict = _load("verdict_derived_av_714.json")
        self.report = edr.compose_report(self.verdict, {"run_id": "714", "event": "CI PR gate"})

    def test_derived_cameras_are_labeled_odvodene_never_a_bare_unknown(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "cam1: UNKNOWN" not in av_section
        assert "ODVODENÉ" in av_section

    def test_derived_offset_value_and_delivery_spread_are_shown(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        # cam1's derived_offset_ms = -8.4, derived_delivery_spread_ms = 11.0.
        assert "cam1: " in av_section
        assert "-8.4ms" in av_section
        assert "±11.0ms" in av_section

    def test_cam2_still_reports_as_a_real_measurement(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "cam2: offset 1.5ms" in av_section

    def test_a_failing_derived_gate_is_reported_with_the_fail_glyph(self):
        # The fixture hardcodes cam6's gate_pass=false alongside its 24.7ms derived offset — the
        # renderer must show the fail glyph from the JSON verdict, never re-derive the gate from
        # the offset value (the live tolerance is AV_OFFSET_GATE_TOLERANCE_MS in Rust, not here).
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        cam6_line = next(line for line in av_section.splitlines() if "cam6" in line)
        assert "❌" in cam6_line
        assert "24.7ms" in cam6_line

    def test_overall_verdict_is_red_and_names_the_av_blocker(self):
        assert "❌ RED" in self.report
        assert "A/V synchronizácia: FAIL" in self.report

    def test_every_camera_gets_a_value_or_a_reasoned_line_never_silent(self):
        # #714's own acceptance bar: a value (measured or derived) or an explicit reasoned bound
        # for EVERY camera -- never a silent "cam2 only".
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        for cam in ["cam1", "cam2", "cam3", "cam4", "cam5", "cam6"]:
            assert f"{cam}: " in av_section, f"{cam} must have its own line: {av_section}"


# ---------------------------------------------------------------------------
# #855 -- an operator-acknowledged offline box is EXCLUDED from A/V-sync, never a judged UNKNOWN
# ---------------------------------------------------------------------------

class TestOfflineAckExcludedFixture:
    def setup_method(self):
        self.verdict = _load("verdict_offline_ack_excluded_855.json")
        self.report = edr.compose_report(self.verdict, {"run_id": "855", "event": "CI PR gate"})

    def test_excluded_cameras_are_never_shown_as_unknown(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        for cam in ["cam5", "cam6", "cam7"]:
            cam_line = next(line for line in av_section.splitlines() if cam in line)
            assert "UNKNOWN" not in cam_line, f"an acked-offline box must never read as UNKNOWN: {cam_line}"
            assert "VYNECHANÉ" in cam_line

    def test_excluded_reason_is_shown(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "powered-off-2026-07-27" in av_section

    def test_measured_cameras_still_report_normally(self):
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "cam1: offset -3.2ms" in av_section

    def test_overall_verdict_is_pass_not_dragged_down_by_the_excluded_boxes(self):
        # #855 acceptance: an acked-offline box must never count as a FAIL for the gate.
        assert "✅ PASS" in self.report
        assert "A/V synchronizácia: FAIL" not in self.report


# ---------------------------------------------------------------------------
# Legacy / non-ALL_CAMBOX single-camera fixture -- graceful degradation
# ---------------------------------------------------------------------------

class TestLegacySingleCameraFixture:
    def setup_method(self):
        self.verdict = _load("verdict_legacy_single_camera.json")
        self.report = edr.compose_report(
            self.verdict, {"run_id": "3", "event": "manuálny beh (recording-e2e.sh)"}
        )

    def test_does_not_crash_and_produces_all_six_sections(self):
        for marker in ["1️⃣", "2️⃣", "3️⃣", "4️⃣", "5️⃣"]:
            assert marker in self.report

    def test_reports_na_for_unmeasured_all_cambox_blocks(self):
        assert "N/A — nemerané v tomto behu" in self.report
        assert "N/A — A/V sync nebol súčasťou tohto behu" in self.report

    def test_imag_falls_back_to_the_single_combined_node(self):
        assert "spoločný signál (nie per-kamera" in self.report

    def test_overall_pass_true(self):
        assert "✅ PASS" in self.report


# ---------------------------------------------------------------------------
# #726 -- presentation-cadence EVENNESS section (REPORTED only, not gate-enforced)
# ---------------------------------------------------------------------------

class TestPresentationCadenceSection:
    def test_na_when_no_all_cambox_continuity_segments_at_all(self):
        # the legacy single-camera fixture has no all_cambox_* blocks whatsoever.
        verdict = _load("verdict_legacy_single_camera.json")
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "Plynulosť obrazu na strih (informatívne, #726)" in report
        assert "chýba okno s namaľovaným tikom" in report

    def test_na_when_segments_present_but_none_carry_presentation_cadence(self):
        # segments exist (e.g. non-cam2 windows only) but none decoded a painted tick.
        verdict = {
            "all_cambox_continuity": {
                "segments": [
                    {"cambox": "cam1", "pass": True, "frames": 10},
                    {"cambox": "cam3", "pass": True, "frames": 12},
                ]
            }
        }
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "chýba okno s namaľovaným tikom" in report

    def test_reports_cam2_evenness_percentage_and_counts(self):
        verdict = {
            "all_cambox_continuity": {
                "segments": [
                    {"cambox": "cam1", "pass": True, "frames": 10},
                    {
                        "cambox": "cam2",
                        "pass": True,
                        "frames": 30,
                        "presentation_cadence": {
                            "expected_step": 2,
                            "sample_deltas": 29,
                            "uniform_steps": 29,
                            "duplicate_steps": 0,
                            "catchup_steps": 0,
                            "other_steps": 0,
                            "paired_events": 0,
                            "uniform_fraction": 1.0,
                            "duplicate_fraction": 0.0,
                            "paired_fraction": 0.0,
                            "evenness_score": 1.0,
                        },
                    },
                ]
            }
        }
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "cam2: rovnomernosť 100% (0 zdvojených z 29 snímok, 0 spárovaných" in report
        # cam1 (no presentation_cadence) must not get its own cadence line.
        section = report.split("Plynulosť obrazu na strih")[1]
        assert "cam1:" not in section

    def test_reports_judder_recording_with_low_percentage_and_paired_count(self):
        verdict = {
            "all_cambox_continuity": {
                "segments": [
                    {
                        "cambox": "cam2",
                        "pass": False,
                        "frames": 30,
                        "presentation_cadence": {
                            "expected_step": 2,
                            "sample_deltas": 29,
                            "uniform_steps": 0,
                            "duplicate_steps": 15,
                            "catchup_steps": 14,
                            "other_steps": 0,
                            "paired_events": 14,
                            "uniform_fraction": 0.0,
                            "duplicate_fraction": 15 / 29,
                            "paired_fraction": 28 / 29,
                            "evenness_score": 0.0,
                        },
                    },
                ]
            }
        }
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "cam2: rovnomernosť 0% (15 zdvojených z 29 snímok, 14 spárovaných" in report
        assert "signatúra '15fps' pri 30fps plátne" in report


class TestResidualEventsSection:
    """#707 EVENT-FORENSICS -- the "Odchýlky s dôvodmi" line, sourced from
    all_cambox_continuity.residual_events (src/residual_events.rs)."""

    def test_absent_when_no_all_cambox_continuity_at_all(self):
        # The legacy single-camera fixture has no all_cambox_* blocks whatsoever -- no sweep ran,
        # so the line must not appear at all (never a spurious "0/0").
        verdict = _load("verdict_legacy_single_camera.json")
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "Odchýlky s dôvodmi" not in report

    def test_reports_zero_zero_for_a_genuinely_clean_sweep(self):
        # The block ran (segments present) but found no residual events at all -- a real "swept
        # clean" signal, distinct from "never swept".
        verdict = {"all_cambox_continuity": {"segments": [{"cambox": "cam1", "residual_events": []}]}}
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "Odchýlky s dôvodmi: 0 s dôkazmi / 0 otvorených" in report

    def test_reads_the_flat_top_level_list_and_splits_by_reason(self):
        verdict = {
            "all_cambox_continuity": {
                "segments": [{"cambox": "cam1"}],
                "residual_events": [
                    {"kind": "copy", "frame_index": 5926, "reason": "known #656 self-heal burst"},
                    {"kind": "copy", "frame_index": 6100, "reason": None},
                    {"kind": "gap", "frame_index": 7000},
                ],
            }
        }
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "Odchýlky s dôvodmi: 1 s dôkazmi / 2 otvorených" in report

    def test_falls_back_to_walking_segments_when_no_top_level_list(self):
        # An older verdict JSON that predates the flattened all_cambox_continuity.residual_events
        # convenience field -- the composer must still aggregate from segments[].residual_events.
        verdict = {
            "all_cambox_continuity": {
                "segments": [
                    {
                        "cambox": "cam1",
                        "residual_events": [{"kind": "copy", "frame_index": 1, "reason": "x"}],
                    },
                    {
                        "cambox": "cam2",
                        "residual_events": [{"kind": "gap", "frame_index": 2}],
                    },
                ]
            }
        }
        report = edr.compose_report(verdict, {"run_id": "x", "event": "test"})
        assert "Odchýlky s dôvodmi: 1 s dôkazmi / 1 otvorených" in report


# ---------------------------------------------------------------------------
# chunk_for_discord -- pure text splitter
# ---------------------------------------------------------------------------

class TestChunkForDiscord:
    def test_short_text_is_a_single_chunk(self):
        text = "para one\n\npara two"
        chunks = edr.chunk_for_discord(text, limit=1900)
        assert chunks == [text]

    def test_splits_at_paragraph_boundaries_never_mid_sentence(self):
        paras = [f"Section {i}: " + ("x" * 100) for i in range(30)]
        text = "\n\n".join(paras)
        chunks = edr.chunk_for_discord(text, limit=500)
        assert len(chunks) > 1
        for c in chunks:
            assert len(c) <= 500
        # every original paragraph must survive intact somewhere in some chunk
        rejoined = "\n\n".join(chunks)
        for p in paras:
            assert p in rejoined

    def test_a_single_oversized_paragraph_is_kept_whole_not_silently_truncated(self):
        huge = "x" * 5000
        chunks = edr.chunk_for_discord(huge, limit=1900)
        assert chunks == [huge]

    def test_real_report_from_red_fixture_chunks_cleanly(self):
        verdict = _load("verdict_all_cambox_red_709.json")
        report = edr.compose_report(verdict, {"run_id": "x", "event": "CI PR gate"})
        chunks = edr.chunk_for_discord(report, limit=1900)
        assert all(len(c) <= 1900 for c in chunks)
        assert len(chunks) >= 1


# ---------------------------------------------------------------------------
# Small pure helpers
# ---------------------------------------------------------------------------

class TestHelpers:
    def test_av_reason_measured_returns_none(self):
        assert edr._av_reason({"verdict": "measured", "candidates": 10, "cluster_samples": 5}) is None

    def test_av_reason_zero_candidates_is_silent_track(self):
        assert edr._av_reason({"verdict": "unknown", "candidates": 0, "cluster_samples": 0}) == "tichá stopa"

    def test_av_reason_candidates_present_no_cluster(self):
        r = edr._av_reason({"verdict": "unknown", "candidates": 400, "cluster_samples": 0})
        assert r == "nedostatok konzistentných vzoriek"

    def test_cameras_present_orders_by_camera_order_and_dedupes(self):
        verdict = {
            "full_chain": {"loss": {"cam3": {}, "cam1": {}}},
            "all_cambox_latency": {"cam1": {}, "cam5": {}},
        }
        assert edr._cameras_present(verdict) == ["cam1", "cam3", "cam5"]

    def test_g_safe_nested_get_missing_path(self):
        assert edr._g({"a": {"b": 1}}, "a", "c", default="fallback") == "fallback"
        assert edr._g({"a": {"b": 1}}, "a", "b") == 1
        assert edr._g(None, "a", "b", default=0) == 0


# ---------------------------------------------------------------------------
# #756 Member 3 -- live latency pins + recommended pins section (the user's repeated request:
# per-camera configured genlock latency, live-read over WS, next to this run's own measured
# delivery p50, plus a computed recommendation).
# ---------------------------------------------------------------------------

class TestLatencyPinsSection:
    def _verdict_with_delivery(self, p50s: dict):
        """A minimal verdict carrying only what _section_latency_pins needs: cameras present
        (via all_cambox_delivery_latency) and each camera's p50_ms."""
        delivery = {cam: {"p50_ms": p50, "mean_ms": p50, "samples": 100} for cam, p50 in p50s.items()}
        return {"all_cambox_delivery_latency": delivery}

    def test_section_is_none_when_no_pins_meta_supplied(self):
        # Never fabricated -- a run that didn't gather a pins snapshot gets NO section at all,
        # not an empty/misleading one.
        verdict = self._verdict_with_delivery({"cam1": 71.0})
        report = edr.compose_report(verdict, {"run_id": "x"})
        assert "Nastavené latencie" not in report

    def test_section_appears_and_shows_current_pin_and_p50_per_camera(self):
        verdict = self._verdict_with_delivery({"cam1": 71.2, "cam2": 68.0})
        pins = {
            "strih": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 14, "mv_ms": 14},
            },
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "Nastavené latencie" in report
        assert "strih=3ms" in report
        assert "p50 tento beh=71ms" in report
        assert "strih=14ms" in report
        assert "p50 tento beh=68ms" in report

    def test_main_vs_mv_mismatch_is_flagged_loudly(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0})
        pins = {"strih": {"cam1": {"main_ms": 3, "mv_ms": 8}}}
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "PARITA main≠MV" in report
        assert "main≠MV klon nesie inú latenciu" in report

    def test_no_mismatch_when_main_and_mv_agree(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0})
        pins = {"strih": {"cam1": {"main_ms": 3, "mv_ms": 3}}}
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "PARITA main≠MV" not in report

    def test_imag_shown_as_one_fixed_summary_line_when_all_compliant(self):
        # #757 (2026-07-15, binding directive): imag is fixed-3ms-always, never a per-camera
        # cell -- ONE summary line, not a repeated "imag=3ms" per camera row.
        verdict = self._verdict_with_delivery({"cam1": 71.0, "cam2": 68.0})
        pins = {
            "strih": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 14, "mv_ms": 14},
            },
            "imag": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 3, "mv_ms": 3},
            },
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "imag: všetky 3 (fixné, IMAG=min latencia)" in report
        assert "imag=" not in report, "must NOT render a per-camera imag cell anymore"

    def test_imag_drift_from_fixed_3ms_is_flagged_loudly(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0, "cam2": 68.0})
        pins = {
            "strih": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 14, "mv_ms": 14},
            },
            "imag": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 67, "mv_ms": 67},  # drifted -- imag_latency_enforce missed it
            },
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "NEODCHÝLENÉ" in report
        assert "cam2" in report
        assert "všetky 3 (fixné" not in report

    def test_missing_camera_in_pins_reports_na_not_silently_omitted(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0, "cam2": 68.0})
        pins = {"strih": {"cam1": {"main_ms": 3, "mv_ms": 3}}}  # cam2 missing on purpose
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "cam2: strih=N/A" in report

    def test_recommended_pins_shown_when_present(self):
        verdict = self._verdict_with_delivery({"cam1": 100.0, "cam2": 90.0})
        pins = {
            "strih": {
                "cam1": {"main_ms": 3, "mv_ms": 3},
                "cam2": {"main_ms": 3, "mv_ms": 3},
            },
            "recommended_pins_ms": {"cam1": 3, "cam2": 13},
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "odporúčané=3ms" in report
        assert "odporúčané=13ms" in report

    def test_stream_hold_shows_live_and_source_of_truth(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0})
        pins = {
            "strih": {"cam1": {"main_ms": 3, "mv_ms": 3}},
            "stream_hold_active_ms": 952,
            "av_sync_last": {"applied_latency_ms": 952, "source": "NDI 2ME PGM"},
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "živé z WS=952ms" in report
        assert "posledný zdroj pravdy (av-sync-last.json)=952ms" in report
        assert "NEZHODA" not in report

    def test_stream_hold_mismatch_between_live_and_source_of_truth_is_flagged(self):
        verdict = self._verdict_with_delivery({"cam1": 71.0})
        pins = {
            "strih": {"cam1": {"main_ms": 3, "mv_ms": 3}},
            "stream_hold_active_ms": 925,  # box is running an OLDER hold than what's recorded
            "av_sync_last": {"applied_latency_ms": 952, "source": "NDI 2ME PGM"},
        }
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "NEZHODA" in report

    def test_cam7_is_included(self):
        verdict = self._verdict_with_delivery({"cam7": 80.0})
        pins = {"strih": {"cam7": {"main_ms": 36, "mv_ms": 36}}}
        report = edr.compose_report(verdict, {"run_id": "x", "pins": pins})
        assert "cam7: strih=36ms" in report


# ---------------------------------------------------------------------------
# #761 -- MV-clone-vs-main presentation skew section
# ---------------------------------------------------------------------------

class TestMvSkewSection:
    def setup_method(self):
        self.verdict = _load("verdict_clean_pass.json")

    def test_section_absent_when_no_mv_skew_meta(self):
        report = edr.compose_report(self.verdict, {"run_id": "x"})
        assert "prezentačný skew" not in report  # never fabricated when not gathered

    def test_shared_source_regression_guard_renders_ok_lines(self):
        mv_skew = {
            "frame_ms": 16.666666666666668,
            "cameras": {
                "cam1": {"median_ms": 1.3, "n_samples": 15, "stdev_ms": 12.0, "alarming": False},
                "cam2": {"median_ms": None, "n_samples": 0, "note": "žiadny dekódovateľný QR"},
                "cam3": {"median_ms": -1.5, "n_samples": 15, "stdev_ms": 7.0, "alarming": False},
            },
        }
        report = edr.compose_report(self.verdict, {"run_id": "x", "mv_skew": mv_skew})
        assert "MV klon vs. program — prezentačný skew" in report
        assert "cam1: +1.3 ms (n=15, ±12 ms)" in report
        assert "cam2: N/A (žiadny dekódovateľný QR)" in report  # honest N/A, never a fabricated 0
        assert "cam3: -1.5 ms (n=15, ±7 ms)" in report
        assert "Prah poplachu" in report
        assert "strihač vidí" not in report  # nothing alarming => no loud per-camera warning

    def test_alarming_skew_is_flagged_loudly(self):
        mv_skew = {
            "frame_ms": 16.666666666666668,
            "cameras": {
                "cam1": {"median_ms": 45.0, "n_samples": 15, "stdev_ms": 9.0, "alarming": True},
            },
        }
        report = edr.compose_report(self.verdict, {"run_id": "x", "mv_skew": mv_skew})
        assert "⚠️ cam1: +45.0 ms" in report
        assert "strihač vidí cam1 o 45 ms neskôr než program" in report
        assert "multiview bunka NEsedí časovo s programom" in report

    def test_gatherer_error_is_reported_not_fabricated(self):
        mv_skew = {"error": "connect failed: timeout", "cameras": {}}
        report = edr.compose_report(self.verdict, {"run_id": "x", "mv_skew": mv_skew})
        assert "nemeralo sa: connect failed: timeout" in report


class TestAvPresentUndecodedFixture:
    """#748 point 3 — every judged camera has candidates==0 (an all-silent A/V run), but the
    verdict's `all_cambox_av_sync.av_audio_silent` is False: the QPSK demod saw preamble energy,
    so the MEASUREMENT AUDIO IS PRESENT and the marker simply never decoded (emit/painter side or
    a decode regression). The report must NOT blame the mbc mute — that would send the operator
    to check a mute that is not the cause."""

    def setup_method(self):
        self.verdict = _load("verdict_av_present_undecoded_748.json")
        self.report = edr.compose_report(
            self.verdict, {"run_id": "3", "event": "CI PR gate"}
        )

    def test_does_not_blame_the_mbc_mute_when_audio_is_present(self):
        # The silent-chain line is the WRONG alert here — audio was present, so it must be absent.
        assert "MERACÍ ZVUK TICHÝ" not in self.report

    def test_states_the_marker_did_not_decode_though_audio_present(self):
        # A distinct, operator-actionable line pointing at the QPSK marker / emit side, not a mute.
        assert "NEDEKÓDOVALA" in self.report
        assert "merací zvuk je prítomný" in self.report

    def test_overall_is_still_red_because_of_the_av_gate(self):
        # A run with no A/V measurement still fails the gate, whatever the cause.
        assert "❌ RED" in self.report

    def test_per_camera_lines_do_not_say_silent_track_when_audio_present(self):
        # The per-camera reason must match the summary — no "tichá stopa" (silent) when the
        # discriminator says the audio was present but undecoded.
        av_section = self.report.split("4️⃣")[1].split("5️⃣")[0]
        assert "tichá stopa" not in av_section
        assert "značka nedekódovaná" in av_section


class TestSwitchInTransientAnnotation1144:
    """issue 1144 -- the detailed imag rendering annotates a cam whose failing segment(s) carry a
    switch-in transient (a raw content FAIL the imag content gate excuses / attributes to cold-cut),
    so an excused ❌ on the detailed view is explained rather than reading as an unexplained failure."""

    def test_aggregate_carries_the_switch_in_transient_flag(self):
        segs = [
            {"cambox": "CAM3", "pass": False, "switch_in_transient": True, "frames": 1682},
            {"cambox": "CAM1", "pass": True, "switch_in_transient": False, "frames": 1697},
        ]
        agg = edr._aggregate_segments(segs)
        assert agg["cam3"]["switch_in_transient"] is True
        assert agg["cam1"]["switch_in_transient"] is False

    def test_absent_field_defaults_false_no_annotation(self):
        # A pre-issue-1144 verdict (no switch_in_transient field) must aggregate to False and add no
        # annotation -- guards the existing real fixtures against a spurious note.
        agg = edr._aggregate_segments([{"cambox": "CAM3", "pass": False, "frames": 1682}])
        assert agg["cam3"]["switch_in_transient"] is False

    def test_imag_line_is_annotated_when_a_transient_is_present(self):
        verdict = {
            "overall_pass": True,
            "full_chain": {"loss": {"cam3": {"zero_loss": True}}},
            "all_cambox_continuity": {
                "imag": {
                    "segments": [
                        {
                            "cambox": "CAM3",
                            "pass": False,
                            "switch_in_transient": True,
                            "frames": 1682,
                        }
                    ]
                }
            },
        }
        report = edr.compose_report(verdict, {"run_id": "1144", "event": "unit"})
        assert "switch-in transient → cold-cut, report-only" in report
