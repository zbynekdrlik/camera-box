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
        # cam6's derived offset (24.7ms) is outside +/-20ms -> gate_pass=false in the fixture.
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
