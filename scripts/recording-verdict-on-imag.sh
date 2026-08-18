#!/usr/bin/env bash
# recording-verdict-on-imag.sh — run recording-verdict ON the imag-nb box (Linux, 10.77.9.182),
# where the imag OBS-program recording already lives, to EXTRACT its small per-box PARTIAL JSON
# (EPIC #466 Topology v2, #461/#462/#463). Third leg of the #208 per-box decode-in-place model
# alongside recording-verdict-on-strih.sh / recording-verdict-on-stream.sh — dev1 holds NOTHING
# big (no multi-GB recording ever lands here, only the small partial JSON + a handful of #186
# pixel-proof PNGs).
#
# WHY THIS SCRIPT ACTUALLY EXECUTES (unlike its Windows siblings BY DEFAULT): win-strih /
# win-stream-snv are reached via the win-* MCP by default, and their two sibling scripts default
# to PURE PLANNER mode: they print the exact command for an agent/operator to paste into the MCP
# Shell — #701 proved plain scp/ssh actually reaches strih/stream too (targets.md creds), and #703
# already wired an opt-in `--execute` mode into both siblings that runs the SAME shape of flow
# this script always has (see their own headers); planner mode just stays their default for a
# manual/workflow_dispatch run. imag-nb is a plain Ubuntu box (same access class as cam1/cam2 — see
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
# onimag_decode_core_range <ncpus> — issue 1094. Pure/testable (no nproc, no network): echo the
# batch-decode CPU-pin range "LO-HI" for a box with <ncpus> online CPUs — the top min(4,ncpus)
# cores (the E-cores on Intel hybrid: 8-11 on the i5-13420H's 12 threads, and 12-15 on the retired
# 16-thread box, reproducing #767's original pin there), always OFF the lower-numbered P-cores OBS
# is pinned to. A non-numeric / absent / zero count echoes EMPTY, telling the caller to run the
# decode UNPINNED rather than emit a broken taskset (fail-open — see build_onimag_command).
onimag_decode_core_range() {
  local n="${1:-}" lo
  case "$n" in ''|*[!0-9]*) echo ""; return 0 ;; esac
  [ "$n" -ge 1 ] 2>/dev/null || { echo ""; return 0; }
  lo=$(( n > 4 ? n - 4 : 0 ))
  echo "${lo}-$(( n - 1 ))"
}

build_onimag_command() {
  local exe="$1" core_range="${2:-}"; shift 2
  # #767: the decode is a BATCH job on a box running PRODUCTION OBS — with the keep-alive build all
  # NDI receivers decode continuously, and an unthrottled decode (313% CPU + ffmpeg child) once
  # drove load to 27 → starved NDI threads → user-visible stutter on the projection while the
  # compositor still reported 60fps (live, 2026-07-15). So: nice -n 19 (batch priority) + pinned to
  # the box's E-cores, fully OFF the P-cores OBS runs on. The ffmpeg child inherits both. Decode
  # takes longer on 4 E-cores — irrelevant for a batch job.
  #
  # issue 1094: the pin range is RESOLVED FROM THE ACTUAL BOX by main() (onimag_decode_core_range
  # over the live nproc + a taskset probe), NOT hardcoded — the retired 16-thread box's literal
  # `taskset -c 12-15` silently failed EVERY extract on the 12-thread i5-13420H replacement (cores
  # 12-15 do not exist → taskset aborts before recording-verdict runs → 0/76 imag-leg verification,
  # issue 1094). FAIL-OPEN: an empty core_range emits NO taskset, so the decode still runs
  # (unpinned, nice 19 alone) instead of a broken taskset aborting the whole extract.
  local ts=""
  [ -n "$core_range" ] && ts="taskset -c ${core_range} "
  printf 'nice -n 19 %senv RUST_LOG=info %q' "$ts" "$exe"
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

  # issue 1094: resolve the batch-decode CPU pin FROM THE ACTUAL BOX. A hardware swap silently
  # broke the old hardcoded `taskset -c 12-15` on the 12-thread i5-13420H replacement (cores 12-15
  # do not exist) — every [8/8c] imag extract failed at taskset, zeroing the imag leg in 0/76 runs.
  # Ask the box its online-CPU count, derive the top-4-cores (E-core) range, and PROBE that taskset
  # accepts it. FAIL-OPEN: any hiccup (unparseable nproc / rejected probe / ssh error) leaves the
  # range EMPTY and the decode runs UNPINNED (nice 19 alone still keeps it off production OBS),
  # never aborting the extract on a pin error again.
  local CORE_RANGE NPROC_REMOTE
  NPROC_REMOTE="$(sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" 'nproc' 2>/dev/null | tr -dc '0-9' || true)"
  CORE_RANGE="$(onimag_decode_core_range "$NPROC_REMOTE")"
  if [ -n "$CORE_RANGE" ] \
     && ! sshpass -p "$IMAG_PW" ssh "${SSH_OPTS[@]}" "$TARGET" "taskset -c '$CORE_RANGE' true" 2>/dev/null; then
    echo "[recording-verdict-on-imag] WARNING: taskset -c $CORE_RANGE rejected on $IMAG_BOX (nproc=${NPROC_REMOTE:-?}) — decoding UNPINNED (nice 19 only)." >&2
    CORE_RANGE=""
  fi
  if [ -n "$CORE_RANGE" ]; then
    echo "[recording-verdict-on-imag] decode pin: taskset -c $CORE_RANGE (nproc=$NPROC_REMOTE, E-cores off the OBS P-cores)"
  else
    echo "[recording-verdict-on-imag] decode pin: none (unpinned, nice 19 only) -- nproc=${NPROC_REMOTE:-?}"
  fi

  # STEP 2: run the verdict ON imag against the LOCAL recording (NEVER copied off-box).
  local ONIMAG_CMD
  ONIMAG_CMD="$(build_onimag_command "$REMOTE_BIN" "$CORE_RANGE" "${PASS_ARGS[@]}")"
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
