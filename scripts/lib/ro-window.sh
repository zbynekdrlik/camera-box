#!/usr/bin/env bash
# airuleset:script-ok source-only lib (pure string builders + two pure parsers; the only top-level
# statement is the lazy source of ro-root.sh) -- deliberately NOT `set -euo pipefail`: sourcing runs
# this file in the CALLER's shell, so strict mode here would leak into every caller (the scripts/lib
# convention, .claude/rules/ci-testing-gotchas.md).
#
# scripts/lib/ro-window.sh -- the ONE verified close of a read-only-root rw window (issue 1407).
#
# A cambox (setup-device STEP 18) and a handheld SBC run on a READ-ONLY root. A tool that changes a
# file there opens a window: `mount -o remount,rw /`, the change, then the ro remount. The close is
# where it went wrong. A service started INSIDE the window opened a writer on /, the ro remount
# failed EBUSY, a `2>/dev/null || true` swallowed that, and the box ran on a writable root until its
# next reboot -- and a cambox is never rebooted remotely. Issue 1405 fixed that on cam2's painter
# handoff; the same hand-written close sat in deploy-fleet, the dantesync upgrade + rollback, the
# bkshading relay deploy + mode switch and the ndi-discovery apply.
#
# So every rw-window site:
#   - runs ONLY file writes, stops and `systemctl enable`/`disable` inside the window;
#   - closes it with ro_window_close_cmds below;
#   - starts / restarts / re-arms anything only AFTER that close succeeded. A failed close starts
#     nothing: the step fails by name.
#
# The close: `sync -f /`, the ro remount, then the ROOT MODE is READ (`findmnt -no OPTIONS /`, the
# /proc/mounts fallback) through the shared ro-root canon -- ro_root_mount_mode, whose definition is
# emitted into the remote text -- so the mount exit code is never trusted on its own. A root that
# does not read `ro` FAILS LOUD naming the WRITERS (the processes with a file open for writing on /,
# and the holders of deleted-but-open files) and exits 1. No retry loop: a writer that keeps the root
# busy keeps it busy on every retry, so a retry only delays the same failure.
#
# ro-root.sh keeps the fstab/tmpfs canon and the mode reading (free of grep/awk/sed, the issue-1311
# heredoc test stubs grep); this lib owns the close, the verify and the writer naming.
#
# Callers: cam2-painter-ro-persist.sh (the cam2 painter enable/disable), deploy-fleet.sh (the
# camera-box and frame-probe swaps), dantesync-rollback.sh (the dantesync upgrade + rollback
# programs), bkshading-relay-mode.sh, bkshading-deploy-relay.sh, ndi-discovery.sh (--cambox-apply).
# tests/python/test_ro_window_1407.py runs the emitted text against a fake box and sweeps scripts/
# for a swallowed ro close outside this lib.

# The mode reading is the ONE first-token reading of the ro-root canon; its definition is emitted
# into the close. Lazy-sourced (a caller may already have it), the cam2-painter-handoff.sh pattern.
command -v ro_root_mount_mode >/dev/null 2>&1 \
  || . "${BASH_SOURCE[0]%/*}/ro-root.sh"

# ro_window_holder_probe_cmd [PROC_ROOT] -> the REMOTE command that lists the holders of
# deleted-but-open files on the box, in `lsof +L1` columns (issue 808, moved here from
# bkshading-deploy-runtime.sh). A cambox does not provision lsof (psmisc/fuser only), so without it
# the same lines are built from /proc. Three places can hold a deleted file there, and the 25.9.2026
# cam6/cam7 incident was the first one: a running binary that was replaced is held through its
# EXECUTABLE (`/proc/<pid>/exe`, lsof `txt`), a library through a MAPPING (`/proc/<pid>/maps`, lsof
# `mem`), and an open file through an fd (`/proc/<pid>/fd/*`). Anonymous memory that always reads
# `(deleted)` but never holds `/` (`/memfd:*`, `/dev/shm/*`, `/SYSV*`) is left out, so it cannot
# crowd the real holder out of the 40-line cap; a process name with spaces has them turned into `_`
# so the columns hold. PROC_ROOT (default /proc) lets a test plant a fake tree. The body is a quoted
# heredoc: nothing expands locally except the one __PROC__ placeholder.
# shellcheck disable=SC2120  # PROC_ROOT is passed only by the tests (a fake /proc tree)
ro_window_holder_probe_cmd() {  # $1 = PROC_ROOT (default /proc)
  local root="${1:-/proc}" body
  body="$(cat <<'PROBE'

if command -v lsof >/dev/null 2>&1; then
  lsof +L1 2>/dev/null | grep -vE ' (/memfd:|/dev/shm/|/SYSV)' | head -n 40
else
  echo "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NLINK NODE NAME"
  for d in "__PROC__/"[0-9]*; do
    p="${d##*/}"
    c="$(tr ' ' _ <"$d/comm" 2>/dev/null)"
    t="$(readlink "$d/exe" 2>/dev/null)" && case "$t" in *" (deleted)") echo "${c:-?} $p - txt - - - 0 - $t" ;; esac
    awk '/ \(deleted\)$/ { $1=$2=$3=$4=$5=""; sub(/^ +/, ""); print }' "$d/maps" 2>/dev/null | sort -u |
      while IFS= read -r t; do echo "${c:-?} $p - mem - - - 0 - $t"; done
    for l in "$d"/fd/*; do
      t="$(readlink "$l" 2>/dev/null)" || continue
      case "$t" in *" (deleted)") echo "${c:-?} $p - fd - - - 0 - $t" ;; esac
    done
  done 2>/dev/null | grep -vE ' (/memfd:|/dev/shm/|/SYSV)' | sort -u | head -n 40
fi
PROBE
)"
  printf '%s\n' "${body//__PROC__/"$root"}"   # quoted: bash 5.2 patsub_replacement expands a bare &
}

# ro_window_writers_cmd -> ONE remote statement that prints the processes holding a file open for
# WRITING on / (issue 1405): the header and every `fuser -vm /` line whose ACCESS field carries F.
# The ACCESS field is found by its own shape (5 characters of `.rcefFm`, the PID right before it),
# never as "the field after the first number": fuser prints an unresolvable USER as a number, and a
# `1000 4242 F.... cmd` line would otherwise read the uid as the PID and skip the writer. `fuser -vm
# /` lists PID 1 and the kernel threads first, so a cut at N lines (`| head -n 40`) hides a writer
# with a high PID. No writer and no header line = fuser printed no listing (missing, failed): it
# says so, never "none". A writer line is printed whatever the header (a localized one is not
# matched). Literal heredoc: every $ is awk's.
ro_window_writers_cmd() {
  cat <<'CMDS'
  { fuser -vm / 2>&1 || true; } | awk '/USER/ && /PID/ && /ACCESS/ { print; h = 1; next } { for (i = 2; i <= NF; i++) if ($i ~ /^[.rcefFm][.rcefFm][.rcefFm][.rcefFm][.rcefFm]$/ && $(i - 1) ~ /^[0-9]+$/) { if ($i ~ /F/) { print; n++ } break } } END { if (n == 0) print (h ? "  (none: no process holds a file open for writing on /)" : "  (fuser printed no listing -- is psmisc installed? check by hand: fuser -vm /)") }' >&2 || true;
CMDS
}

# ro_window_close_cmds TAG BOX CONSEQUENCE HINT -> the REMOTE close of an rw window that is open
# NOW. Embed it via `$(ro_window_close_cmds ...)` -- every statement ends with `;` (the CLAUDE.md
# `$(...)` trailing-newline gotcha), so the caller may put any text right after it. It runs:
#   (1) the definition of ro_root_mount_mode (a statement of its own);
#   (2) `sync -f /` (rc kept, reported on a failure -- the kernel syncs the filesystem on the ro
#       remount itself, so the verify below is the gate) and `mount -o remount,ro /` (rc + error
#       kept);
#   (3) the root mode READ: `findmnt -no OPTIONS /`, else the /proc/mounts fallback, through
#       ro_root_mount_mode. `ro` -> nothing printed, the text goes on; after it `$_row_opts` holds
#       the options read (a caller may print them). Anything else (`rw`, `unknown` = unreadable)
#       -> the FAIL lines on stderr, then `exit 1`.
# The FAIL lines: `FAIL: [TAG] BOX's root is NOT read-only ...` + CONSEQUENCE, the writers
# (ro_window_writers_cmd), the deleted-but-open holders (ro_window_holder_probe_cmd), and
# `FAIL: [TAG] HINT` last. TAG, BOX, CONSEQUENCE and HINT are placed verbatim inside a double-quoted
# remote echo: a `$` in them expands ON THE BOX (cam2-painter-ro-persist.sh reads the unit state that
# way). A double quote or a backtick would break or inject into that echo, so such an argument emits
# a remote text that FAILS LOUD (`exit 1`) instead of a close -- never a silent empty command. The
# shared text names `remount,ro /` exactly once (the command; a caller's HINT may name it again) and
# never `systemctl`, so a caller's text anchors and its "every systemctl line ends || true" rule
# (bkshading-relay-mode.sh) are untouched.
ro_window_close_cmds() {
  local tag="${1:-ro-window}" box="${2:-this box}" consequence="${3:-}" hint="${4:-}" arg
  for arg in "$tag" "$box" "$consequence" "$hint"; do
    case "$arg" in
    *'"'* | *'`'*)
      printf '%s\n' "echo 'FAIL: ro_window_close_cmds: an argument holds a double quote or a backtick -- the rw window was NOT closed by this text; put the root back read-only by hand' >&2; exit 1;"
      return 0
      ;;
    esac
  done
  printf '%s;\n' "$(declare -f ro_root_mount_mode)"
  cat <<CMDS
_row_sync_rc=0;
_row_sync_err="\$(sync -f / 2>&1)" || _row_sync_rc=\$?;
_row_ro_rc=0;
_row_ro_err="\$(mount -o remount,ro / 2>&1)" || _row_ro_rc=\$?;
_row_opts="\$(findmnt -no OPTIONS / 2>/dev/null || awk '\$2=="/"{print \$4; exit}' /proc/mounts 2>/dev/null || true)";
_row_root="\$(ro_root_mount_mode "\$_row_opts")";
if [ "\$_row_root" != "ro" ]; then
  echo "FAIL: [$tag] $box's root is NOT read-only after the remount-rw window ('findmnt -no OPTIONS /' = '\$_row_opts' -> \$_row_root; the ro remount rc=\$_row_ro_rc\${_row_ro_err:+: \$_row_ro_err}; sync rc=\$_row_sync_rc). $consequence" >&2;
  echo "FAIL: [$tag] processes with a file open for WRITING on / ('fuser -vm /', ACCESS F):" >&2;
CMDS
  ro_window_writers_cmd
  cat <<CMDS
  echo "FAIL: [$tag] holders of deleted-but-open files on / (lsof +L1, else the /proc fd scan):" >&2;
  {
CMDS
  ro_window_holder_probe_cmd
  cat <<CMDS
  } >&2 || true;
  echo "FAIL: [$tag] $hint" >&2;
  exit 1;
fi;
CMDS
}

# ro_window_deleted_holders LSOF_TEXT -> ONE line `command[pid] path; command[pid] path` naming the
# holders of deleted-but-open files in `lsof +L1` columns (the header row skipped, the `(deleted)`
# marker dropped). Empty input -> empty output. Pure (awk over the argument), never fails the caller.
# (issue 808, moved here from bkshading-deploy-runtime.sh with the probe.)
ro_window_deleted_holders() {  # $1 = `lsof +L1` output
  printf '%s\n' "${1:-}" | awk '
    NR == 1 && $1 == "COMMAND" { next }
    NF >= 10 {
      path = $10
      for (i = 11; i <= NF; i++) { if ($i == "(deleted)") break; path = path " " $i }
      key = $1 "[" $2 "] " path
      if (seen[key]++) next
      out = out (out == "" ? "" : "; ") key
    }
    END { if (out != "") print out }
  ' || true
}

# ro_window_close_failed TEXT -> 0 when TEXT (a remote program's captured output) holds the shared
# close's "root is NOT read-only" FAIL line, i.e. the program stopped at a close that failed: whatever
# it would have started afterwards was NOT started. 1 otherwise. Pure, never fails the caller.
ro_window_close_failed() {  # $1 = the captured stdout+stderr of a remote program
  case "${1:-}" in
  *"'s root is NOT read-only after the remount-rw window"*) return 0 ;;
  *) return 1 ;;
  esac
}

# ro_window_holders CLOSE_OUTPUT -> ONE line naming every holder a FAILED close listed: the writers
# (`command[pid]`, from the 'processes with a file open for WRITING' section) and the deleted-but-open
# holders (`command[pid] path`, the 'holders of deleted-but-open files' section), `; `-joined, in
# that order. The dev1 side of a close run over ssh uses it for its one-line summary (deploy-fleet's
# FAILED entry, bkshading-deploy-relay's ERROR line). Nothing named -> empty output. Pure, never
# fails the caller.
ro_window_holders() {  # $1 = the captured stdout+stderr of a failed ro_window_close_cmds run
  local writers lsof
  writers="$(printf '%s\n' "${1:-}" | awk '
    /processes with a file open for WRITING on \// { s = 1; next }
    /^FAIL: / { s = 0 }
    s == 1 && !(/USER/ && /PID/ && /ACCESS/) {
      for (i = 2; i <= NF; i++) if ($i ~ /^[.rcefFm][.rcefFm][.rcefFm][.rcefFm][.rcefFm]$/ && $(i - 1) ~ /^[0-9]+$/) {
        if ($i ~ /F/ && (i + 1) <= NF) {
          key = $(i + 1) "[" $(i - 1) "]"
          if (!seen[key]++) out = out (out == "" ? "" : "; ") key
        }
        break
      }
    }
    END { if (out != "") print out }
  ' || true)"
  lsof="$(printf '%s\n' "${1:-}" | awk '
    /holders of deleted-but-open files on \// { s = 1; next }
    /^FAIL: / { s = 0 }
    s == 1 { print }
  ' || true)"
  lsof="$(ro_window_deleted_holders "$lsof")"
  if [ -n "$writers" ] && [ -n "$lsof" ]; then
    printf '%s; %s\n' "$writers" "$lsof"
  elif [ -n "$writers$lsof" ]; then
    printf '%s\n' "$writers$lsof"
  fi
}
