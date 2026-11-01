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
# had no JPEG XS item. Every JPEG XS source and output in a broadcast OBS logs a 5 s window line
# (`HH:MM:SS.mmm: [distroav] jxs-source '<name>': fps=… frames=…`, `jxs-output '<name>': …`); the
# newest one per link says whether frames flow. event_assert.py grades them; this lib only reads them.
#
# BOUNDED (owner 1.11.2026: a measurement that read the whole 100+ MB OBS log on a broadcast box
# dropped frames on the JPEG XS program link): only the last 1 MiB of the newest log (`tail -c`, a
# seek from the end, ~15 min of log) at nice 19, and only its last 40 window lines + the newest
# timestamp leave the box.
#
# Public API:
#   event_jxs_remote_cmd -> the remote bash text: `W:<window line>` lines, then `HEAD:<newest HH:MM:SS.mmm>`;
#       `NOLOG` when the box has no OBS log (the Linux OBS boxes: strih-snv, stream-snv)
#   event_jxs_windows_json USER PW NAME HOST [USER PW NAME HOST ...] -> one JSON object
#       {NAME: {"head": "<HH:MM:SS.mmm>|", "lines": [<window line>, ...]} | null}; null = the box did
#       not answer (ssh failure, timeout). event_assert.py fails null / no line / no head closed.
#
# TRANSPORT: `sshpass -p PW timeout T ssh ...`, sshpass outermost (a test's PATH sshpass stub stays
# in front, the ci-testing-gotchas stub-bypass rule); T = EVENT_JXS_SSH_TIMEOUT (default 20 s).

EVENT_JXS_SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10)

event_jxs_remote_cmd() {
  # shellcheck disable=SC2016  # expands on the box
  printf '%s\n' 'F="$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -n 1)"; if [ -z "$F" ]; then echo NOLOG; else nice -n 19 tail -c 1048576 "$F" | grep -a -E "^[0-9:.]+: \[distroav\] jxs-(source|output) " | tail -n 40 | sed "s/^/W:/"; H="$(nice -n 19 tail -c 65536 "$F" | grep -a -o -E "^[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]+" | tail -n 1)"; printf "HEAD:%s\n" "$H"; fi'
}

event_jxs_windows_json() {
  local out cmd user pw name host
  local -a args=()
  cmd="$(event_jxs_remote_cmd)"
  while [ "$#" -ge 4 ]; do
    user="$1" pw="$2" name="$3" host="$4"
    shift 4
    out="$(sshpass -p "$pw" timeout "${EVENT_JXS_SSH_TIMEOUT:-20}" ssh -n "${EVENT_JXS_SSH_OPTS[@]}" \
      "${user}@${host}" "$cmd" 2>/dev/null)" || out=""
    case "$out" in
      *HEAD:* | NOLOG*) args+=("$name" "$out") ;;
      *) args+=("$name" "") ;;
    esac
  done
  python3 -c '
import json, sys
a = sys.argv[1:]
res = {}
for i in range(0, len(a), 2):
    name, out = a[i], a[i + 1]
    if not out:
        res[name] = None
        continue
    lines = out.splitlines()
    res[name] = {
        "head": next((l[5:] for l in reversed(lines) if l.startswith("HEAD:")), ""),
        "lines": [l[2:] for l in lines if l.startswith("W:")],
    }
print(json.dumps(res))
' "${args[@]}"
}
