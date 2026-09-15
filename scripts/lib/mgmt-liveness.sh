#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by setup-device.sh + the Rust
# harness (tests/harness_mgmt_liveness_1309.rs) — mirrors every sibling in scripts/lib/ (log-diet.sh,
# timesync-authority.sh, watchdog-tcp-probe.sh), none of which set -euo pipefail either: sourcing a
# `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/mgmt-liveness.sh — #1309 ON-BOX management-liveness self-heal.
#
# WHY: on 2026-09-13 a cambox went half-dead after a bkshading-relay (re)start (cam1 during a LIVE
# production, cam2 after an E2E cleanup): dantesync :8898 + the relay :8771 kept answering (already-
# running processes) while ANYTHING that needs a fork — an sshd session, the remoteos MCP, a gphoto2
# spawn — failed. The box still pinged but was UNREACHABLE and UNREPAIRABLE remotely; only the owner's
# physical power-cycle recovered it (destroying the runtime-only journal evidence in the process). The
# headless cam boxes have no operator and no other recovery path, so they need to HEAL THEMSELVES: a
# tiny systemd timer (every 2 min) probes sshd LOCALLY the way a client does — a TCP connect + a
# bounded read of the `SSH-` banner off 127.0.0.1:22 — and, once the banner is dead N times in a row
# (a genuine kex reset / PAM-logind stall, not one blipped connect), dumps a forensic snapshot into
# the (now persistent, #1309) journal and restarts ssh + remoteos-mcp, backing off after a few
# restarts/hour so a truly-broken box is not restart-looped. This is the ON-BOX twin of the dev1
# MGMT_DEAD alert (dantesync_clock_decision.py / dantesync-clock-alert-watchdog.sh, same ticket).
#
# The DECISION (MGMT_OK / MGMT_DEAD / RESTART_ALLOWED / BACKOFF) + the banner classifier are PURE
# bash here (exhaustively unit-testable at Tier-0, #557 kills local cargo). The generated on-box
# script (mgmt_liveness_selfcheck_script) EMBEDS these exact functions via `declare -f`, so there is
# ONE source of truth — no drifting inlined copy, and no repo-lib dependency on the appliance (the
# same self-contained-generated-script pattern as camera_box_free_capture_device_script_content).
#
# Source-only: defines pure functions + constants, no side effects on its own.

# The banner-liveness self-heal knobs (single source of truth, shared with the generated on-box
# script + the verify-device.sh (aj) check + the tests).
MGMT_LIVENESS_FAIL_THRESHOLD="${MGMT_LIVENESS_FAIL_THRESHOLD:-3}"       # consecutive dead-banner probes before acting
MGMT_LIVENESS_MAX_RESTARTS_PER_HOUR="${MGMT_LIVENESS_MAX_RESTARTS_PER_HOUR:-3}"  # restart cap per window, then log-only
MGMT_LIVENESS_RESTART_WINDOW_S="${MGMT_LIVENESS_RESTART_WINDOW_S:-3600}"
MGMT_LIVENESS_SSH_PORT="${MGMT_LIVENESS_SSH_PORT:-22}"
MGMT_LIVENESS_BANNER_TIMEOUT_S="${MGMT_LIVENESS_BANNER_TIMEOUT_S:-5}"
MGMT_LIVENESS_TIMER_INTERVAL="${MGMT_LIVENESS_TIMER_INTERVAL:-2min}"
MGMT_LIVENESS_STATE_FILE="/run/camera-box/mgmt-selfcheck.state"
# restarted (best-effort, in order) on RESTART_ALLOWED. `ssh.socket` FIRST: Ubuntu 24.04 (noble)
# socket-activates OpenSSH (the cam-box provisioning "noble ssh.socket" gotcha), so the LISTENER is
# ssh.socket -- `systemctl restart ssh` alone restarts the (possibly inactive) ssh.service and does
# NOT re-arm a listener-level wedge, i.e. the headline recovery would no-op on exactly the boxes this
# ticket targets (#1309 review 🟡). Restart the socket to re-arm the listener, then ssh.service (for a
# non-socket box), then the MCP surface. A unit that does not exist on a given box just logs a
# harmless failure (the restart loop is `... || log`), so this is correct on BOTH activation models.
MGMT_LIVENESS_SELFHEAL_UNITS="ssh.socket ssh remoteos-mcp"
MGMT_LIVENESS_SCRIPT_PATH="/usr/local/sbin/cambox-mgmt-selfcheck.sh"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + verify-device.sh (aj)
MGMT_LIVENESS_SERVICE_PATH="/etc/systemd/system/cambox-mgmt-selfcheck.service"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + verify-device.sh (aj)
MGMT_LIVENESS_TIMER_UNIT_NAME="cambox-mgmt-selfcheck.timer"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install)
MGMT_LIVENESS_TIMER_PATH="/etc/systemd/system/cambox-mgmt-selfcheck.timer"

# mgmt_liveness_banner_ok BANNER_TEXT -> "1" iff the read-back line is an SSH protocol banner
# (`SSH-...`), else "0". A plain TCP accept during the wedge yields NO banner (empty / reset), so
# this discriminates a genuine kex-reset half-dead sshd from a healthy one — a bare connect probe
# would false-pass.
mgmt_liveness_banner_ok() {
  case "$1" in
    SSH-*) printf '1' ;;
    *) printf '0' ;;
  esac
}

# mgmt_liveness_decide BANNER_OK PREV_CONSECUTIVE FAIL_THRESHOLD RESTARTS_IN_WINDOW MAX_RESTARTS
#   -> two lines: `verdict=<MGMT_OK|MGMT_DEAD|RESTART_ALLOWED|BACKOFF>` and `consecutive=<N>`.
# Pure state transition (the caller persists `consecutive` + the restart timestamps):
#   BANNER_OK=1                              -> MGMT_OK,        consecutive reset to 0
#   BANNER_OK=0, consecutive < threshold     -> MGMT_DEAD,      consecutive incremented (still counting;
#                                               a single blip must never trigger a restart)
#   BANNER_OK=0, consecutive >= threshold,
#     restarts_in_window < max               -> RESTART_ALLOWED (dump snapshot + restart ssh/mcp)
#     restarts_in_window >= max              -> BACKOFF         (snapshot only, do NOT restart-loop a
#                                               genuinely-broken box)
# Fail-safe on non-numeric input: a garbled state file reads as 0 (never a spurious restart).
mgmt_liveness_decide() {
  local banner_ok="$1" prev="$2" threshold="$3" restarts="$4" maxr="$5" consecutive verdict
  case "$prev" in *[!0-9]* | '') prev=0 ;; esac
  case "$threshold" in *[!0-9]* | '') threshold="$MGMT_LIVENESS_FAIL_THRESHOLD" ;; esac
  case "$restarts" in *[!0-9]* | '') restarts=0 ;; esac
  case "$maxr" in *[!0-9]* | '') maxr="$MGMT_LIVENESS_MAX_RESTARTS_PER_HOUR" ;; esac

  if [ "$banner_ok" = "1" ]; then
    verdict="MGMT_OK"; consecutive=0
  else
    consecutive=$((prev + 1))
    if [ "$consecutive" -lt "$threshold" ]; then
      verdict="MGMT_DEAD"
    elif [ "$restarts" -lt "$maxr" ]; then
      verdict="RESTART_ALLOWED"
    else
      verdict="BACKOFF"
    fi
  fi
  printf 'verdict=%s\nconsecutive=%s\n' "$verdict" "$consecutive"
}

# mgmt_liveness_snapshot_cmds -> the forensic-snapshot command block the on-box script runs on a
# CONFIRMED-dead banner (RESTART_ALLOWED / BACKOFF), BEFORE restarting ssh. Its stdout goes to the
# service's journal (StandardOutput=journal) which is now PERSISTENT (#1309), so `journalctl -b -1`
# after a power-cycle shows exactly what a self-heal could not fix. Every command is `timeout`-bounded
# because a fork-exhausted box can hang them; a bounded fail is fine (the marker lines still frame the
# gap). No side effects other than reading.
mgmt_liveness_snapshot_cmds() {
  cat <<'SNAP'
echo "=== #1309 mgmt-selfcheck FORENSIC SNAPSHOT $(date -u +%Y-%m-%dT%H:%M:%SZ) ==="
echo "--- top RSS processes ---";        timeout 5 ps -eo pid,ppid,stat,rss,comm --sort=-rss 2>&1 | head -40
echo "--- cgroup pids.current ---";      for f in /sys/fs/cgroup/system.slice/*/pids.current; do [ -r "$f" ] && printf '%s %s\n' "$f" "$(cat "$f" 2>/dev/null)"; done 2>&1 | sort -t' ' -k2 -nr | head -20
echo "--- pid_max / threads-max ---";    printf 'pid_max=%s threads-max=%s ulimit-u=%s\n' "$(cat /proc/sys/kernel/pid_max 2>/dev/null)" "$(cat /proc/sys/kernel/threads-max 2>/dev/null)" "$(ulimit -u 2>/dev/null)"
echo "--- free -m ---";                  timeout 5 free -m 2>&1
echo "--- ss syn-recv (half-open) ---";  timeout 5 ss -tan state syn-recv 2>&1 | head -20
echo "--- systemctl status ssh ---";     timeout 5 systemctl status ssh --no-pager 2>&1 | head -20
echo "--- journalctl -u ssh -n 30 ---";  timeout 5 journalctl -u ssh --no-pager -n 30 2>&1
echo "--- loginctl list-sessions ---";   timeout 5 loginctl list-sessions --no-pager 2>&1 | head -20
echo "--- logind/D-Bus health ---";      timeout 5 busctl --no-pager status 2>&1 | head -15
echo "=== end snapshot ==="
SNAP
}

# mgmt_liveness_service_unit / mgmt_liveness_timer_unit -> the systemd unit files (written verbatim by
# setup-device.sh). The service is a Type=oneshot that runs the generated script; the timer fires it
# every MGMT_LIVENESS_TIMER_INTERVAL (and shortly after boot). enable-only per provisioning-scripts.md.
mgmt_liveness_service_unit() {
  cat <<EOF
[Unit]
Description=camera-box management-liveness self-heal (#1309) - restart sshd/remoteos-mcp when the ssh banner goes dead
Documentation=https://github.com/zbynekdrlik/camera-box

[Service]
Type=oneshot
ExecStart=${MGMT_LIVENESS_SCRIPT_PATH}
# The self-heal itself must never wedge: bound the whole run.
TimeoutStartSec=60
StandardOutput=journal
StandardError=journal
SyslogIdentifier=cambox-mgmt-selfcheck
EOF
}

mgmt_liveness_timer_unit() {
  cat <<EOF
[Unit]
Description=camera-box management-liveness self-heal timer (#1309) - probe sshd every ${MGMT_LIVENESS_TIMER_INTERVAL}

[Timer]
OnBootSec=2min
OnUnitActiveSec=${MGMT_LIVENESS_TIMER_INTERVAL}
AccuracySec=15s
Persistent=false

[Install]
WantedBy=timers.target
EOF
}

# mgmt_liveness_selfcheck_script -> the full self-contained on-box script written to
# MGMT_LIVENESS_SCRIPT_PATH. It EMBEDS the pure functions above via `declare -f` (one source of
# truth, no drift), then does the I/O (a fork-free /dev/tcp banner read), the state read/prune/write
# (/run tmpfs — per-boot restart window, which is correct: a reboot is itself a recovery), and the
# action (snapshot + restart on RESTART_ALLOWED, snapshot-only on BACKOFF). Runs `set -uo pipefail`
# WITHOUT -e so a bounded probe/command failure never aborts the self-heal.
mgmt_liveness_selfcheck_script() {
  cat <<HEADER
#!/usr/bin/env bash
# GENERATED by scripts/lib/mgmt-liveness.sh (mgmt_liveness_selfcheck_script) — #1309. Do not edit on
# the box; re-provision via setup-device.sh. The pure decision + classifier + snapshot below are
# embedded verbatim from the repo lib (declare -f), so this script and its tests never drift.
set -uo pipefail
FAIL_THRESHOLD=${MGMT_LIVENESS_FAIL_THRESHOLD}
MAX_RESTARTS=${MGMT_LIVENESS_MAX_RESTARTS_PER_HOUR}
WINDOW_S=${MGMT_LIVENESS_RESTART_WINDOW_S}
SSH_PORT=${MGMT_LIVENESS_SSH_PORT}
BANNER_TIMEOUT_S=${MGMT_LIVENESS_BANNER_TIMEOUT_S}
STATE_FILE=${MGMT_LIVENESS_STATE_FILE}
SELFHEAL_UNITS="${MGMT_LIVENESS_SELFHEAL_UNITS}"
HEADER
  declare -f mgmt_liveness_banner_ok
  declare -f mgmt_liveness_decide
  declare -f mgmt_liveness_snapshot_cmds
  cat <<'BODY'

log() { printf '%s cambox-mgmt-selfcheck %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

# --- read the ssh banner off loopback (fork-free: /dev/tcp + read are bash builtins) --------------
# NB (#1309 review 🔵): assumes bash was built WITH /dev/tcp net-redirection (the Ubuntu default). A
# bash built --disable-net-redirections would read every probe as "dead" -> a false restart cycle,
# but that is bounded by the 3/hour backoff to snapshot-only and does not apply to the fleet's stock
# Ubuntu bash.
banner=""
if timeout "$BANNER_TIMEOUT_S" bash -c '
  exec 3<>/dev/tcp/127.0.0.1/"$0" || exit 1
  IFS= read -r -t "$1" line <&3 || exit 1
  printf "%s" "$line"
' "$SSH_PORT" "$BANNER_TIMEOUT_S" >/tmp/.mgmt-banner.$$ 2>/dev/null; then
  banner="$(cat /tmp/.mgmt-banner.$$ 2>/dev/null)"
fi
rm -f /tmp/.mgmt-banner.$$ 2>/dev/null
ok="$(mgmt_liveness_banner_ok "$banner")"

# --- read + prune persisted state (consecutive fails + restart epochs within the window) ----------
mkdir -p "$(dirname "$STATE_FILE")" 2>/dev/null || true
prev_consecutive=0
restart_epochs=""
if [ -r "$STATE_FILE" ]; then
  prev_consecutive="$(sed -n 's/^consecutive=//p' "$STATE_FILE" 2>/dev/null | tail -1)"
  restart_epochs="$(sed -n 's/^restarts=//p' "$STATE_FILE" 2>/dev/null | tail -1)"
fi
case "$prev_consecutive" in *[!0-9]* | '') prev_consecutive=0 ;; esac
now="$(date +%s)"
pruned=""
n_restarts=0
set -f  # #1309 review 🔵: disable globbing so a stray '*' in the /run state file can never expand
        # against the filesystem before the numeric guard below rejects it (defensive; state is
        # root-only /run, so not attacker-writable -- this just removes the surprise).
for e in $restart_epochs; do
  case "$e" in *[!0-9]* | '') continue ;; esac
  if [ $((now - e)) -lt "$WINDOW_S" ]; then
    pruned="${pruned:+$pruned }$e"
    n_restarts=$((n_restarts + 1))
  fi
done
set +f

# --- decide ---------------------------------------------------------------------------------------
decision="$(mgmt_liveness_decide "$ok" "$prev_consecutive" "$FAIL_THRESHOLD" "$n_restarts" "$MAX_RESTARTS")"
verdict="$(printf '%s\n' "$decision" | sed -n 's/^verdict=//p')"
consecutive="$(printf '%s\n' "$decision" | sed -n 's/^consecutive=//p')"

case "$verdict" in
  MGMT_OK)
    : ;;  # healthy — quiet (a per-probe OK line every 2 min is journal noise)
  MGMT_DEAD)
    log "ssh banner DEAD (consecutive=$consecutive/$FAIL_THRESHOLD) — holding, not yet actionable" ;;
  RESTART_ALLOWED)
    log "ssh banner DEAD x$consecutive — CONFIRMED half-dead (#1309); dumping forensic snapshot then restarting: $SELFHEAL_UNITS"
    eval "$(mgmt_liveness_snapshot_cmds)"
    for u in $SELFHEAL_UNITS; do
      timeout 20 systemctl restart "$u" 2>&1 && log "restarted $u" || log "restart $u FAILED (rc=$?) — box may be too far gone; will retry next tick until backoff"
    done
    pruned="${pruned:+$pruned }$now"
    ;;
  BACKOFF)
    log "ssh banner DEAD x$consecutive but restart cap ($MAX_RESTARTS/window) reached — snapshot only, NOT restarting (a truly-broken box must not be restart-looped)"
    eval "$(mgmt_liveness_snapshot_cmds)"
    ;;
esac

# --- persist state atomically ---------------------------------------------------------------------
tmp="$(mktemp "${STATE_FILE}.XXXXXX" 2>/dev/null || true)"
if [ -n "$tmp" ]; then
  { printf 'consecutive=%s\n' "$consecutive"; printf 'restarts=%s\n' "$pruned"; } > "$tmp" 2>/dev/null && mv -f "$tmp" "$STATE_FILE" 2>/dev/null || true
fi
exit 0
BODY
}
