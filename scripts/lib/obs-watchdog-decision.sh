#!/usr/bin/env bash
# scripts/lib/obs-watchdog-decision.sh — the PURE heart of the #391 broadcast-OBS liveness
# watchdog: confirm-counter + alert-throttle. No I/O, no ssh, no OBS, no MCP — pure so it can be
# unit-tested exhaustively (mirrors the shape of scripts/lib/rig-restore-decision.sh).
#
# WHY (#391): stream OBS wedged (obs64 pegged ~168% CPU, Responding=False, 16.0% render-lag) for
# ~25 HOURS and nothing detected it. This watchdog closes that gap by polling OBS WebSocket
# GetStats (+ optionally agent/MCP-sampled process signals) on a dev1 systemd timer, running the
# strict detector (obs_watchdog::classify, via the obs-watchdog-gate binary), and — once a wedge is
# CONFIRMED across N consecutive passes (avoiding a false alarm from one transient WS hiccup) —
# firing a Discord alert. Recovery is AGENT-driven (the alert carries the exact
# `scripts/launch-obs-genlock.sh --box <box> --force` recovery plan): a dev1 timer has no ssh/MCP
# access to the Windows boxes, so it cannot itself force-kill/relaunch obs64 (see the #391 PR
# description for the auto-recover design decision and its rationale).
#
# Source-only: defines obs_watchdog_confirm() + obs_watchdog_alert_throttle(); runs nothing.
#
# obs_watchdog_confirm <prev_confirm> <is_wedged 0|1> [threshold=2]
#   -> stdout: confirm=<n>  act=<0|1>
#   A NOT-wedged pass always resets the counter to 0 (a single healthy read clears any pending
#   confirmation — the same "reset on clean signal" discipline as rig_restore_decide). A wedged
#   pass increments the counter; act=1 only once it reaches the threshold (default 2 — one
#   transient WS hiccup / GetStats blip must never fire an alert on its own).
obs_watchdog_confirm() {
  local prev="${1:-0}" wedged="${2:-0}" threshold="${3:-2}"
  case "$prev$threshold" in *[!0-9]* | "") prev=0; threshold=2 ;; esac

  if [ "$wedged" != "1" ]; then
    printf 'confirm=0\nact=0\n'
    return 0
  fi

  local new_confirm=$(( prev + 1 ))
  if [ "$new_confirm" -ge "$threshold" ]; then
    printf 'confirm=%s\nact=1\n' "$new_confirm"
  else
    printf 'confirm=%s\nact=0\n' "$new_confirm"
  fi
}

# obs_watchdog_alert_throttle <current_sig> <prior_sig> <prior_passes> [throttle_n=10]
#   -> stdout: alert_now=0|1  new_sig=<sig>  new_passes=<n>
#   Once a wedge is confirmed (act=1 above), alert on the FIRST occurrence or whenever the
#   condition's signature changes (a different box, or a different verdict label — e.g.
#   WEDGED-RENDER-LAG escalating to OBS-COUNT-WRONG), then re-alert every throttle_n passes while
#   the SAME condition persists (a ~2min timer cadence * 10 = a reminder ping roughly every 20min,
#   not a ping every single pass while the operator is already working the wedge).
obs_watchdog_alert_throttle() {
  local current_sig="${1:-}" prior_sig="${2:-}" prior_passes="${3:-0}" throttle_n="${4:-10}"
  case "$prior_passes$throttle_n" in *[!0-9]* | "") prior_passes=0; throttle_n=10 ;; esac

  if [ "$current_sig" != "$prior_sig" ]; then
    printf 'alert_now=1\nnew_sig=%s\nnew_passes=1\n' "$current_sig"
  elif [ "$prior_passes" -ge "$throttle_n" ]; then
    printf 'alert_now=1\nnew_sig=%s\nnew_passes=1\n' "$current_sig"
  else
    local new_p=$(( prior_passes + 1 ))
    printf 'alert_now=0\nnew_sig=%s\nnew_passes=%s\n' "$current_sig" "$new_p"
  fi
}

# obs_watchdog_clear_hysteresis <pass_class> <prior_clear_passes> [clear_n=12]
#   pass_class ∈ episode | healthy | unmeasured
#   -> stdout: action=<keep|clear>  clear_passes=<n>
#   Hysteresis on CLEARING a dedup signature so a chronically FLAPPING condition (healthy/unhealthy
#   alternating across passes) is treated as ONE ongoing episode instead of re-paging on every
#   re-onset. The alert-throttle above suppresses REPEAT pages of a persistent condition, but the
#   moment a single healthy pass resets the signature, the next re-onset reads as a brand-new
#   condition (current_sig != prior_sig) and pages again — so a borderline condition that flaps
#   across 5-min passes defeats the ~1h throttle entirely. This gate decides whether a healthy pass
#   has actually RESOLVED the episode: only after `clear_n` CONSECUTIVE measured-healthy passes.
#     - `episode`    (the condition is active this pass) -> keep the signature; RESET the healthy
#                    streak to 0 (any re-onset breaks the run of clean passes).
#     - `healthy`    (a MEASURED-healthy pass) -> increment the streak; CLEAR only once it reaches
#                    clear_n consecutive healthy passes (streak reset to 0); otherwise keep the
#                    signature (a shorter healthy run is still inside the hysteresis window).
#     - `unmeasured` (this pass carried no reading) -> advance NOTHING (the #1076 contract): keep
#                    the signature AND leave the streak unchanged, so a measurement gap neither
#                    counts toward clearing nor resets the healthy run.
obs_watchdog_clear_hysteresis() {
  local cls="${1:-}" prior="${2:-0}" clear_n="${3:-12}"
  case "$prior$clear_n" in *[!0-9]* | "") prior=0; clear_n=12 ;; esac

  case "$cls" in
    episode)
      printf 'action=keep\nclear_passes=0\n'
      ;;
    healthy)
      local new_streak=$(( prior + 1 ))
      if [ "$new_streak" -ge "$clear_n" ]; then
        printf 'action=clear\nclear_passes=0\n'
      else
        printf 'action=keep\nclear_passes=%s\n' "$new_streak"
      fi
      ;;
    *)
      # unmeasured (or any unexpected token, fail-safe): advance nothing, preserve everything.
      printf 'action=keep\nclear_passes=%s\n' "$prior"
      ;;
  esac
}
