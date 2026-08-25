#!/usr/bin/env bash
# airuleset:script-ok watchdog must survive every per-pass failure and keep polling on the next
# timer tick -- same convention as scripts/cadence-alert-watchdog.sh /
# scripts/asio-starve-alert-watchdog.sh / scripts/frozen-input-alert-watchdog.sh (set -uo pipefail,
# NOT -e). The extended header is below; `set -uo pipefail` is on line ~62.
#
# scripts/ndi-halving-watchdog.sh -- #1203: NDI PER-CONNECTION RATE-HALVING auto-heal, DEV1-SIDE. A
# sibling of the dev1 alert-watchdog family (network-reach issue 1001 / frozen-input issue 1052 /
# bundle-state issue 732 / cadence issue 794 / asio-starve issue 1023) -- but with a CURE arm none of
# the siblings has, shipping GATED OFF (the grabber-stuck issue 1128 env-gated-actuator shape).
#
# WHY (#1203, live 2026-08-25): the vendored DistroAV receiver can degrade a per-connection pull to
# ~HALF the sender's cadence (stream 'NDI 2ME PGM': 15,0/s at a 30,0/s sender; recv_capture_v3
# cap_avg ~65,9 ms vs a healthy ~16 ms), starving the genlock FIFO (late_holds self-climbs,
# dropped_due=0, depth 11 << cap 69). A `systemctl restart camera-box` does NOT clear it; a receiver
# REATTACH (obs_phase2.py idle-receiver -> --restore, overlay keeps the latency pin) does -- restored
# 30,0/s / 12,6 ms instantly on the 2ME PGM leg. This watchdog DETECTS the halving from the stream
# (receiver) OBS log and, when armed, drives that proven cure.
#
# TAP: the stream OBS log prints, per input, every >=5.0 s:
#   `HH:MM:SS.mmm: [distroav] recv-timing #797 '<input>': n=<N> cap_avg=<X>ms cap_max=... out_avg=... out_max=...`
# `n=` is PER-INTERVAL (reset-on-read, vendor/distroav/src/ndi-source.cpp:1481) -- like the asio
# starved_blocks tap (#1023), NOT the cumulative `received=` counter (#794/#1052). So the rate is
# measured WITHIN ONE PASS from the last two lines (n_curr / (ts_curr-ts_prev), both timestamps the
# lines' OWN log prefixes -- the #794 phantom-50 avoidance) by the PURE scripts/ndi_halving_decision.py.
#
# DETECT: a per-input HALVED verdict = fps <= 0.6*expected OR cap_avg >= 2x the frame interval,
# CONFIRMED over 2 consecutive passes (the shared obs_watchdog_confirm). BORDERLINE (between 0.6 and
# 0.85 of expected) is report-only and holds the confirm counter; HEALTHY resets it.
#
# CURE (gated): on a CONFIRMED halving, when NDI_HALVING_SELFHEAL=1 (default OFF -- report-only phase
# first, the ships-DISABLED convention) AND a per-input cooldown has elapsed, ONE reattach via
# obs_phase2.py idle-receiver -> --restore, then RE-MEASURE next pass. Cure disabled, or still-halved
# within the cooldown (a cure that didn't take) -> PAGE via the shared obs_watchdog_alert_throttle
# (no reattach-spam). The alert body carries a healthy-SIBLING context line = the box-wide vs
# per-connection discriminator (context only, NEVER a page gate).
#
# NO-DOUBLE-PAGE GUARD: before deciding, read issue-1001's OWN on-disk state (never re-probe) -- if
# the RECEIVER (stream) OR the SENDER (strih, which produces 2ME PGM) is CONFIRMED unreachable there,
# issue-1001 already owns the page and a halving reading is out of scope -> SKIP every input.
#
# FAIL-LOUD TOOL PREFLIGHT (#833): a missing python3/sshpass/ssh/timeout on dev1 would make every
# probe read empty forever (a permanently-blind watchdog that still looks green). require_tools aborts.
#
# BEST-EFFORT PROBE: ONE flat `ssh ... powershell` OBS-log tail per PASS (a session-agnostic file
# read, allowed for a headless dev1 watchdog per win-ssh-vs-mcp; NEVER nested PowerShell -- $env:APPDATA
# has no spaces). A failed/absent read -> empty log -> the pure seam returns UNKNOWN -> never a false
# page. Override the read with NDI_HALVING_PROBE_CMD (<receiver_ip>, stdout=raw log) and the cure with
# NDI_HALVING_CURE_CMD (<receiver_ip> <input>, exit 0 = attempted) for --dry-run / offline tests.
#
# Usage:
#   scripts/ndi-halving-watchdog.sh            # one pass: measure -> decide -> (cure)/alert
#   scripts/ndi-halving-watchdog.sh --dry-run  # measure + decide + LOG only; never cure, never alert
#   scripts/ndi-halving-watchdog.sh --help
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/obs-watchdog-decision.sh
. "$HERE/lib/obs-watchdog-decision.sh"

DRY_RUN=0
case "${1:-}" in
  --dry-run) DRY_RUN=1 ;;
  --help | -h)
    sed -n '5,60p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  "") : ;;
  *) echo "ndi-halving-watchdog: unknown arg '$1' (try --help)" >&2; exit 2 ;;
esac

# -- config (all env-overridable) ---------------------------------------------------------------
# The RECEIVER box whose OBS log carries the recv-timing #797 lines AND whose receiver we reattach,
# as "name|ip". stream receives 2ME PGM from strih; the halving is a receiver-side pull defect.
NDI_HALVING_RECEIVER="${NDI_HALVING_RECEIVER:-stream|10.77.9.204}"
RECV_NAME="${NDI_HALVING_RECEIVER%%|*}"; RECV_IP="${NDI_HALVING_RECEIVER##*|}"
# The SENDER box (for the no-double-page guard only): 2ME PGM is produced by strih.
SENDER_NAME="${NDI_HALVING_SENDER:-strih}"
# Watched inputs as `<name>|<expected_fps>`, ';'-separated (names contain spaces). Extensible: add a
# `;NDI cam1|60` etc. Listing an input here IS its "expected live" scope. Start set = 2ME PGM @ 30.
NDI_HALVING_INPUTS="${NDI_HALVING_INPUTS:-NDI 2ME PGM|30}"
NDI_HALVING_DEFAULT_FPS="${NDI_HALVING_DEFAULT_FPS:-30}"

# Decision thresholds (passed through to the pure module).
HALVING_RATIO="${NDI_HALVING_RATIO:-0.6}"
CAP_MULT="${NDI_HALVING_CAP_MULT:-2.0}"
HEALTHY_RATIO="${NDI_HALVING_HEALTHY_RATIO:-0.85}"
HEALTHY_CAP_MULT="${NDI_HALVING_HEALTHY_CAP_MULT:-1.5}"
MIN_WINDOW_S="${NDI_HALVING_MIN_WINDOW_S:-3.0}"
MAX_WINDOW_S="${NDI_HALVING_MAX_WINDOW_S:-15.0}"

# Cure arm: DEFAULT OFF (report-only phase first; features-default-on does NOT apply to a self-heal
# actuator -- mirrors grabber-stuck #1128's CAMERA_BOX_GRABBER_STUCK_SELFHEAL gate). One reattach per
# cooldown window per input.
SELFHEAL="${NDI_HALVING_SELFHEAL:-0}"
COOLDOWN_S="${NDI_HALVING_COOLDOWN_S:-600}"
# OBS WebSocket password for the cure (obs_phase2.py idle-receiver). rig-mode convention: OBS_PASSWORD.
OBS_WS_PW="${NDI_HALVING_OBS_WS_PW:-${OBS_PASSWORD:-}}"

# 2-pass confirm before curing/paging: one noisy window / transient must never act.
CONFIRM_THRESHOLD="${NDI_HALVING_CONFIRM_THRESHOLD:-2}"
# ~30 min re-alert at the 5-min cadence.
ALERT_THROTTLE_PASSES="${NDI_HALVING_ALERT_THROTTLE_PASSES:-6}"
# An input whose recv-timing line is ABSENT every pass (renamed/dropped/re-created, or a
# NDI_HALVING_INPUTS drift) stays UNKNOWN forever and never pages while the watchdog looks green --
# the "silent unknown" the standing rig-degradation rule forbids. Fire ONE "tap broken" WARN past
# this many CONSECUTIVE blind passes. Default ~2h at the 5-min cadence.
TAP_BROKEN_THRESHOLD="${NDI_HALVING_TAP_BROKEN_THRESHOLD:-24}"

SSH_USER="${NDI_HALVING_SSH_USER:-newlevel}"
SSH_PW="${NDI_HALVING_SSH_PW:-newlevel}"
SSH_TIMEOUT="${NDI_HALVING_SSH_TIMEOUT:-20}"
SSH_OPTS="${NDI_HALVING_SSH_OPTS:--o BatchMode=no -o StrictHostKeyChecking=no -o ConnectTimeout=8}"
OBS_LOG_TAIL="${NDI_HALVING_OBS_LOG_TAIL:-800}"

DECIDE="${NDI_HALVING_DECIDE:-$HERE/ndi_halving_decision.py}"
OBS_PHASE2="${NDI_HALVING_OBS_PHASE2:-$HERE/obs_phase2.py}"
NOTIFY="${AIRULESET_NOTIFY:-$HOME/devel/airuleset/airuleset.py}"
REPO_SLUG="${NDI_HALVING_ALERT_REPO:-zbynekdrlik/camera-box}"

STATE_DIR="${NDI_HALVING_STATE_DIR:-${XDG_RUNTIME_DIR:-/tmp}}"
_state_default="$STATE_DIR/camera-box-ndi-halving.state"
[ "$DRY_RUN" -eq 1 ] && _state_default="$STATE_DIR/camera-box-ndi-halving-dryrun.state"
STATE_FILE="${NDI_HALVING_STATE_FILE:-$_state_default}"
# Issue-1001's OWN state file -- read (never written) for the no-double-page guard.
NETREACH_STATE_FILE="${NDI_HALVING_NETREACH_STATE_FILE:-$STATE_DIR/camera-box-network-reach-alert.state}"

log() { printf '%s [ndi-halving-watchdog] %s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$*" >&2; }

# `now` seam: NDI_HALVING_NOW overrides `date +%s` so the cooldown escalation is deterministically
# testable (a --dry-run harness drives the clock).
now_epoch() { if [ -n "${NDI_HALVING_NOW:-}" ]; then printf '%s' "$NDI_HALVING_NOW"; else date +%s; fi; }

require_tools() {
  local missing=() t need=("python3")
  [ -z "${NDI_HALVING_PROBE_CMD:-}" ] && need+=("sshpass" "ssh" "timeout")
  for t in "${need[@]}"; do
    command -v "$t" >/dev/null 2>&1 || missing+=("$t")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    log "FATAL: required tool(s) not found on dev1: ${missing[*]} -- refusing to run (a missing tool would read every probe as empty forever, a permanently-blind watchdog)"
    return 1
  fi
  return 0
}

# -- I/O probe (dev1-local; NOT pure) -----------------------------------------------------------
# fetch_box_log <receiver_ip> -> stdout: the RAW newest-OBS-log tail (ALL inputs' recv-timing lines),
# or EMPTY on a failed/absent read. Called ONCE per pass. Overridable via NDI_HALVING_PROBE_CMD.
fetch_box_log() {
  local ip="$1"
  if [ -n "${NDI_HALVING_PROBE_CMD:-}" ]; then
    $NDI_HALVING_PROBE_CMD "$ip" 2>/dev/null || true
    return 0
  fi
  # shellcheck disable=SC2086
  timeout "$SSH_TIMEOUT" sshpass -p "$SSH_PW" ssh $SSH_OPTS "$SSH_USER@$ip" \
    "powershell -NoProfile -Command \"gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $OBS_LOG_TAIL\"" \
    2>/dev/null || true
}

# attempt_reattach <receiver_ip> <input> -> exit 0 = a reattach was driven; non-zero = the cure could
# not run. Overridable via NDI_HALVING_CURE_CMD (<ip> <input>). The default idles the receiver (reads
# + prints PREV_NDI_NAME, clears the name) then RESTORES it (overlay keeps the latency pin). CRITICAL:
# once idled, the name is EMPTY (a receiver-thread STOP -> permanent wedge per ndi-name-recovery.md),
# so the restore MUST run whenever PREV was captured -- retried once, and a persistent restore failure
# is logged LOUD (the operator/alert must know the input may be left idled).
attempt_reattach() {
  local ip="$1" input="$2"
  if [ -n "${NDI_HALVING_CURE_CMD:-}" ]; then
    $NDI_HALVING_CURE_CMD "$ip" "$input"
    return $?
  fi
  local idle_out prev
  idle_out="$(python3 "$OBS_PHASE2" idle-receiver --host "$ip" --password "$OBS_WS_PW" --input "$input" 2>&1)"
  prev="$(printf '%s\n' "$idle_out" | sed -n 's/^PREV_NDI_NAME=//p' | head -1)"
  if [ -z "$prev" ]; then
    log "CURE: idle-receiver did not return PREV_NDI_NAME for '$input' (WS auth/reachability?) -- NOT idling; no reattach: $idle_out"
    return 1
  fi
  local attempt
  for attempt in 1 2; do
    if python3 "$OBS_PHASE2" idle-receiver --host "$ip" --password "$OBS_WS_PW" --input "$input" --restore "$prev" >/dev/null 2>&1; then
      log "CURE: reattached '$input' (idle -> restore to '$prev'), attempt $attempt"
      return 0
    fi
  done
  log "CURE: FAILED to restore '$input' to '$prev' after idling -- the input may be LEFT IDLED (empty name); needs a manual set-ndi-mapping"
  return 1
}

# -- issue-1001 reachability read (never re-probed) ---------------------------------------------
netreach_box_alerted() {
  local box="$1"
  [ -f "$NETREACH_STATE_FILE" ] || { printf '0'; return 0; }
  local v
  v="$(sed -n "s/^alerted_${box}=//p" "$NETREACH_STATE_FILE" 2>/dev/null | tail -1)"
  printf '%s' "${v:-0}"
}

# -- persisted per-input state (key=value lines) ------------------------------------------------
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
  tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || echo "$STATE_FILE")"
  { [ -f "$STATE_FILE" ] && grep -v "^${key}=" "$STATE_FILE"; printf '%s=%s\n' "$key" "$val"; } \
    > "$tmp" 2>/dev/null || true
  mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
}

# An input key safe for state-field names (input names carry spaces). A short cksum of the RAW name
# is appended so two names that sanitize identically never share state.
input_key() {
  local san sum
  san="$(printf '%s' "$1" | tr -c 'A-Za-z0-9' '_')"
  sum="$(printf '%s' "$1" | cksum | cut -d' ' -f1)"
  printf '%s_%s' "$san" "$sum"
}

# Extract a `key=value` token (one per line) from the pure module's output.
kv_field() { sed -n "s/^${2}=//p" <<<"$1" | tail -1; }

# A HEALTHY input is not an incident: clear its confirm counter + throttle sig + cure state so a
# genuinely NEW halving later acts fresh. Does NOT clear the `alerted`/`cured` recovery latches.
clear_input_throttle() {
  local k="$1"
  write_state_field "confirm_${k}" 0
  write_state_field "alert_sig_${k}" ""
  write_state_field "alert_passes_${k}" 0
}

# Split a `<name>|<fps>` spec (name may contain spaces, never a '|').
spec_name() { case "$1" in *"|"*) printf '%s' "${1%|*}" ;; *) printf '%s' "$1" ;; esac; }
spec_fps()  { case "$1" in *"|"*) printf '%s' "${1##*|}" ;; *) printf '%s' "$NDI_HALVING_DEFAULT_FPS" ;; esac; }

# -- per-input decision -------------------------------------------------------------------------
# handle_input <input> <expected_fps> <receiver_reachable> <raw_log> <others_healthy_count>
handle_input() {
  local input="$1" exp="$2" reachable="$3" raw_log="$4" others_healthy="$5"
  local k verdict fps cap window samples out
  k="$(input_key "$input")"

  if [ "$reachable" = "1" ]; then
    out="$(printf '%s' "$raw_log" | python3 "$DECIDE" analyze \
      --source "$input" --expected-fps "$exp" --box-reachable 1 --expected-live 1 \
      --halving-ratio "$HALVING_RATIO" --cap-mult "$CAP_MULT" \
      --healthy-ratio "$HEALTHY_RATIO" --healthy-cap-mult "$HEALTHY_CAP_MULT" \
      --min-window-s "$MIN_WINDOW_S" --max-window-s "$MAX_WINDOW_S" 2>/dev/null)"
  else
    out="verdict=SKIP
fps=
cap_avg=
window_s=
n=
samples=0"
  fi
  verdict="$(kv_field "$out" verdict)"
  fps="$(kv_field "$out" fps)"
  cap="$(kv_field "$out" cap_avg)"
  window="$(kv_field "$out" window_s)"
  samples="$(kv_field "$out" samples)"
  [ -n "$verdict" ] || verdict="UNKNOWN"
  [ -n "$samples" ] || samples=0
  log "'$input' on $RECV_NAME (exp=${exp}fps): fps=${fps:-<n/a>} cap_avg=${cap:-<n/a>}ms win=${window:-<n/a>} samples=$samples reachable=$reachable -> $verdict"

  # Tap-liveness -- only meaningful when we actually PROBED. samples>=1 proves the tap works (even a
  # first-line/short-window UNKNOWN). samples==0 = a BLIND tap: track consecutive blind passes and
  # fire ONE "tap broken" WARN past the threshold. A SKIP pass (box down) leaves the counter untouched.
  if [ "$reachable" = "1" ]; then
    if [ "$samples" != "0" ]; then
      write_state_field "unknown_${k}" 0
      write_state_field "tap_broken_${k}" 0
    else
      local unk tap_alerted
      unk="$(read_state_field "unknown_${k}" 0)"
      case "$unk" in '' | *[!0-9]*) unk=0 ;; esac
      unk=$((unk + 1))
      write_state_field "unknown_${k}" "$unk"
      tap_alerted="$(read_state_field "tap_broken_${k}" 0)"
      if [ "$unk" -ge "$TAP_BROKEN_THRESHOLD" ] && [ "$tap_alerted" != "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD alert: '$input' recv-timing tap BROKEN (no #797 line for $unk consecutive passes)"
        else
          python3 "$NOTIFY" notify --body \
            "⚠️ NDI spojenie ($REPO_SLUG): meranie kadencie vstupu '$input' na $RECV_NAME je SLEPÉ už $unk kontrol po sebe (premenovaný/chýbajúci vstup alebo nečitateľný log) — kontrola polovičnej kadencie pre tento vstup je vypnutá, kým sa to neopraví. Rieši Claude automaticky." \
            >/dev/null 2>&1 || log "tap-broken: airuleset.py notify failed (non-fatal)"
        fi
        write_state_field "tap_broken_${k}" 1
      fi
    fi
  fi

  case "$verdict" in
    SKIP)
      return 0 ;;
    HEALTHY)
      # Recovery: fire ONE ping if we had cured or paged this input, then clear state.
      local was_alerted was_cured
      was_alerted="$(read_state_field "alerted_${k}" 0)"
      was_cured="$(read_state_field "cured_${k}" 0)"
      if [ "$was_alerted" = "1" ] || [ "$was_cured" = "1" ]; then
        if [ "$DRY_RUN" -eq 1 ]; then
          log "[dry-run] WOULD send recovery: '$input' back to ~${exp} fps"
        else
          python3 "$NOTIFY" notify --body \
            "✅ NDI spojenie ($REPO_SLUG): **$input** na $RECV_NAME je späť na plnú kadenciu (~${exp} fps)." \
            >/dev/null 2>&1 || log "RECOVERY: airuleset.py notify failed (non-fatal)"
        fi
        write_state_field "alerted_${k}" 0
        write_state_field "cured_${k}" 0
      fi
      clear_input_throttle "$k"
      return 0 ;;
    HALVED)
      : ;;  # fall through to confirm
    *)
      # BORDERLINE / UNKNOWN: hold -- do NOT advance OR reset the confirm counter.
      log "'$input' $verdict this pass -- holding (confirm unchanged)"
      return 0 ;;
  esac

  # HALVED -> confirm across CONFIRM_THRESHOLD consecutive passes.
  local prev_confirm decision confirm act
  prev_confirm="$(read_state_field "confirm_${k}" 0)"
  decision="$(obs_watchdog_confirm "$prev_confirm" 1 "$CONFIRM_THRESHOLD")"
  confirm="$(printf '%s\n' "$decision" | sed -n 's/^confirm=//p')"
  act="$(printf '%s\n' "$decision" | sed -n 's/^act=//p')"
  write_state_field "confirm_${k}" "${confirm:-0}"
  log "'$input' HALVED confirm=$prev_confirm -> $confirm act=$act (threshold=$CONFIRM_THRESHOLD)"
  if [ "${act:-0}" != "1" ]; then
    log "'$input' HALVED this pass but not yet CONFIRMED across $CONFIRM_THRESHOLD passes -- holding"
    return 0
  fi

  # CONFIRMED halved -> cure (if armed + cooldown) or page.
  local now last_cure cd action cooldown_ok
  now="$(now_epoch)"
  last_cure="$(read_state_field "cure_ts_${k}" "")"
  cd="$(python3 "$DECIDE" cure-decision --cure-enabled "$SELFHEAL" --last-cure-ts "$last_cure" --now "$now" --cooldown-s "$COOLDOWN_S" 2>/dev/null)"
  action="$(kv_field "$cd" action)"
  cooldown_ok="$(kv_field "$cd" cooldown_ok)"
  [ -n "$action" ] || action="page"

  # Healthy-sibling context = the box-wide vs per-connection discriminator (CONTEXT only, never gates).
  local sibling_note
  if [ "${others_healthy:-0}" -gt 0 ]; then
    sibling_note="sesterský vstup je zdravý → per-connection degradácia (nie box-wide)"
  else
    sibling_note="žiadny zdravý sesterský vstup tento pass → môže byť aj box-wide"
  fi

  if [ "$action" = "cure" ]; then
    # State bookkeeping (cure_ts / cured) advances in BOTH modes so the cooldown escalation is
    # faithful under --dry-run; only the actual reattach side effect is dry-run-gated (the family
    # "--dry-run skips the POST/action, never the bookkeeping" convention).
    if [ "$DRY_RUN" -eq 1 ]; then
      log "[dry-run] WOULD cure: reattach '$input' (idle-receiver -> restore); cooldown_ok=$cooldown_ok ($sibling_note)"
    elif attempt_reattach "$RECV_IP" "$input"; then
      log "'$input' CONFIRMED halved -> reattach attempted; re-measuring next pass ($sibling_note)"
    else
      log "'$input' CONFIRMED halved -> reattach could NOT run ($sibling_note)"
    fi
    write_state_field "cure_ts_${k}" "$now"
    write_state_field "cured_${k}" 1
    return 0
  fi

  # PAGE (cure disabled, or a cure already ran this episode and it is still halved -> no spam).
  write_state_field "alerted_${k}" 1
  local sig_fps show_fps current_sig prior_sig prior_passes throttle_out alert_now new_sig new_passes cure_txt
  sig_fps="$(LC_ALL=C awk -v f="${fps:-0}" 'BEGIN{printf "%d", f + 0.5}')"
  show_fps="$(LC_ALL=C awk -v f="${fps:-0}" 'BEGIN{printf "%.1f", f + 0}')"
  current_sig="halved:${k}:${sig_fps}"
  prior_sig="$(read_state_field "alert_sig_${k}" "")"
  prior_passes="$(read_state_field "alert_passes_${k}" 0)"
  throttle_out="$(obs_watchdog_alert_throttle "$current_sig" "$prior_sig" "$prior_passes" "$ALERT_THROTTLE_PASSES")"
  alert_now="$(printf '%s\n' "$throttle_out" | sed -n 's/^alert_now=//p')"
  new_sig="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_sig=//p')"
  new_passes="$(printf '%s\n' "$throttle_out" | sed -n 's/^new_passes=//p')"
  write_state_field "alert_sig_${k}" "$new_sig"
  write_state_field "alert_passes_${k}" "$new_passes"

  if [ "$SELFHEAL" = "1" ]; then
    cure_txt="auto-reattach bol skúšaný, no spojenie ostáva polovičné"
  else
    cure_txt="auto-liečenie je vypnuté (report-only) — reštartuje sa reattach-om vstupu (obs_phase2 idle-receiver → restore)"
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] WOULD alert: '$input' CONFIRMED halved (${show_fps} fps, cap ${cap:-?}ms) alert_now=$alert_now"
    return 0
  fi
  if [ "${alert_now:-0}" = "1" ]; then
    python3 "$NOTIFY" notify --body \
      "🚨 NDI spojenie ($REPO_SLUG): vstup **$input** na $RECV_NAME beží na POLOVIČNEJ kadencii — nameraných ${show_fps} fps (očak. ${exp}), recv cap_avg ${cap:-?} ms. Potvrdené počas ${CONFIRM_THRESHOLD} kontrol; $sibling_note. $cure_txt." \
      >/dev/null 2>&1 || log "ALERT: airuleset.py notify failed (non-fatal)"
  else
    log "ALERT: suppressed by throttle (pass ${prior_passes}/${ALERT_THROTTLE_PASSES})"
  fi
}

main() {
  log "pass start (dry_run=$DRY_RUN, receiver='$NDI_HALVING_RECEIVER', sender='$SENDER_NAME', selfheal=$SELFHEAL, inputs='$NDI_HALVING_INPUTS')"
  require_tools || { log "pass end (aborted: missing required tools)"; return 3; }

  # No-double-page guard: BOTH the receiver AND the sender must be reachable per issue-1001's state,
  # else issue-1001 already owns the page and a halving reading is out of scope -> SKIP everything.
  local recv_down sender_down reachable raw_log=""
  recv_down="$(netreach_box_alerted "$RECV_NAME")"
  sender_down="$(netreach_box_alerted "$SENDER_NAME")"
  if [ "$recv_down" = "1" ] || [ "$sender_down" = "1" ]; then
    reachable=0
    log "issue-1001 state: $RECV_NAME down=$recv_down / $SENDER_NAME down=$sender_down -> #1001 owns the page, SKIP all inputs this pass"
  else
    reachable=1
    raw_log="$(fetch_box_log "$RECV_IP")"
  fi

  # Split ';'-separated specs (names contain spaces -> split on ';', set -f so a metachar never globs).
  local old_ifs="$IFS" spec
  local -a specs=()
  set -f
  IFS=';'
  for spec in $NDI_HALVING_INPUTS; do specs+=("$spec"); done
  IFS="$old_ifs"
  set +f

  # PHASE 1 -- classify every input once; count HEALTHY inputs (the sibling discriminator).
  local -a v_name v_exp
  local healthy_count=0 name exp out verdict
  for spec in "${specs[@]}"; do
    name="$(spec_name "$spec")"; exp="$(spec_fps "$spec")"
    name="${name#"${name%%[![:space:]]*}"}"; name="${name%"${name##*[![:space:]]}"}"  # trim
    [ -n "$name" ] || continue
    if [ "$reachable" = "1" ]; then
      out="$(printf '%s' "$raw_log" | python3 "$DECIDE" analyze \
        --source "$name" --expected-fps "$exp" --box-reachable 1 --expected-live 1 \
        --halving-ratio "$HALVING_RATIO" --cap-mult "$CAP_MULT" \
        --healthy-ratio "$HEALTHY_RATIO" --healthy-cap-mult "$HEALTHY_CAP_MULT" \
        --min-window-s "$MIN_WINDOW_S" --max-window-s "$MAX_WINDOW_S" 2>/dev/null)"
      verdict="$(kv_field "$out" verdict)"
      [ "$verdict" = "HEALTHY" ] && healthy_count=$((healthy_count + 1))
    fi
    v_name+=("$name"); v_exp+=("$exp")
  done

  # PHASE 2 -- act on each input, told the count of OTHER healthy inputs (a HALVED input is never
  # HEALTHY, so healthy_count already excludes it -> it IS the others-healthy count).
  local i
  for i in "${!v_name[@]}"; do
    handle_input "${v_name[$i]}" "${v_exp[$i]}" "$reachable" "$raw_log" "$healthy_count"
  done
  log "pass end"
}

# Run only when EXECUTED (systemd/CLI). Sourcing (tests) only defines the functions above.
if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main
fi
