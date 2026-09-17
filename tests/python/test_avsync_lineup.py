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


# ---------------------------------------------------------------------------
# #1331 offset ALERT arm -- parse the SyncNet offset verdict out of a measured heartbeat and page
# when |offset| is out of band during a LIVE stream (a genuine, confident A/V rozladenie on air).
# Fixtures use the REAL av_sync_measure.py print shape (scripts/av_sync_measure.py:422:
#   f"[{stamp}] AV offset {offset_frames:+d} fr ({offset_ms:+d} ms) conf {conf:.1f} :: {verdict}")
# prefixed by avsync-watchdog.ps1 with `measured: db=<X> `.
# ---------------------------------------------------------------------------

# in-band (nonzero but < 60 ms), high confidence -> OK, must not page.
HB_IN_BAND = ("measured: db=-5.4 [2026-08-17 08:00:00] AV offset +1 fr (+40 ms) conf 8.0 :: "
              "audio predbieha video o ~40 ms -> ZNIZ '2ME PGM' latency o 40")
# out of band (>= 60 ms) but LOW confidence (the 2026-07-26 conf-3.6 garbage era) -> SUPPRESSED.
HB_MISALIGNED_LOWCONF = ("measured: db=-5.4 [2026-07-26 15:00:42] AV offset +2 fr (+80 ms) conf 3.6 "
                         ":: audio predbieha video o ~80 ms -> ZNIZ '2ME PGM' latency o 80")
# out of band, video-leading (negative), high confidence -> ALARM with the ZVYS advice.
HB_MISALIGNED_NEG = ("measured: db=-5.4 [2026-08-17 08:00:00] AV offset -3 fr (-120 ms) conf 9.0 :: "
                     "video predbieha audio o ~120 ms -> ZVYS '2ME PGM' latency o 120")
# a measured heartbeat whose offset text is corrupt -> no parseable verdict -> fail-CLOSED.
HB_GARBLED_OFFSET = "measured: db=-5.4 [2026-08-17 08:00:00] AV offset ?? fr (?? ms) conf x.y :: garbled"


# --- parse_offset -- fail-CLOSED on anything but a clean verdict --------------------------------


def test_parse_offset_reads_ms_and_conf_from_a_measured_verdict():
    assert al.parse_offset(HB_MISALIGNED) == (80, 5.1)
    assert al.parse_offset(HB_OK) == (0, 8.2)
    assert al.parse_offset(HB_MISALIGNED_NEG) == (-120, 9.0)
    assert al.parse_offset(HB_MISALIGNED_LOWCONF) == (80, 3.6)


def test_parse_offset_none_for_unmeasurable_or_no_signal_or_garbled():
    assert al.parse_offset(HB_SILENT) == (None, None)          # UNMEASURABLE band, no offset
    assert al.parse_offset(HB_BAND_SEGMENT) == (None, None)
    assert al.parse_offset(HB_NO_SIGNAL) == (None, None)
    assert al.parse_offset(HB_TIMEOUT) == (None, None)
    assert al.parse_offset(HB_GARBLED_OFFSET) == (None, None)


def test_parse_offset_none_for_empty_or_none():
    assert al.parse_offset("") == (None, None)
    assert al.parse_offset(None) == (None, None)


# --- offset_advice_from_status -----------------------------------------------------------------


def test_offset_advice_extracted_after_the_double_colon():
    assert al.offset_advice_from_status(HB_MISALIGNED) == (
        "audio predbieha video o ~80 ms -> ZNIZ '2ME PGM' latency o 80")
    assert al.offset_advice_from_status(HB_OK) == "A/V sync OK (offset 0 ms)"


def test_offset_advice_none_without_a_double_colon():
    assert al.offset_advice_from_status(HB_SILENT) is None
    assert al.offset_advice_from_status("") is None
    assert al.offset_advice_from_status(None) is None


# --- offset_alarm -- the run-time offset alert, BOUND TO STREAM STATE + quality-gated ------------


def _offset_facts():
    return {
        "stream_output_active": True,
        "heartbeat_epoch": 1000,
        "now": 1100,
        "stale_s": 1200,
        "heartbeat_status": HB_MISALIGNED,
        "offset_alarm_ms": 60,
        "offset_conf_floor": 4.0,
    }


def test_offset_ALARM_when_live_measured_over_threshold_and_confident():
    action, reason, sig = al.offset_alarm(_offset_facts())
    assert action == "ALARM" and sig == "offset"
    assert "ROZLADENE" in reason and "+80 ms" in reason and "ZNIZ" in reason


def test_offset_ALARM_negative_video_leading():
    f = _offset_facts()
    f["heartbeat_status"] = HB_MISALIGNED_NEG
    action, reason, sig = al.offset_alarm(f)
    assert action == "ALARM" and sig == "offset"
    assert "-120 ms" in reason and "ZVYS" in reason


def test_offset_OK_when_in_sync():
    f = _offset_facts()
    f["heartbeat_status"] = HB_OK
    action, _, sig = al.offset_alarm(f)
    assert action == "OK" and sig == "ok"


def test_offset_OK_when_nonzero_but_within_band():
    f = _offset_facts()
    f["heartbeat_status"] = HB_IN_BAND  # 40 ms < 60 ms
    action, _, sig = al.offset_alarm(f)
    assert action == "OK" and sig == "ok"


def test_offset_SUPPRESSED_low_confidence_never_pages_the_july_garbage():
    f = _offset_facts()
    f["heartbeat_status"] = HB_MISALIGNED_LOWCONF  # 80 ms but conf 3.6 < 4.0
    action, reason, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "low-conf"
    assert "3.6" in reason


def test_offset_SUPPRESSED_when_stream_off_air():
    f = _offset_facts()
    f["stream_output_active"] = False
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "not-live"


def test_offset_SUPPRESSED_when_stream_state_unknown():
    f = _offset_facts()
    f["stream_output_active"] = None
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "not-live"


def test_offset_SUPPRESSED_when_heartbeat_stale():
    f = _offset_facts()
    f["now"] = 1000 + 1201  # past the stale window
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "stale"


def test_offset_SUPPRESSED_for_a_band_segment_with_no_verdict():
    f = _offset_facts()
    f["heartbeat_status"] = HB_BAND_SEGMENT  # UNMEASURABLE, audio present, no offset
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "no-verdict"


def test_offset_SUPPRESSED_for_timeout_and_no_signal():
    f = _offset_facts()
    f["heartbeat_status"] = HB_TIMEOUT
    assert al.offset_alarm(f)[2] == "not-measured"
    f["heartbeat_status"] = HB_NO_SIGNAL
    assert al.offset_alarm(f)[2] == "not-measured"


def test_offset_SUPPRESSED_for_garbled_offset_text_fail_closed():
    f = _offset_facts()
    f["heartbeat_status"] = HB_GARBLED_OFFSET
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "no-verdict"


def test_offset_no_exception_on_empty_or_none_facts_fail_closed():
    # a missing/garbled fact bag must NEVER crash the pass -- fail-CLOSED to SUPPRESSED.
    for f in ({}, {"stream_output_active": None, "heartbeat_status": None},
              {"stream_output_active": True, "heartbeat_status": None, "heartbeat_epoch": None,
               "now": None}):
        action, _, _ = al.offset_alarm(f)
        assert action in ("OK", "ALARM", "SUPPRESSED")
        assert action != "ALARM"  # nothing to page on


def test_offset_threshold_is_env_overridable_via_facts():
    f = _offset_facts()  # 80 ms
    f["offset_alarm_ms"] = 100  # raise the band above the reading
    action, _, sig = al.offset_alarm(f)
    assert action == "OK" and sig == "ok"


def test_offset_conf_floor_is_env_overridable_via_facts():
    f = _offset_facts()  # conf 5.1
    f["offset_conf_floor"] = 6.0  # raise the floor above the reading
    action, _, sig = al.offset_alarm(f)
    assert action == "SUPPRESSED" and sig == "low-conf"


def test_offset_alarm_uses_documented_defaults_when_facts_omit_them():
    f = _offset_facts()
    del f["offset_alarm_ms"]
    del f["offset_conf_floor"]
    # HB_MISALIGNED = 80 ms conf 5.1 -> default 60 ms / floor 4.0 -> ALARM.
    action, _, sig = al.offset_alarm(f)
    assert action == "ALARM" and sig == "offset"
