#!/usr/bin/env bash
# strih-obs-stop.sh -- graceful OBS stop for the strih-lx box (issue 1317); extended header below.
set -euo pipefail
#
# Sibling of imag-obs-stop.sh (issue 785/882) for the strih-lx notebook. Two modes:
#
#   * PLAIN invocation (deploy / recovery / an operator menu): if strih-obs.service is active, route
#     the stop through `systemctl --user stop` -- systemd only suppresses Restart=on-failure for a
#     stop IT initiated, so a raw pkill of the supervised obs looks like a crash and gets
#     auto-relaunched (the exact issue-788 operator-fighting bug). Use this for EVERY deliberate stop.
#   * `--exec-stop`: the mode the unit's own ExecStop= line passes (systemd is ALREADY stopping it
#     there), so it SKIPS the systemctl delegation and runs the SIGTERM -> grace -> SIGKILL ladder
#     directly, avoiding a recursion back into `systemctl stop` mid-stop.
#
# There is no program-scene save/restore here (unlike imag-obs-stop.sh): the strih scene seeder is a
# separate follow-up, so there is nothing to restore on the next start yet. Exit 0 in every case.
#
# BASH_SOURCE-guarded (like setup-strih.sh) so the unit tests can source it without running the flow.

# --- source-guard: when sourced (the unit tests), define nothing live -- never run the stop flow ---
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0 2>/dev/null || true
fi

EXEC_STOP_MODE=0
if [ "${1:-}" = "--exec-stop" ]; then
  EXEC_STOP_MODE=1
fi

if [ "$EXEC_STOP_MODE" -eq 0 ] && systemctl --user is-active --quiet strih-obs.service 2>/dev/null; then
  echo "strih-obs.service je aktivny -- zastavujem cez systemd (systemctl --user stop), aby to Restart=on-failure nebral ako pad"
  systemctl --user stop strih-obs.service
  exit 0
fi

if pgrep -x obs >/dev/null; then
  pkill -TERM -x obs || true
  for _ in $(seq 1 15); do
    pgrep -x obs >/dev/null || { echo "obs stopped (SIGTERM)"; exit 0; }
    sleep 1
  done
  echo "WARN: obs ignored SIGTERM for 15s -- force-killing"
  pkill -KILL -x obs || true
else
  echo "obs not running"
fi
exit 0
