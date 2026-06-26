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
# Env:
#   STRIH_BOX (default 10.77.9.202) — informational; the MCP target is win-strih.
set -euo pipefail

# Build the PowerShell command line that runs the verdict ON the strih box. RUST_LOG=info so the
# per-recording decode progress is visible (the agent's liveness signal). The verdict writes its
# partial JSON via --out; only THAT comes back to dev1.
#
# Pure-string function so a unit test can source the script and assert the command is well-formed
# (no dev1 path, the verdict runs against the LOCAL strih recording).
build_onbox_command() {
  local exe="$1"; shift
  # PowerShell: set RUST_LOG for the child, run the exe (call operator `&`) with all forwarded
  # args. The args are already Windows-style paths (single backslashes — the caller/harness
  # translates them), so each is wrapped in PowerShell DOUBLE quotes verbatim (NOT bash %q, which
  # would double the backslashes and corrupt the Windows path). A literal double-quote in an arg
  # is PowerShell-escaped as `"" (rare for file paths; handled for safety).
  printf '$env:RUST_LOG="info"; & "%s"' "$exe"
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
  local VERDICT_EXE='C:\camera-box\recording-verdict.exe'
  local OUT_DIR='C:\camera-box\verdict-out'
  local STRIH_REC=""
  # Everything after `--` is passed verbatim to recording-verdict on the box.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --strih-rec)   STRIH_REC="$2"; shift 2 ;;
      --verdict-exe) VERDICT_EXE="$2"; shift 2 ;;
      --out-dir)     OUT_DIR="$2"; shift 2 ;;
      --)            shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  local ONBOX_CMD
  ONBOX_CMD="$(build_onbox_command "$VERDICT_EXE" "${PASS_ARGS[@]}")"

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
# STEP 3: pull back ONLY the small partial JSON (a handful of MB of ids+timestamps):
#   win-strih FileDownload  path='<the --out partial JSON inside ${OUT_DIR}>'
#
# dev1 receives ONLY the tiny partial JSON. The ${STRIH_REC:-<strih recording>} stays on the strih
# box and is NEVER copied — not to the stream box, not to dev1 (#208).
PLAN
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call build_onbox_command
# without main parsing the sourcing shell's args).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
