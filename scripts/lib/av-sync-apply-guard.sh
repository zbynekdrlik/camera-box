#!/usr/bin/env bash
# airuleset:script-ok sourced lib -- defines functions only, runs nothing; the caller
# (recording-e2e.sh) owns `set -euo pipefail`, so every function here is written to ALWAYS return 0
# and to guard every external-command pipeline (never a no-match/SIGPIPE/parse-failure that would
# set -e-abort the E2E cleanup trap around it -- the #1133 class).
#
# scripts/lib/av-sync-apply-guard.sh -- #1265 task 3: the I/O gather + orchestration for the #856
# rig-wide A/V apply STABILITY GUARD. The pure refusal decision lives in scripts/av_sync_apply_guard.py
# (pytest Tier-0); this lib reads the inputs off the rig/filesystem and calls it, following the #675
# sourced-helper pattern so recording-e2e.sh gains only function-CALL lines (no anchored line edited).
#
# Wiring (recording-e2e.sh):
#   [8/8g]  AV_SYNC_BAND_VERDICT="$(av_sync_stream_band_verdict "$STREAM" "$HERE/audio_lag_decision.py")"
#   cleanup (BEFORE the anchored #856 apply `if`):
#           _hold="$(av_sync_apply_guard_decide "$REPORT_JSON" "$AV_SYNC_BAND_VERDICT" \
#                    "$AV_SYNC_APPLY_OFFSET_MS" "$HERE/av_sync_apply_guard.py")"
#           [ -n "$_hold" ] && { loud log; av_sync_persist_hold_reason "$_hold";
#                                printf '%s\n' "$_hold" > "$OUTDIR/av-sync-apply-hold-<run>.txt";
#                                AV_SYNC_APPLY_OFFSET_MS=""; }
#   cleanup (AFTER the guard decide, BEFORE the apply block — EVERY run, held or applied — so the
#            NEXT run's SUSTAINED check has a prev, and pin_at_measure reads the pre-apply pin):
#           av_sync_persist_residual "$REPORT_JSON" "$RUN_ID"
#   cleanup (AFTER the apply block, gated on the OUTDIR success file existing):
#           av_sync_persist_applied_offset "$OUTDIR/av-sync-last-<run>.json"

# The dev1-persistent last-applied reference the jump-vs-last condition reads/writes (the
# default_last_json_path() fallback av_sync_calibrate.py uses on a box with no PROGRAMDATA env).
av_sync_default_last_applied_path() {
  printf '%s' "${AV_SYNC_LAST_APPLIED_JSON:-$HOME/.camera-box/av-sync-last.json}"
}

# av_sync_read_verdict_residual <verdict_json> <key> -> echoes all_cambox_av_sync.<key> (a number),
# or "" (missing file / no all_cambox_av_sync / absent key / non-JSON). Never aborts.
av_sync_read_verdict_residual() {
  local json="${1:-}" key="${2:-}" out=""
  [ -n "$json" ] && [ -f "$json" ] || { printf ''; return 0; }
  out="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    v = (d.get("all_cambox_av_sync") or {}).get(sys.argv[2])
    print("" if v is None else v)
except Exception:
    pass
' "$json" "$key" 2>/dev/null || true)"
  printf '%s' "$out"
  return 0
}

# av_sync_read_last_applied_offset [json_path] -> echoes the last-applied offset_ms, or "" when the
# file is absent (the common case until the first successful apply persists it) / unreadable.
av_sync_read_last_applied_offset() {
  local json="${1:-$(av_sync_default_last_applied_path)}" out=""
  [ -n "$json" ] && [ -f "$json" ] || { printf ''; return 0; }
  out="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    v = d.get("offset_ms")
    print("" if v is None else v)
except Exception:
    pass
' "$json" 2>/dev/null || true)"
  printf '%s' "$out"
  return 0
}

# The dev1-persistent PER-RUN residual reference (#1265b, the SUSTAINED two-run confirmation). SEPARATE
# from av-sync-last.json (the last-APPLIED offset): this records EVERY run's measured residual so the
# NEXT run can tell a one-run outlier from a confirmed real step.
av_sync_default_residual_last_path() {
  printf '%s' "${AV_SYNC_RESIDUAL_LAST_JSON:-$HOME/.camera-box/av-sync-residual-last.json}"
}

# av_sync_read_prev_residual [json_path] -> echoes "<residual_median_ms>\t<age_s>" from the previous
# run's persisted residual file, or "" when absent/unreadable. age_s = wall-now minus the file's `ts`
# (whole seconds, floored at 0); the decide passes both to the pure guard's SUSTAINED check. Never aborts.
av_sync_read_prev_residual() {
  local json="${1:-$(av_sync_default_residual_last_path)}" out=""
  [ -n "$json" ] && [ -f "$json" ] || { printf ''; return 0; }
  out="$(python3 -c '
import json, sys, time
try:
    d = json.load(open(sys.argv[1]))
    r = d.get("residual_median_ms")
    ts = d.get("ts")
    if r is None:
        sys.exit(0)
    age = ""
    if isinstance(ts, (int, float)):
        age = str(max(0, int(time.time() - ts)))
    print("%s\t%s" % (r, age))
except Exception:
    pass
' "$json" 2>/dev/null || true)"
  printf '%s' "$out"
  return 0
}

# av_sync_stream_band_verdict <stream_ip> <decide_py> [bundle_port] [curl_timeout] -> echoes the
# stream reference-source (mbc) ts_lag BAND verdict (DRIFTING/HEALTHY/UNKNOWN/SKIP), or "" on any
# failure. A curl failure -> box_reachable=0 -> SKIP (dormant; the guard treats SKIP/UNKNOWN/"" as
# no-hold), so a pre-deploy box or an unreachable :8899 never false-HOLDs the apply. Best-effort.
av_sync_stream_band_verdict() {
  local ip="${1:-}" decide="${2:-}" port="${3:-8899}" timeout="${4:-10}"
  local body="" reachable=0 out="" verdict=""
  [ -n "$ip" ] && [ -n "$decide" ] && [ -f "$decide" ] || { printf ''; return 0; }
  body="$(curl -fsS --max-time "$timeout" "http://${ip}:${port}/bundle-state.json" 2>/dev/null || true)"
  case "$body" in
    \{*) reachable=1 ;;
    *) reachable=0; body="" ;;
  esac
  # Forward the SAME AUDIO_BAND_* env thresholds the dev1 watchdog uses (issue 1265 finding 9), so
  # the E2E guard and the watchdog band arm stay tuned in lock-step. Unset -> the python defaults.
  local band_args=(band --box-reachable "$reachable")
  [ -n "${AUDIO_BAND_DEV_THRESHOLD_MS:-}" ] && band_args+=(--dev-threshold-ms "$AUDIO_BAND_DEV_THRESHOLD_MS")
  [ -n "${AUDIO_BAND_DUTY_MIN_PCT:-}" ] && band_args+=(--duty-min-pct "$AUDIO_BAND_DUTY_MIN_PCT")
  [ -n "${AUDIO_BAND_MIN_SAMPLES:-}" ] && band_args+=(--min-samples "$AUDIO_BAND_MIN_SAMPLES")
  out="$(printf '%s' "$body" | python3 "$decide" "${band_args[@]}" 2>/dev/null || true)"
  verdict="$(printf '%s\n' "$out" | sed -n 's/^band_verdict=//p' | tail -1 || true)"
  printf '%s' "$verdict"
  return 0
}

# av_sync_apply_guard_decide <verdict_json> <band_verdict> <proposed_offset_ms> <guard_py> [last_json] [residual_last_json]
#   -> echoes a non-empty HOLD reason (the #856 apply should be HELD) or "" (proceed). Gathers the
#   run residual median/spread from the verdict JSON, the last-applied offset, AND the PREVIOUS run's
#   persisted residual + age (the #1265b SUSTAINED two-run confirmation), then calls the pure guard.
#   MUST be called BEFORE av_sync_persist_residual overwrites the prev file with THIS run's residual.
#   Always returns 0 (used in `$(...)` under the caller's set -e).
av_sync_apply_guard_decide() {
  local verdict_json="${1:-}" band_verdict="${2:-}" proposed="${3:-}" guard="${4:-}"
  local last_json="${5:-$(av_sync_default_last_applied_path)}"
  local residual_last_json="${6:-$(av_sync_default_residual_last_path)}"
  [ -n "$guard" ] && [ -f "$guard" ] || { printf ''; return 0; }
  local resid spread last prev_pair prev_resid prev_age out reason
  resid="$(av_sync_read_verdict_residual "$verdict_json" residual_median_ms)"
  spread="$(av_sync_read_verdict_residual "$verdict_json" residual_spread_ms)"
  last="$(av_sync_read_last_applied_offset "$last_json")"
  prev_pair="$(av_sync_read_prev_residual "$residual_last_json")"
  prev_resid="$(printf '%s' "$prev_pair" | cut -f1)"
  prev_age="$(printf '%s' "$prev_pair" | cut -f2)"
  out="$(python3 "$guard" decide \
    --residual-median-ms "$resid" --residual-spread-ms "$spread" \
    --band-verdict "$band_verdict" --last-applied-offset-ms "$last" \
    --proposed-offset-ms "$proposed" \
    --prev-residual-ms "$prev_resid" --prev-residual-age-s "$prev_age" 2>/dev/null || true)"
  reason="$(printf '%s\n' "$out" | sed -n 's/^hold_reason=//p' | tail -1 || true)"
  printf '%s' "$reason"
  return 0
}

# av_sync_persist_residual <verdict_json> <run_id> [last_applied_json] [dest] -> record THIS run's
# measured residual (HELD or APPLIED, EVERY run) to the dev1 residual-last reference, so the NEXT
# run's SUSTAINED check has a prev to compare against (#1265b). Writes {run_id, ts,
# residual_median_ms, residual_spread_ms, pin_at_measure} atomically. pin_at_measure = the
# applied_latency_ms from av-sync-last.json AT THIS POINT (the pin the run measured against) --
# so this MUST run BEFORE av_sync_persist_applied_offset updates that file with this run's new pin.
# No-op when the verdict has no residual_median_ms; best-effort, never aborts.
av_sync_persist_residual() {
  local verdict_json="${1:-}" run_id="${2:-}"
  local last_json="${3:-$(av_sync_default_last_applied_path)}"
  local dest="${4:-$(av_sync_default_residual_last_path)}"
  local resid spread pin
  resid="$(av_sync_read_verdict_residual "$verdict_json" residual_median_ms)"
  [ -n "$resid" ] || return 0
  spread="$(av_sync_read_verdict_residual "$verdict_json" residual_spread_ms)"
  pin="$(python3 -c '
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    v = d.get("applied_latency_ms")
    print("" if v is None else v)
except Exception:
    pass
' "$last_json" 2>/dev/null || true)"
  python3 -c '
import json, os, sys, time
try:
    dest, run_id, resid, spread, pin = sys.argv[1:6]
    payload = {"run_id": run_id, "ts": time.time(), "residual_median_ms": float(resid)}
    if spread != "":
        payload["residual_spread_ms"] = float(spread)
    if pin != "":
        payload["pin_at_measure"] = float(pin)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    tmp = dest + ".tmp"
    with open(tmp, "w") as f:
        json.dump(payload, f)
    os.replace(tmp, dest)
except Exception:
    pass
' "$dest" "$run_id" "$resid" "$spread" "$pin" 2>/dev/null || true
  return 0
}

# av_sync_persist_applied_offset <src_json> [dest_json] -> COPY the calibrate-written success file
# (av_sync_calibrate.py --json-path "$OUTDIR/av-sync-last-<run>.json", written ONLY on a landed
# apply) to the dev1-persistent last-applied reference, so the NEXT run's jump-vs-last condition has
# a baseline. Copies the file WHOLE (issue 1265 finding 1) -- NOT a re-written divergent schema: the
# canonical ~/.camera-box/av-sync-last.json is a live data contract read by latency_pins_snapshot.py
# / rig-mode.sh / drift-guard for its `applied_latency_ms` (+ source/offset_ms/ts) keys, which a
# {source,offset_ms,ts}-only rewrite would strip. Atomic (.tmp + mv). No-op on a missing/empty src
# (a HELD/skipped or FAILED apply never wrote the OUTDIR file) or any copy failure.
av_sync_persist_applied_offset() {
  local src="${1:-}" dest="${2:-$(av_sync_default_last_applied_path)}"
  [ -n "$src" ] && [ -s "$src" ] || return 0
  python3 -c '
import json, os, shutil, sys
try:
    src, dest = sys.argv[1], sys.argv[2]
    with open(src) as f:            # validate it is a JSON object carrying offset_ms before copying
        d = json.load(f)
    if not isinstance(d, dict) or "offset_ms" not in d:
        sys.exit(0)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    tmp = dest + ".tmp"
    shutil.copyfile(src, tmp)       # copy the FULL schema verbatim (applied_latency_ms preserved)
    os.replace(tmp, dest)
except Exception:
    pass
' "$src" "$dest" 2>/dev/null || true
  return 0
}

# av_sync_apply_loop_gain <combined_offset_ms> <gain_py> -> #1265: DAMP the combined offset by the
# fixed loop gain (default 0.4, env AV_SYNC_LOOP_GAIN) so the #856 controller converges instead of
# oscillating against the measured plant gain ~2.31. Echoes "<damped>\t<gain>" on stdout (the caller
# splits with cut -f1/-f2); the pure gain math + validation live in av_sync_loop_gain.py (pytest
# Tier-0). The python's own stderr (the damp/validation line) flows to the run log. Always returns 0
# (runs at [8/8g] on the path into the cleanup EXIT trap); a missing helper / unparseable value ->
# empty damped so the caller's `[ -n ... ]` skips the apply (fail-safe: no correction over a wrong one).
av_sync_apply_loop_gain() {
  local combined="${1:-}" gain_py="${2:-}" out=""
  [ -n "$gain_py" ] && [ -f "$gain_py" ] || { printf ''; return 0; }
  out="$(python3 "$gain_py" damp --combined-ms "$combined" || true)"
  printf '%s' "$out"
  return 0
}

# The dev1-persistent append-only controller history log (#1265, the Prístup 2 adaptive-slope
# corpus). Env-overridable for tests, defaulting beside the other ~/.camera-box state files.
av_sync_default_history_path() {
  printf '%s' "${AV_SYNC_HISTORY_JSONL:-$HOME/.camera-box/av-sync-history.jsonl}"
}

# av_sync_append_history <run_id> <proposed_offset_ms> <hold_reason> <loop_gain> <combined_raw> <history_py>
#   [residual_last_json] [last_applied_json] [dest] -> #1265: append THIS run's line to the append-only
#   controller history. The pure record build + append (append-only, dir/file-tolerant, run-id-mismatch
#   no-op) live in av_sync_history.py (pytest Tier-0); this passes the two state-file paths (so env
#   overrides flow through) + the run context. MUST run AFTER av_sync_persist_residual (which wrote
#   THIS run's residual-last) AND after av_sync_persist_applied_offset (so applied_pin reads the pin
#   that landed). Always returns 0 (bare statement in the cleanup EXIT trap).
av_sync_append_history() {
  local run_id="${1:-}" proposed="${2:-}" hold_reason="${3:-}" loop_gain="${4:-}"
  local combined_raw="${5:-}" history_py="${6:-}"
  local residual_last="${7:-$(av_sync_default_residual_last_path)}"
  local last_applied="${8:-$(av_sync_default_last_applied_path)}"
  local dest="${9:-$(av_sync_default_history_path)}"
  [ -n "$history_py" ] && [ -f "$history_py" ] || return 0
  python3 "$history_py" append \
    --run-id "$run_id" --proposed-offset-ms "$proposed" --hold-reason "$hold_reason" \
    --loop-gain "$loop_gain" --combined-offset-ms-raw "$combined_raw" \
    --residual-last "$residual_last" --last-applied "$last_applied" --dest "$dest" \
    2>/dev/null || true
  return 0
}

# av_sync_persist_hold_reason <reason> [path] -> write the LATEST #856 apply-HOLD reason to a durable
# dev1 file (issue 1265 finding 6a), so a genuine sustained large-residual HOLD is operator-visible
# beyond the per-run $OUTDIR file (swept) and the CI stderr echo. The E2E Discord report is composed
# BEFORE cleanup() runs, so it cannot carry this run's hold; this durable file is the next-run/
# operator surface. Best-effort; a no-op on an empty reason.
av_sync_persist_hold_reason() {
  local reason="${1:-}" path="${2:-$HOME/.camera-box/av-sync-apply-hold-last.txt}"
  [ -n "$reason" ] || return 0
  mkdir -p "$(dirname "$path")" 2>/dev/null || true
  printf '%s\t%s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || printf '?')" "$reason" \
    > "$path" 2>/dev/null || true
  return 0
}
