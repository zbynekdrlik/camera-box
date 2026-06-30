#!/usr/bin/env bash
# scripts/lib/rig-restore-decision.sh — the PURE heart of the #281 Fix#3 rig-restore watchdog.
#
# WHY (#281): the prior #266 auto-watchdog was REMOVED for false positives. This decision function
# is deliberately conservative AND pure (no I/O, no ssh, no OBS) so it can be unit-tested
# exhaustively. The impure watchdog (scripts/rig-restore-watchdog.sh) gathers the observations and
# executes the actions; ALL the "should we act?" logic lives here.
#
# Source-only: defines rig_restore_decide() and runs nothing on its own.
#
# Inputs (env vars):
#   RIG_HB_ACTIVE          1 = a FRESH heartbeat is present (a legit E2E is running) -> NEVER act.
#   RIG_PREV_CONFIRM       the prior consecutive-stranded count the CALLER persisted (default 0).
#   RIG_CONFIRM_THRESHOLD  consecutive confirmations required before acting (default 2 — the #266
#                          "2-live-sample" lesson). 1 = act on the first sighting.
#   RIG_E2E_MARKER         1 = the E2E MARKER file is present (scripts/lib/rig-heartbeat.sh:
#                          rig_e2e_marker_*). #353: the harness writes the marker on entry and
#                          removes it ONLY on clean exit, so a leftover marker (while the heartbeat
#                          is absent/stale — Rule 1 still gates this) means a harness entered a TEST
#                          state and died WITHOUT cleaning up = the rig is stranded, REGARDLESS of
#                          which scene OBS is on. When set, EVERY observed `obs` box is restored.
#                          This is the PRIMARY stranded signal — it replaces the fragile OBS
#                          scene-name scraping (robust to env-overridden/custom scene names, no
#                          two-file scene-list to keep in sync). RIG_KNOWN_TEST_SCENES below stays as
#                          a belt-and-suspenders fallback for runs with no marker (older harness /
#                          manual testing). Cam restore is unaffected (the reliable down/probe
#                          signals drive it — a healthy cam is never restored on the marker alone).
#   RIG_KNOWN_TEST_SCENES  space-separated list of program scene names that PROVE a TEST state
#                          (fallback when no marker; default "PHASE2-PROBE":
#                            PHASE2-PROBE — the canonical obs_phase2.py phase2 probe scene on strih).
#                          #343/#353: REC-STRIH-TMP was DROPPED from the default — recording-e2e.sh
#                          now records the stream box's already-active prod scene (PRO), so the
#                          stream box never lands on the ephemeral REC-STRIH-TMP scene; the marker
#                          (above), not a scene name, detects a stranded stream box now.
#                          #352: each scene name in this list must NOT contain spaces — the matcher
#                          word-splits on $known (see the loop below), so a spaced entry like
#                          "NDI 2ME PGM" would split into 3 tokens and never match the full program
#                          scene name. Use hyphenated test-scene names only.
#   RIG_OBS                newline-separated observation records:
#                            cam <name> down=<0|1> probe=<0|1>
#                            obs <name> scene=<program scene name>
#
# Output (stdout, key=value lines):
#   confirm=<n>      the NEW consecutive-stranded count (caller persists it for the next run).
#   act=<0|1>        1 = execute the restore actions this run.
#   alert=<0|1>      1 = fire the Discord alert (== act; acting ALWAYS alerts).
#   actions=<tok…>   space-separated restore tokens (only when act=1): restore_cam:<name> /
#                    restore_obs:<name>.
#   reason=<text>    short machine/human reason for the decision.
#
# Decision rules:
#   1. RIG_HB_ACTIVE=1            -> confirm=0, act=0, alert=0  (legit E2E — the false-positive gate)
#   2. no clear stranded signal   -> confirm=0, act=0, alert=0  (clean rig — reset the counter)
#   3. a clear stranded signal:
#        new_confirm = prev + 1
#        new_confirm >= threshold -> act=1, alert=1, actions=<matched restores>  (confirmed)
#        else                     -> act=0, alert=0             (1st sighting — observe only)

# rig_restore_decide -> read the env inputs above, print the key=value decision to stdout.
rig_restore_decide() {
  local hb_active="${RIG_HB_ACTIVE:-0}"
  local prev="${RIG_PREV_CONFIRM:-0}"
  local threshold="${RIG_CONFIRM_THRESHOLD:-2}"
  local known="${RIG_KNOWN_TEST_SCENES:-PHASE2-PROBE}"
  local marker="${RIG_E2E_MARKER:-0}"
  case "$prev$threshold" in *[!0-9]* | "") prev=0; threshold=2 ;; esac

  # Rule 1 — fresh heartbeat: a legit E2E is running, NEVER act (resets the counter).
  if [ "$hb_active" = "1" ]; then
    _rig_decide_emit 0 0 0 "" "heartbeat-fresh-legit-e2e"
    return 0
  fi

  # Gather the CLEAR stranded signals from the observations.
  local actions=""
  local line kind name rest
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    kind="${line%% *}"; rest="${line#* }"
    name="${rest%% *}"; rest="${rest#* }"
    case "$kind" in
      cam)
        local down=0 probe=0 tok
        for tok in $rest; do
          case "$tok" in
            down=1)  down=1 ;;
            probe=1) probe=1 ;;
          esac
        done
        if [ "$down" = "1" ] || [ "$probe" = "1" ]; then
          actions="$actions restore_cam:$name"
        fi
        ;;
      obs)
        local scene="${rest#scene=}" ks
        # #353: the E2E MARKER is the PRIMARY stranded signal. When the harness left its marker
        # behind (entered a test state and never cleaned up) while the heartbeat is absent/stale
        # (Rule 1 already short-circuited a fresh heartbeat above), restore EVERY observed OBS box
        # REGARDLESS of which scene it is on. This replaces the fragile scene-name scraping — robust
        # to env-overridden/custom scene names, no two-file scene-list to keep in sync. The
        # RIG_KNOWN_TEST_SCENES match below remains a belt-and-suspenders fallback for runs with no
        # marker (older harness / manual testing).
        if [ "$marker" = "1" ]; then
          actions="$actions restore_obs:$name"
        else
          # #352: scene names in $known must NOT contain spaces — this word-split is INTENTIONAL.
          # A spaced entry (e.g. "NDI 2ME PGM") splits into separate tokens, none of which equals
          # the full program scene name, so it would silently never match. Keep test scenes hyphenated.
          for ks in $known; do
            if [ "$scene" = "$ks" ]; then
              actions="$actions restore_obs:$name"
              break
            fi
          done
        fi
        ;;
    esac
  done <<EOF
${RIG_OBS:-}
EOF

  # collapse leading whitespace
  actions="${actions# }"

  # Rule 2 — no clear stranded signal: clean rig, reset the counter.
  if [ -z "$actions" ]; then
    _rig_decide_emit 0 0 0 "" "no-stranded-signal"
    return 0
  fi

  # Rule 3 — a clear stranded signal while the heartbeat is absent/stale.
  local new_confirm=$(( prev + 1 ))
  if [ "$new_confirm" -ge "$threshold" ]; then
    _rig_decide_emit "$new_confirm" 1 1 "$actions" "confirmed-stranded-${new_confirm}of${threshold}"
  else
    # First sighting: observe only, no action, no alert — but report the pending signals.
    _rig_decide_emit "$new_confirm" 0 0 "" "observe-only-sighting-${new_confirm}of${threshold}"
  fi
  return 0
}

# rig_marker_should_clear <marker> <act> <obs_unreadable> <obs_failed> -> exit 0 to CLEAR the marker.
# #353 (review): the watchdog must clear the E2E marker (left by an unclean harness death) ONLY once
# it has POSITIVELY restored every OBS box that could be stranded — i.e. it acted, AND no OBS box was
# unreadable this pass (an unreadable box might hide a stranded scene the marker must keep flagging),
# AND every OBS teardown succeeded. Otherwise KEEP the marker so a later pass retries — clearing
# prematurely drops the durable signal and a box on a custom/env scene (outside the fallback list)
# would stay stranded forever (the masking bug). PURE: integer args only, unit-tested. Any
# non-numeric/empty arg is treated conservatively as "KEEP" (never clear on garbage input).
rig_marker_should_clear() {
  local marker="${1:-0}" act="${2:-0}" obs_unreadable="${3:-0}" obs_failed="${4:-0}"
  case "$marker$act$obs_unreadable$obs_failed" in *[!0-9]* | "") return 1 ;; esac
  [ "$marker" = "1" ] && [ "$act" = "1" ] && [ "$obs_unreadable" -eq 0 ] && [ "$obs_failed" -eq 0 ]
}

# _rig_decide_emit <confirm> <act> <alert> <actions> <reason>
_rig_decide_emit() {
  printf 'confirm=%s\n' "$1"
  printf 'act=%s\n' "$2"
  printf 'alert=%s\n' "$3"
  printf 'actions=%s\n' "$4"
  printf 'reason=%s\n' "$5"
}
