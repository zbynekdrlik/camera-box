#!/usr/bin/env bash
# recording-verdict-on-stream.sh — run recording-verdict ON stream.lan, where the recording
# already lives, and bring back ONLY the small verdict JSON (+ a few pixel-proof PNGs). dev1
# holds NOTHING big (#193).
#
# WHY (#193): OBS records the 0.7-6 GB program file on the powerful Windows stream box
# (10.77.9.204 — strong CPU, lots of RAM, fast disks). The OLD harness DOWNLOADED that
# multi-GB file over the LAN to dev1 (a slow PC meant only to run Claude) and decoded +
# rqrr'd it there — the root of the slow transfers, the dev1 OOM (#187), the 14GB+ disk fill,
# and the repeated stalls. The FIX runs the decode IN PLACE on the box that holds the video.
#
# HOW THE PIECES FIT:
#   1. CI builds recording-verdict.exe (probe-tools-windows-amd64) — never on dev1 (#192).
#   2. The win-stream-snv MCP FileUpload puts recording-verdict.exe on the stream box ONCE.
#   3. The win-stream-snv MCP Shell runs the verdict THERE against the LOCAL recording
#      (already on the box — NO download to dev1). ffmpeg/ffprobe are already on the box
#      (winget; ffmpeg 8.0.1 on PATH).
#   4. The win-stream-snv MCP FileDownload pulls back ONLY the verdict JSON + the handful of
#      pixel-proof PNGs (tiny) to dev1.
#
# The win-* MCP calls are AGENT-driven (scp/ssh to Windows is DENIED on this rig). This script
# is the PURE, testable planner: given the local recording path + verdict args, it PRINTS the
# exact PowerShell command line to run on the stream box (paths translated to the box's local
# Windows paths), so the agent/operator pastes it into `win-stream-snv Shell`. It NEVER touches
# a multi-GB file on dev1 and NEVER downloads the recording.
#
# Usage (planner mode — prints the on-box command + the upload/download plan):
#   recording-verdict-on-stream.sh \
#       --stream-rec 'C:\\path\\on\\stream\\stream-REC.mp4' \
#       --verdict-exe 'C:\\camera-box\\recording-verdict.exe' \
#       --out-dir 'C:\\camera-box\\verdict-out' \
#       -- <recording-verdict args, paths already Windows-style>
#
# Env:
#   STREAM_BOX (default 10.77.9.204) — informational; the MCP target is win-stream-snv.
set -euo pipefail

# Build the PowerShell command line that runs the verdict ON the stream box. RUST_LOG=info so
# the per-recording decode progress is visible (the agent's liveness signal). The verdict
# writes its JSON via --json and PNGs into --out-dir; only THOSE come back to dev1.
#
# Pure-string function so a unit test can source the script and assert the command is
# well-formed (no dev1 path, the verdict runs against the LOCAL stream recording, JSON +
# out-dir are inside the box-local OUT_DIR that gets pulled back).
build_onbox_command() {
  local exe="$1"; shift
  # PowerShell: set RUST_LOG for the child, run the exe (call operator `&`) with all forwarded
  # args. The args are already Windows-style paths (single backslashes — the caller/harness
  # translates them), so each is wrapped in PowerShell DOUBLE quotes verbatim (NOT bash %q,
  # which would double the backslashes and corrupt the Windows path). A literal double-quote in
  # an arg is PowerShell-escaped as `"" (rare for file paths; handled for safety).
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
  local STREAM_BOX="${STREAM_BOX:-10.77.9.204}"
  local VERDICT_EXE='C:\camera-box\recording-verdict.exe'
  local OUT_DIR='C:\camera-box\verdict-out'
  local STREAM_REC=""
  # Everything after `--` is passed verbatim to recording-verdict on the box.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --stream-rec)  STREAM_REC="$2"; shift 2 ;;
      --verdict-exe) VERDICT_EXE="$2"; shift 2 ;;
      --out-dir)     OUT_DIR="$2"; shift 2 ;;
      --)            shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  local ONBOX_CMD
  ONBOX_CMD="$(build_onbox_command "$VERDICT_EXE" "${PASS_ARGS[@]}")"

  # #186/#208: in PER-BOX extract mode the on-box --extract-partial writes the pixel-proof PNGs of
  # every flagged / undecodable frame into the SIBLING `<partial>-pixels` dir (beside the --out
  # partial JSON) — so the merge's #186 "SEE the missing frame" guarantee survives the per-box
  # split. Derive that dir from the forwarded `--out <partial>` so STEP 3 pulls it back too.
  local OUT_PARTIAL="" PIXELS_DIR=""
  local i
  for ((i = 0; i + 1 < ${#PASS_ARGS[@]}; i++)); do
    if [ "${PASS_ARGS[$i]}" = "--out" ]; then
      OUT_PARTIAL="${PASS_ARGS[$((i + 1))]}"
      break
    fi
  done
  if [ -n "$OUT_PARTIAL" ]; then PIXELS_DIR="${OUT_PARTIAL%.json}-pixels"; fi

  # STEP 3 pull-back text: per-box extract (a partial JSON + its <partial>-pixels dir) vs the
  # legacy fused mode (a --json verdict + the OUT_DIR\pixel-proof PNGs).
  local STEP3
  if [ -n "$OUT_PARTIAL" ]; then
    STEP3="#   win-stream-snv FileDownload  path='${OUT_PARTIAL}'
#   win-stream-snv FileDownload  path='${PIXELS_DIR}'   # #186 pixel proofs (a handful; absent on a clean run)"
  else
    STEP3="#   win-stream-snv FileDownload  path='<the --json path inside ${OUT_DIR}>'
#   win-stream-snv FileDownload  path='<each PNG under ${OUT_DIR}\\pixel-proof>'"
  fi

  cat <<PLAN
# ===== #193 run-on-stream.lan plan (decode where the video is — NOTHING big on dev1) =====
# stream box: ${STREAM_BOX}  (MCP: win-stream-snv)
#
# STEP 1 (once): upload the CI-built verdict to the box
#   win-stream-snv FileUpload  path='${VERDICT_EXE}'  <- probe-tools-windows-amd64/recording-verdict.exe
#
# STEP 2: run the verdict ON the box against the LOCAL recording (NO download to dev1):
#   win-stream-snv Shell:
${ONBOX_CMD}
#
# STEP 3: pull back ONLY the small results (the partial JSON / verdict JSON + a few pixel-proof PNGs):
${STEP3}
#
# dev1 receives ONLY the tiny JSON + the handful of flagged-frame PNGs. The
# ${STREAM_REC:-<stream recording>} stays on the box and is NEVER copied to dev1 (#193/#208). The
# merge derives the same <partial>-pixels dir to locate the #186 proofs.
PLAN
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call
# build_onbox_command without main parsing the sourcing shell's args).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
