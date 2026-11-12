"""issue 1485: the `dante-input timing:` re-anchors and dlink's pump WARN are counted, report-only.

Both are timing signals: a pump WARN that lost blocks also raises `rebootstraps` (gated), a missed=0 WARN
loses nothing (dstack 4.11.2026). So the E2E window grade and verify item 9 count them and never fail on them.
"""
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_strih_dante_input_1485 as base  # noqa: E402

CONT = base.CONT
TIMING = "12:00:15.000: dante-input timing: re-anchored after a 21 ms gap"
PUMP = ("12:00:16.000: dante-input dlink:  WARN strih-obs: engine pump lost or overran blocks this second "
        "overruns=1 missed=0 max_process_us=2700")


def test_the_window_counts_timing_and_pump_warn_lines_report_only():
    w = [base._status("12:00:00.000", 100), TIMING, "mark", base._status("12:00:10.000", 200),
         TIMING, PUMP, PUMP, "stop", base._status("12:00:20.000", 300), base._status("12:00:30.000", 400),
         TIMING, PUMP]
    r = CONT.decide(w, 2, 7)
    assert r["verdict"] == "PASS", r
    assert r["report_only"] == {"timing_lines": 1, "pump_warn_lines": 2}, r
    text = "\n".join(CONT.summary_lines(r))
    assert "timing (report-only): 1 `dante-input timing:` line(s), 2 pump WARN line(s)" in text, text


def test_a_clean_window_reports_zero():
    r = CONT.decide(base._window(), 1, 3)
    assert r["report_only"] == {"timing_lines": 0, "pump_warn_lines": 0}, r


def test_item9_notes_the_counts_without_failing(tmp_path):
    r = base._grade(tmp_path, [base.START, TIMING, PUMP, PUMP, base._status("12:00:10.000", 4800)])
    assert r.returncode == 0, r.stderr
    assert "BAD" not in r.stdout, r.stdout
    assert ("NOTE (program-dante) timing (report-only): 1 'dante-input timing:' line(s), "
            "2 pump WARN line(s) in this OBS log") in r.stdout, r.stdout


def test_item9_notes_zero_on_a_quiet_log(tmp_path):
    r = base._grade(tmp_path, [base.START, base._status("12:00:10.000", 4800)])
    assert ("NOTE (program-dante) timing (report-only): 0 'dante-input timing:' line(s), "
            "0 pump WARN line(s) in this OBS log") in r.stdout, r.stdout
