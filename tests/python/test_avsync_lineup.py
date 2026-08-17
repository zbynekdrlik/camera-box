"""#813 -- unit tests for scripts/avsync_lineup.py, the PURE decision core for the measurement
A/V-sync LINE's GO/NO-GO (pre-event assert) + stream-state-bound liveness alarm.

Trigger: two silent-failure incidents. (1) 2026-07-22: the measurement watchdog was dead the whole
event and nobody noticed ("neprisla ani jedna hlaska") -- silence was indistinguishable from "content
can't be measured". (2) 2026-08-17: the measurement audio chain went digitally silent (~-91 dB) while
the watchdog PROCESS stayed alive (heartbeat fresh), caught only ~7h later at the #748 E2E preflight.
The existing dev1 avsync-heartbeat-alert-watchdog.sh alarms on heartbeat STALENESS only, and
UNCONDITIONALLY (not bound to stream state) -- so it (a) would NOT have paged today (heartbeat was
fresh) and (b) can't tell a legitimately-off box from a dead watchdog during a live event.

These tests exercise avsync_lineup.py directly (no subprocess, no network) -- pure functions on
already-gathered fact dicts, exactly as the CLI is called after the watchdog shell assembles facts.
The heartbeat status vocabulary mirrored here is the REAL one written by scripts/avsync-watchdog.ps1
(Write-Heartbeat) + parsed by scripts/lib/avsync-heartbeat.sh:
  "no-signal: <reason>"          -> dead relay / stale-clip (the #814 case)
  "measured: TIMEOUT: ..."       -> wedged watchdog (av_sync_measure.py killed at 180s)
  "measured: A/V sync OK ..."    -> a live, in-sync reading (HEALTHY)
  "measured: ... ZNIZ/ZVYS ..."  -> a live, misaligned reading (still HEALTHY -- the line is alive)
  "measured: ... unknown, candidates: 0" -> silent/undecodable content on a SUCCESSFUL grab (TODAY)
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import avsync_lineup as al  # noqa: E402


# ---------------------------------------------------------------------------
# heartbeat_fresh -- fail-CLOSED (missing/corrupt/negative-age/too-old = NOT fresh),
# mirroring scripts/lib/avsync-heartbeat.sh's avsync_heartbeat_is_stale contract.
# ---------------------------------------------------------------------------


def test_heartbeat_fresh_within_window():
    assert al.heartbeat_fresh(1000, 1100, 300) is True


def test_heartbeat_fresh_exactly_at_window_boundary_is_fresh():
    assert al.heartbeat_fresh(1000, 1300, 300) is True


def test_heartbeat_stale_past_window():
    assert al.heartbeat_fresh(1000, 1301, 300) is False


def test_heartbeat_none_epoch_is_not_fresh():
    assert al.heartbeat_fresh(None, 1100, 300) is False


def test_heartbeat_nonnumeric_epoch_is_not_fresh():
    assert al.heartbeat_fresh("", 1100, 300) is False
    assert al.heartbeat_fresh("abc", 1100, 300) is False


def test_heartbeat_negative_age_clock_skew_is_not_fresh():
    # a heartbeat stamped in the future (corrupt/clock skew) must NOT read fresh
    assert al.heartbeat_fresh(2000, 1000, 300) is False


# ---------------------------------------------------------------------------
# status_is_healthy_measured -- a REAL, present, decodable reading only.
# ---------------------------------------------------------------------------


def test_status_measured_in_sync_is_healthy():
    assert al.status_is_healthy_measured("measured: A/V sync OK (offset 0 ms)") is True


def test_status_measured_misaligned_verdict_is_still_healthy():
    # a misalignment recommendation still PROVES the chain is alive -- it is a real reading
    assert al.status_is_healthy_measured("measured: [2026-08-17 08:00:00] :: -> ZNIZ '2ME PGM' latency o 80") is True


def test_status_measured_timeout_is_not_healthy():
    assert al.status_is_healthy_measured(
        "measured: TIMEOUT: av_sync_measure.py did not complete within 180s -- killed") is False


def test_status_measured_unknown_silent_content_is_not_healthy_TODAY():
    # THE 2026-08-17 case: silent audio -> grab succeeds -> measurement runs ->
    # unknown / candidates: 0. Heartbeat is FRESH and starts with "measured: " but the
    # CONTENT is dead. This MUST be NO-GO so the stream-bound alarm pages.
    assert al.status_is_healthy_measured(
        'measured: av_sync verdict: "unknown", candidates: 0') is False


def test_status_no_signal_is_not_healthy():
    assert al.status_is_healthy_measured("no-signal: grab failed: ffmpeg rc=-5 (relay/stream down)") is False


def test_status_empty_or_none_is_not_healthy():
    assert al.status_is_healthy_measured("") is False
    assert al.status_is_healthy_measured(None) is False


def test_status_bare_measured_without_prefix_space_is_not_healthy():
    # "measured" without the ": " reading is not a real verdict line
    assert al.status_is_healthy_measured("measuredsomething") is False


# ---------------------------------------------------------------------------
# stream_is_live -> True / False / None (unknown).
# ---------------------------------------------------------------------------


def test_stream_is_live_bool_true():
    assert al.stream_is_live(True) is True


def test_stream_is_live_bool_false():
    assert al.stream_is_live(False) is False


def test_stream_is_live_string_variants():
    assert al.stream_is_live("True") is True
    assert al.stream_is_live("false") is False
    assert al.stream_is_live("1") is True
    assert al.stream_is_live("0") is False


def test_stream_is_live_none_or_garbage_is_unknown():
    assert al.stream_is_live(None) is None
    assert al.stream_is_live("???") is None


# ---------------------------------------------------------------------------
# preflight_verdict -- the pre-event GO/NO-GO of the measurement line.
# ---------------------------------------------------------------------------


def _preflight_go_facts():
    return {
        "heartbeat_epoch": 1000,
        "now": 1100,
        "preflight_stale_s": 300,
        "heartbeat_status": "measured: A/V sync OK (offset 0 ms)",
        "forwarder_present": True,
        "discord_ping_http": 200,
    }


def test_preflight_all_green_is_go():
    go, reasons = al.preflight_verdict(_preflight_go_facts())
    assert go is True
    assert reasons == []


def test_preflight_stale_heartbeat_is_no_go():
    f = _preflight_go_facts()
    f["now"] = 5000  # way past the window
    go, reasons = al.preflight_verdict(f)
    assert go is False
    assert any("heartbeat" in r.lower() for r in reasons)


def test_preflight_silent_content_is_no_go():
    f = _preflight_go_facts()
    f["heartbeat_status"] = 'measured: av_sync verdict: "unknown", candidates: 0'
    go, reasons = al.preflight_verdict(f)
    assert go is False
    assert any("meranie" in r.lower() for r in reasons)


def test_preflight_forwarder_down_is_no_go():
    f = _preflight_go_facts()
    f["forwarder_present"] = False
    go, reasons = al.preflight_verdict(f)
    assert go is False
    assert any("forwarder" in r.lower() for r in reasons)


def test_preflight_discord_not_delivered_is_no_go():
    f = _preflight_go_facts()
    f["discord_ping_http"] = 403
    go, reasons = al.preflight_verdict(f)
    assert go is False
    assert any("discord" in r.lower() for r in reasons)


def test_preflight_missing_discord_ping_is_no_go():
    f = _preflight_go_facts()
    f["discord_ping_http"] = None
    go, reasons = al.preflight_verdict(f)
    assert go is False


# ---------------------------------------------------------------------------
# liveness_alarm -- the run-time alarm BOUND TO STREAM STATE (the incident bar).
# ---------------------------------------------------------------------------


def _live_facts():
    return {
        "stream_output_active": True,
        "heartbeat_epoch": 1000,
        "now": 1100,
        "stale_s": 1200,
        "heartbeat_status": "measured: A/V sync OK (offset 0 ms)",
    }


def test_liveness_ok_when_stream_live_and_line_healthy():
    action, _ = al.liveness_alarm(_live_facts())
    assert action == "OK"


def test_liveness_ALARM_when_stream_live_and_content_silent_TODAY():
    # THE BAR: stream emitting, heartbeat FRESH, but status = unknown/candidates:0
    # (silent audio). The existing staleness-only watchdog misses this entirely.
    f = _live_facts()
    f["heartbeat_status"] = 'measured: av_sync verdict: "unknown", candidates: 0'
    action, reason = al.liveness_alarm(f)
    assert action == "ALARM"
    assert "treba zásah" in reason


def test_liveness_ALARM_when_stream_live_and_heartbeat_stale():
    f = _live_facts()
    f["now"] = 1000 + 1201  # just past the 20-min window
    action, reason = al.liveness_alarm(f)
    assert action == "ALARM"


def test_liveness_SUPPRESSED_when_stream_not_live():
    # stream not emitting -> box off / between events -> silence is fine, no page
    f = _live_facts()
    f["stream_output_active"] = False
    f["heartbeat_status"] = "no-signal: relay down"  # even a dead line does not page when off-air
    f["now"] = 99999
    action, _ = al.liveness_alarm(f)
    assert action == "SUPPRESSED"


def test_liveness_SUPPRESSED_when_stream_state_unknown():
    # OBS-WS unreachable -> owned by network-reach/obs-liveness watchdogs, do not double-page
    f = _live_facts()
    f["stream_output_active"] = None
    f["heartbeat_status"] = "no-signal: relay down"
    action, _ = al.liveness_alarm(f)
    assert action == "SUPPRESSED"


def test_liveness_ALARM_when_stream_live_and_no_signal_status():
    f = _live_facts()
    f["heartbeat_status"] = "no-signal: grab failed: ffmpeg rc=-5 (relay/stream down)"
    action, _ = al.liveness_alarm(f)
    assert action == "ALARM"
