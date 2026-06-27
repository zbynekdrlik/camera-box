"""#266 — unit tests for the NDI-receive stuck-state watchdog.

The watchdog is a SAFETY tool that earned distrust (the first cut generated 14+ false-neg/false-pos
findings over two review rounds), so every discriminator is pinned with a real behavioral test:

  * the genlock-fifo audit parser (incl. the #148 `holds=` field inserted between underruns/overruns),
  * the WINDOW read that lets "was delivering then froze" be told apart from "never delivered",
  * the STUCK-vs-IDLE classification: 0-fps-was-delivering→alert, never-delivered→quiet,
    overrun-dominant-hold→quiet, frozen-after-delivering→alert,
  * named/required-source rules (below-floor→alert, vanished→alert),
  * dantesync runaway checked INDEPENDENTLY of NDI audit (alerts with no audit at all),
  * the exit-code table (0 ok / 1 alert+delivered / 2 no-signal / 3 alert+delivery-FAILED),
  * the timestamp robustness edges (dt-floor, midnight-wrap-only-on-large-negative, missing-ts).

These pin the deterministic core; the raw inputs (OBS log + dantesync CPU) are gathered read-only by
the poller (win-* MCP / SMB).
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


# ---------------------------------------------------------------------------
# fixtures / builders
# ---------------------------------------------------------------------------

def real_audit(ts, source, received, consumed=None, underruns=0, holds=0, overruns=0, depth=2, peak=4):
    """A line in the EXACT current genlock_audit_log format (obs-source.c) — `holds=` (added by
    #148) sits between `underruns=` and `overruns=`, so a positional parser would misread overruns."""
    consumed = received if consumed is None else consumed
    return (
        f"{ts}: genlock-fifo audit '{source}': received={received} consumed={consumed} "
        f"underruns={underruns} holds={holds} overruns={overruns} depth={depth} peak={peak} "
        f"latency_ms=3 (≈0 frames @ 30.000fps) src_latency_ms=0 global_latency_ms=3 "
        f"preload=0 (=0 ms) reserve_ms=3 cap=8 empty_run=0 (re-arm@4) "
        f"ts_present=0 ts_due=0 ts_head_skew_ms=0 (#70/#97/#126/#148/#184/#235/#245)"
    )


def _s(received, underruns=0, overruns=0, depth=2, ts=None, source="X"):
    return wd.AuditSample(
        source=source, received=received, consumed=received, underruns=underruns,
        overruns=overruns, depth=depth, ts_secs=ts,
    )


def _win(source, specs, base_ts=12 * 3600.0, step=5.0):
    """Build a {source: [samples]} window. `specs` = list of (received, underruns, overruns, depth);
    timestamps auto-assigned `step` seconds apart so dt is well-defined."""
    samples = [
        wd.AuditSample(
            source=source, received=r, consumed=r, underruns=u, overruns=o, depth=d,
            ts_secs=base_ts + i * step,
        )
        for i, (r, u, o, d) in enumerate(specs)
    ]
    return {source: samples}


# ---------------------------------------------------------------------------
# parsing
# ---------------------------------------------------------------------------

def test_parse_real_format_with_holds_field():
    # The #148 `holds=` field is between underruns and overruns — overruns must NOT read as holds.
    s = wd.parse_audit_line(
        real_audit("12:00:05.000", "NDI cam5", received=150, consumed=149,
                   underruns=7, holds=11, overruns=3, depth=1)
    )
    assert s is not None
    assert s.source == "NDI cam5"
    assert s.received == 150 and s.consumed == 149
    assert s.underruns == 7  # NOT 11 (holds)
    assert s.overruns == 3  # NOT 11 (holds)
    assert s.depth == 1
    assert s.ts_secs == pytest.approx(12 * 3600 + 5.0)


def test_parse_without_timestamp_prefix_sets_ts_none():
    line = "genlock-fifo audit 'NDI 2ME PGM': received=99 consumed=99 underruns=0 holds=0 overruns=0 depth=1 peak=1"
    s = wd.parse_audit_line(line)
    assert s is not None and s.source == "NDI 2ME PGM" and s.received == 99
    assert s.ts_secs is None


def test_parse_ignores_non_audit_lines():
    assert wd.parse_audit_line("12:00:00.000: info: some other obs message") is None
    assert wd.parse_audit_line("") is None


def test_parse_returns_none_on_missing_required_field():
    # An audit line truncated before `overruns=` is unusable, not a half-parsed sample.
    assert wd.parse_audit_line("12:00:00.000: genlock-fifo audit 'x': received=1 consumed=1 underruns=0") is None


def test_window_per_source_keeps_last_window_chronological():
    lines = [real_audit(f"12:00:{i:02d}.000", "NDI cam5", received=i * 30) for i in range(10)]
    win = wd.window_per_source(lines, window=4)
    assert set(win) == {"NDI cam5"}
    got = win["NDI cam5"]
    assert len(got) == 4
    assert [s.received for s in got] == [6 * 30, 7 * 30, 8 * 30, 9 * 30]  # the LAST 4, in order


# ---------------------------------------------------------------------------
# STUCK vs IDLE classification (the core — default scan-all mode)
# ---------------------------------------------------------------------------

def test_healthy_30fps_no_alert():
    win = _win("NDI cam5", [(0, 0, 0, 2), (150, 0, 0, 2), (300, 0, 0, 2), (450, 0, 0, 2)])
    assert wd.evaluate("strih", win, dantesync_cpu=20.0) == []


def test_degraded_10fps_starved_alerts():
    # The #265 collapse: receive at ~10 fps (50 frames / 5 s), underruns climbing, FIFO drained.
    win = _win("NDI cam5", [
        (0, 0, 0, 0), (50, 200, 0, 0), (100, 400, 0, 0), (150, 600, 0, 0),
        (200, 800, 0, 0), (250, 1000, 0, 0),
    ])
    alerts = wd.evaluate("strih", win, dantesync_cpu=None)
    assert [a.kind for a in alerts] == ["stuck"]
    assert "NDI cam5" in alerts[0].detail


def test_frozen_0fps_was_delivering_alerts_in_default_mode():
    # The WORST case + the old code's worst miss: a broadcast input that WAS delivering (30 fps
    # earlier in the window) then FROZE to 0 fps with the consumer starving (underruns climbing on a
    # drained FIFO). It MUST alert even though it is unnamed (default scan-all).
    win = _win("NDI cam5", [
        (0, 0, 0, 2), (150, 0, 0, 2), (300, 0, 0, 2),  # delivered 30 fps
        (300, 500, 0, 0), (300, 1000, 0, 0), (300, 1500, 0, 0),  # then froze + starved
    ])
    alerts = wd.evaluate("strih", win, dantesync_cpu=None)
    assert [a.kind for a in alerts] == ["stuck"]


def test_never_delivered_idle_no_alert():
    # A source flat at 0 from the FIRST sample (a parked probe/monitor input never on the wire) is
    # benign idle — not the stuck state. No alert.
    win = _win("idle-probe", [(0, 0, 0, 0)] * 6)
    assert wd.evaluate("strih", win, dantesync_cpu=None) == []


def test_never_delivered_even_with_underruns_no_alert():
    # THE window/trajectory gate (impossible to get right with only the last 2 samples): a source
    # that NEVER advanced (received flat 0 across the whole window) must stay quiet even if underruns
    # happen to climb — it was never delivering, so it is not a #265 collapse.
    win = _win("idle-probe", [
        (0, 0, 0, 0), (0, 500, 0, 0), (0, 1000, 0, 0), (0, 1500, 0, 0), (0, 2000, 0, 0),
    ])
    assert wd.evaluate("strih", win, dantesync_cpu=None) == []


def test_overrun_dominant_hold_no_alert():
    # overruns is the holding-vs-starved discriminator: frames arrive faster than consumed → the
    # FIFO overflows (overruns climb, depth near cap). Even below the fps floor, an overrun-dominant
    # source is HOLDING, not starved → no alert.
    win = _win("parked", [
        (0, 0, 0, 8), (50, 100, 1000, 8), (100, 200, 2000, 8), (150, 300, 3000, 8),
        (200, 400, 4000, 8), (250, 500, 5000, 8),
    ])
    assert wd.evaluate("strih", win, dantesync_cpu=None) == []


def test_low_fps_but_not_starved_no_alert():
    # A source legitimately delivering below the floor but NOT starving (underruns flat, FIFO healthy)
    # is not the #265 collapse — the redesign must not false-alarm it (the old code alerted on any
    # below-floor delivery).
    win = _win("low-by-design", [(0, 0, 0, 4), (50, 0, 0, 4), (100, 0, 0, 4), (150, 0, 0, 4)])
    assert wd.evaluate("strih", win, dantesync_cpu=None) == []


def test_sustained_freeze_beyond_window_alerts():
    # THE real-rig sustained collapse (requirement 1, "full freeze must alert"): `received` frozen at
    # a HIGH cumulative value for the WHOLE window (the delivering samples already scrolled out, so the
    # in-window peak_fps has decayed to ~0) while underruns climb on a drained FIFO. The cumulative
    # `received` (41421 >> delivered_frames) is what proves this is a real feed, not a never-delivered
    # idle → ALERT. The old within-window-only logic MISSED exactly this (peak_fps=0 → "never
    # delivered").
    win = _win("NDI cam5", [
        (41421, 614, 0, 0), (41421, 765, 0, 0), (41421, 916, 0, 0), (41421, 1067, 0, 0),
        (41421, 1218, 0, 0), (41421, 1369, 0, 0),
    ])
    alerts = wd.evaluate("strih", win, dantesync_cpu=None)
    assert [a.kind for a in alerts] == ["stuck"]


def test_frozen_low_cumulative_received_no_alert():
    # A source frozen with underruns climbing but whose cumulative `received` never exceeded the
    # delivered-frames floor (a probe that got a couple frames then never a real feed) → not a real
    # feed → no alert. Pins the delivered_frames discriminator (boundary of sustained-freeze alert).
    win = _win("blip-probe", [
        (5, 100, 0, 0), (5, 300, 0, 0), (5, 500, 0, 0), (5, 700, 0, 0), (5, 900, 0, 0),
    ])
    assert wd.evaluate("strih", win, dantesync_cpu=None) == []


# ---------------------------------------------------------------------------
# staleness — the live-rig false-alert safeguard
# ---------------------------------------------------------------------------

def _sustained_frozen_win(source="NDI cam5", base_ts=12 * 3600.0):
    # received frozen at a high value, underruns climbing, depth drained — a stuck signature.
    return _win(source, [
        (41421, 614, 0, 0), (41421, 765, 0, 0), (41421, 916, 0, 0),
        (41421, 1067, 0, 0), (41421, 1218, 0, 0), (41421, 1369, 0, 0),
    ], base_ts=base_ts)


def test_fresh_stuck_source_alerts_with_now():
    win = _sustained_frozen_win(base_ts=12 * 3600.0)  # newest sample at 12:00:25
    now = 12 * 3600 + 30  # 5 s later → fresh
    alerts = wd.evaluate("strih", win, dantesync_cpu=None, now_secs=now)
    assert [a.kind for a in alerts] == ["stuck"]


def test_stale_source_skipped_with_now():
    # The SAME stuck signature, but the newest sample is ~1.5 h old (OBS stopped logging it — it left
    # program). Stale → skipped → no alert. This is what stops a stopped-log's frozen tail from
    # false-alarming a healthy box.
    win = _sustained_frozen_win(base_ts=12 * 3600.0)  # newest at 12:00:25
    now = 13 * 3600 + 1800  # 13:30:00 → ~1.5 h later → stale
    assert wd.evaluate("strih", win, dantesync_cpu=None, now_secs=now) == []


def test_named_stale_source_vanished():
    win = _sustained_frozen_win(source="NDI cam5", base_ts=12 * 3600.0)
    now = 13 * 3600 + 1800  # stale
    alerts = wd.evaluate("strih", win, dantesync_cpu=None, monitored_sources=["NDI cam5"], now_secs=now)
    assert [a.kind for a in alerts] == ["vanished"]


def test_staleness_off_when_now_none():
    # now_secs None → staleness filtering OFF → the stuck signature is judged on its merits (alert).
    win = _sustained_frozen_win()
    assert [a.kind for a in wd.evaluate("strih", win, dantesync_cpu=None, now_secs=None)] == ["stuck"]


def test_sample_age_normal_and_skew_and_midnight():
    # normal: 30 s old
    assert wd.sample_age_secs(12 * 3600.0, 12 * 3600 + 30) == pytest.approx(30.0)
    # small clock skew (sample 2 s "ahead" of now) → small negative, treated fresh by abs() in caller
    assert wd.sample_age_secs(12 * 3600 + 2, 12 * 3600.0) == pytest.approx(-2.0)
    # midnight wrap: newest 23:59:55 yesterday, now 00:00:05 today → 10 s, not ~86390
    age = wd.sample_age_secs(23 * 3600 + 59 * 60 + 55, 5.0)
    assert age == pytest.approx(10.0)


def test_is_stale_uses_abs_for_large_gap():
    samples = _sustained_frozen_win()["NDI cam5"]  # newest at 12:00:25
    assert wd.is_stale(samples, now_secs=12 * 3600 + 30, max_age=60) is False  # 5 s → fresh
    assert wd.is_stale(samples, now_secs=12 * 3600 + 600, max_age=60) is True  # ~9.5 min → stale
    assert wd.is_stale(samples, now_secs=None, max_age=60) is False  # off


# ---------------------------------------------------------------------------
# real strih rig data (2026-06-26, post-19:45-reboot OBS log) — genuine #265 collapse
# ---------------------------------------------------------------------------

def test_real_rig_healthy_head_no_alert():
    # The first audit samples after the 19:45 reboot: received +151/5s == clean 30 fps, underruns ~0.
    win = _win("NDI cam5", [
        (152, 0, 0, 1), (303, 0, 0, 1), (454, 0, 0, 1), (605, 0, 0, 1), (756, 0, 0, 1),
    ], base_ts=19 * 3600 + 48 * 60 + 5, step=5.033)
    assert wd.evaluate("strih", win, dantesync_cpu=0.0) == []


def test_real_rig_collapse_transition_alerts():
    # The real 20:11 collapse: delivering 30 fps (41040→41342) then FROZE at 41421 with underruns
    # climbing ~30/s on depth=0 — the exact #265 starved-genlock signature. Must alert.
    win = _win("NDI cam5", [
        (41040, 542, 0, 1), (41191, 542, 0, 1), (41342, 542, 0, 1),
        (41421, 614, 0, 0), (41421, 765, 0, 0), (41421, 916, 0, 0),
    ], base_ts=20 * 3600 + 11 * 60 + 5, step=5.033)
    alerts = wd.evaluate("strih", win, dantesync_cpu=0.0)
    assert [a.kind for a in alerts] == ["stuck"]


# ---------------------------------------------------------------------------
# named / required source rules
# ---------------------------------------------------------------------------

def test_named_source_at_zero_fps_alerts():
    # An EXPLICITLY declared broadcast input that drops to ~0 fps IS the worst case — it alerts
    # regardless of the starvation trajectory (a required cam at 0 fps is bad, period).
    win = _win("NDI cam5", [(100, 0, 0, 2)] * 3)  # frozen, no starvation signature
    alerts = wd.evaluate("strih", win, dantesync_cpu=None, monitored_sources=["NDI cam5"])
    assert [a.kind for a in alerts] == ["stuck"]


def test_named_source_vanished_alerts():
    # A declared input that produced no usable audit pair (NDI dropped / fifo stopped logging) is
    # absent from the window — it must still alert (we can no longer confirm a required input alive).
    alerts = wd.evaluate("strih", {}, dantesync_cpu=None, monitored_sources=["NDI cam5"])
    assert [a.kind for a in alerts] == ["vanished"]
    assert "NDI cam5" in alerts[0].detail


def test_named_source_single_sample_vanished():
    win = {"NDI cam5": [_s(100, source="NDI cam5")]}  # only 1 sample → no pair
    alerts = wd.evaluate("strih", win, dantesync_cpu=None, monitored_sources=["NDI cam5"])
    assert [a.kind for a in alerts] == ["vanished"]


def test_present_named_source_does_not_vanish_or_alert():
    win = _win("NDI cam5", [(0, 0, 0, 2), (150, 0, 0, 2), (300, 0, 0, 2)])
    alerts = wd.evaluate("strih", win, dantesync_cpu=None, monitored_sources=["NDI cam5"])
    assert alerts == []


# ---------------------------------------------------------------------------
# dantesync runaway — INDEPENDENT of NDI audit
# ---------------------------------------------------------------------------

def test_dantesync_runaway_with_no_audit_alerts():
    # A pegged dantesync.exe must alert even with NO genlock-fifo audit data at all.
    alerts = wd.evaluate("stream", {}, dantesync_cpu=98.0)
    assert [a.kind for a in alerts] == ["dantesync_runaway"]


def test_dantesync_below_threshold_no_alert():
    assert wd.evaluate("stream", {}, dantesync_cpu=40.0) == []


def test_dantesync_runaway_added_alongside_ndi_stuck():
    win = _win("NDI cam5", [
        (0, 0, 0, 0), (150, 0, 0, 2), (300, 0, 0, 2),
        (300, 600, 0, 0), (300, 1200, 0, 0), (300, 1800, 0, 0),
    ])
    kinds = [a.kind for a in wd.evaluate("strih", win, dantesync_cpu=99.0)]
    assert "stuck" in kinds and "dantesync_runaway" in kinds


# ---------------------------------------------------------------------------
# timestamp robustness (dt-floor, midnight-wrap, missing-ts)
# ---------------------------------------------------------------------------

def test_pair_dt_floors_sub_second_double_log():
    prev, curr = _s(0, ts=12 * 3600 + 5.0), _s(150, ts=12 * 3600 + 5.2)  # 0.2 s apart
    assert wd.pair_dt(prev, curr) == pytest.approx(wd.MIN_PAIR_DT)


def test_received_fps_dt_floor_caps_rate():
    prev, curr = _s(0, ts=12 * 3600 + 5.0), _s(150, ts=12 * 3600 + 5.2)
    # 150 frames over 0.2 s would read 750 fps without the floor; floored to MIN_PAIR_DT.
    assert wd.received_fps(prev, curr) == pytest.approx(150.0 / wd.MIN_PAIR_DT)


def test_dt_floor_prevents_false_starvation_alert():
    # A frozen-after-delivering source whose RECENT pairs are sub-second double-logs with tiny
    # underrun ticks. Without the dt-floor those +3 ticks over 0.1 s read as 30/s (false starvation);
    # the floor makes them ~3/s (below the 10/s floor) → correctly NO alert.
    base = 12 * 3600.0
    samples = [
        _s(0, underruns=0, depth=2, ts=base, source="NDI cam5"),
        _s(150, underruns=0, depth=2, ts=base + 5, source="NDI cam5"),  # delivered 30 fps
        _s(300, underruns=10, depth=0, ts=base + 10, source="NDI cam5"),  # froze
        _s(300, underruns=10, depth=0, ts=base + 15.0, source="NDI cam5"),
        _s(300, underruns=13, depth=0, ts=base + 15.1, source="NDI cam5"),  # +3 over 0.1 s
        _s(300, underruns=16, depth=0, ts=base + 15.2, source="NDI cam5"),  # +3 over 0.1 s
    ]
    alerts = wd.evaluate("strih", {"NDI cam5": samples}, dantesync_cpu=None)
    assert alerts == []  # underrun rate floored to ~3/s, below the 10/s floor


def test_received_fps_midnight_wrap_only_on_large_negative():
    # Genuine midnight rollover (backwards by ~a day) → +86400.
    prev = wd.parse_audit_line(real_audit("23:59:58.000", "x", received=0))
    curr = wd.parse_audit_line(real_audit("00:00:03.000", "x", received=150))  # 5 s across midnight
    assert wd.received_fps(prev, curr) == pytest.approx(30.0)


def test_small_backwards_jitter_is_not_treated_as_wrap():
    # A tiny backwards jitter (-0.1 s) must NOT be read as a day wrap (which would give a ~86400 s dt
    # and a near-0 fps); it falls back to the cadence → a sane rate.
    prev, curr = _s(0, ts=12 * 3600 + 5.0), _s(150, ts=12 * 3600 + 4.9)  # 0.1 s backwards
    fps = wd.received_fps(prev, curr)
    assert fps == pytest.approx(150.0 / wd.AUDIT_INTERVAL_SECS)
    assert fps > 1.0  # NOT a ~0.0017 fps day-wrap artifact


def test_estimate_cadence_from_timestamps():
    samples = [_s(i * 100, ts=12 * 3600 + i * 10.0) for i in range(4)]  # 10 s apart
    assert wd.estimate_cadence(samples) == pytest.approx(10.0)


def test_estimate_cadence_falls_back_when_no_timestamps():
    samples = [_s(i * 100, ts=None) for i in range(4)]
    assert wd.estimate_cadence(samples) == pytest.approx(wd.AUDIT_INTERVAL_SECS)


def test_pair_dt_missing_timestamp_uses_cadence_not_fabricated():
    prev, curr = _s(0, ts=None), _s(150, ts=None)
    assert wd.pair_dt(prev, curr, cadence=10.0) == pytest.approx(10.0)


# ---------------------------------------------------------------------------
# compose body
# ---------------------------------------------------------------------------

def test_compose_alert_body_is_actionable_slovak():
    win = _win("NDI cam5", [
        (0, 0, 0, 0), (150, 0, 0, 2), (300, 0, 0, 2),
        (300, 600, 0, 0), (300, 1200, 0, 0), (300, 1800, 0, 0),
    ])
    alerts = wd.evaluate("strih", win, dantesync_cpu=99.0)
    body = wd.compose_alert_body("strih", "10.77.9.202", alerts)
    assert "watchdog" in body
    assert "strih" in body and "10.77.9.202" in body
    assert "reštart" in body.lower()  # documented recovery
    assert "#265" in body


# ---------------------------------------------------------------------------
# main() exit codes + delivery
# ---------------------------------------------------------------------------

def _write_log(tmp_path, lines):
    p = tmp_path / "obs.log"
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return str(p)


def _frozen_log(tmp_path):
    # A broadcast input that delivered 30 fps then FROZE with the consumer starving → real alert.
    return _write_log(tmp_path, [
        real_audit("12:00:00.000", "NDI cam5", received=0, underruns=0, depth=2),
        real_audit("12:00:05.000", "NDI cam5", received=150, underruns=0, depth=2),
        real_audit("12:00:10.000", "NDI cam5", received=300, underruns=0, depth=2),
        real_audit("12:00:15.000", "NDI cam5", received=300, underruns=500, depth=0),
        real_audit("12:00:20.000", "NDI cam5", received=300, underruns=1000, depth=0),
        real_audit("12:00:25.000", "NDI cam5", received=300, underruns=1500, depth=0),
    ])


def _healthy_log(tmp_path):
    return _write_log(tmp_path, [
        real_audit(f"12:00:{i * 5:02d}.000", "NDI cam5", received=i * 150, underruns=0, depth=2)
        for i in range(6)
    ])


def test_main_no_signal_returns_2(tmp_path, capsys):
    # No obs-log, no --source, no --dantesync-cpu → nothing to judge → exit 2.
    rc = wd.main(["--box", "strih"])
    assert rc == 2
    assert "no signal" in capsys.readouterr().err.lower()


def test_main_dantesync_only_healthy_is_not_no_signal(tmp_path):
    # A dantesync reading (even healthy) IS a signal → never exit 2; healthy → exit 0.
    rc = wd.main(["--box", "strih", "--dantesync-cpu", "40"])
    assert rc == 0


def test_main_dantesync_runaway_no_audit_returns_1(tmp_path):
    # The #266 requirement: a runaway dantesync alerts even with no NDI audit data at all.
    rc = wd.main(["--box", "stream", "--dantesync-cpu", "98", "--dry-run"])
    assert rc == 1


def test_main_healthy_returns_0(tmp_path):
    # --now just after the log so the source is FRESH (else the default-clock staleness skips the
    # 12:00 fixture and the test is meaningless).
    rc = wd.main(["--box", "strih", "--obs-log", _healthy_log(tmp_path), "--dantesync-cpu", "20",
                  "--now", "12:00:30"])
    assert rc == 0


def test_main_alert_delivery_failure_returns_3(tmp_path, monkeypatch, capsys):
    # send_alert must inspect the airuleset-notify returncode. A FAILED Discord delivery must fail
    # LOUDLY with the distinct exit 3, never a silent "alert fired" (exit 1).
    class _Res:
        returncode = 7

    monkeypatch.setattr(wd.subprocess, "run", lambda *a, **k: _Res())
    rc = wd.main(["--box", "strih", "--obs-log", _frozen_log(tmp_path), "--now", "12:00:30"])
    assert rc == 3
    assert "deliver" in capsys.readouterr().err.lower()


def test_main_alert_delivery_success_returns_1(tmp_path, monkeypatch):
    class _Res:
        returncode = 0

    monkeypatch.setattr(wd.subprocess, "run", lambda *a, **k: _Res())
    rc = wd.main(["--box", "strih", "--obs-log", _frozen_log(tmp_path), "--now", "12:00:30"])
    assert rc == 1


def test_main_stale_frozen_log_no_false_alert(tmp_path):
    # THE live-rig safeguard: a frozen+starving tail from HOURS ago (OBS stopped logging the source
    # when it left program) must NOT false-alarm — with --now well past the log, the source is stale
    # → skipped → healthy exit 0. (This is the exact strih state observed during verification.)
    rc = wd.main(["--box", "strih", "--obs-log", _frozen_log(tmp_path), "--now", "13:30:00",
                  "--dantesync-cpu", "5"])
    assert rc == 0


def test_main_fresh_stuck_alerts(tmp_path):
    # The SAME frozen+starving log, but FRESH (--now just after) → a genuinely stuck on-program
    # source → alert (dry-run → exit 1).
    rc = wd.main(["--box", "strih", "--obs-log", _frozen_log(tmp_path), "--now", "12:00:30",
                  "--dry-run"])
    assert rc == 1


def test_main_obs_log_read_failure_does_not_suppress_dantesync(tmp_path, capsys):
    # A missing/unreadable obs-log must NOT suppress the independent dantesync runaway alert (#266).
    rc = wd.main(["--box", "strih", "--obs-log", "/nonexistent/obs.log",
                  "--dantesync-cpu", "97", "--dry-run"])
    assert rc == 1
    assert "cannot read obs log" in capsys.readouterr().err.lower()
