#!/usr/bin/env bash
# scripts/lib/qr-align-reinit.sh — see the extended header below.
set -euo pipefail
# ---------------------------------------------------------------------------------------------
# issue 1349 — the [4i/8align] per-open capture-lag RE-INIT loop.
#
# WHY: each cambox's capture lag `k` (source frames) is DRAWN when the V4L2 device is opened at
# recording-e2e.sh's [2/8]/[2b/8] burn deploy and then HOLDS (the frame a grabber delivers at time t
# shows the HDMI image from t - k*16.667ms). With 7 boxes of mixed grabbers that is a per-open
# lottery: the floor-aware plan's owner-mandated 94 ms ceiling absorbs at most ~1 source frame, so
# the run passes only when all 7 draws land within one frame (design 5742993777 — two red release
# E2Es 19.9.2026, and the chronic cam3 +13..20 ms A/V bias). Instead of ABORTING on a bad draw, this
# bounded loop runs immediately BEFORE [4i/8align]: measure the per-source frame lag (measure-only),
# restart the BURN instance of any box more than REINIT_OK_FRAMES behind the fastest (RE-DRAWING its
# k by re-opening /dev/videoN), settle, re-measure — at most REINIT_MAX_ROUNDS rounds, then fall
# through to the existing floor-aware plan whose HARD-FAIL stays the arbiter. It NEVER aborts the run
# and ALWAYS returns 0 under the caller's `set -euo pipefail`.
#
# RE-INIT MECHANISM (worker finding, on the ticket): re-init = `systemctl restart` of the SAME
# transient burn unit the [2/8]/[2b/8] step created (cam1/source-role: camera-box-burn-<RUN_ID>;
# secondaries: camera-box-burn-<cam>-<RUN_ID>). Re-running the FULL start block is structurally WRONG
# for a re-init — its `systemd-run --unit=<same>` fails on an already-existing unit, and its
# `systemctl stop camera-box; pkill -x camera-box` would kill the running burn (the burn binary's
# process name IS camera-box). Restarting the unit re-execs the same binary → reopens the device →
# re-draws k, without touching the heavily anchor-tested inline [2/8]/[2b/8] lines.
#
# SEAMS (Tier-0): QR_ALIGN_REINIT_MEASURE_CMD (a command NAME called `<cmd> <host> <sources_csv>` →
# the measure-only JSON on stdout) and QR_ALIGN_REINIT_RESTART_CMD (a command NAME called
# `<cmd> <camN>` → restart that box's burn instance). Both default to the production helpers below
# when unset; a test injects fakes so no rig is touched. Knobs: REINIT_OK_FRAMES (default 1),
# REINIT_MAX_ROUNDS (default 3), QR_ALIGN_SETTLE_S (default 15, mirroring qr-align.sh's existing
# QR_ALIGN_RESET_SETTLE_S align-settle default).
#
# `set -euo pipefail` above is set within the first lines (the #821 hook + owner script policy); the
# caller (recording-e2e.sh) already runs under it, so this is a no-op there. Every pure function is
# written to be set-e-safe (failing sub-commands live in conditions / substitutions / process
# substitution), and the orchestrator ALWAYS returns 0.

# qr_align_reinit_cam_of_source <source> -> the cam token of an "NDI camN" source (-> camN). Also
# tolerant of a bare "camN" (returns it unchanged).
qr_align_reinit_cam_of_source() {
  local s="${1:-}"
  printf '%s\n' "${s##* }"
}

# qr_align_reinit_pick_laggards <measure_json> [ok_frames] -> the "NDI camN" sources that are MORE
# than ok_frames behind the fastest source, ONE PER LINE (empty when the spread is <= ok_frames or
# nothing parseable). NEWLINE-separated because a source string ("NDI camN") itself contains a
# space, so a space-joined list is not word-split-safe. A missing / non-integer per-source field is
# SKIPPED (never a crash): the grep only matches `"NDI camN": <int>` pairs, so meta keys
# (spread_frames/rounds_used/error) and any garbage value are ignored by construction.
qr_align_reinit_pick_laggards() {
  local json="${1:-}" ok="${2:-1}"
  local -a names=() vals=()
  local src val maxv i
  while IFS='|' read -r src val; do
    [ -n "$src" ] || continue
    names+=("$src"); vals+=("$val")
  done < <(printf '%s' "$json" \
      | grep -oE '"NDI cam[0-9]+"[[:space:]]*:[[:space:]]*-?[0-9]+' \
      | sed -E 's/^"(NDI cam[0-9]+)"[[:space:]]*:[[:space:]]*(-?[0-9]+)$/\1|\2/' \
      || true)
  [ "${#vals[@]}" -gt 0 ] || return 0
  maxv="${vals[0]}"
  for val in "${vals[@]}"; do [ "$val" -gt "$maxv" ] && maxv="$val"; done
  for i in "${!names[@]}"; do
    if [ "$(( maxv - vals[i] ))" -gt "$ok" ]; then
      printf '%s\n' "${names[$i]}"
    fi
  done
}

# qr_align_reinit_spread_of <measure_json> -> the spread_frames value (or "?" when absent/null).
qr_align_reinit_spread_of() {
  local json="${1:-}" v
  v="$(printf '%s' "$json" \
      | grep -oE '"spread_frames"[[:space:]]*:[[:space:]]*-?[0-9]+' \
      | grep -oE '\-?[0-9]+$' | head -1 || true)"
  printf '%s\n' "${v:-?}"
}

# qr_align_reinit_burn_unit_name <camN> [run_id] -> the transient burn systemd unit the [2/8]/[2b/8]
# deploy created for that box: the source-role box (camera_source_box) uses camera-box-burn-<RUN_ID>,
# every secondary uses camera-box-burn-<camN>-<RUN_ID>. Best-effort source lookup (subshell so it
# never leaks camera-set.sh globals; a missing camera_source_box just yields the secondary form).
qr_align_reinit_burn_unit_name() {
  local cam="${1:-}" run_id="${2:-${RUN_ID:-}}" src=""
  if command -v camera_source_box >/dev/null 2>&1; then
    src="$( (camera_source_box) 2>/dev/null || true )"
  fi
  if [ -n "$src" ] && [ "$cam" = "$src" ]; then
    printf 'camera-box-burn-%s' "$run_id"
  else
    printf 'camera-box-burn-%s-%s' "$cam" "$run_id"
  fi
}

# qr_align_reinit_restart_burn_cmds <unit> -> the REMOTE bash text that re-execs the burn instance
# (restart the existing transient unit -> reopen /dev/videoN -> re-draw k). ;-terminated so it is
# splice-safe under the `$(...)` newline-strip. `start` fallback covers a unit that had already
# collected/exited. Never fails the remote chain.
qr_align_reinit_restart_burn_cmds() {
  local unit="${1:-}"
  printf 'systemctl restart %s 2>/dev/null || systemctl start %s 2>/dev/null || true;\n' \
    "$unit" "$unit"
}

# qr_align_reinit_default_measure <host> <sources_csv> -> the measure-only JSON on stdout. Production
# default when QR_ALIGN_REINIT_MEASURE_CMD is unset. Degrades to EMPTY stdout (return 3) when python3
# is missing so the loop treats it as "nothing to re-init" (report-only), never an abort.
qr_align_reinit_default_measure() {
  local host="${1:-}" sources="${2:-}" here
  command -v python3 >/dev/null 2>&1 || return 3
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # scripts/ dir
  python3 "$here/qr_align_pins.py" --measure-only \
    --host "$host" --password "${STRIH_PW:-}" --sources "$sources" 2>/dev/null || return 0
}

# qr_align_reinit_default_restart <camN> -> restart that box's burn instance over ssh. Production
# default when QR_ALIGN_REINIT_RESTART_CMD is unset. Best-effort: a missing IP / ssh flake is a
# logged NOTE, never fatal (the floor-aware plan below stays the arbiter).
qr_align_reinit_default_restart() {
  local cam="${1:-}" ip="" unit
  if command -v camera_resolve >/dev/null 2>&1; then
    ip="$( (camera_resolve "$cam" >/dev/null 2>&1 && printf '%s' "${CAMERA_IP:-}") || true )"
  fi
  if [ -z "$ip" ]; then
    echo "[qr-align-reinit] NOTE: no IP resolved for ${cam} — skipping its burn re-init" >&2
    return 0
  fi
  unit="$(qr_align_reinit_burn_unit_name "$cam")"
  sshpass -p "${CAM_PW:-}" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 root@"$ip" \
    "$(qr_align_reinit_restart_burn_cmds "$unit")" 2>/dev/null || true
}

# qr_align_reinit_loop <strih_host> <sources_csv> -> the bounded re-init control loop. Each round:
# measure (measure-only) -> pick laggards -> if none, log CONVERGED and return 0 -> else restart the
# burn instance of every laggard box, settle, log the round, repeat -> after REINIT_MAX_ROUNDS log
# GAVE-UP and return 0 (the existing [4i/8align] floor-aware plan HARD-FAIL is the final arbiter).
# ALWAYS returns 0.
qr_align_reinit_loop() {
  local strih_host="${1:-}" sources_csv="${2:-}"
  local ok="${REINIT_OK_FRAMES:-1}" maxr="${REINIT_MAX_ROUNDS:-3}"
  local settle="${QR_ALIGN_SETTLE_S:-15}"
  local measure_cmd="${QR_ALIGN_REINIT_MEASURE_CMD:-qr_align_reinit_default_measure}"
  local restart_cmd="${QR_ALIGN_REINIT_RESTART_CMD:-qr_align_reinit_default_restart}"
  local round=1 json spread laggards src cam relabel

  while [ "$round" -le "$maxr" ]; do
    json="$( "$measure_cmd" "$strih_host" "$sources_csv" 2>/dev/null || true )"
    spread="$(qr_align_reinit_spread_of "$json")"
    laggards="$(qr_align_reinit_pick_laggards "$json" "$ok")"
    if [ -z "$laggards" ]; then
      # No laggards can mean two DIFFERENT things: a genuine converged spread (a numeric
      # spread_frames), or an UNMEASURABLE round (spread "?" -- empty/failed measure, e.g. a missing
      # python3 or an undecodable painter). Both safely proceed to the floor-aware plan, but say so
      # honestly rather than claim "converged" on a round that measured nothing (review #1349 LOW).
      if [ "$spread" = "?" ]; then
        printf '[qr-align-reinit] measure unavailable round %s — report-only, proceeding to the floor-aware plan\n' "$round"
      else
        printf '[qr-align-reinit] converged round %s spread=%s\n' "$round" "$spread"
      fi
      return 0
    fi
    relabel=""
    # NEWLINE-separated laggards (a source contains a space) -> read line by line.
    while IFS= read -r src; do
      [ -n "$src" ] || continue
      cam="$(qr_align_reinit_cam_of_source "$src")"
      relabel="${relabel:+$relabel,}$cam"
      "$restart_cmd" "$cam" || true
    done <<< "$laggards"
    printf '[qr-align-reinit] round %s: spread=%s re-init=%s\n' "$round" "$spread" "$relabel"
    # Persist the additive report-only telemetry (issue 1349 item 4) — a tiny JSON the verdict merge
    # can pick up later (wiring it INTO the verdict JSON needs a src/probe Rust change, deferred).
    qr_align_reinit_write_telemetry "$round" "$spread" || true
    sleep "$settle" || true
    round=$(( round + 1 ))
  done
  printf '[qr-align-reinit] gave up after %s rounds spread=%s (the floor-aware plan decides)\n' \
    "$maxr" "$spread"
  qr_align_reinit_write_telemetry "$maxr" "$spread" || true
  return 0
}

# qr_align_reinit_write_telemetry <rounds> <spread_frames> -> write the additive report-only
# telemetry JSON to the run dir (OUTDIR), when present. Silent no-op when OUTDIR is unset (a
# standalone / test call) or the write fails. NEVER fatal.
qr_align_reinit_write_telemetry() {
  local rounds="${1:-0}" spread="${2:-?}" dir="${OUTDIR:-}" sp
  [ -n "$dir" ] && [ -d "$dir" ] || return 0
  case "$spread" in ''|*[!0-9-]*) sp="null" ;; *) sp="$spread" ;; esac
  printf '{"qr_align_reinit_rounds": %s, "qr_align_reinit_spread_frames": %s}\n' \
    "$rounds" "$sp" > "$dir/qr-align-reinit-${RUN_ID:-$$}.json" 2>/dev/null || true
}
