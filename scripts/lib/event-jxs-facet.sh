#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# sourcing runs in the CALLER's shell (rig-mode.sh, `set -euo pipefail`), so strict mode here would
# leak into it. Every public function returns 0 and is safe as a bare statement under errexit.
#
# scripts/lib/event-jxs-facet.sh -- issue 1387 (1.11.2026): the JPEG XS facet of the EVENT-mode
# contract (#722, scripts/event_assert.py item `jxs_live`).
#
# WHY: on the SNV event morning the cameras were moved and rebooted after the rig was set up; their
# JPEG XS senders stayed off and every strih camera scene was black on air for ~8 min. The contract
# had no JPEG XS item. Each broadcast OBS box's in-OBS JPEG XS indicator (GenlockJxsReport) logs ONE
# `jxs-state: state=<S> reason=<key> inputs=N down=N degraded=N outputs=N down=N degraded=N
# entry='<row>'|- (issue 1387)` line on every change, so the NEWEST such line of the newest OBS log is
# the box's current JPEG XS state. event_assert.py parses and grades it; this lib only reads it.
#
# Public API:
#   event_jxs_line_remote_cmd -> the remote bash text: `LINE:<newest jxs-state line or empty>`, or
#       `NOLOG` when the box has no OBS log at all (the Linux OBS boxes: strih-snv, stream-snv)
#   event_jxs_state_lines_json USER PW NAME HOST [USER PW NAME HOST ...] -> one JSON object
#       {NAME: "<line>" | "" | null}: the line, "" for no line / no log, null for a box that did
#       not answer (ssh failure, timeout). event_assert.py fails "" and null closed.
#
# TRANSPORT: `sshpass -p PW timeout T ssh ...`, sshpass outermost (a test's PATH sshpass stub stays
# in front, the ci-testing-gotchas stub-bypass rule); T = EVENT_JXS_SSH_TIMEOUT (default 20 s).

EVENT_JXS_SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10)

event_jxs_line_remote_cmd() {
  # shellcheck disable=SC2016  # expands on the box
  printf '%s\n' 'F="$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -n 1)"; if [ -z "$F" ]; then echo NOLOG; else L="$(grep -a "jxs-state: state=" "$F" 2>/dev/null | tail -n 1)"; printf "LINE:%s\n" "$L"; fi'
}

event_jxs_state_lines_json() {
  local out cmd user pw name host
  local -a pairs=()
  cmd="$(event_jxs_line_remote_cmd)"
  while [ "$#" -ge 4 ]; do
    user="$1" pw="$2" name="$3" host="$4"
    shift 4
    out="$(sshpass -p "$pw" timeout "${EVENT_JXS_SSH_TIMEOUT:-20}" ssh -n "${EVENT_JXS_SSH_OPTS[@]}" \
      "${user}@${host}" "$cmd" 2>/dev/null)" || out=""
    case "$out" in
      LINE:*) out="${out#LINE:}"; pairs+=("$name" "line" "${out%%$'\n'*}") ;;
      NOLOG*) pairs+=("$name" "line" "") ;;
      *) pairs+=("$name" "null" "") ;;
    esac
  done
  python3 -c '
import json, sys
a = sys.argv[1:]
print(json.dumps({a[i]: (a[i + 2] if a[i + 1] == "line" else None) for i in range(0, len(a), 3)}))
' "${pairs[@]}"
}
