#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statements) — matches the
# sibling scripts/lib/*.sh convention (rig-test-dropin.sh, camera-box-restart-verify.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so imposing strict mode here would leak into whichever caller sources it. recording-e2e.sh
# (the only caller today) already sets -euo pipefail itself.
#
# scripts/lib/stray-session-check.sh — the `[0/8]` READ-ONLY stray recording/streaming preflight
# (issue 758), REORDERED to run BEFORE any rig mutation (issue 1271). Sourced by recording-e2e.sh.
#
# WHY (issue 1271): the check used to sit inside the `[0/8] OBS pre-run state` block, AFTER the two
# fleet MUTATIONS — cambox_parity_align_before_gate (issue 1202, restarts camera-box.service on the
# whole active cam fleet) and frame_probe_parity_align_before_gate (issue 1138, redeploys cam2's
# painter). On run 33571774966 a production broadcast started AFTER the job-start rig-busy-gate.sh
# passed but BEFORE `[0/8]`, so the harness restarted every camera's binary while the stream box was
# broadcasting, then refused — too late. Moving this read-only check to the FIRST step after the
# reachability preflight closes that minutes-wide window.
#
# It does NOT re-define "what is a REAL broadcast" — it CALLS the SAME shared
# `obs_phase2.py rig-busy-check` (streaming and/or recording on strih/stream) that the job-start
# gate scripts/rig-busy-gate.sh (#406/#312) uses, reads its `busy` boolean, and refuses on
# busy=true. It ADDS only the issue-1271-requested detail: for each STREAMING box, the ingest SERVER
# url + GetStreamStatus.outputDuration (obs_phase2.py stream-detail), NEVER the stream key.
#
# SEMANTICS UNCHANGED (issue 1271 is an ORDERING fix, not a new gate): like the pre-1271 inline
# check (which read per-box status behind `2>/dev/null || true`), this fail-OPENS on an
# unreadable/unreachable read (proceeds with a WARN) — the job-start rig-busy-gate.sh already
# fail-CLOSED on an unreachable rig; this second in-script check exists only to catch a broadcast
# that STARTED after that gate, so a momentary WS blip here must never newly abort a healthy run.

# stray_session_check_assert HERE STRIH STREAM
#   HERE   = the scripts/ dir holding obs_phase2.py (recording-e2e.sh's $HERE).
#   STRIH  = strih OBS host/IP.
#   STREAM = stream OBS host/IP.
# Refuses (exit 1) BEFORE returning if the shared rig-busy-check reports busy=true; otherwise
# returns 0. MUST be called as a BARE statement (never $()/a pipe/an `if` condition) so its `exit 1`
# propagates to the harness — the same discipline the adjacent #860 optical preflight uses.
stray_session_check_assert() {
  local HERE="$1" STRIH="$2" STREAM="$3"
  echo "[0/8] OBS stray-session check — strih + stream must NOT be recording/streaming before any rig mutation (#758/#1271; shared rig-busy-check, mirrors rig-busy-gate.sh)"
  local out busy
  out="$(python3 "$HERE/obs_phase2.py" rig-busy-check --strih-host "$STRIH" --stream-host "$STREAM" 2>/dev/null || true)"
  busy="$(printf '%s' "$out" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('busy'))" 2>/dev/null || true)"

  if [ "$busy" = "False" ]; then
    echo "    ok: strih + stream are idle (no recording/streaming)"
    return 0
  fi
  if [ "$busy" != "True" ]; then
    # busy is None/empty -> the rig state could not be read (WS blip / rig-busy-check error). Match
    # the pre-1271 inline check's fail-OPEN semantics: proceed, but WARN loudly so it is visible.
    echo "    WARNING: could not read rig-busy state (${out:-no output}); proceeding — the job-start rig-busy-gate.sh already gated a live broadcast at job start (#1271)" >&2
    return 0
  fi

  # busy=true -> a REAL broadcast (streaming and/or recording) is live on strih and/or stream.
  # REFUSE now, BEFORE any fleet mutation.
  echo "ERROR: [preflight] FAIL: strih/stream OBS is ALREADY recording/streaming — refusing to mutate the rig (fleet camera-box restart / cam2 painter redeploy) while a broadcast may be LIVE (#758/#1271)." >&2
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
    _detail="$(python3 "$HERE/obs_phase2.py" stream-detail --host "$_ip" 2>/dev/null || true)"
    [ -n "$_detail" ] && echo "    ${_label} streaming: $_detail" >&2
  done
  exit 1
}
