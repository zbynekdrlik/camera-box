"""#813 -- unit tests for scripts/avsync_lineup.py, the PURE decision core for the measurement
A/V-sync LINE's GO/NO-GO (pre-event assert) + stream-state-bound liveness alarm.

Trigger: two silent-failure incidents. (1) 2026-07-22: the measurement watchdog was dead the whole
event and nobody noticed. (2) 2026-08-17: the measurement audio chain went digitally silent (~-91 dB)
while the watchdog PROCESS stayed alive (heartbeat FRESH), caught only ~7h later at the #748 E2E
preflight. The existing dev1 avsync-heartbeat-alert-watchdog.sh alarms on staleness only + always.

CRITICAL: the fixtures below use the REAL heartbeat vocabulary. avsync-watchdog.ps1 writes
`measured: db=<max_volume> <last line of av_sync_measure.py>`. av_sync_measure.py (verified: zero
hits for `unknown`/`candidates`) prints `[stamp] UNMEASURABLE window (... band/graphics segments are
expected to skip)` for BOTH silent audio AND a normal no-face band segment -- so the SyncNet text
CANNOT distinguish them. The discriminator is the audio dB (silence ~-91 dB, a live QPSK marker
~-5 dB), which avsync-watchdog.ps1 now prefixes as `db=`. These tests pin exactly that.
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import avsync_lineup as al  # noqa: E402

# --- real heartbeat status strings (byte-shaped like avsync-watchdog.ps1's Write-Heartbeat) -------
HB_OK = "measured: db=-5.4 [2026-08-17 08:00:00] AV offset +0 fr (+0 ms) conf 8.2 :: A/V sync OK (offset 0 ms)"
HB_MISALIGNED = ("measured: db=-5.4 [2026-08-17 08:00:00] AV offset +2 fr (+80 ms) conf 5.1 :: "
                 "audio predbieha video o ~80 ms -> ZNIZ '2ME PGM' latency o 80")
# THE 2026-08-17 case: audio digitally silent, so the grab succeeds but the reading is UNMEASURABLE
# AND the level is ~-91 dB. A fresh, "measured:" heartbeat that is nonetheless a DEAD line.
HB_SILENT = ("measured: db=-91.0 [2026-08-17 08:00:00] UNMEASURABLE window (best confidence 3.2 < 4.0"
             " - no usable face/lips; band/graphics segments are expected to skip)")
# an ORDINARY band/graphics segment: no face to lock (UNMEASURABLE) but the audio IS present. This
# MUST NOT page -- the instrument is alive, SyncNet just had nothing to measure.
HB_BAND_SEGMENT = ("measured: db=-5.4 [2026-08-17 08:00:00] UNMEASURABLE window (best confidence 3.2 <"
                   " 4.0 - no usable face/lips; band/graphics segments are expected to skip)")
HB_TIMEOUT = "measured: db=-5.4 TIMEOUT: av_sync_measure.py did not complete within 180s -- killed"
HB_NO_DB = "measured: [2026-08-17 08:00:00] AV offset +0 fr (+0 ms) conf 8.2 :: A/V sync OK (offset 0 ms)"
HB_NO_SIGNAL = "no-signal: grab failed: ffmpeg rc=-5 (relay/stream down)"


# ---------------------------------------------------------------------------
# heartbeat_fresh -- fail-CLOSED.
# ---------------------------------------------------------------------------


def test_heartbeat_fresh_within_window():
    assert al.heartbeat_fresh(1000, 1100, 300) is True


def test_heartbeat_fresh_exactly_at_window_boundary_is_fresh():
    assert al.heartbeat_fresh(1000, 1300, 300) is True


def test_heartbeat_stale_past_window():
    assert al.heartbeat_fresh(1000, 1301, 300) is False


def test_heartbeat_none_or_nonnumeric_epoch_is_not_fresh():
    assert al.heartbeat_fresh(None, 1100, 300) is False
    assert al.heartbeat_fresh("", 1100, 300) is False
    assert al.heartbeat_fresh("abc", 1100, 300) is False


def test_heartbeat_negative_age_clock_skew_is_not_fresh():
    assert al.heartbeat_fresh(2000, 1000, 300) is False


# ---------------------------------------------------------------------------
# audio dB parsing + presence (the real content-liveness signal).
# ---------------------------------------------------------------------------


def test_audio_db_parsed_from_a_measured_heartbeat():
    assert al.audio_db_from_status(HB_OK) == -5.4
    assert al.audio_db_from_status(HB_SILENT) == -91.0


def test_audio_db_none_when_absent_or_unreadable():
    assert al.audio_db_from_status(HB_NO_DB) is None
    assert al.audio_db_from_status("measured: db=unreadable [stamp] ...") is None
    assert al.audio_db_from_status("") is None
    assert al.audio_db_from_status(None) is None


def test_audio_present_true_above_floor_false_below():
    assert al.audio_present(HB_OK) is True         # -5.4 >= -60
    assert al.audio_present(HB_SILENT) is False     # -91.0 < -60


def test_audio_present_fail_closed_when_db_unreadable():
    assert al.audio_present(HB_NO_DB) is False


def test_audio_present_exactly_at_floor_is_present():
    assert al.audio_present("measured: db=-60 [stamp] A/V sync OK") is True


# ---------------------------------------------------------------------------
# status_is_healthy_measured -- a VALID reading (measured + present + not wedged).
# ---------------------------------------------------------------------------


def test_status_healthy_for_in_sync_and_misaligned_with_audio_present():
    assert al.status_is_healthy_measured(HB_OK) is True
    assert al.status_is_healthy_measured(HB_MISALIGNED) is True


def test_status_healthy_for_a_band_segment_when_audio_is_present():
    # UNMEASURABLE (no face) but audio present -> the instrument is alive -> VALID, must not page.
    assert al.status_is_healthy_measured(HB_BAND_SEGMENT) is True


def test_status_NOT_healthy_for_silent_audio_the_2026_08_17_case():
    # UNMEASURABLE AND db < -60 -> silent audio -> dead line -> INVALID.
    assert al.status_is_healthy_measured(HB_SILENT) is False


def test_status_NOT_healthy_for_timeout():
    assert al.status_is_healthy_measured(HB_TIMEOUT) is False


def test_status_NOT_healthy_without_a_db_reading():
    assert al.status_is_healthy_measured(HB_NO_DB) is False


def test_status_NOT_healthy_for_no_signal_or_empty():
    assert al.status_is_healthy_measured(HB_NO_SIGNAL) is False
    assert al.status_is_healthy_measured("") is False
    assert al.status_is_healthy_measured(None) is False


def test_measured_vs_no_signal_prefix_classification():
    assert al.is_measured_heartbeat(HB_OK) is True
    assert al.is_measured_heartbeat(HB_NO_SIGNAL) is False
    assert al.is_no_signal_heartbeat(HB_NO_SIGNAL) is True
    assert al.is_no_signal_heartbeat(HB_OK) is False


# ---------------------------------------------------------------------------
# stream_is_live -> True / False / None.
# ---------------------------------------------------------------------------


def test_stream_is_live_bool_and_string_variants():
    assert al.stream_is_live(True) is True
    assert al.stream_is_live(False) is False
    assert al.stream_is_live("True") is True
    assert al.stream_is_live("false") is False


def test_stream_is_live_none_or_garbage_is_unknown():
    assert al.stream_is_live(None) is None
    assert al.stream_is_live("???") is None


# ---------------------------------------------------------------------------
# preflight_verdict -- pre-event GO/NO-GO of the measurement line.
# ---------------------------------------------------------------------------


def _preflight_go_facts():
    return {
        "heartbeat_epoch": 1000,
        "now": 1100,
        "preflight_stale_s": 300,
        "heartbeat_status": HB_OK,
        "forwarder_present": True,
        "discord_ping_http": 200,
        "stream_output_active": True,
    }


def test_preflight_all_green_is_go():
    go, reasons = al.preflight_verdict(_preflight_go_facts())
    assert go is True and reasons == []


def test_preflight_go_when_stream_off_at_assert_time_with_a_no_signal_heartbeat():
    # before the stream starts, the heartbeat is a fresh no-signal (grab fails) -> the audio check is
    # N/A, but the infra (fresh process, forwarder, discord, WS-readable) must still pass -> GO.
    f = _preflight_go_facts()
    f["heartbeat_status"] = HB_NO_SIGNAL
    f["stream_output_active"] = False  # a definite read (not None) -> WS works
    go, reasons = al.preflight_verdict(f)
    assert go is True, reasons


def test_preflight_stale_heartbeat_is_no_go():
    f = _preflight_go_facts()
    f["now"] = 5000
    go, reasons = al.preflight_verdict(f)
    assert go is False and any("heartbeat" in r.lower() for r in reasons)


def test_preflight_live_but_silent_audio_is_no_go():
    f = _preflight_go_facts()
    f["heartbeat_status"] = HB_SILENT
    go, reasons = al.preflight_verdict(f)
    assert go is False and any("ticha" in r.lower() for r in reasons)


def test_preflight_forwarder_down_is_no_go():
    f = _preflight_go_facts()
    f["forwarder_present"] = False
    go, reasons = al.preflight_verdict(f)
    assert go is False and any("forwarder" in r.lower() for r in reasons)


def test_preflight_discord_not_delivered_is_no_go():
    f = _preflight_go_facts()
    f["discord_ping_http"] = 403
    go, reasons = al.preflight_verdict(f)
    assert go is False and any("discord" in r.lower() for r in reasons)


def test_preflight_ws_unreadable_is_no_go():
    # #3: a None stream-state read means the run-time alarm's stream gate can't work -> NO-GO.
    f = _preflight_go_facts()
    f["stream_output_active"] = None
    go, reasons = al.preflight_verdict(f)
    assert go is False and any("outputactive" in r.lower() for r in reasons)


# ---------------------------------------------------------------------------
# liveness_alarm -- the run-time alarm BOUND TO STREAM STATE (the incident bar).
# ---------------------------------------------------------------------------


def _live_facts():
    return {
        "stream_output_active": True,
        "heartbeat_epoch": 1000,
        "now": 1100,
        "stale_s": 1200,
        "heartbeat_status": HB_OK,
    }


def test_liveness_ok_when_stream_live_and_line_healthy():
    action, _, sig = al.liveness_alarm(_live_facts())
    assert action == "OK" and sig == "ok"


def test_liveness_ok_for_a_band_segment_with_audio_present_no_false_page():
    f = _live_facts()
    f["heartbeat_status"] = HB_BAND_SEGMENT
    action, _, _ = al.liveness_alarm(f)
    assert action == "OK"


def test_liveness_ALARM_when_stream_live_and_content_silent_the_2026_08_17_case():
    # THE BAR: fresh "measured:" heartbeat, silent audio (db=-91). Must ALARM.
    f = _live_facts()
    f["heartbeat_status"] = HB_SILENT
    action, reason, sig = al.liveness_alarm(f)
    assert action == "ALARM" and sig == "no-audio"
    assert "treba zasah" in reason


def test_liveness_ALARM_on_silent_audio_even_when_ws_read_is_broken():
    # #3 robustness: a fresh "measured:" heartbeat proves the stream is publishing (the grab
    # succeeded), so silent audio ALARMS even if outputActive can't be read (None).
    f = _live_facts()
    f["heartbeat_status"] = HB_SILENT
    f["stream_output_active"] = None
    action, _, sig = al.liveness_alarm(f)
    assert action == "ALARM" and sig == "no-audio"


def test_liveness_ALARM_when_measured_but_timeout():
    f = _live_facts()
    f["heartbeat_status"] = HB_TIMEOUT
    action, _, sig = al.liveness_alarm(f)
    assert action == "ALARM" and sig == "wedged"


def test_liveness_ALARM_when_stream_live_and_heartbeat_stale():
    f = _live_facts()
    f["heartbeat_status"] = HB_NO_SIGNAL  # process still writing but nothing to measure...
    f["now"] = 1000 + 1201                # ...and now the process is stale too
    action, _, sig = al.liveness_alarm(f)
    assert action == "ALARM" and sig == "stale"


def test_liveness_ALARM_when_stream_live_and_no_signal_grab_failed():
    f = _live_facts()
    f["heartbeat_status"] = HB_NO_SIGNAL
    action, _, sig = al.liveness_alarm(f)
    assert action == "ALARM" and sig == "no-signal"


def test_liveness_SUPPRESSED_when_stream_off_air_even_with_a_dead_line():
    f = _live_facts()
    f["stream_output_active"] = False
    f["heartbeat_status"] = HB_NO_SIGNAL
    f["now"] = 99999
    action, _, sig = al.liveness_alarm(f)
    assert action == "SUPPRESSED" and sig == "off"


def test_liveness_SUPPRESSED_when_stream_state_unknown_and_line_down():
    f = _live_facts()
    f["stream_output_active"] = None
    f["heartbeat_status"] = HB_NO_SIGNAL
    action, _, sig = al.liveness_alarm(f)
    assert action == "SUPPRESSED" and sig == "unknown"
