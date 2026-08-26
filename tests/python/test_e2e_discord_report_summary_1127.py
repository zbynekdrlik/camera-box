"""#1127 — unit tests for the REDESIGNED, phone-readable Discord summary of the full-path E2E
verdict JSON (owner directive 2026-08-19: "pár riadkov pre človeka — verdikt prvý, PASS=3 riadky,
FAIL=len zlyhané gaty, report-only nikdy ❌").

The OLD detailed renderer (`compose_report`, six 1️⃣-6️⃣ sections) is UNCHANGED — it stays the
plain-text / CI-log path (`scripts/e2e_discord_report.py` without `--json-chunks`). This file
tests the NEW `compose_summary(verdict, meta)` that the Discord path (`--json-chunks`) uses.

Two REAL verdict fixtures (copied from live rig runs) anchor the classification against genuine
field shapes, never an invented approximation:
  - verdict_real_pass_reportonly_1104689227.json  overall_pass=TRUE, but the OLD report showed
    4× ❌ on REPORT-ONLY seams (imag leg overall_pass=false + gates_overall_pass=false; delivery
    cross-camera spread 84.7ms spread_gate_pass=false) — the exact run that confused the owner.
    The NEW summary must be 3 lines, verdict-first, with ZERO ❌ and NO report-only mention.
  - verdict_real_fail_cam1_77008829.json  overall_pass=FALSE via all_cambox_continuity.overall_pass
    =false (a CAM1 window with gaps=4 over the copies/gaps tolerance 3, windows_over_copies_gaps_
    tolerance=1). headline zero-loss + av_sync gate are BOTH green — so the summary must name the
    STREAM continuity blocking gate and CAM1, and must NOT render the report-only imag/delivery
    seams as ❌.

Blocking-vs-report-only is DERIVED from the verdict JSON's own gate semantics (each LIVE seam node
carries gates_overall_pass=true; report-only seams carry gates_overall_pass=false, mirroring how
src/bin/recording-verdict.rs folds `all_pass`) — never a hardcoded guess.
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import e2e_discord_report as edr  # noqa: E402

_FIXTURES = pathlib.Path(__file__).resolve().parent / "fixtures" / "e2e_discord_report"
_PASS_FIXTURE = "verdict_real_pass_reportonly_1104689227.json"
_FAIL_FIXTURE = "verdict_real_fail_cam1_77008829.json"


def _load(name):
    with open(_FIXTURES / name, encoding="utf-8") as f:
        return json.load(f)


# ---------------------------------------------------------------------------
# REAL PASS fixture — the run that confused the owner (overall_pass TRUE, report-only ❌s)
# ---------------------------------------------------------------------------

class TestRealPassSummary:
    def setup_method(self):
        self.verdict = _load(_PASS_FIXTURE)
        self.meta = {"run_id": "1104689227", "event": "CI PR gate", "duration_secs": 300}
        self.summary = edr.compose_summary(self.verdict, self.meta)
        self.lines = [ln for ln in self.summary.splitlines() if ln.strip()]

    def test_first_line_is_the_pass_verdict_with_run_id(self):
        assert self.lines[0].startswith("✅ E2E TEST PREŠIEL")
        assert "1104689227" in self.lines[0]

    def test_pass_is_at_most_three_lines(self):
        # Owner's hard cap: PASS = verdikt + zero-loss suma + link. Never a wall.
        assert len(self.lines) <= 3, f"PASS must be <=3 lines, got {len(self.lines)}: {self.lines}"

    def test_zero_loss_summary_line_present(self):
        assert "0 stratených snímok" in self.summary
        # 7 cameras were present in this run's blocks.
        assert "7 kamier" in self.summary

    def test_a_link_line_is_present(self):
        assert "Plný detail" in self.summary

    def test_never_renders_a_red_cross_on_a_passing_run(self):
        # THE core bug: report-only ❌ on a green run. Must be entirely gone.
        assert "❌" not in self.summary

    def test_report_only_metrics_are_not_mentioned_on_pass(self):
        # imag leg + delivery-side spread both "tripped" (report-only) in this fixture — they must
        # NOT appear at all on a PASS (that was the owner's confusion).
        assert "imag" not in self.summary.lower()
        assert "84.7" not in self.summary
        assert "spread" not in self.summary.lower()


# ---------------------------------------------------------------------------
# REAL FAIL fixture — CAM1 stream-continuity window over tolerance
# ---------------------------------------------------------------------------

class TestRealFailSummary:
    def setup_method(self):
        self.verdict = _load(_FAIL_FIXTURE)
        self.meta = {"run_id": "77008829", "event": "CI PR gate", "duration_secs": 300}
        self.summary = edr.compose_summary(self.verdict, self.meta)
        self.lines = [ln for ln in self.summary.splitlines() if ln.strip()]

    def test_first_line_is_the_fail_verdict_with_run_id(self):
        assert self.lines[0].startswith("❌ E2E TEST ZLYHAL")
        assert "77008829" in self.lines[0]

    def test_names_the_stream_continuity_blocking_gate(self):
        assert "kontinuita" in self.summary.lower() or "plynulos" in self.summary.lower()

    def test_attributes_the_failing_cambox_cam1(self):
        assert "CAM1" in self.summary

    def test_each_blocking_gate_line_carries_ownership(self):
        # #1117 convention: agent-recoverable → "Rieši Claude", physical → "fyzicky skontrolovať".
        gate_lines = [ln for ln in self.lines if ln.lstrip().startswith("•")]
        assert gate_lines, f"a FAIL must list at least one blocking gate: {self.lines}"
        for ln in gate_lines:
            assert ("Rieši Claude" in ln) or ("fyzicky skontrol" in ln), ln

    def test_does_not_render_report_only_seams_as_blocking_failures(self):
        # imag leg + delivery spread are report-only — they must never appear as a "•" gate line.
        gate_block = "\n".join(ln for ln in self.lines if ln.lstrip().startswith("•"))
        assert "IMAG" not in gate_block.upper()
        assert "doručen" not in gate_block.lower()  # delivery-side spread

    def test_report_only_info_line_if_present_is_a_single_neutral_line(self):
        info = [ln for ln in self.lines if ln.lstrip().startswith("ℹ️")]
        assert len(info) <= 1
        for ln in info:
            assert "❌" not in ln
            assert "neovplyvňuje" in ln  # explicitly marked as non-gating, plain Slovak

    def test_a_link_line_is_present_on_fail_too(self):
        assert "Plný detail" in self.summary


# ---------------------------------------------------------------------------
# Derived classification — the engine, tested directly on the two real fixtures
# ---------------------------------------------------------------------------

class TestBlockingClassification:
    def test_pass_fixture_has_no_blocking_failures(self):
        v = _load(_PASS_FIXTURE)
        assert edr._blocking_failures(v) == []

    def test_fail_fixture_has_at_least_one_blocking_failure(self):
        v = _load(_FAIL_FIXTURE)
        failures = edr._blocking_failures(v)
        assert failures, "the FAIL fixture must yield at least one blocking failure"
        # every entry is a (label, ownership) pair
        for label, owner in failures:
            assert isinstance(label, str) and label
            assert isinstance(owner, str) and owner

    def test_pass_fixture_report_only_seams_are_detected_as_report_only(self):
        # Proves imag leg + delivery spread are classified REPORT-ONLY (not blocking) — the whole
        # point of #1127. They tripped in this fixture, so they must show up in the report-only set.
        v = _load(_PASS_FIXTURE)
        tripped = edr._report_only_tripped(v)
        joined = " ".join(tripped).lower()
        assert "imag" in joined
        assert "doručen" in joined  # delivery-side spread

    def test_strih_stream_aggregate_zero_loss_node_is_blocking(self):
        # recording-verdict.rs :3795 folds full_chain.loss.strih / .stream (aggregate delivery
        # nodes) into all_pass — a per-cam scan alone would miss a stream-only failure.
        v = {
            "overall_pass": False,
            "full_chain": {"zero_loss": False, "loss": {"stream": {"zero_loss": False}}},
        }
        failures = edr._blocking_failures(v)
        assert failures, "a failing strih/stream aggregate node must be a named blocking gate"
        assert any("STREAM" in label for label, _ in failures)
        summary = edr.compose_summary(v, {"run_id": "x"})
        assert "konkrétnu blokujúcu bránu sa nepodarilo rozpoznať" not in summary  # not the fallback

    def test_burn_hold_over_bound_is_blocking(self):
        # recording-verdict.rs :3868 LIVE burn_hold fold; JSON at full_chain.loss.<node>.hold.
        v = {
            "overall_pass": False,
            "full_chain": {
                "zero_loss": True,
                "loss": {"stream": {"zero_loss": True,
                                    "hold": {"within_bound": False, "gates_overall_pass": True}}},
            },
        }
        failures = edr._blocking_failures(v)
        assert any("max-hold" in label for label, _ in failures)

    def test_burn_hold_report_only_when_seam_off_is_not_blocking(self):
        # If the seam is ever flipped to report-only (gates_overall_pass=false), an over-bound hold
        # must NOT be a blocking failure — the classifier follows the JSON seam.
        v = {
            "overall_pass": True,
            "full_chain": {"loss": {"stream": {"zero_loss": True,
                           "hold": {"within_bound": False, "gates_overall_pass": False}}}},
        }
        assert not any("max-hold" in label for label, _ in edr._blocking_failures(v))

    def test_report_only_seams_never_leak_into_blocking(self):
        # A synthetic verdict whose ONLY failing thing is a report-only imag leg must be PASS-shaped:
        # zero blocking failures.
        v = {
            "overall_pass": True,
            "all_cambox_continuity": {
                "overall_pass": True,
                "imag": {"overall_pass": False, "gates_overall_pass": False},
            },
            "all_cambox_delivery_latency": {"spread_gate_pass": False, "cross_camera_spread_ms": 90.0},
            "full_chain": {"zero_loss": True},
        }
        assert edr._blocking_failures(v) == []


# ---------------------------------------------------------------------------
# Ownership derivation — physical vs Claude
# ---------------------------------------------------------------------------

class TestOwnershipDerivation:
    def test_v4l2_capture_drop_is_a_physical_fault(self):
        # Use the REAL verbose `source` shape recording-verdict.rs writes, to prove the label is
        # derived from the cam2_<label> KEY and never echoes that whole sentence into the line.
        real_source = "cam1 V4L2 sequence-gap capture-drop (camera leg) — not a painter-tick compare"
        v = {
            "overall_pass": False,
            "full_chain": {
                "zero_loss": False,
                "loss": {"cam2_cam1": {"zero_loss": False, "v4l2_dropped": 12, "source": real_source}},
            },
        }
        summary = edr.compose_summary(v, {"run_id": "x"})
        assert "fyzicky skontrol" in summary  # a capture-card drop needs a human check
        assert "CAM1" in summary              # attributed from the cam2_cam1 key
        assert "sequence-gap" not in summary  # the verbose source sentence must NOT reach the line

    def test_silent_audio_av_failure_is_physical(self):
        v = {
            "overall_pass": False,
            "all_cambox_av_sync": {
                "gate_pass": False,
                "gates_overall_pass": True,
                "av_audio_silent": True,
            },
        }
        summary = edr.compose_summary(v, {"run_id": "x"})
        assert "fyzicky skontrol" in summary
        assert "tich" in summary.lower()  # silent measurement audio

    def test_continuity_failure_is_claude_owned(self):
        v = {
            "overall_pass": False,
            "all_cambox_continuity": {
                "overall_pass": False,
                "copies_gaps_tolerance": 3,
                "segments": [{"cambox": "CAM3", "copies": 0, "gaps": 5, "undecodable": 0, "pass": False}],
            },
        }
        summary = edr.compose_summary(v, {"run_id": "x"})
        assert "Rieši Claude" in summary
        assert "CAM3" in summary


# ---------------------------------------------------------------------------
# Safety net — a FAIL is NEVER silently hidden
# ---------------------------------------------------------------------------

class TestFailNeverHidden:
    def test_overall_fail_with_no_recognized_gate_still_shows_a_fail_line(self):
        # e.g. a burn_hold-only fold failure the summary doesn't enumerate — must still surface a
        # blocking line pointing at the CI log, never look PASS-ish.
        v = {"overall_pass": False}
        summary = edr.compose_summary(v, {"run_id": "x"})
        lines = [ln for ln in summary.splitlines() if ln.strip()]
        assert lines[0].startswith("❌ E2E TEST ZLYHAL")
        gate_lines = [ln for ln in lines if ln.lstrip().startswith("•")]
        assert gate_lines, "a FAIL with no recognized gate must still emit a generic blocking line"
        assert "CI log" in summary or "CI logu" in summary


# ---------------------------------------------------------------------------
# Graceful degradation — never crash
# ---------------------------------------------------------------------------

class TestGracefulDegradation:
    def test_empty_verdict_pass_shaped_does_not_crash(self):
        summary = edr.compose_summary({"overall_pass": True}, {"run_id": "x"})
        assert summary.splitlines()[0].startswith("✅ E2E TEST PREŠIEL")

    def test_missing_run_id_is_tolerated(self):
        summary = edr.compose_summary({"overall_pass": True}, {})
        assert "E2E TEST" in summary

    def test_duration_absent_omits_duration_but_keeps_verdict(self):
        summary = edr.compose_summary({"overall_pass": True}, {"run_id": "9"})
        assert summary.splitlines()[0].startswith("✅ E2E TEST PREŠIEL")
        assert "9" in summary


# ---------------------------------------------------------------------------
# Duration + plural helpers
# ---------------------------------------------------------------------------

class TestFormatHelpers:
    def test_duration_seconds_only(self):
        assert edr._fmt_duration(45) == "45s"

    def test_duration_minutes_and_seconds(self):
        assert edr._fmt_duration(300) == "5m 0s"
        assert edr._fmt_duration(325) == "5m 25s"

    def test_duration_none(self):
        assert edr._fmt_duration(None) is None

    def test_camera_plural_slovak(self):
        assert edr._camera_plural(1) == "kamera"
        assert edr._camera_plural(2) == "kamery"
        assert edr._camera_plural(4) == "kamery"
        assert edr._camera_plural(5) == "kamier"
        assert edr._camera_plural(7) == "kamier"
        assert edr._camera_plural(0) == "kamier"


# ---------------------------------------------------------------------------
# CLI routing — --json-chunks emits the SHORT summary; plain emits the FULL detail
# ---------------------------------------------------------------------------

class TestCliRouting:
    def _run(self, extra):
        script = _SCRIPTS / "e2e_discord_report.py"
        fixture = _FIXTURES / _PASS_FIXTURE
        cmd = [sys.executable, str(script), "--json", str(fixture), "--run-id", "1104689227",
               "--event", "CI PR gate", "--duration", "300"] + extra
        out = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return out.stdout

    def test_json_chunks_emits_the_short_summary_not_the_wall(self):
        stdout = self._run(["--json-chunks"])
        chunks = json.loads(stdout)
        assert isinstance(chunks, list) and chunks
        joined = "\n".join(chunks)
        assert joined.splitlines()[0].startswith("✅ E2E TEST PREŠIEL")
        # the whole Discord body must be tiny now — not the 60+-line wall
        assert len(joined.splitlines()) <= 3
        assert "❌" not in joined
        # nothing but pure JSON on stdout (the caller captures 2>&1 and jq-parses it)
        assert stdout.strip().startswith("[")

    def test_plain_mode_still_emits_the_full_detail_for_the_ci_log(self):
        stdout = self._run([])
        # the detailed renderer is preserved as the plain/CI-log path — it still has the sections.
        assert "1️⃣" in stdout
        assert "Celkový verdikt" in stdout

    def test_run_url_is_rendered_in_the_summary_when_supplied(self):
        stdout = self._run(["--json-chunks", "--run-url", "https://example/actions/runs/999"])
        chunks = json.loads(stdout)
        assert "https://example/actions/runs/999" in "\n".join(chunks)


# ---------------------------------------------------------------------------
# #1142 STRICT flips — delivery spread + cadence uniformity + imag presence now BLOCK
# ---------------------------------------------------------------------------

class TestStrictFlips1142:
    def test_delivery_spread_blocks_on_a_1142_shape_verdict(self):
        # #1142: the DELIVERY-side spread now folds blocking (its block carries gates_overall_pass).
        v = {
            "overall_pass": False,
            "all_cambox_delivery_latency": {
                "spread_gate_pass": False,
                "gates_overall_pass": True,
                "cross_camera_spread_ms": 85.0,
            },
        }
        failures = edr._blocking_failures(v)
        assert any("doručovacej latencie" in label.lower() or "rozptyl doruč" in label.lower()
                   for label, _ in failures), failures
        # …and it must NOT ALSO be listed as report-only (auto-follow, no double-count).
        assert not any("doručen" in n.lower() for n in edr._report_only_tripped(v))

    def test_delivery_spread_stays_report_only_on_a_pre_1142_verdict(self):
        # A pre-#1142 verdict has no gates_overall_pass on the delivery block → still report-only.
        v = {
            "overall_pass": True,
            "all_cambox_delivery_latency": {"spread_gate_pass": False, "cross_camera_spread_ms": 85.0},
        }
        assert edr._blocking_failures(v) == []
        assert any("doručen" in n.lower() for n in edr._report_only_tripped(v))

    def test_cadence_uniformity_floor_blocks(self):
        v = {
            "overall_pass": False,
            "all_cambox_continuity": {
                "cadence_uniformity_gate": {
                    "pass": False, "gates_overall_pass": True, "worst_uniform_fraction": 0.70,
                },
            },
        }
        assert any("plynulý pohyb" in label.lower() or "rovnomernosť" in label.lower()
                   for label, _ in edr._blocking_failures(v))

    def test_cadence_uniformity_report_only_when_seam_off_is_not_blocking(self):
        v = {
            "overall_pass": True,
            "all_cambox_continuity": {
                "cadence_uniformity_gate": {"pass": False, "gates_overall_pass": False,
                                            "worst_uniform_fraction": 0.70},
            },
        }
        assert edr._blocking_failures(v) == []

    def test_imag_leg_not_verified_blocks_unless_offline_acked(self):
        # A run that silently skipped imag (verified=false, not acked) REDs — the honesty flip.
        v_red = {
            "overall_pass": False,
            "full_chain": {
                "imag_leg_verified": False,
                "imag_leg_verified_offline_acked": False,
                "imag_leg_verified_gates_overall_pass": True,
            },
        }
        assert any("nebola overená" in label.lower() for label, _ in edr._blocking_failures(v_red))
        # The ONE sanctioned skip: operator-offline-acked imag → NOT a blocking failure.
        v_ack = {
            "overall_pass": True,
            "full_chain": {
                "imag_leg_verified": False,
                "imag_leg_verified_offline_acked": True,
                "imag_leg_verified_gates_overall_pass": True,
            },
        }
        assert not any("nebola overená" in label.lower() for label, _ in edr._blocking_failures(v_ack))

    def test_imag_presence_term_blocks_but_content_term_stays_report_only(self):
        # A #1142-shape verdict: the imag PRESENCE term fails (blocking) while a CONTENT failure is
        # report-only. The presence failure must be a blocking gate; the content failure must NOT.
        v = {
            "overall_pass": False,
            "full_chain": {
                "imag_leg_verified": True,
                "loss": {"imag": {"imag_presence_pass": False, "imag_content_pass": True,
                                  "gates_overall_pass": True, "content_gates_overall_pass": False}},
            },
        }
        assert any("prezenčná kontrola" in label.lower() for label, _ in edr._blocking_failures(v))

    def test_imag_content_failure_is_report_only_never_blocking(self):
        # Only the imag per-frame CONTENT fails (presence OK) → report-only, never a blocking ❌.
        v = {
            "overall_pass": True,
            "all_cambox_continuity": {"imag": {"overall_pass": False, "gates_overall_pass": False}},
            "full_chain": {
                "imag_leg_verified": True,
                "loss": {"imag": {"imag_presence_pass": True, "imag_content_pass": False,
                                  "gates_overall_pass": True, "content_gates_overall_pass": False}},
            },
        }
        assert edr._blocking_failures(v) == []
        assert any("imag" in n.lower() for n in edr._report_only_tripped(v))

    def test_summary_never_renders_a_report_only_imag_content_failure_as_a_cross(self):
        # The owner's angry directive: a report-only failure must NEVER render as ❌ on the phone.
        v = {
            "overall_pass": True,
            "all_cambox_continuity": {"overall_pass": True,
                                      "imag": {"overall_pass": False, "gates_overall_pass": False}},
            "full_chain": {"zero_loss": True, "imag_leg_verified": True,
                           "loss": {"imag": {"imag_presence_pass": True, "imag_content_pass": False,
                                             "gates_overall_pass": True,
                                             "content_gates_overall_pass": False}}},
        }
        summary = edr.compose_summary(v, {"run_id": "x"})
        assert "❌" not in summary
