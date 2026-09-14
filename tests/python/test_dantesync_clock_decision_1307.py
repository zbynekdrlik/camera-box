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
import subprocess

_MOD = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "dantesync_clock_decision.py"
_spec = importlib.util.spec_from_file_location("dantesync_clock_decision", _MOD)
dc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(dc)


# A realistic healthy :8898/status body (the shape read live off the fleet, cf. tests/dantesync_gate.rs).
def _status(is_locked="true", mode="NANO", gm="10.77.9.230", storm="false",
            steps="0", alarm=None, updated_ts=1783647854):
    parts = [
        '"offset_ns":164707', '"ntp_offset_us":1249', '"drift_ppm":-7.68',
        '"settled":true',
    ]
    if updated_ts is not None:
        parts.append(f'"updated_ts":{updated_ts}')
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


# ------------------------------------------------------------------ updated_ts freshness (mirror the gate)
# The E2E gate FAILS a reachable-but-STALE :8898/status (clock-offset-guard.sh pipe_json_freshness_
# verdict, #550/#591/#595): a wedged dantesync (HTTP thread alive, servo dead) serving a FROZEN
# is_locked:true must not read OK forever -- exactly the silent clock-loss the owner banned.
_FRESH = 300  # DANTE_CLOCK_FRESHNESS_S default (mirrors the gate's DANTESYNC_OFFSET_FRESHNESS_S)


def test_stale_updated_ts_is_no_clock_even_when_locked():
    # is_locked:true + mode NANO + correct gm, but updated_ts is far older than now -> STALE -> the
    # frozen "locked" reading is untrustworthy -> NO_CLOCK.
    body = _status(updated_ts=1000)
    r = dc.analyze(body, 1, GM, now=1000 + _FRESH + 60, freshness_s=_FRESH)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_STALE in r["reason"]


def test_fresh_updated_ts_within_window_is_ok():
    body = _status(updated_ts=1000)
    r = dc.analyze(body, 1, GM, now=1000 + 30, freshness_s=_FRESH)
    assert r["verdict"] == "OK", r


def test_absent_updated_ts_never_pages_on_freshness():
    # No updated_ts field -> freshness UNKNOWN -> never a stale page (false-page-safe); falls through
    # to the lock/gm/storm checks (healthy here -> OK).
    body = _status(updated_ts=None)
    assert dc.analyze(body, 1, GM, now=9_999_999_999, freshness_s=_FRESH)["verdict"] == "OK"


def test_no_now_skips_freshness_backward_compatible():
    # now omitted -> freshness not graded (the 3-arg call the sibling watchdogs use); a very old
    # updated_ts alone does not page.
    assert dc.analyze(_status(updated_ts=1000), 1, GM)["verdict"] == "OK"


def test_stale_minimal_payload_is_no_clock_not_unknown():
    # A payload carrying ONLY updated_ts (no is_locked/mode/storm/alarm) that is STALE is a judgeable
    # fault (NO_CLOCK stale), NOT UNKNOWN -- a frozen daemon serving a skeleton payload still pages.
    body = '{"updated_ts":1000}'
    r = dc.analyze(body, 1, GM, now=1000 + _FRESH + 60, freshness_s=_FRESH)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_STALE in r["reason"]


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
    # an interval below the floor is clamped to 60, never a per-pass ping. 120 and 179 fall in the
    # same clamped 60s bucket (floor(t/60) == 2); 180 starts the next bucket (3).
    a = dc.dedup_key("b", 120, 10)
    b = dc.dedup_key("b", 179, 10)
    c = dc.dedup_key("b", 180, 10)
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


# ------------------------------------------------------------------ NO_DANTESYNC (#1308)
# The #1307 blind spot documented in .claude/rules/dantesync-clock-alert-watchdog.md: a box that is
# UP but whose :8898 (dantesync HTTP) is dead reads UNREACHABLE -> SKIP, so a :8898-specific outage on
# a live box was paged by NEITHER this watchdog nor #1001. #1308 closes it: when :8898 is unreachable,
# the orchestrator probes box up-ness (TCP) and passes box_up; a proven-up box with dead :8898 is
# NO_DANTESYNC (production-critical page), a down/unknown box stays SKIP (defer #1001, never a false
# page).
def _status_with_version(version, **kw):
    """A healthy (or **kw-overridden) :8898 body with a `version` field appended."""
    body = _status(**kw)
    return body[:-1] + f',"version":"{version}"}}'


def test_no_dantesync_when_box_up_but_8898_unreachable():
    r = dc.analyze("", 0, GM, box_up=1)
    assert r["verdict"] == "NO_DANTESYNC", r
    assert r["reason"] == dc.R_NO_HTTP


def test_no_dantesync_verdict_and_reason_constants_exist():
    assert dc.V_NO_DANTESYNC == "NO_DANTESYNC"
    assert dc.R_NO_HTTP == "no_dantesync_http"


def test_skip_when_box_down_and_8898_unreachable():
    # box genuinely down -> SKIP (defer #1001), never NO_DANTESYNC.
    assert dc.analyze("", 0, GM, box_up=0)["verdict"] == "SKIP"


def test_skip_when_box_up_unknown_and_8898_unreachable():
    # up-ness not probed / probe errored (None) -> SKIP, never a false NO_DANTESYNC page.
    assert dc.analyze("", 0, GM, box_up=None)["verdict"] == "SKIP"
    # backward-compat: the pre-#1308 3-arg call (no box_up) still SKIPs on unreachable.
    assert dc.analyze("", 0, GM)["verdict"] == "SKIP"


def test_box_up_ignored_when_8898_reachable():
    # a reachable+healthy :8898 grades normally regardless of box_up (the up-ness probe only runs on
    # an unreachable :8898).
    assert dc.analyze(_status(), 1, GM, box_up=0)["verdict"] == "OK"


# ------------------------------------------------------------------ version reporting (never a page) #1308
# A daemon version != DANTESYNC_VERSION_PIN is REPORTED in the card/log text, never a page on its own
# (a stale-but-locked node still has the clock); an absent `version` field is silent.
def test_version_mismatch_is_reported_never_changes_the_verdict():
    r = dc.analyze(_status_with_version("1.8.40"), 1, GM, version_pin="1.8.53")
    assert r["verdict"] == "OK", r  # a version mismatch alone is never a page
    assert r["version"] == "1.8.40"
    assert "1.8.40" in r["version_note"] and "1.8.53" in r["version_note"]


def test_version_match_leaves_no_note():
    r = dc.analyze(_status_with_version("1.8.53"), 1, GM, version_pin="1.8.53")
    assert r["version"] == "1.8.53"
    assert r["version_note"] is None


def test_version_absent_is_silent():
    r = dc.analyze(_status(), 1, GM, version_pin="1.8.53")
    assert r["version"] is None
    assert r["version_note"] is None


def test_no_version_pin_never_notes():
    r = dc.analyze(_status_with_version("1.8.40"), 1, GM)  # no pin passed
    assert r["version_note"] is None


def test_version_note_present_even_on_no_clock():
    # a mismatched version is worth reporting whether the clock is OK or lost.
    r = dc.analyze(_status_with_version("1.8.40", is_locked="false", mode="ACQ"), 1, GM,
                   version_pin="1.8.53")
    assert r["verdict"] == "NO_CLOCK", r
    assert r["version_note"] is not None


# ------------------------------------------------------------------ MGMT_DEAD (#1309)
# The 13.9. P0 wedge: a cambox goes half-dead after a bkshading-relay (re)start -- dantesync :8898
# keeps answering (already-running process) but the ssh MANAGEMENT banner is dead (kex reset /
# timeout: anything that needs a fork fails). The dev1 watchdog probes the ssh banner and passes
# mgmt_ssh_ok; a :8898-reachable box with a dead banner is MGMT_DEAD (production-critical page), a
# NEW axis orthogonal to the clock verdict. mgmt_ssh_ok defaults to None so every pre-#1309 call is
# unchanged.
def test_mgmt_dead_when_8898_reachable_but_ssh_banner_dead():
    r = dc.analyze(_status(), 1, GM, mgmt_ssh_ok=0)
    assert r["verdict"] == "MGMT_DEAD", r
    assert r["reason"] == dc.R_SSH_DEAD


def test_mgmt_dead_verdict_and_reason_constants_exist():
    assert dc.V_MGMT_DEAD == "MGMT_DEAD"
    assert isinstance(dc.R_SSH_DEAD, str) and dc.R_SSH_DEAD


def test_mgmt_dead_takes_precedence_over_a_lost_clock():
    # even when the clock is ALSO lost, an unmanageable box is the salient P0 -- MGMT_DEAD wins, and
    # the underlying clock verdict is carried as context.
    r = dc.analyze(_status(is_locked="false", mode="ACQ"), 1, GM, mgmt_ssh_ok=0)
    assert r["verdict"] == "MGMT_DEAD", r
    assert r["clock_verdict"] == "NO_CLOCK", r


def test_mgmt_ssh_ok_true_never_overrides_the_clock_verdict():
    assert dc.analyze(_status(), 1, GM, mgmt_ssh_ok=1)["verdict"] == "OK"
    assert dc.analyze(_status(is_locked="false", mode="ACQ"), 1, GM,
                      mgmt_ssh_ok=1)["verdict"] == "NO_CLOCK"


def test_mgmt_ssh_none_is_backward_compatible():
    # not probed (default None) -> the pre-#1309 clock verdict, byte-for-byte.
    assert dc.analyze(_status(), 1, GM)["verdict"] == "OK"
    assert dc.analyze(_status(), 1, GM, mgmt_ssh_ok=None)["verdict"] == "OK"


def test_mgmt_dead_not_fired_when_8898_unreachable():
    # ssh dead on a box whose :8898 is ALSO dead is NOT MGMT_DEAD: that is SKIP (box down, #1001) or
    # NO_DANTESYNC (box up, :8898 dead) -- the mgmt axis is consulted ONLY when :8898 answered.
    assert dc.analyze("", 0, GM, box_up=0, mgmt_ssh_ok=0)["verdict"] == "SKIP"
    assert dc.analyze("", 0, GM, box_up=1, mgmt_ssh_ok=0)["verdict"] == "NO_DANTESYNC"


def test_mgmt_dead_even_when_body_unparseable():
    # :8898 answered a non-JSON body (UNKNOWN clock) but ssh banner dead -> still MGMT_DEAD.
    r = dc.analyze("not json", 1, GM, mgmt_ssh_ok=0)
    assert r["verdict"] == "MGMT_DEAD", r
    assert r["clock_verdict"] == "UNKNOWN", r


# ------------------------------------------------------------------ dev1 as a `local` node (#1313)
# dev1 is not a probed cam/obs node, yet it runs dantesync and its clock feeds every dev1-hosted gate
# (clock-offset-painter-gate.sh, the recording-verdict wall references, every date-stamped E2E
# window). On 14.9.2026 it sat NTP-only for ~a day unpaged (gm_allowlist on the retired literal + a
# fleet roll that skipped it). `analyze_local` is the ONE tested policy point for the local node:
# probed at 127.0.0.1:8898 with NO ssh/TCP reach probe, because the box the watchdog runs ON is up by
# definition. Two policy differences vs a remote node, both asserted HERE (not in bash):
#   * box UP by definition -> a dead :8898 is NO_DANTESYNC, never SKIP (no "box down, defer #1001").
#   * no ssh MANAGEMENT axis (we ARE the box) -> MGMT_DEAD can never fire for the local node.
# Everything else (OK / NO_CLOCK / UNKNOWN / gm / storm / stale / version) is the SAME generic
# grading, so a local node can never disagree with a remote node about what a lost clock is.
def test_analyze_local_ok_when_locked_correct_gm():
    r = dc.analyze_local(_status(), 1, GM)
    assert r["verdict"] == "OK", r
    assert r["reason"] == dc.R_NONE


def test_analyze_local_no_clock_when_not_locked():
    r = dc.analyze_local(_status(is_locked="false", mode="ACQ"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_NOT_LOCKED in r["reason"]


def test_analyze_local_no_clock_when_gm_foreign():
    # exactly the 14.9. shape: dev1 fell to a foreign/none GM while the fleet roll skipped it.
    r = dc.analyze_local(_status(gm="10.77.7.109"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_WRONG_GM in r["reason"]


def test_analyze_local_no_clock_on_ntp_step_storm():
    r = dc.analyze_local(_status(storm="true", steps="165"), 1, GM)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_STORM in r["reason"]


def test_analyze_local_no_dantesync_when_8898_dead_box_up_by_definition():
    # THE key local difference: the generic analyze() SKIPs an unreachable :8898 (box may be down,
    # defer #1001). For the LOCAL box there is no "down" case -- the watchdog runs on it -- so a dead
    # :8898 is the daemon crashed/wedged on a live box: NO_DANTESYNC, a production-critical page.
    r = dc.analyze_local("", 0, GM)
    assert r["verdict"] == "NO_DANTESYNC", r
    assert r["reason"] == dc.R_NO_HTTP
    # contrast: the generic remote grading SKIPs the very same unreachable read.
    assert dc.analyze("", 0, GM)["verdict"] == "SKIP"


def test_analyze_local_never_mgmt_dead_no_ssh_axis():
    # the local box has no ssh MANAGEMENT axis (we ARE the box); analyze_local forces mgmt off, so
    # even a lost clock stays NO_CLOCK, never MGMT_DEAD.
    assert dc.analyze_local(_status(), 1, GM)["verdict"] == "OK"
    assert dc.analyze_local(_status(is_locked="false", mode="ACQ"), 1, GM)["verdict"] == "NO_CLOCK"


def test_analyze_local_unknown_when_body_unparseable():
    # a reachable local :8898 serving an unparseable body is UNKNOWN, never a fabricated NO_CLOCK.
    assert dc.analyze_local("not json", 1, GM)["verdict"] == "UNKNOWN"


def test_analyze_local_version_mismatch_reported_never_a_page():
    r = dc.analyze_local(_status_with_version("1.8.40"), 1, GM, version_pin="1.8.53")
    assert r["verdict"] == "OK", r  # a version mismatch alone is never a page (#1308 rule)
    assert "1.8.40" in r["version_note"] and "1.8.53" in r["version_note"]


def test_analyze_local_stale_updated_ts_is_no_clock():
    # a frozen local :8898 payload (HTTP alive, updated_ts frozen) is a silent clock loss -> NO_CLOCK.
    r = dc.analyze_local(_status(updated_ts=1000), 1, GM, now=1000 + 10_000, freshness_s=300)
    assert r["verdict"] == "NO_CLOCK", r
    assert dc.R_STALE in r["reason"]


# ------------------------------------------------------------------ CLI --local flag (#1313)
# The orchestrator's local arm passes `--local 1` to the analyze subcommand. That flag routes to
# analyze_local: box_up is forced 1 (a dead :8898 -> NO_DANTESYNC) and the ssh axis is off, regardless
# of any --box-up / --mgmt-ssh-ok also passed. The flag defaults off, so every remote-node invocation
# is byte-for-byte unchanged.
def _cli(args, stdin=""):
    r = subprocess.run(["python3", str(_MOD), "analyze", *args], input=stdin,
                       capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    return dict(ln.split("=", 1) for ln in r.stdout.splitlines() if "=" in ln)


def test_cli_local_flag_makes_unreachable_8898_a_no_dantesync():
    out = _cli(["--box-reachable", "0", "--grandmaster-ip", GM, "--local", "1"])
    assert out["verdict"] == "NO_DANTESYNC", out


def test_cli_local_flag_forces_box_up_ignoring_box_up_zero():
    # even an explicit --box-up 0 cannot turn the local box into a SKIP: it is up by definition.
    out = _cli(["--box-reachable", "0", "--grandmaster-ip", GM, "--box-up", "0", "--local", "1"])
    assert out["verdict"] == "NO_DANTESYNC", out


def test_cli_without_local_flag_is_unchanged_skip():
    # the remote path (no --local) still SKIPs an unreachable read with no --box-up -- byte-for-byte.
    out = _cli(["--box-reachable", "0", "--grandmaster-ip", GM])
    assert out["verdict"] == "SKIP", out


def test_cli_local_flag_never_mgmt_dead_even_with_dead_ssh():
    # --mgmt-ssh-ok 0 would make a REMOTE reachable box MGMT_DEAD; --local overrides it off.
    out = _cli(["--box-reachable", "1", "--grandmaster-ip", GM, "--mgmt-ssh-ok", "0", "--local", "1"],
               stdin=_status())
    assert out["verdict"] == "OK", out
