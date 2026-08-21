#!/usr/bin/env bash
# obs-backup-retention.sh (#789 residual B) -- dry-run-first sweep of the deploy/backup dirs.
set -euo pipefail
#
# The ONE fleet deploy path (scripts/deploy-genlock-fleet.sh) leaves two kinds of directory behind
# on every box and neither is swept outside a deploy: dated box-backup dirs <stamp>-789 (win
# C:\obs-backup, imag /opt/obs-backup) and per-sha stage dirs (stage-genlock-<sha> under C:\ on win,
# genlock-stage-<sha> under /tmp on imag). This tool sweeps them, DRY-RUN by default, keeping the
# newest N of EACH kind UNION anything younger than D days, deleting ONLY dirs whose name matches
# the deploy's OWN naming allowlist -- NEVER a generic sweep (the imag 'previous' rollback dir and
# any operator folder are always protected).
#
# Three modes:
#   (default, --host <win-ip>)   DRIVER for a Windows box (strih/stream): scp -O obs-backup-retention.ps1
#                                to the box and run it via `powershell -File` (dry-run leg read-only),
#                                NEVER a nested `powershell -Command` over ssh.
#   --imag                       ssh to imag and run THIS script there in --local-sweep mode over
#                                /opt/obs-backup + /tmp (imag has no PowerShell).
#   --local-sweep                run the bash decision on THE CURRENT machine (used by --imag; also
#                                for local testing against --backup-root/--stage-parent fixtures).
#
# ** The first real --execute run is the SUPERVISOR's explicit, reviewed step (#789). ** Run the
# dry-run first, read the printed plan, and only then re-run with --execute.
#
# PARITY: the --local-sweep bash decision and obs-backup-retention.ps1 are faithful ports of the
# PURE decision in src/obs_backup_retention.rs (keep newest-N per kind UNION younger-than-D, same
# allowlist shapes). That Rust module + tests/obs_backup_retention.rs are the canonical spec.
#
# Usage:
#   scripts/obs-backup-retention.sh --host 10.77.9.202                 # dry-run on strih (win)
#   scripts/obs-backup-retention.sh --host 10.77.9.204                 # dry-run on stream (win)
#   scripts/obs-backup-retention.sh --imag                             # dry-run on imag (bash)
#   scripts/obs-backup-retention.sh --host 10.77.9.202 --execute       # SUPERVISOR only
#   scripts/obs-backup-retention.sh --imag --execute                   # SUPERVISOR only
#   scripts/obs-backup-retention.sh --local-sweep --backup-root <dir> --stage-parent <dir>  # test
#
# Env: STRIH_SSH_PW (win boxes, default "newlevel"); IMAG_IP (default 10.77.9.182),
#      IMAG_USER (default newlevel), IMAG_PW (default newlevel).

MODE="win"                      # win | imag | local-sweep
HOST="10.77.9.202"
USER="newlevel"
WIN_BACKUP_ROOT='C:\obs-backup'
WIN_STAGE_PARENT='C:\'
BACKUP_ROOT="/opt/obs-backup"   # local-sweep (imag) default
STAGE_PARENT="/tmp"             # local-sweep (imag) default
KEEP_RUNS="3"
KEEP_DAYS="7"
EXECUTE=0
REMOTE_PS1='C:\Users\newlevel\obs-backup-retention.ps1'

while [ $# -gt 0 ]; do
  case "$1" in
    --host)          HOST="$2"; MODE="win"; shift 2 ;;
    --user)          USER="$2"; shift 2 ;;
    --imag)          MODE="imag"; shift ;;
    --local-sweep)   MODE="local-sweep"; shift ;;
    --backup-root)   BACKUP_ROOT="$2"; shift 2 ;;
    --stage-parent)  STAGE_PARENT="$2"; shift 2 ;;
    --win-backup-root)  WIN_BACKUP_ROOT="$2"; shift 2 ;;
    --win-stage-parent) WIN_STAGE_PARENT="$2"; shift 2 ;;
    --keep-runs)     KEEP_RUNS="$2"; shift 2 ;;
    --keep-days)     KEEP_DAYS="$2"; shift 2 ;;
    --remote-ps1)    REMOTE_PS1="$2"; shift 2 ;;
    --execute)       EXECUTE=1; shift ;;
    -h|--help)       sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# ${BASH_SOURCE[0]:-$0}: BASH_SOURCE is unset when this script is fed to `bash -s` (the --imag leg),
# which would trip `set -u`. HERE is only used by the win/imag driver legs, not --local-sweep.
HERE="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"

# EXPLICIT allowlists -- byte-mirror of is_dated_backup()/is_stage_dir() in
# src/obs_backup_retention.rs. `[0-9]` (never a locale digit class); lowercase-hex sha only.
DATED_RE='^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}-[0-9]{2}-[0-9]{2}-789$'
STAGE_RE='^(stage-genlock|genlock-stage)-[0-9a-f]+$'

# obs_backup_sweep -- the PURE bash decision (newest-N per kind UNION younger-than-D). Prints the
# KEEP/DELETE plan and, when EXECUTE=1, deletes the DELETE set. Operates on the CURRENT machine.
obs_backup_sweep() {
  local now; now="$(date +%s)"
  local horizon; horizon="$(awk -v d="$KEEP_DAYS" 'BEGIN{printf "%d", d*86400}')"
  echo "=== obs-backup-retention (#789, local sweep) ==="
  echo "BackupRoot  : $BACKUP_ROOT  (dated <stamp>-789 dirs)"
  echo "StageParent : $STAGE_PARENT  (genlock-stage-<sha> / stage-genlock-<sha> dirs)"
  echo "Policy      : keep newest $KEEP_RUNS of EACH kind UNION younger than $KEEP_DAYS days"
  echo "Mode        : $([ "$EXECUTE" = 1 ] && echo 'EXECUTE (deleting)' || echo 'DRY-RUN (no deletion)')"
  echo ""

  local del_total=0
  # $1 = parent dir, $2 = allowlist regex, $3 = human kind label
  # $4 protect_mode: "list" (dedicated backup root -> show each protected name) or "count" (a shared
  # parent like /tmp or C:\ -> a count only, so we never dump every system dir).
  _sweep_kind() {
    local parent="$1" re="$2" label="$3" protect_mode="${4:-count}"
    echo "--- $label ---"
    [ -d "$parent" ] || { echo "  NOTE: parent not found (nothing to prune): $parent"; echo ""; return 0; }
    # One pass: split top-level dirs into matching ("mtime<TAB>path") and protected (non-matching).
    local rows="" protected_count=0
    local p name
    for p in "$parent"/*/; do
      [ -d "$p" ] || continue
      name="$(basename "$p")"
      if [[ "$name" =~ $re ]]; then
        rows+="$(stat -c '%Y' "$p")"$'\t'"${p%/}"$'\n'
      else
        protected_count=$(( protected_count + 1 ))
        [ "$protect_mode" = "list" ] && printf '  PROTECT  %s\n' "$name"
      fi
    done
    [ "$protect_mode" != "list" ] && [ "$protected_count" -gt 0 ] && \
      echo "  ($protected_count non-matching top-level dir(s) protected, not listed)"
    # Newest first: mtime (field 1) numeric-descending, path (field 2) ascending as a deterministic
    # tie-break — matches obs_backup_retention::plan() and the .ps1 (per-parent, so path-asc == name-asc).
    rows="$(printf '%s' "$rows" | sort -t"$(printf '\t')" -k1,1rn -k2,2)"
    local i=0
    [ -z "$rows" ] && { echo "  (none matched)"; echo ""; return 0; }
    while IFS=$'\t' read -r mt path; do
      [ -n "$path" ] || continue
      local name age keep
      name="$(basename "$path")"
      age=$(( now - mt ))
      keep=""
      if [ "$i" -lt "$KEEP_RUNS" ]; then keep="newest"
      elif [ "$KEEP_DAYS" != "0" ] && [ "$age" -lt "$horizon" ]; then keep="within-days"
      fi
      i=$(( i + 1 ))
      if [ -n "$keep" ]; then
        printf '  KEEP     %-30s  [%s, %dd]\n' "$name" "$keep" "$(( age / 86400 ))"
      else
        printf '  DELETE   %-30s  [%dd]\n' "$name" "$(( age / 86400 ))"
        del_total=$(( del_total + 1 ))
        if [ "$EXECUTE" = 1 ]; then
          rm -rf -- "$path" && echo "    deleted $path" || echo "    ERROR deleting $path" >&2
        fi
      fi
    done <<< "$rows"
    echo ""
  }

  _sweep_kind "$BACKUP_ROOT" "$DATED_RE" "dated box-backups (<stamp>-789)" "list"
  _sweep_kind "$STAGE_PARENT" "$STAGE_RE" "stage dirs (genlock-stage-<sha>)" "count"

  echo "--- SUMMARY ---"
  echo "  DELETE total : $del_total dir(s)  ($([ "$EXECUTE" = 1 ] && echo 'deleted' || echo 'would delete'))"
  if [ "$EXECUTE" != 1 ]; then
    echo ""
    echo "DRY-RUN -- nothing deleted. Re-run with --execute to delete the DELETE set above."
    echo "(The first --execute run is the supervisor's explicit, reviewed step -- #789.)"
  fi
}

case "$MODE" in
  local-sweep)
    obs_backup_sweep
    ;;

  imag)
    IMAG_IP="${IMAG_IP:-10.77.9.182}"; IMAG_USER="${IMAG_USER:-newlevel}"; IMAG_PW="${IMAG_PW:-newlevel}"
    command -v sshpass >/dev/null || { echo "sshpass not installed (sudo apt-get install -y sshpass)" >&2; exit 1; }
    MODE_ARG=""; [ "$EXECUTE" = 1 ] && MODE_ARG="--execute"
    echo "[imag] ssh ${IMAG_USER}@${IMAG_IP} -> --local-sweep (${BACKUP_ROOT} + ${STAGE_PARENT}) $([ "$EXECUTE" = 1 ] && echo EXECUTE || echo DRY-RUN)"
    # Feed ONE stdin stream to the remote `sudo -S bash -s`: the sudo password line FIRST, then THIS
    # script as the program. `sudo -S` consumes the first line (password), the child `bash -s`
    # inherits the same stdin and reads the rest (the program). A `printf|sudo` remote pipeline would
    # instead leave `bash -s` reading an empty program (the script would land on printf's ignored
    # stdin) -- a silent no-op. (sudo -- deploy backups are root-owned.)
    # shellcheck disable=SC2029
    sshpass -p "$IMAG_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=15 "${IMAG_USER}@${IMAG_IP}" \
      "sudo -S -p '' bash -s -- --local-sweep --backup-root '$BACKUP_ROOT' --stage-parent '$STAGE_PARENT' --keep-runs '$KEEP_RUNS' --keep-days '$KEEP_DAYS' $MODE_ARG" \
      < <(printf '%s\n' "$IMAG_PW"; cat "$HERE/obs-backup-retention.sh")
    ;;

  win)
    PS1_LOCAL="$HERE/obs-backup-retention.ps1"
    PW="${STRIH_SSH_PW:-newlevel}"
    [ -f "$PS1_LOCAL" ] || { echo "missing $PS1_LOCAL" >&2; exit 1; }
    command -v sshpass >/dev/null || { echo "sshpass not installed (sudo apt-get install -y sshpass)" >&2; exit 1; }
    SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=15)
    echo "[1/2] scp -O $PS1_LOCAL -> ${USER}@${HOST}:${REMOTE_PS1}"
    sshpass -p "$PW" scp -O "${SSH_OPTS[@]}" "$PS1_LOCAL" "${USER}@${HOST}:${REMOTE_PS1}"
    MODE_ARG=""
    if [ "$EXECUTE" = 1 ]; then MODE_ARG="-Execute"; echo "[2/2] run (EXECUTE -- DELETING): $REMOTE_PS1"
    else echo "[2/2] run (DRY-RUN -- no deletion): $REMOTE_PS1"; fi
    # Build the -BackupRoot/-StageParent args ONLY when they differ from the .ps1's own defaults.
    # The default -StageParent is `C:\` -- a trailing backslash immediately before the closing `\"`
    # over ssh reads as an escaped quote and corrupts the arg stream; omitting it lets the .ps1's own
    # (correctly-parsed) `C:\` default apply. A custom root/parent WITHOUT a trailing `\` is passed as
    # normal (the recordings precedent only ever passes such paths).
    PS_PATH_ARGS=""
    [ "$WIN_BACKUP_ROOT" != 'C:\obs-backup' ] && PS_PATH_ARGS="$PS_PATH_ARGS -BackupRoot \"${WIN_BACKUP_ROOT}\""
    [ "$WIN_STAGE_PARENT" != 'C:\' ] && PS_PATH_ARGS="$PS_PATH_ARGS -StageParent \"${WIN_STAGE_PARENT}\""
    # `powershell -File` with named params -- NOT a nested `powershell -Command` over ssh.
    # shellcheck disable=SC2029
    sshpass -p "$PW" ssh "${SSH_OPTS[@]}" "${USER}@${HOST}" \
      "powershell -NoProfile -ExecutionPolicy Bypass -File \"${REMOTE_PS1}\"${PS_PATH_ARGS} -KeepRuns ${KEEP_RUNS} -KeepDays ${KEEP_DAYS} ${MODE_ARG}"
    ;;

  *) echo "unknown mode: $MODE" >&2; exit 2 ;;
esac
