#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/obs-liveness-watchdog.sh / scripts/obs-session-
# watchdog.sh (set -uo pipefail, NOT -e).
#
# scripts/obs-burn-reconcile-watchdog.sh -- #1060 dev1-side fresh-OBS-start burn reconciliation for
# the UNATTENDED strih/stream OBS start paths.
#
# WHY (#1060, a 1057 follow-up): issue 1057 closed the burn-resurrection window for the DELIBERATE
# dev1-driven relaunch (launch-obs-genlock.sh's PLAN now directs a post-launch obs_burn_filter.py
# sweep-off). Still open -- the UNATTENDED starts: box boot autostart, NL_STARTUP.ahk obs64
# auto-respawn (strih), and the issue-411 self-heal Task-Scheduler relaunch, ALL of which reuse
# launch-obs-genlock.sh's emitted PowerShell PROGRAM, which never touches the burn (the box has no
# on-box python/OBS-WebSocket client, and obs_burn_filter.py is not deployed there). On any of
# them a saved genlock_burn=true reloads and renders the QR measurement burn onto the LIVE program
# until the next dev1 gate run's [0/8] sweep. This ONE dev1 watchdog covers all three at once,
# because it keys on the OBS RESTART -- not on which path caused it.
#
# THE LOAD-BEARING DISCRIMINATOR: a FRESH OBS START, never merely "a burn is present". A persistent
# TEST-mode burn on strih/stream is a LEGITIMATE, deliberately-persistent operator state (the rig
# "TEST mode must stay alive" convention) whose rig-active heartbeat (#281) goes STALE after ~10
# min while the burn should remain -- so "burn present + stale heartbeat" is idle TEST mode, NOT a
# leak. Only at a fresh OBS restart is a reloaded saved burn definitively a resurrection. The
# fresh-start signal is GetStats.renderTotalFrames (monotone since OBS process start, RESETS on a
# restart), read over the SAME OBS WebSocket obs_burn_filter already speaks (no ssh/MCP). All
# "should I sweep?" logic is the PURE scripts/lib/obs-burn-reconcile-decision.sh (unit-tested).
#
# COORDINATION (never fight a live gate/TEST harness): even at a fresh start we DEFER while a live
# harness is coordinating -- a fresh #281 rig-active heartbeat (recording-e2e.sh / rig-mode.sh
# TEST) OR a held #830 rig lease (a CI gate driving the rig). Both are dev1-side lockfile/lockdir
# checks, no ssh. This is exactly the "gate-run coordination so it never clears a burn a live gate
# deliberately set mid-run" the ticket asks for.
#
# DETECT + RECONCILE only for the burn; NO OBS relaunch, NO GUI/desktop action -- the sweep is a
# session-agnostic dev1-side WS op (win-ssh-vs-mcp), exactly like 1057's dev1-driven sweep. Forcing
# a resurrected measurement burn OFF at a fresh start is unconditionally safe (a measurement burn
# is never legitimate operator state -- 1057). Every sweep also fires ONE deduped Discord alert.
#
# FAIL CLOSED: a failed GetInputList (sweep-check exit 2 = SWEEP_ENUM_FAILED) is NOT "clean" -- an
# out-of-set burn would be invisible (burn-target-enumeration rule, guard class #246/#844); it
# ALERTS "could not verify" and never claims the box clean.
#
# SHIPS DISABLED -- see systemd/obs-burn-reconcile-watchdog.README.md. The SUPERVISOR installs +
# live-verifies it (a genuine unattended restart with a saved burn gets swept; a persistent TEST
# burn with no restart is untouched; a live gate's burn is deferred) before enabling the timer. Do
# NOT enable it as part of merging this PR.
#
# Usage:
#   scripts/obs-burn-reconcile-watchdog.sh            # one pass: probe -> decide -> reconcile
#   scripts/obs-burn-reconcile-watchdog.sh --dry-run  # probe + decide + LOG only; never sweep/alert
#   scripts/obs-burn-reconcile-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-burn-reconcile-decision.sh
. "$HERE/lib/obs-burn-reconcile-decision.sh"
# shellcheck source=scripts/lib/rig-heartbeat.sh
. "$HERE/lib/rig-heartbeat.sh"
# shellcheck source=scripts/lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help|-h)
    sed -n '6,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "obs-burn-reconcile-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# ── config (all env-overridable) ─────────────────────────────────────────────
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_PASSWORD="${OBS_PASSWORD:-}"

BURN_FILTER_PY="${OBS_BURN_FILTER_PY:-$HERE/obs_burn_filter.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${OBS_BURN_RECONCILE_WATCHDOG_REPO:-zbynekdrlik/camera-box}"

# The rig lease is a "live holder" only while its heartbeat is fresh; use the lease's own default
# stale threshold (comfortably above a CI job's cap) so a genuinely-running gate is never mistaken
# for a stale one and its deliberate burn wrongly swept.
RIG_LEASE_STALE_SECS="${RIG_LEASE_STALE_SECS:-5400}"

# DURABLE state dir (NOT tmpfs, #1060 review): the per-box renderTotalFrames baseline must survive
# a dev1 reboot. A tmpfs baseline (the per-boot runtime dir / /tmp) is wiped on reboot -> the first post-
# reboot pass would read prev="" which, combined with a fresh-start=fresh reading, could false-clear
# a deliberately-persistent TEST-mode burn (its #281 heartbeat is stale by design). ~/.camera-box is
# the durable, repo-owned dir (same as phase-sync-last.json). Paired with the decision lib's
# "unknown prev is NOT a restart" rule, this makes an OBS restart detectable across a dev1 reboot
# (prev survives) while a wiped/first baseline never sweeps.
STATE_DIR="${OBS_BURN_RECONCILE_WATCHDOG_STATE_DIR:-$HOME/.camera-box}"
# Its OWN state file (persists the per-box renderTotalFrames baseline + unresolved-burn flag),
# distinct from #391's camera-box-obs-watchdog.state and #979's camera-box-obs-session-watchdog.state.
STATE_FILE="${OBS_BURN_RECONCILE_WATCHDOG_STATE_FILE:-$STATE_DIR/camera-box-obs-burn-reconcile-watchdog.state}"

# Exit code obs_burn_filter.py's sweep-* actions return when the ndi-input ENUMERATION itself
# failed (WS error / #328 timeout) -- must be treated fail-CLOSED, never as "no burns".
SWEEP_ENUM_FAILED=2

log() { printf '%s [obs-burn-reconcile-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# ── read / write per-box persisted renderTotalFrames baseline ─────────────────
read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  # A FIXED temp path (never a fallback to STATE_FILE itself, #1060 review 🔵): the old
  # `mktemp ... || echo "$STATE_FILE"` fallback truncated the real file via the `>` redirect before
  # grep read it, dropping the SIBLING box's baseline whenever mktemp failed. On any write failure
  # we leave the existing state untouched rather than corrupt it.
  tmp="${STATE_FILE}.tmp.$$"
  if { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
       > "$tmp" 2>/dev/null; then
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || rm -f "$tmp" 2>/dev/null || true
  else
    rm -f "$tmp" 2>/dev/null || true
  fi
}

# ── coordination: is a live gate/TEST harness driving the rig right now? ──────
# 0 (coordinating) if a FRESH #281 rig-active heartbeat exists (recording-e2e.sh / rig-mode.sh
# TEST) OR the #830 rig lease is held by a LIVE holder (a CI gate). Either => a burn set right now
# is legitimate and this watchdog must DEFER, never sweep it.
rig_is_coordinating() {
  if rig_heartbeat_active; then
    return 0
  fi
  if [ -d "$(rig_lease_dir)" ] && ! rig_lease_is_stale "$RIG_LEASE_STALE_SECS"; then
    return 0
  fi
  return 1
}

# obs_burn_filter.py wrapper (reused for session-probe / sweep-check / sweep-off).
burn_filter() {
  python3 "$BURN_FILTER_PY" "$@" --password "$OBS_PASSWORD"
}

# alert <body> -> fire ONE Discord notification via the same airuleset.py notify path #391/#979 use
# (a no-op LOG in --dry-run). Never fatal.
alert() {
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $1"
    return 0
  fi
  python3 "$NOTIFY" notify --body "$1" --dedup-key "${2:-obs-burn-reconcile}" >/dev/null 2>&1 || log "alert: airuleset.py notify failed (non-fatal)"
}

# ── process ONE box: probe -> decide -> reconcile ────────────────────────────
# Reconciles a box on an OBSERVED OBS restart (a renderTotalFrames drop) OR a still-`unresolved`
# burn carried over from a prior pass (self-healing retry). A wiped/first baseline is NEVER treated
# as a restart (it seeds + NOOPs) -- so a dev1 reboot can never false-clear a persistent TEST burn.
# `${box}_unresolved=1` is set ONLY after an observed restart whose reconcile could not confirm the
# box clean (sweep-off left a burn, enumeration failed, or the reconcile was deferred to a live
# gate), and is cleared once a later pass confirms clean -- so a retry can never sweep a burn that
# was not already tied to an observed restart.
process_box() {
  local box="$1" host="$2"

  # 1) fresh-start signal: current renderTotalFrames (stdout), or unreadable => skip this pass.
  local cur rc
  cur="$(burn_filter session-probe --host "$host" 2>/dev/null)"; rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$cur" ]; then
    log "$box: session-probe unreadable (WS error / OBS down) — nothing to decide this pass (baseline unchanged)"
    return 0
  fi

  local prev fresh=0
  prev="$(read_state_field "${box}_rtf" "")"
  if obs_burn_reconcile_is_fresh_start "$prev" "$cur"; then fresh=1; fi
  # Advance the (durable) baseline EVERY readable pass, so each restart triggers exactly one
  # fresh-driven reconcile; a lost/first baseline is seeded here without ever counting as a restart.
  write_state_field "${box}_rtf" "$cur"

  local unresolved
  unresolved="$(read_state_field "${box}_unresolved" 0)"
  [ "$unresolved" = "1" ] || unresolved=0
  log "$box: renderTotalFrames prev=${prev:-<none>} cur=$cur fresh_start=$fresh unresolved=$unresolved"

  # NOOP fast path — same OBS session AND nothing pending. A persistent TEST-mode burn (no restart)
  # is untouched here: this is THE invariant-#1 guard.
  if [ "$fresh" -ne 1 ] && [ "$unresolved" -ne 1 ]; then
    return 0
  fi

  # 2) coordination — never disturb a live gate/TEST harness. Remember we owe a reconcile (persist
  #    unresolved=1) so we retry once the coordination releases and its own cleanup has run.
  if rig_is_coordinating; then
    write_state_field "${box}_unresolved" 1
    log "$box: decision=$(obs_burn_reconcile_decide "$fresh" 1 0) — a live gate/TEST harness is coordinating the rig; deferring (never clear its burn; will retry after it releases)"
    return 0
  fi

  # 3) burn presence — enumerate every ndi_source input (never a static list); fail CLOSED.
  burn_filter sweep-check --host "$host" >/dev/null 2>&1; rc=$?
  if [ "$rc" -eq "$SWEEP_ENUM_FAILED" ]; then
    log "$box: sweep-check FAILED to enumerate after an OBS restart — fail-closed (a burn may be invisible); marked unresolved (will retry)"
    [ "$unresolved" -ne 1 ] && alert \
      "⚠️ #1060 obs-burn-reconcile-watchdog: **$box** OBS restarted (unattended) but its burn state could NOT be verified ($REPO_SLUG) — GetInputList enumeration failed (fail-closed, guard #246/#844). A resurrected measurement burn may be rendering on the LIVE program. Check + clear from dev1: \`python3 scripts/obs_burn_filter.py sweep-off --host $host\`" \
      "obs-burn-reconcile-unresolved-$box"
    write_state_field "${box}_unresolved" 1
    return 0
  fi
  local burn_present=0
  [ "$rc" -eq 1 ] && burn_present=1

  # fresh OR unresolved got us here, and we are uncoordinated -> decide on burn presence alone.
  local decision
  decision="$(obs_burn_reconcile_decide 1 0 "$burn_present")"
  log "$box: decision=$decision (fresh=$fresh unresolved=$unresolved burn_present=$burn_present)"

  case "$decision" in
    CLEAN)
      if [ "$unresolved" -eq 1 ]; then
        write_state_field "${box}_unresolved" 0
        log "$box: previously-unresolved burn is no longer present — resolved"
      else
        log "$box: OBS restart, no burn rendered — nothing to clear"
      fi
      ;;
    SWEEP)
      log "$box: an unattended OBS restart resurrected a measurement burn — forcing it OFF (dev1-side WS sweep)"
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD sweep-off + alert: $box resurrected a measurement burn"
        return 0
      fi
      if burn_filter sweep-off --host "$host" >/dev/null 2>&1; then
        write_state_field "${box}_unresolved" 0
        alert \
          "🧹 #1060 obs-burn-reconcile-watchdog: auto-cleared a resurrected QR measurement burn on **$box** after an UNATTENDED OBS restart ($REPO_SLUG) — a saved genlock_burn=true had reloaded onto the LIVE program. Burn(s) forced OFF from dev1 and read-back verified." \
          "obs-burn-reconcile-autocleared-$box"
      else
        log "$box: sweep-off did NOT fully clear (still rendering / enum failed) — marked unresolved (will retry)"
        [ "$unresolved" -ne 1 ] && alert \
          "🚨 #1060 obs-burn-reconcile-watchdog: **$box** OBS restarted (unattended) with a resurrected measurement burn and the automatic sweep-off did NOT fully clear it ($REPO_SLUG). It will keep retrying; clear it manually before broadcast if it persists: \`python3 scripts/obs_burn_filter.py sweep-off --host $host\`" \
          "obs-burn-reconcile-sweepfail-$box"
        write_state_field "${box}_unresolved" 1
      fi
      ;;
    *)
      log "$box: unexpected decision '$decision' — taking no action (fail-safe)"
      ;;
  esac
  return 0
}

# ── main pass ────────────────────────────────────────────────────────────────
main() {
  log "pass start (dry_run=$DRY_RUN)"
  process_box strih "$STRIH_HOST"
  process_box stream "$STREAM_HOST"
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
