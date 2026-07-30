"""#707 EVENT-FORENSICS — unit tests for scripts/event-forensics-dossier.py, the pure collector
that groups already-pulled strih genlock-FIFO-audit lines + per-camera sender journal lines under
each residual copy/gap event (src/residual_events.rs) by wall-clock second.

No SSH/MCP, no real rig logs — every test feeds synthetic log lines / verdict dicts, matching this
repo's established pure-module test pattern (test_switch_schedule.py, test_e2e_discord_report.py).
"""
import importlib.util
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "event_forensics_dossier",
        SCRIPTS / "event-forensics-dossier.py",
    )
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()
extract_time_of_day = _mod.extract_time_of_day
epoch_to_time_of_day = _mod.epoch_to_time_of_day
match_lines_for_event = _mod.match_lines_for_event
assemble_dossier = _mod.assemble_dossier


# ---------------------------------------------------------------------------
# extract_time_of_day
# ---------------------------------------------------------------------------

class TestExtractTimeOfDay:
    def test_plain_hh_mm_ss(self):
        assert extract_time_of_day("23:17:57  received=810475") == 23 * 3600 + 17 * 60 + 57

    def test_hh_mm_ss_with_fraction(self):
        v = extract_time_of_day("23:17:57.567  ctx=[...]")
        assert abs(v - (23 * 3600 + 17 * 60 + 57.567)) < 1e-6

    def test_journalctl_style_line_with_month_day_prefix(self):
        # journalctl's default format: "Jul 13 23:17:55 cam1 camera-box[1234]: Streaming: ..."
        v = extract_time_of_day("Jul 13 23:17:55 cam1 camera-box[1234]: Streaming: ok")
        assert v == 23 * 3600 + 17 * 60 + 55

    def test_no_timestamp_returns_none(self):
        assert extract_time_of_day("no time here at all") is None

    def test_out_of_range_hour_or_minute_returns_none(self):
        # A stray "99:99:99"-shaped substring (e.g. from an unrelated numeric log field) must not
        # be misread as a valid time.
        assert extract_time_of_day("code=99:99:99 something") is None

    def test_first_match_wins_when_multiple_timestamps_present(self):
        v = extract_time_of_day("23:00:00 first, then 23:59:59 second")
        assert v == 23 * 3600


# ---------------------------------------------------------------------------
# epoch_to_time_of_day
# ---------------------------------------------------------------------------

class TestEpochToTimeOfDay:
    def test_zero_offset_matches_utc_time_of_day(self):
        # 2026-07-13T23:17:57Z -- epoch second computed directly (no datetime import needed for
        # the assertion itself, just verify the modulo-day arithmetic holds).
        epoch = 1_784_856_000  # an arbitrary epoch second
        expected = epoch % 86_400
        assert epoch_to_time_of_day(epoch, 0.0) == expected

    def test_positive_tz_offset_shifts_forward(self):
        epoch = 0  # 1970-01-01T00:00:00Z
        assert epoch_to_time_of_day(epoch, 2.0) == 2 * 3600

    def test_wraps_across_the_day_boundary(self):
        epoch = 86_399  # 23:59:59 UTC
        assert epoch_to_time_of_day(epoch, 1.0) == 3599  # wraps to 00:59:59


# ---------------------------------------------------------------------------
# match_lines_for_event
# ---------------------------------------------------------------------------

class TestMatchLinesForEvent:
    def test_matches_lines_within_tolerance(self):
        lines = [
            "23:17:56.000 depth=1",
            "23:17:57.500 depth=1",  # within 2s of 23:17:57.567
            "23:18:30.000 depth=1",  # far away
        ]
        event_tod = 23 * 3600 + 17 * 60 + 57.567
        matched = match_lines_for_event(event_tod, lines, tolerance_s=2.0)
        assert len(matched) == 2
        assert "23:18:30" not in matched[0] and "23:18:30" not in matched[1]

    def test_lines_without_a_timestamp_never_match(self):
        lines = ["no timestamp here", "23:17:57.500 depth=1"]
        matched = match_lines_for_event(23 * 3600 + 17 * 60 + 57.5, lines, tolerance_s=1.0)
        assert matched == ["23:17:57.500 depth=1"]

    def test_matches_across_the_midnight_wrap(self):
        lines = ["00:00:01.000 depth=1"]
        event_tod = 23 * 3600 + 59 * 60 + 59.5  # 23:59:59.5
        matched = match_lines_for_event(event_tod, lines, tolerance_s=2.0)
        assert matched == lines


# ---------------------------------------------------------------------------
# assemble_dossier
# ---------------------------------------------------------------------------

class TestAssembleDossier:
    def test_no_events_produces_an_empty_dossier(self):
        verdict = {"all_cambox_continuity": {"segments": [], "residual_events": []}}
        dossier = assemble_dossier(verdict)
        assert dossier["events"] == []
        assert dossier["summary"] == {"total": 0, "with_reason": 0, "open": 0}

    def test_attaches_matching_strih_and_camera_lines_to_each_event(self):
        verdict = {
            "all_cambox_continuity": {
                "residual_events": [
                    {
                        "kind": "copy",
                        "cambox": "cam1",
                        "frame_index": 5926,
                        "wall_clock_epoch_s": 1_784_856_000,
                    }
                ]
            }
        }
        # Build strih/camera lines whose time-of-day matches the event's epoch second exactly.
        tod = 1_784_856_000 % 86_400
        hh, rem = divmod(int(tod), 3600)
        mm, ss = divmod(rem, 60)
        ts = f"{hh:02d}:{mm:02d}:{ss:02d}"
        strih_lines = [f"{ts}.100  received=810475  depth=1", "12:00:00.000  unrelated"]
        camera_lines = {"CAM1": [f"Jul 13 {ts} cam1 camera-box[1]: Streaming: ok"]}
        dossier = assemble_dossier(verdict, strih_lines, camera_lines, tolerance_s=1.0)
        assert dossier["summary"]["total"] == 1
        ev = dossier["events"][0]
        assert ev["frame_index"] == 5926
        assert len(ev["strih_fifo_lines"]) == 1
        assert "810475" in ev["strih_fifo_lines"][0]
        assert len(ev["camera_journal_lines"]) == 1

    def test_falls_back_to_segments_when_no_top_level_residual_events(self):
        verdict = {
            "all_cambox_continuity": {
                "segments": [
                    {
                        "cambox": "cam2",
                        "residual_events": [
                            {"kind": "gap", "cambox": "cam2", "wall_clock_epoch_s": 100}
                        ],
                    }
                ]
            }
        }
        dossier = assemble_dossier(verdict)
        assert dossier["summary"]["total"] == 1

    def test_reason_is_carried_through_and_counted_in_summary(self):
        verdict = {
            "all_cambox_continuity": {
                "residual_events": [
                    {"kind": "copy", "cambox": "cam1", "wall_clock_epoch_s": 1, "reason": "known cause"},
                    {"kind": "gap", "cambox": "cam1", "wall_clock_epoch_s": 2},
                ]
            }
        }
        dossier = assemble_dossier(verdict)
        assert dossier["summary"] == {"total": 2, "with_reason": 1, "open": 1}
        assert dossier["events"][0]["reason"] == "known cause"
        assert dossier["events"][1].get("reason") is None

    def test_event_with_no_epoch_gets_no_matched_lines_but_is_still_reported(self):
        verdict = {
            "all_cambox_continuity": {
                "residual_events": [{"kind": "copy", "cambox": "cam1", "frame_index": 1}]
            }
        }
        dossier = assemble_dossier(verdict, strih_lines=["23:17:57.500 depth=1"])
        assert dossier["summary"]["total"] == 1
        assert dossier["events"][0]["strih_fifo_lines"] == []

    def test_cambox_lookup_is_case_insensitive(self):
        verdict = {
            "all_cambox_continuity": {
                "residual_events": [
                    {"kind": "copy", "cambox": "cam3", "wall_clock_epoch_s": 100}
                ]
            }
        }
        camera_lines = {"CAM3": ["Jan 01 00:01:40 cam3 camera-box[1]: Streaming: ok"]}
        dossier = assemble_dossier(verdict, camera_lines_by_cambox=camera_lines, tolerance_s=5.0)
        assert len(dossier["events"][0]["camera_journal_lines"]) == 1
