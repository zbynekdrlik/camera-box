#!/usr/bin/env bash
# scripts/lib/rig-heartbeat.sh — the "rig is LEGITIMATELY in a test state" heartbeat. Refs #281.
#
# WHY (#281 Fix#3): the rig-restore watchdog must NEVER fight a real E2E. A rig-touching harness
# (recording-e2e.sh, rig-mode.sh TEST) writes a well-known heartbeat file while it holds the rig in
# a TEST/BURN state; the watchdog treats a FRESH heartbeat as "a legit test is running — do not
# act". When the harness exits cleanly (its trap) OR a worker dies (its refresher dies with it),
# the heartbeat is removed or goes STALE — and only THEN may the watchdog consider the rig stranded.
# This is the conservative gate that prevents the #266-style false positives.
#
# Source-only: this file defines functions and runs nothing on its own.
#
# Heartbeat path (single well-known location, documented):
#   $CAMERA_BOX_RIG_HEARTBEAT                       (explicit override, used by tests)
#   else ${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/camera-box-rig-active   (preferred)
#   else /tmp/camera-box-rig-active                 (fallback when the runtime dir is unusable)
#
# File format (one line):  <epoch_seconds>\t<label>\t<pid>
# The watchdog age-checks the embedded epoch (a stale heartbeat > N min = no longer live proof).
#
# Tunables:
#   RIG_HEARTBEAT_REFRESH_SEC  refresher re-write interval (default 30s)
#   RIG_HEARTBEAT_STALE_SEC    age beyond which a heartbeat is "stale" (default 600s = 10 min)

# rig_heartbeat_path -> echo the resolved heartbeat file path (computed once per call).
rig_heartbeat_path() {
  if [ -n "${CAMERA_BOX_RIG_HEARTBEAT:-}" ]; then
    printf '%s\n' "$CAMERA_BOX_RIG_HEARTBEAT"
    return 0
  fi
  local runtime="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  if [ -d "$runtime" ] && [ -w "$runtime" ]; then
    printf '%s\n' "$runtime/camera-box-rig-active"
  else
    printf '%s\n' "/tmp/camera-box-rig-active"
  fi
}

# _rig_hb_refresher_pidfile -> the sidecar file holding the background refresher PID.
_rig_hb_refresher_pidfile() {
  printf '%s\n' "$(rig_heartbeat_path).refresher.pid"
}

# rig_heartbeat_is_fresh <last_epoch> <now_epoch> <stale_sec> -> exit 0 (fresh) / 1 (stale).
# PURE: arithmetic only, no I/O — unit-tested directly. Non-numeric / future timestamps are treated
# conservatively as STALE so a corrupt heartbeat can never wrongly suppress the watchdog.
rig_heartbeat_is_fresh() {
  local last="${1:-}" now="${2:-}" stale="${3:-}"
  case "$last$now$stale" in
    *[!0-9]* | "") return 1 ;;  # any non-digit (or empty) field -> not provably fresh -> stale
  esac
  local age=$(( now - last ))
  [ "$age" -ge 0 ] && [ "$age" -le "$stale" ]
}

# rig_heartbeat_write [label] -> write/refresh the heartbeat (epoch\tlabel\tpid). Creates the
# parent dir if needed. This is also the refresher's per-tick primitive.
rig_heartbeat_write() {
  local label="${1:-rig-test}"
  local path; path="$(rig_heartbeat_path)"
  local dir; dir="$(dirname "$path")"
  [ -d "$dir" ] || mkdir -p "$dir" 2>/dev/null || true
  printf '%s\t%s\t%s\n' "$(date +%s)" "$label" "$$" > "$path"
}

# rig_heartbeat_age_seconds -> echo the heartbeat age in seconds; exit 1 if the file is absent.
# Reads the embedded epoch (first TAB field); falls back to the file mtime if it is unparsable.
rig_heartbeat_age_seconds() {
  local path; path="$(rig_heartbeat_path)"
  [ -f "$path" ] || { printf '%s\n' 999999; return 1; }
  local now; now="$(date +%s)"
  local last; last="$(cut -f1 < "$path" 2>/dev/null)"
  case "$last" in
    "" | *[!0-9]*) last="$(stat -c %Y "$path" 2>/dev/null || echo 0)" ;;
  esac
  printf '%s\n' "$(( now - last ))"
}

# rig_heartbeat_active -> exit 0 if a FRESH heartbeat exists (legit E2E), else 1.
rig_heartbeat_active() {
  local path; path="$(rig_heartbeat_path)"
  [ -f "$path" ] || return 1
  local now; now="$(date +%s)"
  local last; last="$(cut -f1 < "$path" 2>/dev/null)"
  rig_heartbeat_is_fresh "$last" "$now" "${RIG_HEARTBEAT_STALE_SEC:-600}"
}

# rig_heartbeat_start [label] -> write the heartbeat once, then spawn a background refresher that
# re-writes it every RIG_HEARTBEAT_REFRESH_SEC so the watchdog sees a continuously-fresh heartbeat
# for the whole run. The refresher PID is recorded so rig_heartbeat_stop can kill it.
rig_heartbeat_start() {
  local label="${1:-rig-test}"
  rig_heartbeat_write "$label"
  local interval="${RIG_HEARTBEAT_REFRESH_SEC:-30}"
  # The OWNER is the harness shell that called start. The refresher self-terminates AND removes the
  # heartbeat the moment the owner dies — even on SIGKILL (-9), which bypasses the harness's cleanup
  # trap. Without this, the disowned refresher would keep re-writing a "fresh" heartbeat forever
  # after a -9'd harness, so the watchdog would NEVER recover that genuinely-stranded rig (the exact
  # #281 failure). `$$` is the harness PID even inside the subshell (overridable for tests).
  local owner="${RIG_HEARTBEAT_OWNER_PID:-$$}"
  local path; path="$(rig_heartbeat_path)"
  # Subshell refresher: re-write each tick while the owner lives; on owner death / interrupt, remove
  # the heartbeat so it can never lie "fresh". Detached from the caller's job control.
  (
    while :; do
      sleep "$interval" || break
      kill -0 "$owner" 2>/dev/null || break   # owner gone (incl. SIGKILL) -> stop lying "fresh"
      rig_heartbeat_write "$label" || break
    done
    rm -f "$path" 2>/dev/null
  ) >/dev/null 2>&1 &
  local rpid=$!
  printf '%s\n' "$rpid" > "$(_rig_hb_refresher_pidfile)" 2>/dev/null || true
  # Best-effort: don't leave the refresher as a tracked job that blocks the caller on exit.
  disown "$rpid" 2>/dev/null || true
}

# rig_heartbeat_clear -> remove the heartbeat file (and refresher pidfile). Idempotent: clearing an
# absent heartbeat is a no-op success. Also kills the refresher if one is recorded.
rig_heartbeat_clear() {
  local pf; pf="$(_rig_hb_refresher_pidfile)"
  if [ -f "$pf" ]; then
    local rpid; rpid="$(cat "$pf" 2>/dev/null)"
    case "$rpid" in
      "" | *[!0-9]*) : ;;
      *) kill "$rpid" 2>/dev/null || true ;;
    esac
    rm -f "$pf" 2>/dev/null || true
  fi
  rm -f "$(rig_heartbeat_path)" 2>/dev/null || true
  return 0
}

# rig_heartbeat_stop -> alias for clear (stop the refresher + remove the heartbeat). Provided as the
# symmetric counterpart to rig_heartbeat_start for callers that pair start/stop.
rig_heartbeat_stop() {
  rig_heartbeat_clear
}

# ── #353: the "rig is in an UNCLEANED E2E test state" MARKER ──────────────────────────────────
# DISTINCT from the heartbeat above (which stays a separate "don't act during a LIVE E2E" gate):
#   - The HEARTBEAT is REFRESHED while a legit E2E runs and the refresher REMOVES it the instant the
#     harness dies — so a FRESH heartbeat means "a legit E2E is live, never act".
#   - The MARKER is written ONCE on harness entry and removed ONLY by the harness's clean-exit
#     cleanup(). An UNCLEAN death (SIGKILL / interrupted trap) leaves it behind.
# Therefore "marker present AND heartbeat absent/stale" = a harness entered a test state and never
# cleaned up = the rig is STRANDED, regardless of which scene OBS is on. This replaces the fragile
# OBS scene-name scraping (#353): no scene-list to keep in sync across two files, robust to
# env-overridden / custom scene names.
#
# Marker path (single well-known location, documented):
#   $CAMERA_BOX_RIG_E2E_MARKER                       (explicit override, used by tests)
#   else ${XDG_RUNTIME_DIR:-/run}/rig-in-e2e         (preferred / fallback)
rig_e2e_marker_path() {
  if [ -n "${CAMERA_BOX_RIG_E2E_MARKER:-}" ]; then
    printf '%s\n' "$CAMERA_BOX_RIG_E2E_MARKER"
    return 0
  fi
  printf '%s\n' "${XDG_RUNTIME_DIR:-/run}/rig-in-e2e"
}

# rig_e2e_marker_set [label] -> create the marker (records epoch<TAB>label<TAB>pid for diagnosis).
# Creates the parent dir if needed. Presence — not freshness — is the signal.
rig_e2e_marker_set() {
  local label="${1:-rig-e2e}"
  local path; path="$(rig_e2e_marker_path)"
  local dir; dir="$(dirname "$path")"
  [ -d "$dir" ] || mkdir -p "$dir" 2>/dev/null || true
  printf '%s\t%s\t%s\n' "$(date +%s)" "$label" "$$" > "$path" 2>/dev/null || true
}

# rig_e2e_marker_clear -> remove the marker. Idempotent: clearing an absent marker is a no-op success.
rig_e2e_marker_clear() {
  rm -f "$(rig_e2e_marker_path)" 2>/dev/null || true
  return 0
}

# rig_e2e_marker_present -> exit 0 if the marker file exists, else 1. The marker has NO freshness
# window — it persists until a clean cleanup() removes it (that persistence IS the stranded signal).
rig_e2e_marker_present() {
  [ -f "$(rig_e2e_marker_path)" ]
}
