#!/usr/bin/env bash
# scripts/background-load-bench.sh -- issue 1387: prove OBS is immune to background load (header below).
set -euo pipefail
#
# Three phases on one Linux OBS box, read off program-render-audit `lagged`: no load, CPU load inside
# rig-background.slice, the same load outside it at nice 19 + ionice idle (how the backup ran). The
# load is sha256sum of /dev/zero on every online CPU, bounded by `timeout` on the box. No sound, no
# scene change, no recording, no OBS write. Graded by scripts/background_load_decision.py.
#
# USAGE: background-load-bench.sh [--plan] | --facts | --run [--box NAME] [--phase-s S] [--run-dir D]
#   --plan   (default) print what --run would do; touches nothing, no network
#   --facts  ONE read-only ssh read at nice 19: SMT pairs, the slice CPUs, what lives on them
#   --run    behind the rig lease, TEST mode and a proven idle rig; a broadcast aborts at once
# OPTIONS (env): --box [BACKGROUND_BENCH_BOX, strih-snv]  --phase-s [BACKGROUND_BENCH_PHASE_S, 180]
#   --run-dir [BACKGROUND_BENCH_RUN_DIR, ~/.camera-box/background-bench/<UTC stamp>]
#   BACKGROUND_BENCH_POLL_S (20), BACKGROUND_BENCH_SETTLE_S (30), BACKGROUND_BENCH_SSH_TIMEOUT_S (30),
#   BACKGROUND_BENCH_MAX_READ_BYTES (5000000), STREAM_HOST, PAINTER_IP.
#   Required: STRIH_USER + STRIH_PW (--facts, --run), CAM_PW (--run, the cam2 TEST-mode read).
#
# The OBS log can be 100 MB: a phase remembers the byte size and reads only what grew, through
# `tail -c +N | head -c MAX`, at nice 19. It never counts lines and never reads the whole file.
#
# EXIT: 0 PASS, 1 FAIL, 2 UNKNOWN (the run proved nothing), 3 usage, 4 refused before any load,
#       5 aborted after load started (the load dies by its own `timeout`, cleanup stops it sooner).
# Rule: .claude/rules/obs-box-baseline.md.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"
# shellcheck source=scripts/lib/obs-fleet.sh
. "$HERE/lib/obs-fleet.sh"
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh"
# shellcheck source=scripts/lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"
# shellcheck source=scripts/lib/rig-mode-state.sh
. "$HERE/lib/rig-mode-state.sh"
# shellcheck source=scripts/lib/av-soak.sh
. "$HERE/lib/av-soak.sh"

log() { printf '%s [background-bench] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { local rc="$1"; shift; log "ERROR: $*" >&2; exit "$rc"; }
is_uint() { case "${1:-}" in '' | *[!0123456789]*) return 1 ;; *) return 0 ;; esac; }

MODE=plan
BOX="${BACKGROUND_BENCH_BOX:-strih-snv}"
PHASE_S="${BACKGROUND_BENCH_PHASE_S:-180}"
RUN_DIR="${BACKGROUND_BENCH_RUN_DIR:-}"
POLL_S="${BACKGROUND_BENCH_POLL_S:-20}"
SETTLE_S="${BACKGROUND_BENCH_SETTLE_S:-30}"
SSH_TIMEOUT_S="${BACKGROUND_BENCH_SSH_TIMEOUT_S:-30}"
MAX_READ_BYTES="${BACKGROUND_BENCH_MAX_READ_BYTES:-5000000}"
while [ $# -gt 0 ]; do
  case "$1" in
    --plan) MODE=plan ;;
    --facts) MODE=facts ;;
    --run) MODE=run ;;
    --box) BOX="${2:?--box needs a name}"; shift ;;
    --phase-s) PHASE_S="${2:?--phase-s needs seconds}"; shift ;;
    --run-dir) RUN_DIR="${2:?--run-dir needs a path}"; shift ;;
    *) die 3 "unknown option $1" ;;
  esac
  shift
done
for _n in "$PHASE_S" "$POLL_S" "$SETTLE_S" "$SSH_TIMEOUT_S" "$MAX_READ_BYTES"; do
  is_uint "$_n" || die 3 "the timings must be integers"
done
[ "$PHASE_S" -ge "${BACKGROUND_BENCH_MIN_PHASE_S:-60}" ] || die 3 "--phase-s must be at least 60: a phase needs at least 6 audit lines"

BOX_HOST="${BACKGROUND_BENCH_HOST:-$(obs_fleet_host "$BOX" || true)}"
[ -n "$BOX_HOST" ] || die 3 "the box '$BOX' is not in the obs fleet"
[ "$(obs_fleet_class "$BOX" 2>/dev/null || echo linux-genlock)" = linux-genlock ] || die 3 "'$BOX' is not a Linux OBS box"
STREAM_HOST="${STREAM_HOST:-$(obs_fleet_host stream-snv)}"
camera_resolve cam2
PAINTER_IP="${PAINTER_IP:-${CAMERA_IP:-}}"
OBS_DIR="${BACKGROUND_BENCH_OBS_DIR:-$HERE}"
RIG_STATE="$HERE/av_soak_rig_state.py"
OBS_TIMEOUT_S=30
BROADCAST_READS="${AV_SOAK_BROADCAST_READS:-3}"
BROADCAST_RETRY_S="${AV_SOAK_BROADCAST_RETRY_S:-20}"
LEASE_REPO="camera-box-background-bench"
LEASE_ID="background-bench-$$-$(date -u +%s)"
LOAD_TAG="bg-load-1387"
RUNNER=/usr/local/sbin/obs-box-background-run
EXPECTED_S=$(((PHASE_S + SETTLE_S) * 3 + 300))

# background_bench_load_cmd -> REMOTE text: one sha256sum of /dev/zero per online CPU for PHASE_S. Pure.
background_bench_load_cmd() {
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf 'timeout %s sh -c '"'"'for i in $(seq 1 "$(getconf _NPROCESSORS_ONLN)"); do sha256sum /dev/zero & done; wait'"'"' %s' "$PHASE_S" "$LOAD_TAG"
}
# The remote commands of the two loaded phases: `timeout` ends the load on the box whatever happens here.
background_bench_inside_cmd() { printf 'sudo -n %s %s' "$RUNNER" "$(background_bench_load_cmd)"; }
background_bench_outside_cmd() { printf 'nice -n 19 ionice -c 3 %s' "$(background_bench_load_cmd)"; }
# The proof the load runs where the phase says: `<sha256sum processes> <of them in the slice>`. Pure.
# shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
background_bench_load_proof_cmd() { printf '%s' 'n=0; m=0; for p in $(pgrep -x sha256sum); do n=$((n + 1)); if grep -q "/rig-background.slice/" "/proc/$p/cgroup" 2>/dev/null; then m=$((m + 1)); fi; done; echo "$n $m"'; }
# The newest OBS log's name + byte size. Pure.
# shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
background_bench_log_size_cmd() { printf '%s' 'F="$(ls -t ~/.config/obs-studio/logs/*.txt | head -1)"; printf "%s\t" "$F"; stat -c %s "$F"'; }
# background_bench_log_since_cmd OFFSET -> the audit lines after OFFSET, reading at most MAX bytes. Pure.
background_bench_log_since_cmd() {
  # shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
  printf 'F="$(ls -t ~/.config/obs-studio/logs/*.txt | head -1)"; nice -n 19 tail -c +%s "$F" | head -c %s | grep -a "program-render-audit:" || true' \
    "$(($1 + 1))" "$MAX_READ_BYTES"
}
# shellcheck disable=SC2016  # expanded by the REMOTE shell, on purpose
background_bench_obs_mask_cmd() { printf '%s' 'awk "/^Cpus_allowed_list:/{print \$2; exit}" "/proc/$(pgrep -xo obs)/status"'; }
# The --facts read: SMT pairs, the OBS + slice CPUs, realtime and busy threads and IRQs on the background CPUs.
background_bench_facts_cmd() {
  cat <<'EOF'
echo "== SMT pairs"; grep -H . /sys/devices/system/cpu/cpu[0-9]*/topology/thread_siblings_list | sed 's#.*/cpu\([0-9]*\)/topology/thread_siblings_list:#cpu\1 #' | sort -n -k1.4
echo "== masks"; echo "online $(cat /sys/devices/system/cpu/online)"; for f in /etc/*-isolated-cpus.conf; do echo "$f $(cat "$f")"; done
P="$(pgrep -xo obs)"; [ -z "$P" ] || echo "OBS live mask: $(awk '/^Cpus_allowed_list:/{print $2; exit}' "/proc/$P/status")"
echo "== cgroup"; cat /sys/fs/cgroup/cgroup.subtree_control; uname -r; systemctl show -p AllowedCPUs,CPUWeight,IOWeight,MemoryHigh rig-background.slice
echo "== realtime threads"; ps -eLo psr,cls,rtprio,ni,pid,comm --no-headers | awk '$2 != "TS" && $2 != "IDL"' | sort -n | head -40
echo "== threads last seen on cpu 0-1 (top by count)"; ps -eLo psr,comm --no-headers | awk '$1 <= 1 {print $2}' | sort | uniq -c | sort -rn | head -20
echo "== dantesync + hub + restic masks"; for c in dantesync intercom-hub restic fbagent; do for p in $(pgrep -x "$c"); do echo "$c $p $(awk '/^Cpus_allowed_list:/{print $2; exit}' "/proc/$p/status") $(cut -d: -f3 "/proc/$p/cgroup" | head -1)"; done; done
echo "== IRQs by effective cpu"; for d in /proc/irq/[0-9]*; do [ -r "$d/effective_affinity_list" ] && echo "$(cat "$d/effective_affinity_list") $(ls "$d" | grep -vE '^(affinity_hint|effective_affinity|effective_affinity_list|node|smp_affinity|smp_affinity_list|spurious)$' | head -1)"; done | sort | uniq -c | sort -k2 -n | head -40
echo "== io scheduler"; cat /sys/block/nvme*/queue/scheduler
EOF
}

if [ "$MODE" = plan ]; then
  echo "BACKGROUND LOAD BENCH -- PLAN (issue 1387; nothing below has run, nothing was touched)"
  echo "  box ${BOX} (${BOX_HOST}); phases ${PHASE_S} s each, settle ${SETTLE_S} s; lease ${LEASE_REPO}, hold ceiling ${EXPECTED_S} s"
  echo "1. reads (refuse = exit 4): the rig lease; TEST mode from the cam2 painter (${PAINTER_IP:-unresolved}); a proven idle"
  echo "   rig (rig-busy-check ${BOX_HOST} + ${STREAM_HOST}); ${RUNNER} on the box; the OBS mask and log size"
  echo "2. baseline: no load, ${PHASE_S} s"
  echo "3. inside: $(background_bench_inside_cmd)"
  echo "4. outside: $(background_bench_outside_cmd)"
  echo "   every ${POLL_S} s: lease refreshed; a live broadcast aborts and stops the load"
  echo "5. each phase reads the log only past its size mark: $(background_bench_log_since_cmd 0)"
  echo "   a read during each loaded phase proves the load runs (rc 124 at the end) and where (cgroup of every sha256sum)"
  echo "6. verdict: python3 ${HERE}/background_load_decision.py (PASS, CPU load only: quiet baseline, inside 0 lagged, outside > 0, OBS mask unmoved)"
  exit 0
fi

for _v in STRIH_USER STRIH_PW; do
  [ -n "${!_v:-}" ] || die 3 "$_v is required for --facts and --run"
done
ssh_to() {  # BOUND USER PW HOST CMD -> run CMD on HOST (password via the environment, never argv)
  SSHPASS="$3" timeout "$1" sshpass -e ssh -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=10 "$2@$4" "$5"
}
remote() { ssh_to "$SSH_TIMEOUT_S" "$STRIH_USER" "$STRIH_PW" "$BOX_HOST" "$1"; }

if [ "$MODE" = facts ]; then
  remote "nice -n 19 bash -c $(printf '%q' "$(background_bench_facts_cmd)")"
  exit 0
fi

[ -n "${CAM_PW:-}" ] || die 3 "CAM_PW is required for --run (the cam2 TEST-mode read)"
[ -n "$RUN_DIR" ] || RUN_DIR="$HOME/.camera-box/background-bench/$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$RUN_DIR"
LOAD_PID=""
HOLDING=0
LOAD_STARTED=0
stop_load() {
  if [ -n "$LOAD_PID" ]; then kill "$LOAD_PID" 2>/dev/null || true; LOAD_PID=""; fi
  # The tag sits on the `timeout` line, which passes the TERM to its process group (the sha256sums). The
  # bracket keeps pkill from matching the remote shell's own command line.
  remote "pkill -f '[b]g-load-1387'; true" 2>/dev/null || true
}
cleanup() {
  local rc=$?
  trap - EXIT
  trap '' INT TERM HUP
  if [ "$LOAD_STARTED" = 1 ]; then stop_load; fi
  if [ "$HOLDING" = 1 ]; then rig_lease_release "$LEASE_ID" >/dev/null 2>&1 || true; fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 5' INT TERM HUP
refuse() {
  log "REFUSED: $1"
  if [ "$LOAD_STARTED" = 1 ]; then exit 5; fi
  exit 4
}
broadcast_now() {  # -> live | unknown | idle (the soak's retried read + pure decision)
  av_soak_broadcast_of "$RIG_STATE" "$(av_soak_rig_busy_settled "$OBS_DIR" "$BOX_HOST" "$STREAM_HOST" \
    "$OBS_TIMEOUT_S" "$RIG_STATE" "$BROADCAST_READS" "$BROADCAST_RETRY_S" 0)"
}
keepalive() {
  local out="" r=0
  out="$(rig_lease_refresh_if_mine "$LEASE_REPO" "$LEASE_ID" 2>&1)" || r=$?
  case "$r" in
    0 | 2) return 0 ;;
    *) refuse "the rig lease is no longer ours (${out#RIG_LEASE_REFRESH=})" ;;
  esac
}
log_mark() {  # -> sets MARK_FILE + MARK_SIZE
  local line
  line="$(remote "$(background_bench_log_size_cmd)")" || refuse "the OBS log size is unreadable"
  MARK_FILE="${line%%$'\t'*}"
  MARK_SIZE="${line##*$'\t'}"
  is_uint "$MARK_SIZE" || refuse "the OBS log size is unreadable ('$line')"
}
run_phase() {  # NAME REMOTE_CMD (empty = no load) -> $RUN_DIR/NAME.audit
  local name="$1" cmd="$2" file="$MARK_FILE" end proof="" load_rc=0
  log_mark
  [ -z "$file" ] || [ "$file" = "$MARK_FILE" ] || refuse "OBS started a new log during the bench"
  local since="$MARK_SIZE"
  log "phase ${name}: ${PHASE_S} s"
  end=$((SECONDS + PHASE_S))
  if [ -n "$cmd" ]; then
    LOAD_STARTED=1
    ssh_to "$((PHASE_S + 30))" "$STRIH_USER" "$STRIH_PW" "$BOX_HOST" "$cmd" &
    LOAD_PID=$!
  fi
  while [ "$SECONDS" -lt "$end" ]; do
    sleep "$POLL_S"
    keepalive
    [ "$(broadcast_now)" = idle ] || refuse "the rig is no longer provably idle"
    if [ -n "$cmd" ] && [ -z "$proof" ]; then proof="$(remote "$(background_bench_load_proof_cmd)" || true)"; fi
    if [ -n "$cmd" ] && ! kill -0 "$LOAD_PID" 2>/dev/null; then break; fi
  done
  if [ -n "$LOAD_PID" ]; then
    wait "$LOAD_PID" 2>/dev/null || load_rc=$?
    LOAD_PID=""
    read -r _n _m <<<"${proof:-0 0}"
    printf '%s:%s:%s\n' "$load_rc" "${_n:-0}" "${_m:-0}" > "$RUN_DIR/${name}.load"
    log "phase ${name}: load rc ${load_rc}, processes/in slice ${proof:-unread}"
  fi
  remote "$(background_bench_log_since_cmd "$since")" > "$RUN_DIR/${name}.audit" || refuse "the OBS log is unreadable"
  sleep "$SETTLE_S"
}

# ---- reads (exit 4, nothing changed) ----
log "run dir $RUN_DIR; box $BOX ($BOX_HOST); phases ${PHASE_S} s"
export RIG_LEASE_MAX_HOLD_SECS="$EXPECTED_S"
HOLDING=1  # before the acquire: a signal inside it must still release (the release checks the run id)
set +e
lease_out="$(RIG_LEASE_RESOURCES="$BOX" rig_lease_acquire "$LEASE_REPO" "$LEASE_ID" "" background-load-bench \
  "$(date -u -d "+${EXPECTED_S} seconds" +%Y-%m-%dT%H:%M:%SZ)")"
lease_rc=$?
set -e
log "$lease_out"
[ "$lease_rc" -eq 0 ] || { HOLDING=0; refuse "the rig lease is held (${lease_out#RIG_LEASE_HELD_BY=})"; }
_mode="$(rig_mode_from_painter_snapshot "$(ssh_to "$SSH_TIMEOUT_S" root "$CAM_PW" "$PAINTER_IP" "$(rig_mode_state_probe_remote_snippet)" 2>/dev/null || true)")"
[ "$_mode" = TEST ] || refuse "the rig is not in TEST mode (cam2 painter reads ${_mode:-unreadable})"
( stray_session_check_assert "$OBS_DIR" "$BOX_HOST" "$STREAM_HOST" "background load bench" ) || refuse "the rig-busy guard refused"
[ "$(broadcast_now)" = idle ] || refuse "the rig is not provably idle"
remote "[ -x $RUNNER ] && sudo -n -l $RUNNER >/dev/null" || refuse "$RUNNER is missing or not allowed by sudo: run setup first"
MASK_BEFORE="$(remote "$(background_bench_obs_mask_cmd)")" || refuse "the OBS CPU mask is unreadable (is OBS running?)"
[ -n "$MASK_BEFORE" ] || refuse "the OBS CPU mask is empty (is OBS running?)"

# ---- the phases: inside before outside, so the harmful phase cannot leak into the one under test ----
MARK_FILE=""
run_phase baseline ""
run_phase inside "$(background_bench_inside_cmd)"
run_phase outside "$(background_bench_outside_cmd)"
MASK_AFTER="$(remote "$(background_bench_obs_mask_cmd)")" || refuse "the OBS CPU mask is unreadable after the run"

set +e
python3 "$HERE/background_load_decision.py" --baseline "$RUN_DIR/baseline.audit" --inside "$RUN_DIR/inside.audit" \
  --outside "$RUN_DIR/outside.audit" --mask-before "$MASK_BEFORE" --mask-after "$MASK_AFTER" \
  --inside-load "$(cat "$RUN_DIR/inside.load")" --outside-load "$(cat "$RUN_DIR/outside.load")" | tee "$RUN_DIR/verdict.json"
rc=${PIPESTATUS[0]}
set -e
exit "$rc"
