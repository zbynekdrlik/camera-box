#!/usr/bin/env bash
# recording-verdict-on-imag.sh — run recording-verdict ON the imag-nb box (Linux, 10.77.9.182),
# where the imag OBS-program recording already lives, to EXTRACT its small per-box PARTIAL JSON
# (EPIC #466 Topology v2, #461/#462/#463). Third leg of the #208 per-box decode-in-place model
# alongside recording-verdict-on-strih.sh / recording-verdict-on-stream.sh — dev1 holds NOTHING
# big (no multi-GB recording ever lands here, only the small partial JSON + a handful of #186
# pixel-proof PNGs).
#
# WHY THIS SCRIPT ACTUALLY EXECUTES (unlike its Windows siblings): win-strih / win-stream-snv are
# reached ONLY via the win-* MCP — ssh/scp to those Windows boxes is DENIED on this rig, so those
# two sibling scripts are PURE PLANNERS: they print the exact command for an agent/operator to
# paste into the MCP Shell. imag-nb is a plain Ubuntu box (same access class as cam1/cam2 — see
# targets.md's "Linux OBS Targets" table, SSH newlevel/newlevel) — bash CAN ssh/scp it directly.
# So THIS script does the real work itself: deploy the verdict binary (if not already there),
# run the extract-partial ON imag against its LOCAL recording, and scp the small results back to
# dev1 — "no Windows-MCP detach dance needed" (#462).
#
# HOW THE PIECES FIT:
#   1. CI builds `recording-verdict` (probe-tools-linux-amd64) — the SAME Linux binary
#      recording-e2e.sh's PROBE_BIN_DIR already holds for the cam1/cam2 probe binaries (never
#      built on dev1, #192). imag-nb is x86_64 Ubuntu 24.04 — the identical binary runs there.
#   2. scp that binary to imag ONCE (skipped when it is already present + executable, unless
#      --force-upload is given) — imag-nb is a 16-thread Linux box, no cross-compile needed.
#   3. ssh runs `recording-verdict --extract-partial imag --imag <local rec>` ON imag against the
#      recording as it lives there (NEVER copied off-box, NEVER to dev1). ffmpeg/ffprobe must be
#      on imag's PATH (provisioned by setup-imag.sh, #458).
#   4. scp pulls back ONLY the small partial JSON (+ the sibling `<partial>-pixels` dir, #186 —
#      absent on a clean run) to dev1.
#
# Usage (EXECUTES the deploy+decode+pull-back — ssh/scp to imag is allowed, unlike the Windows
# boxes, so there is no separate "paste this into MCP" step):
#   recording-verdict-on-imag.sh \
#       --imag-rec /home/newlevel/imag-REC.mkv \
#       --verdict-bin target/release/recording-verdict \
#       --out-dir /home/newlevel/verdict-out \
#       --local-out-dir /tmp/recording-e2e-12345 \
#       -- --extract-partial imag --imag /home/newlevel/imag-REC.mkv --imag-capture-fps 60 \
#          --out /home/newlevel/verdict-out/imag-partial-12345.json
#
# Env:
#   IMAG_BOX / IMAG_USER / IMAG_PW  — ssh target (default 10.77.9.182 / newlevel / newlevel — the
#                                     same convention as CAM_PW and the targets.md Linux OBS row).
set -euo pipefail

# Build the shell command line that runs the verdict ON imag. RUST_LOG=info so the per-recording
# decode progress is visible (the agent's liveness signal) in the ssh output. Pure-string function
# so a unit test can source the script and assert the command is well-formed (imag-local paths
# only, %q-quoted — no dev1 path, no unescaped argument) WITHOUT touching the network.
build_onimag_command() {
  local exe="$1"; shift
  printf 'RUST_LOG=info %q' "$exe"
  local a
  for a in "$@"; do
    printf ' %q' "$a"
  done
  printf '\n'
}

# Parse flags + run the plan for real. Wrapped in a function so SOURCING the script (a unit test
# calling build_onimag_command) does NOT trigger arg-parsing / ssh against the sourcing shell's $@.
main() {
  local IMAG_BOX="${IMAG_BOX:-10.77.9.182}"
  local IMAG_USER="${IMAG_USER:-newlevel}"
  local IMAG_PW="${IMAG_PW:-newlevel}"
  local VERDICT_BIN=""                              # LOCAL (dev1) Linux binary to deploy
  local REMOTE_BIN="/home/newlevel/recording-verdict" # where it lands / already lives on imag
  local OUT_DIR="/home/newlevel/verdict-out"          # box-local (imag) out dir
  local LOCAL_OUT_DIR="."                             # dev1-side dir to pull results into
  local IMAG_REC=""
  local FORCE_UPLOAD=0
  # Everything after `--` is passed verbatim to recording-verdict on imag.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      # --skip-if-exists <partial-path>: if the partial JSON from a PREVIOUS run already exists on
      # dev1 (durable state, #281), skip re-decode entirely so a re-dispatched worker is idempotent.
      --skip-if-exists)
        if [ -f "$2" ]; then
          echo "SKIP: imag partial already exists at $2 — skipping re-decode (#281)"
          return 0
        fi
        shift 2 ;;
      --imag-rec)      IMAG_REC="$2"; shift 2 ;;
      --verdict-bin)   VERDICT_BIN="$2"; shift 2 ;;
      --remote-bin)    REMOTE_BIN="$2"; shift 2 ;;
      --out-dir)       OUT_DIR="$2"; shift 2 ;;
      --local-out-dir) LOCAL_OUT_DIR="$2"; shift 2 ;;
      --force-upload)  FORCE_UPLOAD=1; shift 1 ;;
      --)              shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  if [ -z "$IMAG_REC" ]; then
    echo "ERROR: --imag-rec <recording path as it lives on imag> is required" >&2
    exit 2
  fi

  command -v sshpass >/dev/null 2>&1 || {
    echo "ERROR: sshpass not found — needed to ssh/scp into imag-nb (apt install sshpass)." >&2
    exit 1
  }

  local -a SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=10)
  local TARGET="${IMAG_USER}@${IMAG_BOX}"

  # STEP 1: deploy the Linux verdict binary to imag — ONLY if missing/not-executable there, or
  # --force-upload/--verdict-bin's caller explicitly wants a fresh copy. Skipping a needless
  # re-upload keeps a re-dispatched worker's repeat runs fast (#281 idempotency spirit).
  if [ -n "$VERDICT_BIN" ]; then
    local need_upload=0
    if [ "$FORCE_UPLOAD" = "1" ]; then
      need_upload=1
    elif ! sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" \
        "[ -x '$REMOTE_BIN' ]" 2>/dev/null; then
      need_upload=1
    fi
    if [ "$need_upload" = "1" ]; then
      echo "[recording-verdict-on-imag] deploying $VERDICT_BIN -> ${TARGET}:${REMOTE_BIN}"
      sshpass -p "$IMAG_PW" scp "${SSH_OPTS[@]}" "$VERDICT_BIN" "${TARGET}:${REMOTE_BIN}"
      sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "chmod +x '$REMOTE_BIN'"
    else
      echo "[recording-verdict-on-imag] $REMOTE_BIN already present+executable on imag — skipping upload"
    fi
  fi

  # STEP 2: run the verdict ON imag against the LOCAL recording (NEVER copied off-box).
  local ONIMAG_CMD
  ONIMAG_CMD="$(build_onimag_command "$REMOTE_BIN" "${PASS_ARGS[@]}")"
  echo "[recording-verdict-on-imag] running on imag (${IMAG_BOX}): $ONIMAG_CMD"
  sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" \
    "mkdir -p '$OUT_DIR' && $ONIMAG_CMD"

  # #186/#208: the on-imag --extract-partial writes the pixel-proof PNGs of every flagged /
  # undecodable frame into the SIBLING `<partial>-pixels` dir (beside the --out partial JSON) — so
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

  if [ -z "$OUT_PARTIAL" ]; then
    echo "WARNING: no --out <partial.json> found in the forwarded args — nothing to pull back." >&2
    return 0
  fi
  PIXELS_DIR="${OUT_PARTIAL%.json}-pixels"

  # STEP 3: pull back ONLY the small partial JSON (+ its sibling <partial>-pixels dir — a handful
  # of flagged-frame PNGs, absent on a clean run). The imag RECORDING itself never leaves the box.
  mkdir -p "$LOCAL_OUT_DIR"
  local partial_base local_partial
  partial_base="$(basename "$OUT_PARTIAL")"
  local_partial="$LOCAL_OUT_DIR/$partial_base"
  echo "[recording-verdict-on-imag] pulling back $OUT_PARTIAL -> $local_partial"
  sshpass -p "$IMAG_PW" scp "${SSH_OPTS[@]}" "${TARGET}:${OUT_PARTIAL}" "$local_partial"
  # Pixel-proof dir is BEST EFFORT — absent entirely on a clean (zero-loss, fully decodable) run.
  if sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "[ -d '$PIXELS_DIR' ]" 2>/dev/null; then
    echo "[recording-verdict-on-imag] pulling back #186 pixel proofs $PIXELS_DIR -> $LOCAL_OUT_DIR/"
    sshpass -p "$IMAG_PW" scp -r "${SSH_OPTS[@]}" "${TARGET}:${PIXELS_DIR}" "$LOCAL_OUT_DIR/"
  else
    echo "[recording-verdict-on-imag] no pixel-proof dir on imag — clean run, nothing flagged"
  fi
  echo "[recording-verdict-on-imag] done: partial at $local_partial"
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call build_onimag_command
# without main parsing the sourcing shell's args or touching the network).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
