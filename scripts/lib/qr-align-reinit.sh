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

# qr_align_reinit_modal_value <v1> <v2> ... -> the MODAL lag value (the most frequent). On a TIE,
# prefer the HIGHER value (= closest to the fastest, the less-negative one). Pure, deterministic
# regardless of arg order (a numeric sort by count then value, tail = most-frequent + highest).
# Called only with >= 1 value.
qr_align_reinit_modal_value() {
  printf '%s\n' "$@" | sort -n | uniq -c | sort -k1,1n -k2,2n | tail -1 | awk '{print $2}'
}

# qr_align_reinit_pick_off_modal <measure_json> [ok_frames] -> the "NDI camN" sources whose lag is
# MORE than ok_frames away from the MODAL lag -- laggards AND leaders -- ONE PER LINE, so one round
# re-draws every off-modal box at once (issue 1349, run-4 finding: chasing the single slowest never
# converges with 7 boxes at k in {0,1,2}). Same robust grep/skip-garbage parse as pick_laggards.
qr_align_reinit_pick_off_modal() {
  local json="${1:-}" ok="${2:-1}"
  local -a names=() vals=()
  local src val modal i d
  while IFS='|' read -r src val; do
    [ -n "$src" ] || continue
    names+=("$src"); vals+=("$val")
  done < <(printf '%s' "$json" \
      | grep -oE '"NDI cam[0-9]+"[[:space:]]*:[[:space:]]*-?[0-9]+' \
      | sed -E 's/^"(NDI cam[0-9]+)"[[:space:]]*:[[:space:]]*(-?[0-9]+)$/\1|\2/' \
      || true)
  [ "${#vals[@]}" -gt 0 ] || return 0
  modal="$(qr_align_reinit_modal_value "${vals[@]}")"
  for i in "${!names[@]}"; do
    d=$(( vals[i] - modal )); [ "$d" -lt 0 ] && d=$(( -d ))
    if [ "$d" -gt "$ok" ]; then
      printf '%s\n' "${names[$i]}"
    fi
  done
}

# qr_align_reinit_fastest <measure_json> -> the "NDI camN" source with the MAX lag value (0 =
# fastest). Tie -> the lowest-numbered box (deterministic). Empty when nothing parseable.
qr_align_reinit_fastest() {
  local json="${1:-}"
  printf '%s' "$json" \
    | grep -oE '"NDI cam[0-9]+"[[:space:]]*:[[:space:]]*-?[0-9]+' \
    | sed -E 's/^"(NDI cam[0-9]+)"[[:space:]]*:[[:space:]]*(-?[0-9]+)$/\2|\1/' \
    | sort -t'|' -k1,1n -k2,2r | tail -1 | cut -d'|' -f2 || true
}

# qr_align_reinit_spread_not_improved <prev_spread> <new_spread> -> exit 0 (TRUE) iff BOTH are
# integers AND new_spread >= prev_spread (the spread did not get smaller). A non-integer ("?") on
# either side returns 1 (we cannot conclude it failed to improve -> do not switch to the fastest).
qr_align_reinit_spread_not_improved() {
  local prev="${1:-}" new="${2:-}"
  case "$prev" in ''|*[!0-9-]*) return 1 ;; esac
  case "$new" in ''|*[!0-9-]*) return 1 ;; esac
  [ "$new" -ge "$prev" ]
}

# qr_align_reinit_next_set <prev_set> <new_set> <prev_spread> <new_spread> <fastest> -> the set to
# ACTUALLY re-init this round (NEWLINE-separated sources). The second lottery lever (issue 1349,
# run-4 finding): when this round's off-modal set is IDENTICAL to the previous round's AND the spread
# did not improve, the off-modal lever is stuck -> re-init the FASTEST box instead (re-draw its k so
# the modal shifts). Otherwise re-init the new off-modal set. Sets compared order-independently.
qr_align_reinit_next_set() {
  local prev_set="${1:-}" new_set="${2:-}" prev_spread="${3:-?}" new_spread="${4:-?}" fastest="${5:-}"
  local prev_norm new_norm
  prev_norm="$(printf '%s\n' "$prev_set" | grep -v '^[[:space:]]*$' | sort || true)"
  new_norm="$(printf '%s\n' "$new_set" | grep -v '^[[:space:]]*$' | sort || true)"
  if [ -n "$new_norm" ] && [ "$prev_norm" = "$new_norm" ] \
     && qr_align_reinit_spread_not_improved "$prev_spread" "$new_spread"; then
    printf '%s\n' "$fastest"
    return 0
  fi
  printf '%s' "$new_set"
}

# qr_align_reinit_measure_with_retry <strih_host> <sources_csv> <round> -> the measure-only JSON on
# stdout. Issue 1349 (runs 3+5): the measure-only call sometimes FAILS with no numeric spread and
# its stderr SWALLOWED. On such a failure this LOGS the stderr tail (never swallowed), settles
# QR_ALIGN_REINIT_MEASURE_RETRY_S (default 5), and retries ONCE. Sets the global
# _qr_align_reinit_retries (0 or 1). ALWAYS returns 0.
qr_align_reinit_measure_with_retry() {
  local strih_host="${1:-}" sources_csv="${2:-}" round="${3:-?}"
  local measure_cmd="${QR_ALIGN_REINIT_MEASURE_CMD:-qr_align_reinit_default_measure}"
  local retry_s="${QR_ALIGN_REINIT_MEASURE_RETRY_S:-5}"
  local json errfile spread tail
  _qr_align_reinit_retries=0
  errfile="$(mktemp 2>/dev/null || echo "/tmp/qr-align-reinit-err.$$")"
  json="$( "$measure_cmd" "$strih_host" "$sources_csv" 2>"$errfile" || true )"
  spread="$(qr_align_reinit_spread_of "$json")"
  if [ "$spread" = "?" ]; then
    # stdout is this function's JSON return channel, so the diagnostic MUST go to STDERR (never
    # swallowed to /dev/null -- the runs 3+5 gap -- and never polluting the returned JSON).
    tail="$(printf '%s' "$(tail -n 3 "$errfile" 2>/dev/null || true)")"
    printf '[qr-align-reinit] measure failed round %s: %s\n' "$round" "${tail:-<no stderr>}" >&2
    sleep "$retry_s" || true
    _qr_align_reinit_retries=1
    json="$( "$measure_cmd" "$strih_host" "$sources_csv" 2>"$errfile" || true )"
  fi
  rm -f "$errfile" 2>/dev/null || true
  printf '%s' "$json"
}

# qr_align_reinit_detail_obj <round> <spread> <relabel_csv> <retries> -> one JSON object for the
# report-only rounds_detail array (issue 1349 item 3). spread "?"/non-numeric -> null; relabel_csv
# ("cam3,cam7") -> a JSON array of cam tokens.
qr_align_reinit_detail_obj() {
  local round="${1:-0}" spread="${2:-?}" relabel="${3:-}" retries="${4:-0}"
  local sp arr cam
  case "$spread" in ''|*[!0-9-]*) sp="null" ;; *) sp="$spread" ;; esac
  arr=""
  local IFS=','
  for cam in $relabel; do
    [ -n "$cam" ] || continue
    arr="${arr:+$arr,}\"$cam\""
  done
  printf '{"round": %s, "spread": %s, "reinit": [%s], "measure_retries": %s}' \
    "$round" "$sp" "$arr" "${retries:-0}"
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
# measure (measure-only, with a 1x retry on a swallowed failure) -> pick the OFF-MODAL set (every
# box more than ok_frames from the most-frequent lag, laggards AND leaders) -> if none, log CONVERGED
# / measure-unavailable and return 0 -> else pick the set to re-init (the off-modal set, OR the
# FASTEST box when the same set repeated with no improvement -- the second lottery lever), restart
# each box's burn instance, settle, log the round + telemetry, repeat -> after REINIT_MAX_ROUNDS log
# GAVE-UP and return 0 (the existing [4i/8align] floor-aware plan HARD-FAIL is the final arbiter).
# ALWAYS returns 0. (issue 1349, run-4 finding: one-slowest-box re-draws never converge; the modal
# round + fastest lever do.)
qr_align_reinit_loop() {
  local strih_host="${1:-}" sources_csv="${2:-}"
  local ok="${REINIT_OK_FRAMES:-1}" maxr="${REINIT_MAX_ROUNDS:-3}"
  local settle="${QR_ALIGN_SETTLE_S:-15}"
  local restart_cmd="${QR_ALIGN_REINIT_RESTART_CMD:-qr_align_reinit_default_restart}"
  local round=1 json spread off_modal chosen fastest src cam relabel retries
  local prev_off_modal="" prev_spread="?" details=""

  while [ "$round" -le "$maxr" ]; do
    json="$(qr_align_reinit_measure_with_retry "$strih_host" "$sources_csv" "$round")"
    retries="${_qr_align_reinit_retries:-0}"
    spread="$(qr_align_reinit_spread_of "$json")"
    off_modal="$(qr_align_reinit_pick_off_modal "$json" "$ok")"
    if [ -z "$off_modal" ]; then
      # No off-modal box can mean two DIFFERENT things: a genuine converged spread (a numeric
      # spread_frames), or an UNMEASURABLE round (spread "?" -- the measure failed twice). Both safely
      # proceed to the floor-aware plan, but say so honestly rather than claim "converged" on a round
      # that measured nothing.
      if [ "$spread" = "?" ]; then
        printf '[qr-align-reinit] measure unavailable round %s — report-only, proceeding to the floor-aware plan\n' "$round"
      else
        printf '[qr-align-reinit] converged round %s spread=%s\n' "$round" "$spread"
      fi
      details="${details:+$details,}$(qr_align_reinit_detail_obj "$round" "$spread" "" "$retries")"
      qr_align_reinit_write_telemetry "$round" "$spread" "$details" || true
      return 0
    fi
    fastest="$(qr_align_reinit_fastest "$json")"
    chosen="$(qr_align_reinit_next_set "$prev_off_modal" "$off_modal" "$prev_spread" "$spread" "$fastest")"
    relabel=""
    # NEWLINE-separated set (a source contains a space) -> read line by line.
    while IFS= read -r src; do
      [ -n "$src" ] || continue
      cam="$(qr_align_reinit_cam_of_source "$src")"
      relabel="${relabel:+$relabel,}$cam"
      "$restart_cmd" "$cam" || true
    done <<< "$chosen"
    printf '[qr-align-reinit] round %s: spread=%s re-init=%s\n' "$round" "$spread" "$relabel"
    # Persist the additive report-only telemetry (issue 1349 items 3+4) — a tiny JSON the verdict
    # merge can pick up later (wiring it INTO the verdict JSON needs a src/probe Rust change, deferred).
    details="${details:+$details,}$(qr_align_reinit_detail_obj "$round" "$spread" "$relabel" "$retries")"
    qr_align_reinit_write_telemetry "$round" "$spread" "$details" || true
    prev_off_modal="$off_modal"; prev_spread="$spread"
    sleep "$settle" || true
    round=$(( round + 1 ))
  done
  printf '[qr-align-reinit] gave up after %s rounds spread=%s (the floor-aware plan decides)\n' \
    "$maxr" "$spread"
  qr_align_reinit_write_telemetry "$maxr" "$spread" "$details" || true
  return 0
}

# qr_align_reinit_write_telemetry <rounds> <spread_frames> [rounds_detail_json] -> write the additive
# report-only telemetry JSON to the run dir (OUTDIR), when present. rounds_detail is the comma-joined
# object list built by qr_align_reinit_detail_obj (empty -> []). Silent no-op when OUTDIR is unset
# (a standalone / test call) or the write fails. NEVER fatal.
qr_align_reinit_write_telemetry() {
  local rounds="${1:-0}" spread="${2:-?}" details="${3:-}" dir="${OUTDIR:-}" sp
  [ -n "$dir" ] && [ -d "$dir" ] || return 0
  case "$spread" in ''|*[!0-9-]*) sp="null" ;; *) sp="$spread" ;; esac
  printf '{"qr_align_reinit_rounds": %s, "qr_align_reinit_spread_frames": %s, "rounds_detail": [%s]}\n' \
    "$rounds" "$sp" "$details" > "$dir/qr-align-reinit-${RUN_ID:-$$}.json" 2>/dev/null || true
}
