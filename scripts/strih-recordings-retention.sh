#!/usr/bin/env bash
# strih-recordings-retention.sh (#1122, issue 1317 part 5) — dry-run-first E2E recordings retention on a rig OBS box.
set -euo pipefail
#
# The E2E harness (scripts/recording-e2e.sh) records one OBS program capture per run into the box's
# live OBS record directory; [8/8e] only deletes each run's OWN file, so aborted / skipped /
# failed-download runs leak forever (the retired Windows strih reached 344 .mkv = ~691 GiB). This tool
# keeps the newest N runs UNION anything younger than D days and deletes ONLY files matching the
# harness's OWN OBS-timestamp allowlist -- never a generic *.mkv sweep, so a differently-named operator
# recording (e.g. strih700105.mkv) is always protected -- and it PROTECTS every recording at or above
# the 1 GiB production-size floor (owner ruling, issue 1276).
#
# ONE decision, three executors (keep them in sync):
#   * src/recordings_retention.rs  plan()      -- the canonical pure spec (Rust, Tier-0 tested)
#   * scripts/strih-recordings-retention.ps1  -- the Windows executor (scp -O + powershell -File)
#   * rr_plan() in THIS file (--local-sweep)  -- the Linux executor (bash, runs ON the box)
# PARITY: tests/fixtures/recordings_retention_parity.tsv is ONE case table read by BOTH
# tests/recordings_retention.rs (against plan()) and tests/python/test_strih_lx_recordings_retention_1317.py
# (against rr_plan() over a real fixture dir) -- a drift in either fails its own side.
#
# Modes (there is NO default box -- issue 1317 part 3: the old default 10.77.9.202 was the Windows
# strih PC, RETIRED at the M4 cut-over; that address is the Linux strih-lx now):
#   --box <fleet-name>   the fleet-list way (scripts/lib/obs-fleet.sh): the box's host + CLASS pick the
#                        executor -- linux-genlock (strih-lx) -> ssh + THIS script fed to `bash -s` in
#                        --local-sweep mode (the box resolves its own record dir from its active OBS
#                        profile: [Output] Mode=Advanced -> [AdvOut] RecFilePath (FFFilePath when
#                        RecType=FFmpeg), else [SimpleOutput] FilePath; strih-lx = /srv/_REC);
#                        windows-genlock (stream, resolume) -> the .ps1 driver below.
#   --host <win-ip>      the Windows .ps1 driver by address: scp -O the .ps1 and run it via
#                        `powershell -NoProfile -ExecutionPolicy Bypass -File` -- NEVER a nested
#                        `powershell -Command` over ssh (fails silently on this rig,
#                        .claude/rules/rig-state-inspection.md). A linux-genlock fleet address is REFUSED
#                        by the class gate -- use --box <name> for it.
#   --local-sweep        run the bash decision on THE CURRENT machine (what a linux --box runs on the
#                        box; also for local testing against a fixture dir via --record-dir).
#                        --plan-tsv prints the raw machine plan (PROTECT/KEEP/DELETE rows) instead of
#                        the human report; RETENTION_NOW_EPOCH overrides "now" (test seam).
#
# ** The first real --execute run is the SUPERVISOR's explicit, reviewed step (#1122). ** Run the
# dry-run first, read the printed plan, and only then re-run with --execute. The dry-run leg is
# READ-ONLY (it lists a plan and deletes nothing). The Linux executor refuses --execute with
# --keep-runs 0 (it could delete the recording OBS is writing right now).
#
# Usage:
#   scripts/strih-recordings-retention.sh --box strih-lx                                 # strih-lx dry-run
#   scripts/strih-recordings-retention.sh --box strih-lx --keep-runs 20 --keep-days 3 --execute   # SUPERVISOR only
#   scripts/strih-recordings-retention.sh --box stream --record-dir 'C:\Users\newlevel\Videos'    # stream box, dry-run
#   scripts/strih-recordings-retention.sh --host <win-ip> --keep-runs 20 --keep-days 3   # a Windows box recording to C:\_REC
#   scripts/strih-recordings-retention.sh --local-sweep --record-dir <dir>               # this machine, dry-run
#
# Env: STRIH_SSH_PW (Windows boxes, default "newlevel"); LINUX_BOX_USER / LINUX_BOX_PW (a linux --box,
#      default newlevel / newlevel -- the rig's shared Linux-box creds, targets.md).

MODE=""                          # win | box | linux | local-sweep (no default -- issue 1317)
HOST=""
BOX=""
USER="newlevel"
RECORD_DIR="C:\\_REC"
RECORD_DIR_SET=0                 # 1 = --record-dir given (a linux sweep otherwise reads the OBS profile)
OBS_CONFIG_DIR="${HOME:-}/.config/obs-studio"
KEEP_RUNS="20"
KEEP_DAYS="3"
BUDGET_GB="50"
EXECUTE=0
PLAN_TSV=0
REMOTE_PATH='C:\Users\newlevel\strih-recordings-retention.ps1'

# Byte-mirror of PRODUCTION_SIZE_FLOOR_BYTES in src/recordings_retention.rs (1 GiB, issue 1276); the
# pytest pins the two constants equal.
RR_PRODUCTION_SIZE_FLOOR_BYTES=1073741824
# Byte-mirror of is_harness_recording(): `YYYY-MM-DD HH-MM-SS[ (n)].mkv|.mp4`, case-sensitive. An
# EXPLICIT digit list (never a locale digit class) so a fullwidth / Arabic-Indic digit can never match
# -- the Rust spec is is_ascii_digit(). Anchored both ends; no REG_NEWLINE, so `$` is end-of-string.
RR_ALLOW_RE='^[0123456789]{4}-[0123456789]{2}-[0123456789]{2} [0123456789]{2}-[0123456789]{2}-[0123456789]{2}( \([0123456789]+\))?\.(mkv|mp4)$'

while [ $# -gt 0 ]; do
  case "$1" in
    --host)           HOST="$2"; MODE="win"; shift 2 ;;
    --box)            BOX="$2"; MODE="box"; shift 2 ;;
    --local-sweep)    MODE="local-sweep"; shift ;;
    --user)           USER="$2"; shift 2 ;;
    --record-dir)     RECORD_DIR="$2"; RECORD_DIR_SET=1; shift 2 ;;
    --obs-config-dir) OBS_CONFIG_DIR="$2"; shift 2 ;;
    --keep-runs)      KEEP_RUNS="$2"; shift 2 ;;
    --keep-days)      KEEP_DAYS="$2"; shift 2 ;;
    --budget-gb)      BUDGET_GB="$2"; shift 2 ;;
    --remote-path)    REMOTE_PATH="$2"; shift 2 ;;
    --plan-tsv)       PLAN_TSV=1; shift ;;
    --execute)        EXECUTE=1; shift ;;
    -h|--help)        sed -n '2,50p' "${BASH_SOURCE[0]:-$0}"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# ---- the pure bash decision (mirror of src/recordings_retention.rs) -----------------------------

# rr_is_harness_recording <name> -> 0 iff NAME is an OBS-timestamp recording (the allowlist).
rr_is_harness_recording() {
  local LC_ALL=C
  [[ "$1" =~ $RR_ALLOW_RE ]]
}

# rr_horizon_ceil <keep_days> -> ceil(keep_days * 86400). The Rust rule is `age < days*86400` on f64;
# for an INTEGER age that is exactly `age < ceil(days*86400)`. awk does the same double arithmetic as
# Rust f64; %.0f (not %d) because mawk (strih-lx's awk) clamps %d at 2^31-1.
rr_horizon_ceil() {
  awk -v d="$1" 'BEGIN { h = d * 86400; c = int(h); if (c < h) c = c + 1; printf "%.0f", c }'
}

# rr_plan <dir> <now_epoch> <keep_runs> <keep_days> -> the machine plan on stdout, one row per
# top-level REGULAR file (a dir or a symlink is never a recording and is never touched), tab-separated,
# name LAST:
#   PROTECT <size> <mtime> <name>           non-matching name (never deleted)
#   KEEP <reason> <size> <mtime> <name>     reason = production-sized | newest-run | within-days
#   DELETE <size> <mtime> <name>            in plan order: newest first, name ascending on a tie
# Mirrors plan(): non-matching -> PROTECT; size >= floor -> production-sized (pulled OUT of the newest-N
# pool); the remaining below-floor files newest-first: index < keep_runs -> newest-run, else
# keep_days > 0 AND age < horizon -> within-days, else DELETE.
rr_plan() {
  local dir="$1" now="$2" keep_runs="$3" keep_days="$4"
  local tab=$'\t' horizon days_on=0 p name st size mtime shown
  local rows="" protect="" prod=""
  horizon="$(rr_horizon_ceil "$keep_days")"
  # keep_days is validated `[0-9]+(.[0-9]+)?`: it is > 0 iff it carries a non-zero digit.
  if [[ "$keep_days" =~ [123456789] ]]; then days_on=1; fi
  shopt -s nullglob dotglob
  for p in "$dir"/*; do
    if [ ! -f "$p" ] || [ -L "$p" ]; then continue; fi
    name="${p##*/}"
    st="$(stat -c '%s %Y' -- "$p" 2>/dev/null)" || continue   # vanished mid-scan: not ours to plan
    size="${st%% *}"
    mtime="${st##* }"
    if rr_is_harness_recording "$name"; then
      if [ "$size" -ge "$RR_PRODUCTION_SIZE_FLOOR_BYTES" ]; then
        prod+="KEEP${tab}production-sized${tab}${size}${tab}${mtime}${tab}${name}"$'\n'
      else
        rows+="${mtime}${tab}${size}${tab}${name}"$'\n'
      fi
    else
      # A protected name is display-only; keep the row one line even for a name with a tab/newline.
      shown="${name//[$'\t\n\r']/?}"
      protect+="PROTECT${tab}${size}${tab}${mtime}${tab}${shown}"$'\n'
    fi
  done
  shopt -u nullglob dotglob
  printf '%s' "$protect" | LC_ALL=C sort -t "$tab" -k4
  printf '%s' "$prod"
  # Newest first (mtime numeric-descending), name ascending (bytewise) on an exact tie -- plan()'s
  # sort. A matching name can hold no tab, so field 3 is the whole name.
  local sorted i=0 age reason
  sorted="$(printf '%s' "$rows" | LC_ALL=C sort -t "$tab" -k1,1nr -k3)"
  while IFS="$tab" read -r mtime size name; do
    [ -n "$name" ] || continue
    age=$(( now - mtime ))
    reason=""
    if [ "$i" -lt "$keep_runs" ]; then
      reason="newest-run"
    elif [ "$days_on" = 1 ] && [ "$age" -lt "$horizon" ]; then
      reason="within-days"
    fi
    i=$(( i + 1 ))
    if [ -n "$reason" ]; then
      printf 'KEEP\t%s\t%s\t%s\t%s\n' "$reason" "$size" "$mtime" "$name"
    else
      printf 'DELETE\t%s\t%s\t%s\n' "$size" "$mtime" "$name"
    fi
  done <<< "$sorted"
}

# rr_ini_get <file> <section> <key> -> the value (empty when absent); CRLF-tolerant; never fails.
rr_ini_get() {
  [ -r "$1" ] || return 0
  awk -v sec="[$2]" -v key="$3" '
    { sub(/\r$/, "") }
    /^\[/ { insec = ($0 == sec); next }
    insec { i = index($0, "="); if (i > 0 && substr($0, 1, i - 1) == key) { print substr($0, i + 1); exit } }
  ' "$1"
}

# rr_obs_profile_record_dir <obs_config_dir> -> the ACTIVE OBS profile's record path, or a named
# error + return 1 (never a guessed default -- a wrong dir would sweep the wrong files).
rr_obs_profile_record_dir() {
  local cfg="$1" pdir="" ini mode rectype key dir
  for ini in "$cfg/user.ini" "$cfg/global.ini"; do
    pdir="$(rr_ini_get "$ini" Basic ProfileDir)"
    if [ -z "$pdir" ]; then pdir="$(rr_ini_get "$ini" Basic Profile)"; fi
    if [ -n "$pdir" ]; then break; fi
  done
  if [ -z "$pdir" ]; then
    echo "ERROR: cannot resolve the active OBS profile (no [Basic] ProfileDir in $cfg/user.ini or global.ini) -- pass --record-dir" >&2
    return 1
  fi
  ini="$cfg/basic/profiles/$pdir/basic.ini"
  if [ ! -r "$ini" ]; then
    echo "ERROR: the active OBS profile '$pdir' has no readable basic.ini ($ini) -- pass --record-dir" >&2
    return 1
  fi
  mode="$(rr_ini_get "$ini" Output Mode)"
  if [ "$mode" = "Advanced" ]; then
    rectype="$(rr_ini_get "$ini" AdvOut RecType)"
    if [ "$rectype" = "FFmpeg" ]; then key="FFFilePath"; else key="RecFilePath"; fi
    dir="$(rr_ini_get "$ini" AdvOut "$key")"
    key="AdvOut] $key"
  else
    key="SimpleOutput] FilePath"
    dir="$(rr_ini_get "$ini" SimpleOutput FilePath)"
  fi
  if [ -z "$dir" ]; then
    echo "ERROR: the active OBS profile '$pdir' ($ini) has no [$key record path -- pass --record-dir" >&2
    return 1
  fi
  printf '%s\n' "$dir"
}

rr_gb() { awk -v b="$1" 'BEGIN { printf "%.2f", b / 1e9 }'; }

rr_day() { date -d "@$1" +%F 2>/dev/null || echo "?"; }

# rr_local_sweep -- validate, resolve the record dir, plan, render, and (only with --execute) delete.
rr_local_sweep() {
  [[ "$KEEP_RUNS" =~ ^[0123456789]+$ ]] || { echo "ERROR: --keep-runs must be a non-negative integer, got '$KEEP_RUNS'" >&2; exit 2; }
  [[ "$KEEP_DAYS" =~ ^[0123456789]+(\.[0123456789]+)?$ ]] || { echo "ERROR: --keep-days must be a non-negative decimal (e.g. 3 or 0.5), got '$KEEP_DAYS'" >&2; exit 2; }
  local runs=$(( 10#$KEEP_RUNS ))
  if [ "$EXECUTE" = 1 ] && [ "$runs" -lt 1 ]; then
    echo "ERROR: --execute with --keep-runs 0 is refused -- it could delete the recording OBS is writing right now; keep at least the newest run" >&2
    exit 2
  fi
  if [ "$EXECUTE" = 1 ] && [ "$PLAN_TSV" = 1 ]; then
    echo "ERROR: --plan-tsv is a print-only view; it cannot be combined with --execute" >&2
    exit 2
  fi
  local dir src
  if [ "$RECORD_DIR_SET" = 1 ]; then
    dir="$RECORD_DIR"; src="--record-dir"
  else
    dir="$(rr_obs_profile_record_dir "$OBS_CONFIG_DIR")" || exit 1
    src="the active OBS profile in $OBS_CONFIG_DIR"
  fi
  [ -d "$dir" ] || { echo "ERROR: record directory not found: $dir" >&2; exit 1; }
  local now="${RETENTION_NOW_EPOCH:-}"
  if [ -z "$now" ]; then now="$(date +%s)"; fi
  [[ "$now" =~ ^-?[0123456789]+$ ]] || { echo "ERROR: RETENTION_NOW_EPOCH must be an integer epoch, got '$now'" >&2; exit 2; }

  local plan
  plan="$(rr_plan "$dir" "$now" "$runs" "$KEEP_DAYS")"
  if [ "$PLAN_TSV" = 1 ]; then
    [ -z "$plan" ] || printf '%s\n' "$plan"
    return 0
  fi

  local tab=$'\t' kind a b c d
  local n_prot=0 b_prot=0 n_keep=0 b_keep=0 n_del=0 b_del=0
  echo "=== strih-recordings-retention (#1122, issue 1317 -- Linux local sweep) ==="
  echo "Host      : $(uname -n)"
  echo "RecordDir : $dir  (from $src)"
  echo "Policy    : keep newest $runs runs UNION younger than $KEEP_DAYS days"
  echo "SizeFloor : $RR_PRODUCTION_SIZE_FLOOR_BYTES bytes ($(rr_gb "$RR_PRODUCTION_SIZE_FLOOR_BYTES") GB) -- files >= this are PROTECTED as production-sized (#1276)"
  echo "Mode      : $([ "$EXECUTE" = 1 ] && echo 'EXECUTE (deleting)' || echo 'DRY-RUN (no deletion)')"
  echo ""
  echo "--- PROTECT (non-matching names -- never deleted) ---"
  while IFS="$tab" read -r kind a b c; do
    [ "$kind" = "PROTECT" ] || continue
    printf '  PROTECT  %6s GB  %s  %s\n' "$(rr_gb "$a")" "$(rr_day "$b")" "$c"
    n_prot=$(( n_prot + 1 )); b_prot=$(( b_prot + a ))
  done <<< "$plan"
  echo "--- KEEP ---"
  while IFS="$tab" read -r kind a b c d; do
    [ "$kind" = "KEEP" ] || continue
    printf '  KEEP     %6s GB  %s  %s  [%s, %sd]\n' "$(rr_gb "$b")" "$(rr_day "$c")" "$d" "$a" \
      "$(awk -v s=$(( now - c )) 'BEGIN { printf "%.1f", s / 86400 }')"
    n_keep=$(( n_keep + 1 )); b_keep=$(( b_keep + b ))
  done <<< "$plan"
  echo "--- DELETE ---"
  while IFS="$tab" read -r kind a b c; do
    [ "$kind" = "DELETE" ] || continue
    printf '  DELETE   %6s GB  %s  %s  [%sd]\n' "$(rr_gb "$a")" "$(rr_day "$b")" "$c" \
      "$(awk -v s=$(( now - b )) 'BEGIN { printf "%.1f", s / 86400 }')"
    n_del=$(( n_del + 1 )); b_del=$(( b_del + a ))
  done <<< "$plan"
  echo ""
  echo "--- SUMMARY ---"
  echo "  files total   : $(( n_prot + n_keep + n_del ))  ($(rr_gb $(( b_prot + b_keep + b_del ))) GB)"
  echo "  PROTECT       : $n_prot  ($(rr_gb "$b_prot") GB)"
  echo "  KEEP          : $n_keep  ($(rr_gb "$b_keep") GB)"
  echo "  DELETE        : $n_del  ($(rr_gb "$b_del") GB)  ($([ "$EXECUTE" = 1 ] && echo 'deleting' || echo 'would free'))"
  echo "  after cleanup : $(rr_gb $(( b_prot + b_keep ))) GB"
  local free
  free="$(df -B1 --output=avail -- "$dir" 2>/dev/null | tail -n 1 | tr -d ' ' || true)"
  if [[ "$free" =~ ^[0123456789]+$ ]]; then echo "  volume free   : $(rr_gb "$free") GB (before cleanup)"; fi

  if [ "$EXECUTE" != 1 ]; then
    echo ""
    echo "DRY-RUN -- nothing deleted. Re-run with --execute to delete the DELETE set above."
    echo "(The first --execute run is the supervisor's explicit, reviewed step -- #1122.)"
    return 0
  fi

  # EXECUTE: delete ONLY the DELETE rows, re-checking each one right before rm (still a regular
  # non-symlink file, still an allowlisted name, still below the production floor).
  echo ""
  echo "--- EXECUTE ---"
  local failed=0 path cur
  while IFS="$tab" read -r kind a b c; do
    [ "$kind" = "DELETE" ] || continue
    path="$dir/$c"
    if ! rr_is_harness_recording "$c" || [ ! -f "$path" ] || [ -L "$path" ]; then
      echo "  SKIP     $c  (no longer a regular allowlisted file)"
      continue
    fi
    cur="$(stat -c '%s' -- "$path" 2>/dev/null)" || { echo "  SKIP     $c  (vanished)"; continue; }
    if [ "$cur" -ge "$RR_PRODUCTION_SIZE_FLOOR_BYTES" ]; then
      echo "  SKIP     $c  (grew to production size since the plan)"
      continue
    fi
    if rm -f -- "$path"; then
      printf '  deleted  %6s GB  %s\n' "$(rr_gb "$cur")" "$c"
    else
      echo "  ERROR deleting $path" >&2
      failed=$(( failed + 1 ))
    fi
  done <<< "$plan"
  if [ "$failed" -gt 0 ]; then
    echo "ERROR: $failed file(s) could not be deleted" >&2
    exit 1
  fi
}

# ---- mode dispatch ------------------------------------------------------------------------------

# BASH_SOURCE is unset when this script is fed to `bash -s` (the linux --box leg runs it that way ON
# the box); HERE is only needed by the dev1 driver legs.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"

if [ "$MODE" = "box" ] || [ "$MODE" = "win" ]; then
  # shellcheck source=scripts/lib/obs-fleet.sh
  . "$HERE/lib/obs-fleet.sh"
fi
if [ "$MODE" = "box" ]; then
  [ "$BOX" != "strih" ] || { echo "ERROR: --box strih is the RETIRED Windows strih PC (issue 1317) -- the strih is 'strih-lx' (Linux): use --box strih-lx" >&2; exit 2; }
  BOX_CLASS="$(obs_fleet_class "$BOX")" || { echo "ERROR: --box '$BOX' is not in the fleet list (scripts/lib/obs-fleet.sh)" >&2; exit 2; }
  HOST="$(obs_fleet_host "$BOX")"
  case "$BOX_CLASS" in
    windows-genlock) MODE="win" ;;
    linux-genlock)   MODE="linux" ;;
    *) echo "ERROR: --box '$BOX' has an unknown fleet class '$BOX_CLASS'" >&2; exit 2 ;;
  esac
elif [ "$MODE" = "win" ]; then
  [ -n "$HOST" ] || { echo "ERROR: --host needs a Windows OBS box address" >&2; exit 2; }
  obs_fleet_refuse_linux_target "$HOST" "strih-recordings-retention.sh --host (the Windows .ps1 driver)" \
    || { echo "       use --box <fleet-name> for a Linux box (e.g. --box strih-lx: ssh + the bash --local-sweep)" >&2; exit 2; }
fi
[ -n "$MODE" ] || { echo "ERROR: name a target: --box <fleet-name> | --host <windows-obs-box> | --local-sweep -- there is no default box (the Windows strih PC this tool defaulted to is RETIRED, issue 1317; the strih is --box strih-lx)" >&2; exit 2; }

case "$MODE" in
  local-sweep)
    rr_local_sweep
    ;;

  linux)
    command -v sshpass >/dev/null || { echo "sshpass not installed (sudo apt-get install -y sshpass)" >&2; exit 1; }
    LB_USER="${LINUX_BOX_USER:-newlevel}"
    LB_PW="${LINUX_BOX_PW:-newlevel}"
    REMOTE_ARGS=(--local-sweep --keep-runs "$KEEP_RUNS" --keep-days "$KEEP_DAYS")
    [ "$RECORD_DIR_SET" = 0 ] || REMOTE_ARGS+=(--record-dir "$RECORD_DIR")
    [ "$PLAN_TSV" = 0 ] || REMOTE_ARGS+=(--plan-tsv)
    [ "$EXECUTE" = 0 ] || REMOTE_ARGS+=(--execute)
    REMOTE_CMD="bash -s --$(printf ' %q' "${REMOTE_ARGS[@]}")"
    echo "[$BOX] ssh ${LB_USER}@${HOST} -> $REMOTE_CMD  ($([ "$EXECUTE" = 1 ] && echo 'EXECUTE -- DELETING' || echo 'DRY-RUN -- no deletion'))"
    # THIS script is the program `bash -s` reads from stdin; no sudo -- the OBS record dir is owned by
    # the OBS user (strih-lx /srv/_REC = newlevel:newlevel 775).
    # shellcheck disable=SC2029  # REMOTE_CMD is built with printf %q for the remote shell on purpose.
    sshpass -p "$LB_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=15 "${LB_USER}@${HOST}" \
      "$REMOTE_CMD" < "$HERE/strih-recordings-retention.sh"
    ;;

  win)
    PS1_LOCAL="$HERE/strih-recordings-retention.ps1"
    PW="${STRIH_SSH_PW:-newlevel}"
    [ -f "$PS1_LOCAL" ] || { echo "missing $PS1_LOCAL" >&2; exit 1; }
    command -v sshpass >/dev/null || { echo "sshpass not installed (sudo apt-get install -y sshpass)" >&2; exit 1; }
    SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=15)
    echo "[1/2] scp -O $PS1_LOCAL -> ${USER}@${HOST}:${REMOTE_PATH}"
    sshpass -p "$PW" scp -O "${SSH_OPTS[@]}" "$PS1_LOCAL" "${USER}@${HOST}:${REMOTE_PATH}"
    MODE_ARG=""
    if [ "$EXECUTE" = "1" ]; then
      MODE_ARG="-Execute"
      echo "[2/2] run (EXECUTE — DELETING): $REMOTE_PATH"
    else
      echo "[2/2] run (DRY-RUN — no deletion): $REMOTE_PATH"
    fi
    # `powershell -File` with named params — NOT a nested `powershell -Command` over ssh.
    # shellcheck disable=SC2029  # the remote-side expansion of these vars is intentional.
    sshpass -p "$PW" ssh "${SSH_OPTS[@]}" "${USER}@${HOST}" \
      "powershell -NoProfile -ExecutionPolicy Bypass -File \"${REMOTE_PATH}\" -RecordDir \"${RECORD_DIR}\" -KeepRuns ${KEEP_RUNS} -KeepDays ${KEEP_DAYS} -BudgetGb ${BUDGET_GB} ${MODE_ARG}"
    ;;

  *) echo "unknown mode: $MODE" >&2; exit 2 ;;
esac
