"""#1307 -- pure-decision tests for the dev1 dante-clock alert watchdog.

WHY: the camboxes cam1-7 are HEADLESS -- their only path to a human is dev1 -> Discord. On
2026-09-13 the Yamaha AIC128-D grandmaster's DHCP lease moved, every node's gm_allowlist stopped
matching, the whole fleet silently ran NTP-only for hours and nobody noticed until the release E2E
gate failed (#1307 / #1297). scripts/dantesync_clock_decision.py is the PURE kernel of the dev1
watchdog that reads each node's :8898/status and decides when to page -- no I/O, exhaustively
unit-testable (Tier-0, #557 kills cargo), the #1199 python-mirror precedent (genlock_lock_decision
/ audio_lag_decision / ndi_halving_decision).

Two families of invariant:
  1. analyze(status_json, box_reachable, grandmaster_ip) -> the per-node verdict, mirroring the
     E2E gate's clock-offset-guard.sh field semantics (ptp_locked_from_pipe_json / gm_matches_expected
     / ntp_master_step_storm_verdict): OK / NO_CLOCK / SKIP(unreachable) / UNKNOWN(unparseable),
     plus dantesync#114's clock_alarm forward-compat.
  2. The production-critical TIME-BUCKETED dedup cadence (owner ruling ROZHODNUTÉ #1307,
     2026-09-13: notify „dokolecka", not once) + grandmaster-change detection.
"""
import importlib.util
import pathlib

_MOD = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "dantesync_clock_decision.py"
_spec = importlib.util.spec_from_file_location("dantesync_clock_decision", _MOD)
dc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dc)


# A realistic healthy :8898/status body (the shape read live off the fleet, cf. tests/dantesync_gate.rs).
def _status(is_locked="true", mode="NANO", gm="10.77.9.230", storm="false",
            steps="0", alarm=None):
    parts = [
        '"offset_ns":164707', '"ntp_offset_us":1249', '"drift_ppm":-7.68',
        '"settled":true', '"updated_ts":1783647854',
    ]
    if is_locked is not None:
        parts.append(f'"is_locked":{is_locked}')
    if mode is not None:
        parts.append(f'"mode":"{mode}"')
    if gm is not None:
        parts.append(f'"gm_source_ip":"{gm}"')
    if storm is not None:
        parts.append(f'"ntp_step_storm":{storm}')
    if steps is not None:
        parts.append(f'"ntp_steps_last_hour":{steps}')
    if alarm is not None:
        parts.append(f'"clock_alarm":{alarm}')
    return "{" + ",".join(parts) + "}"


GM = "10.77.9.230"


# ------------------------------------------------------------------ analyze: verdict semantics
def test_ok_when_locked_correct_gm_no_storm():
    r = dc.analyze(_status(), 1, GM)
    assert r["verdict"] == "OK", r
    assert r["reason"] == dc.R_NONE


def test_no_clock_when_not_locked():
    r = dc.analyze(_status(is_locked="false", mode="ACQ"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_NOT_LOCKED in r["reason"]


def test_no_clock_when_mode_not_lock_or_nano():
    r = dc.analyze(_status(is_locked="true", mode="ACQ"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_NOT_LOCKED in r["reason"]


def test_lock_mode_accepts_both_nano_and_lock():
    assert dc.analyze(_status(mode="NANO"), 1, GM)["verdict"] == "OK"
    assert dc.analyze(_status(mode="LOCK"), 1, GM)["verdict"] == "OK"


def test_no_clock_when_gm_foreign():
    r = dc.analyze(_status(gm="10.77.7.109"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_WRONG_GM in r["reason"]


def test_no_clock_when_ntp_step_storm():
    r = dc.analyze(_status(storm="true", steps="165"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_STORM in r["reason"]
    # the steps/hour count is carried through for the reason text (never a re-hardcoded threshold).
    assert r["ntp_steps_last_hour"] == "165"


def test_skip_when_unreachable():
    # box_reachable != 1 -> SKIP without even parsing (defers to #1001; an OFF box never pages).
    r = dc.analyze("", 0, GM)
    assert r["verdict"] == "SKIP", r


def test_unknown_when_body_unparseable():
    assert dc.analyze("not json at all", 1, GM)["verdict"] == "UNKNOWN"
    assert dc.analyze("", 1, GM)["verdict"] == "UNKNOWN"


def test_unknown_when_no_clock_fields_present():
    # A reachable body with none of is_locked/mode/storm/clock_alarm -> nothing to judge -> UNKNOWN,
    # never a fabricated NO_CLOCK (never a false page on a stock/partial payload).
    body = '{"offset_ns":1,"settled":true,"updated_ts":1}'
    assert dc.analyze(body, 1, GM)["verdict"] == "UNKNOWN"


def test_gm_unknown_while_locked_is_ok_not_a_page():
    # gm_source_ip ABSENT but otherwise fully locked -> OK (report-first gm stance mirrors the E2E
    # gate's DANTESYNC_GATE_GM_ENFORCE=0 default); the false-page-safe direction. A genuinely lost
    # clock is is_locked=false (caught above), never this shape.
    r = dc.analyze(_status(gm=None), 1, GM)
    assert r["verdict"] == "OK", r


def test_gm_check_skipped_when_grandmaster_ip_empty():
    # No grandmaster IP passed (orchestrator could not resolve) -> the gm comparison is skipped, so
    # gm never triggers a page here; not-locked/storm still do.
    assert dc.analyze(_status(gm="10.77.7.109"), 1, "")["verdict"] == "OK"
    assert dc.analyze(_status(is_locked="false", mode="ACQ"), 1, "")["verdict"] == "NO_CLOCK"


# ------------------------------------------------------------------ analyze: clock_alarm (dantesync#114)
def test_clock_alarm_active_is_authoritative_no_clock():
    r = dc.analyze(_status(alarm='{"active":true,"since":1,"reason":"no_grandmaster"}'), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert r["reason"] == dc.R_CLOCK_ALARM


def test_clock_alarm_inactive_but_derived_no_clock_still_pages():
    # #114 says healthy, but the derived cross-check catches a not-locked node -> still NO_CLOCK.
    r = dc.analyze(_status(is_locked="false", mode="ACQ",
                           alarm='{"active":false}'), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r


def test_clock_alarm_field_name_is_a_single_constant():
    assert dc.CLOCK_ALARM_FIELD == "clock_alarm"


# ------------------------------------------------------------------ time-bucketed dedup cadence
def test_dedup_key_same_within_a_bucket():
    # same state at t and t+599 -> SAME key (an identical state edits the card, no re-ping).
    k0 = dc.dedup_key("dante-clock-cam1", 600000, 600)
    k1 = dc.dedup_key("dante-clock-cam1", 600599, 600)
    assert k0 == k1, (k0, k1)


def test_dedup_key_changes_at_next_bucket():
    # at t+600 -> a NEW key (a fresh ping while the fault persists -- „dokolecka").
    k0 = dc.dedup_key("dante-clock-cam1", 600000, 600)
    k2 = dc.dedup_key("dante-clock-cam1", 600600, 600)
    assert k0 != k2, (k0, k2)


def test_dedup_key_interval_floor_60():
    # an interval below the floor is clamped to 60, never a per-pass ping.
    a = dc.dedup_key("b", 100, 10)
    b = dc.dedup_key("b", 159, 10)
    c = dc.dedup_key("b", 160, 10)
    assert a == b and a != c, (a, b, c)


def test_dedup_key_invalid_interval_defaults():
    # garbage interval -> the 600s default, never a crash.
    assert dc.dedup_key("b", 0, "xxx") == dc.dedup_key("b", 599, "xxx")
    assert dc.dedup_key("b", 0, "xxx") != dc.dedup_key("b", 600, "xxx")


def test_dedup_key_carries_the_base():
    assert dc.dedup_key("dante-clock-strih", 0, 600).startswith("dante-clock-strih-")


# ------------------------------------------------------------------ grandmaster-change detection
def test_grandmaster_change_true_on_move():
    assert dc.grandmaster_change("10.77.9.184", "10.77.9.230") is True


def test_grandmaster_change_false_when_same():
    assert dc.grandmaster_change("10.77.9.230", "10.77.9.230") is False


def test_grandmaster_change_false_when_prev_empty():
    # first-ever pass (no persisted prior) is never a change.
    assert dc.grandmaster_change("", "10.77.9.230") is False
    assert dc.grandmaster_change("10.77.9.230", "") is False
