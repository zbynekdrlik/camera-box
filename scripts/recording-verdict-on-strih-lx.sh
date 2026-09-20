#!/usr/bin/env bash
# recording-verdict-on-strih-lx.sh — run recording-verdict ON the strih-lx box (Linux notebook,
# 10.77.9.202, M4 cut-over — issue 1351), where the strih recording already lives, to EXTRACT its
# small per-box PARTIAL JSON. This is the Linux sibling of recording-verdict-on-strih.sh (Windows,
# unchanged) and mirrors recording-verdict-on-imag.sh's shape: strih-lx is a plain Ubuntu box
# reached over plain ssh/scp (targets.md's "Linux OBS Targets" access class, SAME as imag-nb and
# cam1/cam2) — never the win-* MCP, never PowerShell.
#
# WHY THIS SCRIPT ALWAYS EXECUTES (unlike recording-verdict-on-strih.sh's Windows-only planner-by-
# default shape): ssh/scp to a plain Linux box is always allowed on this rig (#701/#462 precedent),
# so there is no "paste into the MCP Shell" fallback to preserve — this script runs the deploy +
# decode + pull-back for real every time it is invoked, exactly like recording-verdict-on-imag.sh.
#
# HOW THE PIECES FIT (per-box decode-in-place, #208):
#   1. CI builds `recording-verdict` (probe-tools-linux-amd64) — the SAME Linux binary
#      recording-e2e.sh's PROBE_BIN_DIR already holds for imag-nb (#462). strih-lx is x86_64
#      Ubuntu — the identical binary runs there unmodified.
#   2. scp that binary to strih-lx ONCE (skipped when already present + executable there).
#   3. ssh runs `recording-verdict --extract-partial strih --strih <local rec>` ON strih-lx
#      against the recording as it lives there (NEVER copied off-box).
#   4. scp pulls back ONLY the small partial JSON (+ the sibling `<partial>-pixels` dir, #186 —
#      absent on a clean run) to dev1.
#
# Usage:
#   recording-verdict-on-strih-lx.sh \
#       --strih-rec /home/newlevel/... \
#       --verdict-bin target/release/recording-verdict \
#       --out-dir /home/newlevel/verdict-out \
#       --local-out-dir /tmp/recording-e2e-12345 \
#       -- --extract-partial strih --strih /home/newlevel/... --capture-fps 30 \
#          --out /home/newlevel/verdict-out/strih-partial-12345.json
#
# Env: STRIH_LX_BOX / STRIH_USER / STRIH_PW — ssh target (default 10.77.9.202 / newlevel /
#      newlevel, the SAME creds recording-e2e.sh's own $STRIH_USER/$STRIH_PW use for strih today).
set -euo pipefail

# Pure-string function so a unit test can source the script and assert the command is well-formed
# (strih-lx-local paths only, %q-quoted — no dev1 path, no unescaped argument) WITHOUT touching
# the network. RUST_LOG=info so the decode progress is visible in the ssh output (the agent's
# liveness signal), mirroring build_onimag_command's shape.
build_onstrihlx_command() {
  local exe="$1"; shift
  printf 'env RUST_LOG=info %q' "$exe"
  local a
  for a in "$@"; do
    printf ' %q' "$a"
  done
  printf '\n'
}

# Parse flags + run the plan for real. Wrapped in a function so SOURCING the script (a unit test
# calling build_onstrihlx_command) does NOT trigger arg-parsing / ssh against the sourcing shell's
# $@.
main() {
  local STRIH_LX_BOX="${STRIH_LX_BOX:-10.77.9.202}"
  local STRIH_USER="${STRIH_USER:-newlevel}"
  local STRIH_PW="${STRIH_PW:-newlevel}"
  local VERDICT_BIN=""                                 # LOCAL (dev1) Linux binary to deploy
  local REMOTE_BIN="/home/newlevel/recording-verdict"  # where it lands / already lives on strih-lx
  local OUT_DIR="/home/newlevel/verdict-out"           # box-local (strih-lx) out dir
  local LOCAL_OUT_DIR="."                              # dev1-side dir to pull results into
  local STRIH_REC=""
  # Everything after `--` is passed verbatim to recording-verdict on strih-lx.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      # --skip-if-exists <partial-path>: if the partial JSON from a PREVIOUS run already exists on
      # dev1 (durable state, #281), skip re-decode entirely so a re-dispatched worker is idempotent.
      --skip-if-exists)
        if [ -f "$2" ]; then
          echo "SKIP: strih-lx partial already exists at $2 — skipping re-decode (#281)"
          return 0
        fi
        shift 2 ;;
      --strih-rec)     STRIH_REC="$2"; shift 2 ;;
      --verdict-bin)   VERDICT_BIN="$2"; shift 2 ;;
      --remote-bin)    REMOTE_BIN="$2"; shift 2 ;;
      --out-dir)       OUT_DIR="$2"; shift 2 ;;
      --local-out-dir) LOCAL_OUT_DIR="$2"; shift 2 ;;
      --)              shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  if [ -z "$STRIH_REC" ]; then
    echo "ERROR: --strih-rec <recording path as it lives on strih-lx> is required" >&2
    exit 2
  fi

  command -v sshpass >/dev/null 2>&1 || {
    echo "ERROR: sshpass not found — needed to ssh/scp into strih-lx (issue 1351)." >&2
    exit 1
  }

  local -a SSH_OPTS=(-o StrictHostKeyChecking=no -o ConnectTimeout=10)
  local TARGET="${STRIH_USER}@${STRIH_LX_BOX}"

  # STEP 1: deploy the Linux verdict binary to strih-lx — ONLY if missing/not-executable there.
  # (A sha256 version gate, per recording-verdict-on-imag.sh's issue-1118 fix, is a natural
  # follow-up once strih-lx carries the same schema-drift exposure imag-nb does.)
  if [ -n "$VERDICT_BIN" ]; then
    if sshpass -p "$STRIH_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "[ -x '$REMOTE_BIN' ]" 2>/dev/null; then
      echo "[recording-verdict-on-strih-lx] $REMOTE_BIN already present+executable on strih-lx — skipping upload"
    else
      echo "[recording-verdict-on-strih-lx] deploying $VERDICT_BIN -> ${TARGET}:${REMOTE_BIN}"
      sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "$VERDICT_BIN" "${TARGET}:${REMOTE_BIN}"
      sshpass -p "$STRIH_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "chmod +x '$REMOTE_BIN'"
    fi
  fi

  # STEP 2: run the verdict ON strih-lx against the LOCAL recording (NEVER copied off-box).
  local ONSTRIHLX_CMD
  ONSTRIHLX_CMD="$(build_onstrihlx_command "$REMOTE_BIN" "${PASS_ARGS[@]}")"
  echo "[recording-verdict-on-strih-lx] running on strih-lx (${STRIH_LX_BOX}): $ONSTRIHLX_CMD"
  sshpass -p "$STRIH_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "mkdir -p '$OUT_DIR' && $ONSTRIHLX_CMD"

  # #186/#208: the on-box --extract-partial writes the pixel-proof PNGs of every flagged /
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
  # of flagged-frame PNGs, absent on a clean run). The strih recording itself never leaves the box.
  mkdir -p "$LOCAL_OUT_DIR"
  local partial_base local_partial
  partial_base="$(basename "$OUT_PARTIAL")"
  local_partial="$LOCAL_OUT_DIR/$partial_base"
  echo "[recording-verdict-on-strih-lx] pulling back $OUT_PARTIAL -> $local_partial"
  sshpass -p "$STRIH_PW" scp "${SSH_OPTS[@]}" "${TARGET}:${OUT_PARTIAL}" "$local_partial"
  if sshpass -p "$STRIH_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "[ -d '$PIXELS_DIR' ]" 2>/dev/null; then
    echo "[recording-verdict-on-strih-lx] pulling back #186 pixel proofs $PIXELS_DIR -> $LOCAL_OUT_DIR/"
    sshpass -p "$STRIH_PW" scp -r "${SSH_OPTS[@]}" "${TARGET}:${PIXELS_DIR}" "$LOCAL_OUT_DIR/"
  else
    echo "[recording-verdict-on-strih-lx] no pixel-proof dir on strih-lx — clean run, nothing flagged"
  fi
  echo "[recording-verdict-on-strih-lx] done: partial at $local_partial"
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call
# build_onstrihlx_command without main parsing the sourcing shell's args or touching the network).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
