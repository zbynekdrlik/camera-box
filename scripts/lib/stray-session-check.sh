#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statements) — matches the
# sibling scripts/lib/*.sh convention (rig-test-dropin.sh, camera-box-restart-verify.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so imposing strict mode here would leak into whichever caller sources it. recording-e2e.sh
# (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/stray-session-check.sh — the READ-ONLY stray recording/streaming guard (issue 758),
# reordered + REPEATED to run immediately BEFORE EVERY rig-mutation step (issue 1271). Sourced by
# recording-e2e.sh, which calls it before the bkshading-relay pause, before the [0/8] parity/painter
# auto-align, before the [2/8] cam1 deploy, and before the [2b/8] ALL_CAMBOX deploy loop (the
# existing pre-[4/8] rig-busy re-check stays).
#
# WHY (issue 1271, TWO live incidents): the check used to sit inside `[0/8] OBS pre-run state`, AFTER
# the fleet MUTATIONS — cambox_parity_align_before_gate (issue 1202, restarts camera-box.service on
# the whole active cam fleet) and frame_probe_parity_align_before_gate (issue 1138, redeploys cam2's
# painter). Run 33571774966: a broadcast started AFTER the job-start rig-busy-gate.sh passed but
# BEFORE `[0/8]`, so the harness restarted every camera's binary while the stream box was
# broadcasting, then refused — too late. Run 33573594588: a broadcast started DURING the ~5 min
# `[1/8]` build (after an early `[0/8]` check passed), and `[2/8]`/`[2b/8]` then deployed to all 7
# cams while live. So ONE early check is not enough — the same read-only predicate must run
# immediately before EACH mutation, catching a broadcast that starts in any gap.
#
# It does NOT re-define "what is a REAL broadcast" — it CALLS the SAME shared
# `obs_phase2.py rig-busy-check` (streaming and/or recording on strih/stream) that the job-start
# gate scripts/rig-busy-gate.sh (#406/#312) uses, reads its `busy` boolean, and refuses on
# busy=true. It ADDS only the issue-1271-requested detail: for each STREAMING box, the ingest SERVER
# url + GetStreamStatus.outputDuration (obs_phase2.py stream-detail), NEVER the stream key.
#
# SEMANTICS (issue 1271 is an ORDERING/repetition fix, not a new gate): like the pre-1271 inline
# check (per-box status behind `2>/dev/null || true`) it fail-OPENS (WARN + proceed) ONLY when NO
# readable box is busy — the job-start rig-busy-gate.sh already fail-CLOSED on a fully-unreachable
# rig. But if rig-busy-check hits a partial outage (one box WS-unreachable, busy=None) while a box
# it COULD read is busy, it REFUSES — the pre-1271 loop refused if EITHER box was active, and never
# mutating during a live broadcast is the whole point.

# stray_session_check_assert HERE STRIH STREAM [WHAT]
#   HERE   = the scripts/ dir holding obs_phase2.py (recording-e2e.sh's $HERE).
#   STRIH  = strih OBS host/IP.
#   STREAM = stream OBS host/IP.
#   WHAT   = optional label of the mutation about to run (e.g. "[2/8] cam1 camera-box deploy"),
#            surfaced in the guard banner + the refusal so the log names WHICH mutation was blocked.
# Refuses (exit 1) BEFORE returning if a REAL broadcast is live; otherwise returns 0. MUST be called
# as a BARE statement (never $()/a pipe/an `if` condition) so its `exit 1` propagates to the harness
# — the same discipline the adjacent #860 optical preflight uses. Idempotent + read-only, safe to
# call repeatedly.
stray_session_check_assert() {
  local HERE="$1" STRIH="$2" STREAM="$3" WHAT="${4:-a rig mutation}"
  echo "[preflight] OBS stray-session guard (before ${WHAT}) — strih + stream must NOT be recording/streaming (#758/#1271; shared rig-busy read, mirrors rig-busy-gate.sh)"
  local out busy
  out="$(python3 "$HERE/obs_phase2.py" rig-busy-check --strih-host "$STRIH" --stream-host "$STREAM" --password "${OBS_PASSWORD:-}" 2>/dev/null || true)"
  busy="$(printf '%s' "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('busy'))" 2>/dev/null || true)"

  if [ "$busy" = "False" ]; then
    echo "    ok: strih + stream are idle (no recording/streaming) — proceeding to ${WHAT}"
    return 0
  fi
  if [ "$busy" != "True" ]; then
    # busy is None/empty -> rig-busy-check hit an all-or-nothing error (a box WS-unreachable) or
    # produced no/garbled output. issue 1271 🟡4: don't blindly fail-open — the pre-1271 per-box
    # loop refused if EITHER box was active even when the other's read failed. So if ANY box
    # rig-busy-check COULD read is busy (streaming/recording), REFUSE (fall through); only when NO
    # readable box is busy do we fail-OPEN (WARN + proceed), matching the pre-1271 empty-read.
    local _readable_busy
    _readable_busy="$(printf '%s' "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print('yes' if any(x.get('streaming') or x.get('recording') for x in d.get('diagnostics',[])) else '')" 2>/dev/null || true)"
    if [ "$_readable_busy" != "yes" ]; then
      echo "    WARNING: could not read rig-busy state (${out:-no output}); proceeding to ${WHAT} — the job-start rig-busy-gate.sh already gated a live broadcast at job start (#1271)" >&2
      return 0
    fi
    # a readable box IS busy despite the partial-outage exit 3 -> fall through and REFUSE.
  fi

  # busy=true (or busy=None with a readable busy box) -> a REAL broadcast (streaming and/or
  # recording) is live on strih and/or stream. REFUSE now, BEFORE the mutation.
  echo "ERROR: [preflight] FAIL: strih/stream OBS is ALREADY recording/streaming — refusing to run ${WHAT} while a broadcast may be LIVE (#758/#1271)." >&2
  echo "    rig-busy-check: $out" >&2
  local _hint _streaming _label _ip _detail
  _hint="$(printf '%s' "$out" | python3 -c "import json,sys; print(json.load(sys.stdin).get('hint',''))" 2>/dev/null || true)"
  [ -n "$_hint" ] && echo "    hint: $_hint" >&2
  # issue 1271: name WHAT is streaming for each streaming box — the ingest SERVER url (key-free) +
  # GetStreamStatus.outputDuration — so a LIVE production broadcast is obvious in the log.
  _streaming="$(printf '%s' "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(' '.join(x['host'] for x in d.get('diagnostics',[]) if x.get('streaming')))" 2>/dev/null || true)"
  for _label in $_streaming; do
    case "$_label" in
      strih) _ip="$STRIH" ;;
      stream) _ip="$STREAM" ;;
      *) continue ;;
    esac
    _detail="$(python3 "$HERE/obs_phase2.py" stream-detail --host "$_ip" --password "${OBS_PASSWORD:-}" 2>/dev/null || true)"
    [ -n "$_detail" ] && echo "    ${_label} streaming: $_detail" >&2
  done
  exit 1
}
