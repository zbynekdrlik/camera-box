#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/frozen-input-alert-watchdog.sh / obs-liveness-watchdog.sh
# (set -uo pipefail, not -e).
#
# scripts/mv-fps-alert-watchdog.sh -- #1083 (continuation of #771): dev1-side MULTIVIEW-FPS alarm.
#
# WHY (#1083): #771 shipped the observability CORE -- vendored libobs `render_display()` emits
# `multiview-audit: monitor=N divisor=D rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per
# throttleable Multiview projector, `src/mv_audit.rs` parses+gates it, and the `mv-fps-gate` bin is
# an E2E-preflight / drift-guard consumer. But NOTHING read it LIVE, so a MV render collapse
# (measured live 2026-08-17: imag monitor-3 to ~12fps for 5 min, strih 4K MV to 9-11fps under an
# app stealing GPU/CPU) went unalarmed unless someone ran `mv-fps-gate` by hand. This dev1-side
# systemd --user timer reads each OBS box's newest log, runs `mv-fps-gate` (each projector's
# recent-window MEDIAN `multiview-audit:` cadence, #1212), and pages Discord on a sustained
# below-floor collapse -- imag + strih.
#
# TAP: the newest `~/.config/obs-studio/logs/*.txt` (imag) / `%APPDATA%\obs-studio\logs\*.txt`
# (strih) OBS log, read via one flat `ssh` (linux `tail` / windows `powershell gc -Tail`). Reading
# an OBS LOG FILE is a session-agnostic FILE read, allowed for a headless dev1 watchdog per
# win-ssh-vs-mcp (never a GUI atom over ssh). The confirm-counter + alert throttle are the SAME
# shared obs_watchdog_confirm / obs_watchdog_alert_throttle (scripts/lib/obs-watchdog-decision.sh)
# the #391/#1001/#1052 siblings use -- no second alert mechanism.
#
# IMAG-AUTOSTART-AWARE: a fresh OBS start (autostart via imag-obs.service / an operator relaunch)
# writes a NEW log file. The watchdog tracks each box's OBS-log IDENTITY and, when it changes,
# RESETS the confirm streak (the #391/#799 "counter reset -> None -> NEVER page" fail-safe), so a
# fresh instance's warm-up below-floor read can never inherit the previous instance's pending
# confirmation. Combined with the 2-pass confirm at the 5-min cadence (a warm-up recovers within
# seconds), a fresh start never false-pages.
#
# NO-DOUBLE-PAGE: before deciding, read #1001's OWN on-disk state -- if the box is CONFIRMED
# unreachable there, reachability already owns the page and MV-fps skips (a genuinely-down box is
# the reachability/#882 watchdogs' job, never a MV-fps collapse). A failed log read (box down) or a
# missing audit line likewise -> UNKNOWN, never a page.
#
# NEVER reboots (memory rule imag-no-reboot-root-cause): DETECT + ALERT only, agent-driven recovery.
#
# SHIPS DISABLED -- see systemd/mv-fps-alert-watchdog.README.md. The SUPERVISOR installs + live-
# verifies it (confirm detect -> confirm -> alert with a genuine collapse, no false-page against a
# healthy box) before turning the timer on. Do NOT enable it as part of merging this PR.
#
# Usage:
#   scripts/mv-fps-alert-watchdog.sh            # one pass: measure -> decide -> alert
#   scripts/mv-fps-alert-watchdog.sh --dry-run  # measure + decide + LOG only; never alert
#   scripts/mv-fps-alert-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"
# shellcheck source=scripts/lib/mv-fps-health.sh
. "$HERE/lib/mv-fps-health.sh"
# shellcheck source=scripts/lib/ps-encoded.sh
. "$HERE/lib/ps-encoded.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,45p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "mv-fps-alert-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The OBS boxes to watch, space-separated "name|ip|os" (os = linux | win). Default: imag + strih --
# the two boxes whose Multiview render cadence #771 emits. (stream also runs OBS but has no operator
# Multiview projector; add it here only if it grows one.)
MV_FPS_BOXES="${MV_FPS_BOXES:-imag|10.77.9.182|linux strih|10.77.9.202|win}"

SSH_USER="${MV_FPS_SSH_USER:-newlevel}"
SSH_PW="${MV_FPS_SSH_PW:-newlevel}"
SSH_TIMEOUT="${MV_FPS_SSH_TIMEOUT:-20}"
SSH_OPTS="${MV_FPS_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"
OBS_LOG_TAIL="${MV_FPS_OBS_LOG_TAIL:-2000}"

# The `mv-fps-gate` bin (default features) -- the SAME decision engine #771's E2E preflight /
# drift-guard use (ticket: "Použiť existujúci mv-fps-gate"). Same search convention as
# OBS_WATCHDOG_GATE_BIN: env override, else target/release. The bin is built + uploaded by CI (the
# probe-tools artifact); dev1 uses that artifact -- never a local Tier-0 build (no-local-builds.md).
MV_FPS_GATE_BIN="${MV_FPS_GATE_BIN:-$HERE/../target/release/mv-fps-gate}"
# Full override for the per-box OBS-log read (run with <ip> <os>, stdout = the RAW probe text:
# a `MVFPS_LOGID:<id>` line + the OBS log tail). Used by tests / a --dry-run smoke run.
MV_FPS_PROBE_CMD="${MV_FPS_PROBE_CMD:-}"

# 2-pass confirm before paging (matches the sibling watchdogs): one missed read / transient collapse
# must never page. A genuine render collapse stays below floor across the 5-min cadence.
CONFIRM_THRESHOLD="${MV_FPS_ALERT_CONFIRM_THRESHOLD:-2}"
ALERT_THROTTLE_PASSES="${MV_FPS_ALERT_THROTTLE_PASSES:-12}"   # ~1h at the 5-min cadence
# A box that is READABLE but emits NO `multiview-audit:` line (OBS up on a pre-#771 build, or the
# audit stopped) stays UNKNOWN forever and never pages while the watchdog looks green -- the "silent
# unknown" the standing rig-degradation rule forbids. After this many CONSECUTIVE readable-but-blind
# passes, fire ONE "tap blind" WARN. Default ~2h at the 5-min cadence.
TAP_BLIND_THRESHOLD="${MV_FPS_ALERT_TAP_BLIND_THRESHOLD:-24}"

NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${MV_FPS_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${MV_FPS_ALERT_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-mv-fps-alert.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-mv-fps-alert-dryrun.state"
STATE_FILE="${MV_FPS_ALERT_STATE_FILE:-$_state_default}"
# #1001's OWN state file -- read (never written) for the no-double-page guard.
NETREACH_STATE_FILE="${MV_FPS_NETREACH_STATE_FILE:-$STATE_DIR/camera-box-network-reach-alert.state}"

log() { printf '%s [mv-fps-alert-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# -- I/O probe (dev1-local; NOT pure -- kept out of the lib) -------------------------------------
# probe_mv_log <ip> <os> -> stdout: the RAW newest-OBS-log probe text: a `MVFPS_LOGID:<id>` line
# (the log's identity, for the autostart restart-reset) followed by the log tail. EMPTY when the
# read failed (box down / ssh error) -- the caller then treats the box as UNKNOWN, never a page.
# Overridable whole via MV_FPS_PROBE_CMD.
probe_mv_log() {
  local ip="$1" os="$2"
  if [ -n "$MV_FPS_PROBE_CMD" ]; then
    # shellcheck disable=SC2086
    $MV_FPS_PROBE_CMD "$ip" "$os" 2>/dev/null || true
    return 0
  fi
  case "$os" in
    linux)
      # Single-quoted remote script (so $F / $(...) run REMOTELY, not on dev1); the dev1 tail count
      # is spliced in via a single-quote break. Prints the log basename as the identity, then tail.
      local remote='F=$(ls -t ~/.config/obs-studio/logs/*.txt 2>/dev/null | head -1); [ -n "$F" ] && { echo "MVFPS_LOGID:$(basename "$F")"; tail -n '"$OBS_LOG_TAIL"' "$F"; }'
      # shellcheck disable=SC2086
      timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" "$remote" 2>/dev/null || true
      ;;
    win)
      # #1259: -EncodedCommand (base64 UTF-16LE), NEVER the naive -Command "$f=(…| sort …); if(…){…}".
      # Win32-OpenSSH's default cmd.exe shell leaks the unescaped `|`/`;`/`{}` -> a mangled/blind read
      # (the issue-1258 root cause). ps_encoded_command (scripts/lib/ps-encoded.sh) encodes the whole
      # program (incl. its MVFPS_LOGID identity line) to a pure-ASCII blob cmd.exe cannot touch; an
      # empty encode -> empty read -> the caller treats the box as UNKNOWN, never an abort. `'MVFPS_LOGID:'`
      # stays single-quoted -> literal to powershell; every `$` powershell must see is `\$`-escaped so
      # dev1 bash keeps it literal, while $OBS_LOG_TAIL (a dev1 var) is spliced. `if($f)` guards no-log.
      local _enc _tail
      _tail="$(ps_clamp_numeric "$OBS_LOG_TAIL" 2000)" # #1259: guard the env count before the payload
      _enc="$(ps_encoded_command "\$f=(gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1); if(\$f){ 'MVFPS_LOGID:'+\$f.Name; gc \$f.FullName -Tail $_tail }")"
      # shellcheck disable=SC2086
      timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" \
        "powershell -NoProfile -NonInteractive -EncodedCommand $_enc" \
        2>/dev/null || true
      ;;
    *)
      log "unknown os '$os' -- cannot read its OBS log; skipping"
      ;;
  esac
}

# -- #1001 reachability read (never re-probed) --------------------------------------------------
# netreach_box_alerted <box> -> "1" iff #1001 has that box CONFIRMED unreachable (paged). Absent
# state file / field -> "0".
netreach_box_alerted() {
  local box="$1"
  [ -f "$NETREACH_STATE_FILE" ] || { printf '0'; return 0; }
  local v
  v="$(sed -n "s/^alerted_${box}=//p" "$NETREACH_STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-0}"
}

# -- persisted per-box state (key=value lines) --------------------------------------------------
read_state_field() {
  local key="$1" default="$2"
  [ -f "$STATE_FILE" ] || { printf '%s' "$default"; return 0; }
  local v
  v="$(sed -n "s/^${key}=//p" "$STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-$default}"
}
write_state_field() {
  local key="$1" val="$2" tmp existing=""
  mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
  # Read the OTHER keys into memory FIRST, before any file is opened for writing (mirrors
  # obs-liveness-watchdog's issue-732 state-loss fix).
  [ -f "$STATE_FILE" ] && existing="$(grep -v "^${key}=" "$STATE_FILE" 2>/dev/null)"
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
  if [ -n "$tmp" ]; then
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$tmp" 2>/dev/null || true
    mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
  else
    { [ -n "$existing" ] && printf '%s\n' "$existing"; printf '%s=%s\n' "$key" "$val"; } \
      > "$STATE_FILE" 2>/dev/null || true
  fi
}

# A box name safe for state-field names (imag/strih are alnum, but stay defensive).
box_key() { printf '%s' "$1" | tr -c 'A-Za-z0-9' '_'; }

# -- per-box decision ---------------------------------------------------------------------------
handle_box() {
  local name="$1" ip="$2" os="$3"
  local k; k="$(box_key "$name")"

  # No-double-page guard: #1001 has this box CONFIRMED unreachable -> it owns the page; SKIP probing
  # a down box (also avoids a pointless ssh timeout). Reset the confirm streak (UNKNOWN restarts it).
  if [ "$(netreach_box_alerted "$name")" = "1" ]; then
    log "$name: #1001 CONFIRMED unreachable -> reachability owns the page, SKIP MV-fps this pass"
    write_state_field "confirm_${k}" 0
    return 0
  fi

  local raw logid mv_lines gate_out gate_exit verdict prev_logid reset
  raw="$(probe_mv_log "$ip" "$os" | tr -d '\r')"
  logid="$(printf '%s\n' "$raw" | sed -n 's/^MVFPS_LOGID://p' | tail -1)"
  # #1262: byte-safe extraction (mv_fps_extract_audit_lines, scripts/lib/mv-fps-health.sh) -- a
  # PS-5.1-ANSI-reencoded invalid byte from an adjacent genlock-fifo audit line can glue onto this
  # line at a transport-chunk boundary and blind a plain `grep -F`.
  mv_lines="$(printf '%s\n' "$raw" | mv_fps_extract_audit_lines)"

  # Autostart restart-reset: a CHANGED OBS-log identity means OBS restarted -> clear the pending
  # confirm BEFORE this pass is scored, so a fresh instance's warm-up below-floor read starts a new
  # streak (never inherits a stale confirmation). Persist the new identity only when we read one.
  prev_logid="$(read_state_field "logid_${k}" "")"
  [ -n "$logid" ] && write_state_field "logid_${k}" "$logid"
  reset="$(mv_fps_restart_reset "$prev_logid" "$logid" | sed -n 's/^reset=//p')"
  if [ "$reset" = "1" ]; then
    log "$name: OBS restarted (log '$prev_logid' -> '$logid') -> reset confirm streak (autostart-aware)"
    write_state_field "confirm_${k}" 0
  fi

  # Verdict from the gate. No read (box down) or no audit line -> UNKNOWN (never a page).
  if [ -z "$raw" ]; then
    verdict=UNKNOWN; gate_out=""
    log "$name: no OBS-log read (ssh/log read failed) -> UNKNOWN, nothing to decide"
  elif [ -z "$mv_lines" ]; then
    verdict=UNKNOWN; gate_out=""
    log "$name: OBS log present but NO multiview-audit line -> UNKNOWN (tap blind?)"
  else
    gate_out="$(printf '%s\n' "$mv_lines" | "$MV_FPS_GATE_BIN" 2>/dev/null)"; gate_exit=$?
    verdict="$(mv_fps_verdict "$gate_exit")"
    log "$name: gate exit=$gate_exit -> $verdict"
  fi

  # -- PASS: healthy -> recovery ping if we paged for this box; clear all streaks.
  if [ "$verdict" = "PASS" ]; then
    local was_alerted recover
    was_alerted="$(read_state_field "alerted_${k}" 0)"
    recover="$(mv_fps_recovery_decision "$was_alerted" 1 | sed -n 's/^recover=//p')"
    if [ "$recover" = "1" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] WOULD send recovery: $name MV fps recovered"
      else
        log "RECOVERY: $name MV fps recovered (render fps back above minimum) -- machine-channel only (#1206: recovery is not a phone ping)"
      fi
      write_state_field "alerted_${k}" 0
    fi
    write_state_field "confirm_${k}" 0
    write_state_field "alert_sig_${k}" ""
    write_state_field "alert_passes_${k}" 0
    write_state_field "unknown_${k}" 0
    write_state_field "tap_blind_${k}" 0
    log "$name: MV fps PASS"
    return 0
  fi

  # -- UNKNOWN: reset the confirm streak; a READABLE box that STAYS unknown counts toward the
  # tap-blind WARN -- either no `multiview-audit:` line (a pre-#771 OBS build / the audit stopped) OR
  # audit lines present but the gate could not classify them (exit != 0/1 -- a missing/broken
  # `mv-fps-gate` binary → 127, which UNKNOWN would otherwise swallow SILENTLY forever). A failed
  # read (raw empty, box down) is #1001/#882's job -> never counted here.
  if [ "$verdict" = "UNKNOWN" ]; then
    write_state_field "confirm_${k}" 0
    if [ -n "$raw" ]; then
      local unk tap_alerted reason
      unk="$(read_state_field "unknown_${k}" 0)"; case "$unk" in '' | *[!0-9]*) unk=0 ;; esac
      unk=$((unk + 1)); write_state_field "unknown_${k}" "$unk"
      if [ -z "$mv_lines" ]; then
        reason="no \`multiview-audit:\` line (a pre-#771 OBS build, or the audit stopped)"
      else
        reason="\`mv-fps-gate\` could not classify the audit lines -- a missing/broken gate binary at MV_FPS_GATE_BIN?"
      fi
      tap_alerted="$(read_state_field "tap_blind_${k}" 0)"
      if [ "$unk" -ge "$TAP_BLIND_THRESHOLD" ] && [ "$tap_alerted" != "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD alert: $name MV-fps tap BLIND for $unk passes ($reason)"
        else
          log "ALERT: firing Discord notification for $name MV-fps tap blind"
          python3 "$NOTIFY" notify --body \
            "⚠️ Multiview fps ($REPO_SLUG): meranie Multiview fps pre **$name** je SLEPÉ už $unk kontrol po sebe — OBS beží, ale $reason. Kontrola Multiview fps pre $name je vypnutá, kým sa to neopraví. Rieši Claude automaticky, ty nemusíš nič robiť." \
            --dedup-key "mv-fps-tap-$name" \
            >/dev/null 2>&1 || log "tap-blind: airuleset.py notify failed (non-fatal)"
        fi
        write_state_field "tap_blind_${k}" 1
      fi
    fi
    return 0
  fi

  # -- BELOW: feed the confirm counter. A real audit read proves the tap works -> clear tap-blind.
  write_state_field "unknown_${k}" 0
  write_state_field "tap_blind_${k}" 0
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${k}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${k}" "${confirm:-0}"
  log "$name: MV fps BELOW floor confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "$name: below floor this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED collapse -> latch the recovery flag, throttle-dedup on the failing-monitor signature.
  write_state_field "alerted_${k}" 1
  local current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes detail mons
  mons="$(printf '%s\n' "$gate_out" | grep '^FAIL' | sed -n 's/.*\(monitor=[0-9]*\).*/\1/p' | sort -u | paste -sd, -)"
  current_sig="mvfps:${k}:${mons}"
  prior_sig="$(read_state_field "alert_sig_${k}" "")"
  prior_passes="$(read_state_field "alert_passes_${k}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${k}" "$new_sig"
  write_state_field "alert_passes_${k}" "$new_passes"

  detail="$(mv_fps_alert_detail "$name" "$gate_out")"
  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: $detail (alert_now=$alert_now)"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    log "ALERT: firing Discord notification for $name MV fps collapse"
    python3 "$NOTIFY" notify --body \
      "🚨 Multiview fps ($REPO_SLUG): **$detail**. Render Multiview projektora na $name klesol pod povolené minimum (cieľ − tolerancia). Rieši Claude automaticky (reštart OBS na $name, resp. nájde proces, čo kradne GPU/CPU; nikdy nereštartuje celý počítač), ty nemusíš nič robiť. Potvrdené počas ${CONFIRM_THRESHOLD} po sebe idúcich kontrol." \
      --dedup-key "mv-fps-$name" \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, boxes='$MV_FPS_BOXES', threshold=$CONFIRM_THRESHOLD)"

  # Fail-loud preflight (log only -- an install problem the supervisor fixes at enable-time, never a
  # Discord page): the gate binary must be runnable on the real path.
  if [ -z "$MV_FPS_PROBE_CMD" ] && [ ! -x "$MV_FPS_GATE_BIN" ]; then
    log "ERROR: mv-fps-gate binary not executable at '$MV_FPS_GATE_BIN' -- point MV_FPS_GATE_BIN at the probe-tools-linux-amd64 CI artifact's mv-fps-gate (#771); every box will read UNKNOWN until then"
  fi

  # `set -f` disables globbing so a `*`/`?`/`[` in a box spec never pathname-expands during the split
  # (glob-safety parity with frozen-input-alert-watchdog). Box names/ips/os have none today, defensive.
  local spec name rest ip os
  set -f
  for spec in $MV_FPS_BOXES; do
    name="${spec%%|*}"; rest="${spec#*|}"; ip="${rest%%|*}"; os="${rest##*|}"
    [ -n "$name" ] && [ -n "$ip" ] && [ -n "$os" ] && handle_box "$name" "$ip" "$os"
  done
  set +f
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
