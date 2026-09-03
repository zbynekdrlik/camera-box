#!/usr/bin/env bash
# recording-verdict-on-strih.sh — run recording-verdict ON the STRIH box, where the strih
# recording already lives, to EXTRACT its small per-box PARTIAL JSON. dev1 holds NOTHING big
# (#208 — refines #193).
#
# WHY (#208): the verdict needs the STRIH recording (cam1 contiguity #133 + cam→strih) AND the
# STREAM recording (the full chain). The OLD on-stream flow ran a SINGLE fused verdict on the
# stream box, which forced the ~700 MB strih .mkv to be COPIED strih→stream first — a needless
# box-to-box copy of a multi-GB file. The FIX decodes the strih recording IN PLACE on the strih
# box and emits a SMALL partial JSON (ids + timestamps, no frames/pixels); dev1 merges the strih
# + stream partials. The strih recording NEVER leaves the strih box.
#
# HOW THE PIECES FIT (mirrors recording-verdict-on-stream.sh, MCP target win-strih):
#   1. CI builds recording-verdict.exe (probe-tools-windows-amd64) — never on dev1 (#192).
#   2. The win-strih MCP FileUpload puts recording-verdict.exe on the strih box ONCE.
#   3. The win-strih MCP Shell runs `recording-verdict --extract-partial strih --strih <local>`
#      THERE against the box-LOCAL strih recording (already on the box — NO copy anywhere).
#      ffmpeg/ffprobe are already on the box.
#   4. The win-strih MCP FileDownload pulls back ONLY the small partial JSON to dev1.
#
# The win-* MCP calls are AGENT-driven (scp/ssh to Windows is DENIED on this rig). This script is
# the PURE, testable planner: given the box-local recording path + the recording-verdict args, it
# PRINTS the exact PowerShell command to run on the strih box (paths translated to the box's local
# Windows paths), so the agent/operator pastes it into `win-strih Shell`. It NEVER touches a
# multi-GB file on dev1 and NEVER copies the strih recording off the box.
#
# Usage (planner mode — prints the on-box command + the upload/download plan):
#   recording-verdict-on-strih.sh \
#       --strih-rec 'C:\\path\\on\\strih\\strih-REC.mkv' \
#       --verdict-exe 'C:\\camera-box\\recording-verdict.exe' \
#       --out-dir 'C:\\camera-box\\verdict-out' \
#       -- <recording-verdict args, paths already Windows-style>
#
# #703: --execute switches this script to ACTUALLY RUN the plan over ssh/scp (no MCP, no
# human paste-step) — #701 proved plain OpenSSH+password ssh/scp works on this rig for strih
# specifically. Needs --verdict-exe-local <dev1 path to the CI-built recording-verdict.exe>
# (always uploaded fresh — correctness over speed, the exe is ~3MB) and --local-out-dir <dev1
# dir to pull the partial + #186 pixel-proof dir into>. Planner mode (no --execute, the
# default) is UNCHANGED — still the pure text-only plan for a human/MCP operator run.
#
# Usage (execute mode):
#   recording-verdict-on-strih.sh --execute \
#       --strih-rec 'D:\_REC\...\strih-REC.mkv' \
#       --verdict-exe-local target/release-windows/recording-verdict.exe \
#       --out-dir 'C:\camera-box\verdict-out' --local-out-dir /tmp/recording-e2e-12345 \
#       -- --extract-partial strih --strih 'D:\_REC\...\strih-REC.mkv' ... --out 'C:\camera-box\verdict-out\strih-partial-12345.json'
#
# Env:
#   STRIH_BOX (default 10.77.9.202) — ssh/MCP target.
#   STRIH_USER / STRIH_PW (default newlevel / newlevel, per targets.md) — --execute only.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/win-ssh-exec.sh
. "$HERE/lib/win-ssh-exec.sh"

# Build the PowerShell command line that runs the verdict ON the strih box. RUST_LOG=info so the
# per-recording decode progress is visible (the agent's liveness signal). The verdict writes its
# partial JSON via --out; only THAT comes back to dev1.
#
# Pure-string function so a unit test can source the script and assert the command is well-formed
# (no dev1 path, the verdict runs against the LOCAL strih recording).
build_onbox_command() {
  local exe="$1"; shift
  # PowerShell: set RUST_LOG for the child, set the HOST process's PriorityClass (issue 1260 —
  # the &-invoked child inherits it, keeping the ~20-min decode from starving the live obs64
  # process on this box; see onbox_decode_priority_class's own comment in win-ssh-exec.sh), then
  # run the exe (call operator `&`) with all forwarded args. The args are already Windows-style
  # paths (single backslashes — the caller/harness translates them), so each is wrapped in
  # PowerShell DOUBLE quotes verbatim (NOT bash %q, which would double the backslashes and
  # corrupt the Windows path). A literal double-quote in an arg is PowerShell-escaped as `""
  # (rare for file paths; handled for safety).
  local prio
  prio="$(onbox_decode_priority_class)"
  # shellcheck disable=SC2016  # single-quoted PowerShell syntax; $env: must NOT expand in bash
  printf '$env:RUST_LOG="info"; [System.Diagnostics.Process]::GetCurrentProcess().PriorityClass = "%s"; & "%s"' "$prio" "$exe"
  local a esc
  for a in "$@"; do
    esc="${a//\"/\"\"}" # PowerShell escapes an embedded " as ""
    printf ' "%s"' "$esc"
  done
  printf '\n'
}

# Parse flags + emit the full plan. Wrapped in a function so SOURCING the script (a unit test
# calling build_onbox_command) does NOT trigger arg-parsing against the sourcing shell's $@.
main() {
  local STRIH_BOX="${STRIH_BOX:-10.77.9.202}"
  local STRIH_USER="${STRIH_USER:-newlevel}"
  local STRIH_PW="${STRIH_PW:-newlevel}"
  local VERDICT_EXE='C:\camera-box\recording-verdict.exe'
  local OUT_DIR='C:\camera-box\verdict-out'
  local STRIH_REC=""
  local EXECUTE=0
  local VERDICT_EXE_LOCAL=""
  local LOCAL_OUT_DIR=""
  # Everything after `--` is passed verbatim to recording-verdict on the box.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      # --skip-if-exists <partial-path>: if the partial JSON from a PREVIOUS run already exists on
      # dev1 (durable state, #281), skip re-decode entirely so a re-dispatched worker is idempotent.
      --skip-if-exists)
        if [ -f "$2" ]; then
          echo "SKIP: strih partial already exists at $2 — skipping re-decode (#281)"
          return 0
        fi
        shift 2 ;;
      --strih-rec)        STRIH_REC="$2"; shift 2 ;;
      --verdict-exe)      VERDICT_EXE="$2"; shift 2 ;;
      --out-dir)          OUT_DIR="$2"; shift 2 ;;
      --execute)          EXECUTE=1; shift 1 ;;
      --verdict-exe-local) VERDICT_EXE_LOCAL="$2"; shift 2 ;;
      --local-out-dir)    LOCAL_OUT_DIR="$2"; shift 2 ;;
      --)                 shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  local ONBOX_CMD
  ONBOX_CMD="$(build_onbox_command "$VERDICT_EXE" "${PASS_ARGS[@]}")"

  # issue 1260: read the RESOLVED priority class back out of the already-built $ONBOX_CMD (rather
  # than calling onbox_decode_priority_class a second time) so the run-log line below never
  # double-prints an invalid-value WARNING that build_onbox_command's own internal resolve above
  # already emitted once.
  local RESOLVED_PRIO="${ONBOX_CMD#*PriorityClass = \"}"
  RESOLVED_PRIO="${RESOLVED_PRIO%%\"*}"

  # #186/#208: the on-box --extract-partial writes the pixel-proof PNGs of every flagged /
  # undecodable frame into the SIBLING `<partial>-pixels` dir (beside the --out partial JSON), so
  # the merge's #186 "SEE the missing frame" guarantee survives the per-box split. Derive that dir
  # from the forwarded `--out <partial>` so STEP 3 pulls it back too.
  local OUT_PARTIAL="" PIXELS_DIR=""
  local i
  for ((i = 0; i + 1 < ${#PASS_ARGS[@]}; i++)); do
    if [ "${PASS_ARGS[$i]}" = "--out" ]; then
      OUT_PARTIAL="${PASS_ARGS[$((i + 1))]}"
      break
    fi
  done
  if [ -n "$OUT_PARTIAL" ]; then PIXELS_DIR="${OUT_PARTIAL%.json}-pixels"; fi

  if [ "$EXECUTE" = "1" ]; then
    if [ -z "$OUT_PARTIAL" ]; then
      echo "ERROR: --execute needs a --out <partial.json> inside the forwarded args" >&2
      exit 2
    fi
    if [ -z "$LOCAL_OUT_DIR" ]; then
      echo "ERROR: --execute needs --local-out-dir <dev1 dir to pull results into>" >&2
      exit 2
    fi
    command -v sshpass >/dev/null 2>&1 || {
      echo "ERROR: sshpass not found — needed to ssh/scp into strih (#701/#703)." >&2
      exit 1
    }
    echo "[recording-verdict-on-strih] --execute: ensuring $OUT_DIR exists on strih (${STRIH_BOX})"
    win_ssh_run "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" \
      "New-Item -ItemType Directory -Force -Path \"$OUT_DIR\" | Out-Null"
    if [ -n "$VERDICT_EXE_LOCAL" ]; then
      echo "[recording-verdict-on-strih] deploying $VERDICT_EXE_LOCAL -> ${STRIH_BOX}:${VERDICT_EXE} (always fresh — correctness over speed)"
      win_ssh_upload "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" "$VERDICT_EXE_LOCAL" "$VERDICT_EXE"
    fi
    echo "[recording-verdict-on-strih] decode priority: $RESOLVED_PRIO (E2E_ONBOX_DECODE_PRIORITY)"
    echo "[recording-verdict-on-strih] running on strih (${STRIH_BOX}): $ONBOX_CMD"
    win_ssh_run "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" "$ONBOX_CMD"
    mkdir -p "$LOCAL_OUT_DIR"
    local partial_base local_partial
    partial_base="$(win_ssh_basename "$OUT_PARTIAL")" # #703: plain `basename` doesn't split on \
    local_partial="$LOCAL_OUT_DIR/$partial_base"
    echo "[recording-verdict-on-strih] pulling back $OUT_PARTIAL -> $local_partial"
    win_ssh_download "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" "$OUT_PARTIAL" "$local_partial"
    if win_ssh_path_exists "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" "$PIXELS_DIR"; then
      echo "[recording-verdict-on-strih] pulling back #186 pixel proofs $PIXELS_DIR -> $LOCAL_OUT_DIR/"
      win_ssh_download_dir "$STRIH_USER" "$STRIH_PW" "$STRIH_BOX" "$PIXELS_DIR" "$LOCAL_OUT_DIR/"
    else
      echo "[recording-verdict-on-strih] no pixel-proof dir on strih — clean run, nothing flagged"
    fi
    echo "[recording-verdict-on-strih] done: partial at $local_partial"
    return 0
  fi

  cat <<PLAN
# ===== #208 extract-strih-partial ON the strih box (decode where the video is) =====
# strih box: ${STRIH_BOX}  (MCP: win-strih)
#
# STEP 1 (once): upload the CI-built verdict to the box
#   win-strih FileUpload  path='${VERDICT_EXE}'  <- probe-tools-windows-amd64/recording-verdict.exe
#
# STEP 2: extract the strih PARTIAL ON the box against the LOCAL recording (NO copy anywhere):
#   win-strih Shell:
${ONBOX_CMD}
#
# STEP 3: pull back the small partial JSON (ids+timestamps) AND the #186 pixel-proof PNGs:
#   win-strih FileDownload  path='${OUT_PARTIAL:-<the --out partial JSON inside ${OUT_DIR}>}'
#   win-strih FileDownload  path='${PIXELS_DIR:-<the <partial>-pixels dir beside the partial>}'   # #186 pixel proofs (a handful; absent on a clean run)
#
# dev1 receives ONLY the tiny partial JSON + the handful of flagged-frame PNGs. The
# ${STRIH_REC:-<strih recording>} stays on the strih box and is NEVER copied — not to the stream
# box, not to dev1 (#208). The merge derives the same <partial>-pixels dir to locate the proofs.
PLAN
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call build_onbox_command
# without main parsing the sourcing shell's args).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
