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
#           [ -n "$_hold" ] && { loud log + persist reason + AV_SYNC_APPLY_OFFSET_MS=""; }
#   cleanup (AFTER the apply block): av_sync_persist_applied_offset "$AV_SYNC_APPLY_OFFSET_MS"

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
  out="$(printf '%s' "$body" | python3 "$decide" band --box-reachable "$reachable" 2>/dev/null || true)"
  verdict="$(printf '%s\n' "$out" | sed -n 's/^band_verdict=//p' | tail -1 || true)"
  printf '%s' "$verdict"
  return 0
}

# av_sync_apply_guard_decide <verdict_json> <band_verdict> <proposed_offset_ms> <guard_py> [last_json]
#   -> echoes a non-empty HOLD reason (the #856 apply should be HELD) or "" (proceed). Gathers the
#   run residual median/spread from the verdict JSON and the last-applied offset from the reference
#   file, then calls the pure guard. Always returns 0 (used in `$(...)` under the caller's set -e).
av_sync_apply_guard_decide() {
  local verdict_json="${1:-}" band_verdict="${2:-}" proposed="${3:-}" guard="${4:-}"
  local last_json="${5:-$(av_sync_default_last_applied_path)}"
  [ -n "$guard" ] && [ -f "$guard" ] || { printf ''; return 0; }
  local resid spread last out reason
  resid="$(av_sync_read_verdict_residual "$verdict_json" residual_median_ms)"
  spread="$(av_sync_read_verdict_residual "$verdict_json" residual_spread_ms)"
  last="$(av_sync_read_last_applied_offset "$last_json")"
  out="$(python3 "$guard" decide \
    --residual-median-ms "$resid" --residual-spread-ms "$spread" \
    --band-verdict "$band_verdict" --last-applied-offset-ms "$last" \
    --proposed-offset-ms "$proposed" 2>/dev/null || true)"
  reason="$(printf '%s\n' "$out" | sed -n 's/^hold_reason=//p' | tail -1 || true)"
  printf '%s' "$reason"
  return 0
}

# av_sync_persist_applied_offset <offset_ms> [json_path] -> write the dev1-persistent last-applied
# reference (so the NEXT run's jump-vs-last condition has a baseline). Best-effort, atomic-ish
# (write .tmp + mv). A no-op on an empty offset (nothing was applied) or any write failure.
av_sync_persist_applied_offset() {
  local offset="${1:-}" json="${2:-$(av_sync_default_last_applied_path)}"
  [ -n "$offset" ] || return 0
  python3 -c '
import json, os, sys, time
try:
    offset = float(sys.argv[2])
    path = sys.argv[1]
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump({"source": "NDI 2ME PGM", "offset_ms": offset, "ts": time.time(),
                   "written_by": "recording-e2e #856 apply-guard"}, f)
    os.replace(tmp, path)
except Exception:
    pass
' "$json" "$offset" 2>/dev/null || true
  return 0
}
