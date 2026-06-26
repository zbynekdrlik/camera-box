"""#266 — unit tests for the NDI-receive stuck-state watchdog parse + threshold logic.

The watchdog's value is the deterministic parse/rate/threshold core; the raw inputs (OBS log +
dantesync CPU) are gathered read-only by the poller (win-* MCP / SMB). These tests pin that core:
the genlock-fifo audit parser, the received-fps rate, and every alert threshold (incl. the #70
idle-source guard so a parked probe source never false-alarms).
"""
import importlib.util
import os

import pytest

# Load scripts/stuck-watchdog.py (hyphenated filename → import by path).
_HERE = os.path.dirname(__file__)
_WD_PATH = os.path.normpath(os.path.join(_HERE, "..", "..", "scripts", "stuck-watchdog.py"))
_spec = importlib.util.spec_from_file_location("stuck_watchdog", _WD_PATH)
wd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(wd)


# A real audit line (vendor/obs-studio/libobs/obs-source.c::genlock_audit_log), with the OBS
# `HH:MM:SS.mmm` log-line timestamp prefix.
def audit(ts, source, received, consumed=0, underruns=0, overruns=0):
    return (
        f"{ts}: genlock-fifo audit '{source}': received={received} "
        f"consumed={consumed} underruns={underruns} overruns={overruns} depth=2 peak=4 "
        f"latency_ms=3 (≈0 frames @ 30.000fps) src_latency_ms=0 global_latency_ms=3 "
        f"preload=0 (=0 ms) reserve_ms=3 cap=8 empty_run=0 (re-arm@4) (#70/#97/#126/#184/#235/#245)"
    )


# ---- parse ----

def test_parse_audit_line_extracts_fields():
    s = wd.parse_audit_line(audit("12:00:05.000", "NDI cam5", 150, 150, 7, 1))
    assert s is not None
    assert s.source == "NDI cam5"
    assert s.received == 150
    assert s.consumed == 150
    assert s.underruns == 7
    assert s.overruns == 1
    assert s.ts_secs == pytest.approx(12 * 3600 + 5.0)


def test_parse_audit_line_without_timestamp_prefix():
    line = "genlock-fifo audit 'NDI 2ME PGM': received=99 consumed=99 underruns=0 overruns=0 depth=1 peak=1"
    s = wd.parse_audit_line(line)
    assert s is not None and s.source == "NDI 2ME PGM" and s.received == 99
    assert s.ts_secs is None


def test_parse_ignores_non_audit_lines():
    assert wd.parse_audit_line("12:00:00.000 info: some other obs message") is None
    assert wd.parse_audit_line("") is None


def test_last_two_per_source_keeps_newest_pair():
    lines = [
        audit("12:00:00.000", "NDI cam5", 0),
        audit("12:00:05.000", "NDI cam5", 150),
        audit("12:00:10.000", "NDI cam5", 300),  # newest pair = (150, 300)
        audit("12:00:05.000", "other", 10),  # only one sample → dropped
    ]
    pairs = wd.last_two_per_source(lines)
    assert set(pairs) == {"NDI cam5"}
    prev, curr = pairs["NDI cam5"]
    assert prev.received == 150 and curr.received == 300


# ---- received-fps ----

def test_received_fps_uses_log_timestamps():
    prev = wd.parse_audit_line(audit("12:00:00.000", "x", 0))
    curr = wd.parse_audit_line(audit("12:00:05.000", "x", 150))  # 150 frames / 5 s = 30 fps
    assert wd.received_fps(prev, curr) == pytest.approx(30.0)


def test_received_fps_falls_back_to_5s_interval_without_timestamps():
    prev = wd.AuditSample("x", received=0, consumed=0, underruns=0, overruns=0, ts_secs=None)
    curr = wd.AuditSample("x", received=50, consumed=0, underruns=0, overruns=0, ts_secs=None)
    assert wd.received_fps(prev, curr) == pytest.approx(50.0 / wd.AUDIT_INTERVAL_SECS)


def test_received_fps_none_on_counter_reset():
    prev = wd.parse_audit_line(audit("12:00:05.000", "x", 300))
    curr = wd.parse_audit_line(audit("12:00:10.000", "x", 10))  # OBS restart → counter reset
    assert wd.received_fps(prev, curr) is None


def test_received_fps_handles_midnight_wrap():
    prev = wd.parse_audit_line(audit("23:59:58.000", "x", 0))
    curr = wd.parse_audit_line(audit("00:00:03.000", "x", 150))  # 5 s across midnight
    assert wd.received_fps(prev, curr) == pytest.approx(30.0)


# ---- thresholds ----

def _pair(source, r0, r1, u0=0, u1=0):
    p = wd.AuditSample(source, received=r0, consumed=r0, underruns=u0, overruns=0, ts_secs=0.0)
    c = wd.AuditSample(source, received=r1, consumed=r1, underruns=u1, overruns=0, ts_secs=5.0)
    return {source: (p, c)}


def test_healthy_30fps_no_alert():
    alerts = wd.evaluate("strih", _pair("NDI cam5", 0, 150), dantesync_cpu=20.0)
    assert alerts == []


def test_stuck_10fps_alerts_low_fps():
    # The #265 stuck state: receive collapsed to ~10 fps (50 frames / 5 s).
    alerts = wd.evaluate("strih", _pair("NDI cam5", 0, 50), dantesync_cpu=None)
    assert [a.kind for a in alerts] == ["low_fps"]
    assert "9." in alerts[0].detail or "10." in alerts[0].detail


def test_underrun_spike_alerts():
    # Receive still ~30 fps but underruns jumped 1500 in one 5 s window → genlock starving.
    alerts = wd.evaluate("strih", _pair("NDI cam5", 0, 150, u0=10, u1=1510), dantesync_cpu=None)
    assert "underrun_spike" in [a.kind for a in alerts]


def test_dantesync_runaway_alerts():
    alerts = wd.evaluate("stream", {}, dantesync_cpu=98.0)
    assert [a.kind for a in alerts] == ["dantesync_runaway"]


def test_dantesync_below_threshold_no_alert():
    assert wd.evaluate("stream", {}, dantesync_cpu=40.0) == []


def test_idle_unnamed_source_is_not_false_alarmed():
    # #70: a parked/probe NDI source sits at ~0 fps with underrun spam. Unnamed → treated as idle,
    # so neither low_fps nor underrun_spike fires.
    alerts = wd.evaluate("strih", _pair("idle-probe", 0, 0, u0=0, u1=9000), dantesync_cpu=None)
    assert alerts == []


def test_named_source_at_zero_fps_alerts_even_if_idle_floor():
    # An EXPLICITLY declared broadcast input that drops to ~0 fps IS the worst case — must alert.
    alerts = wd.evaluate(
        "strih", _pair("NDI cam5", 0, 0), dantesync_cpu=None, monitored_sources=["NDI cam5"]
    )
    assert "low_fps" in [a.kind for a in alerts]


def test_compose_alert_body_is_actionable_slovak():
    alerts = wd.evaluate("strih", _pair("NDI cam5", 0, 50, u0=0, u1=2000), dantesync_cpu=99.0)
    body = wd.compose_alert_body("strih", "10.77.9.202", alerts)
    assert "watchdog" in body
    assert "strih" in body and "10.77.9.202" in body
    assert "reštart" in body.lower()  # documented recovery
    assert "#265" in body
