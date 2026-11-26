#!/usr/bin/env bash
# recording-verdict-on-strih-snv.sh — the [8/8a] strih (and, through recording-verdict-on-stream-snv.sh,
# the [8/8b] stream) recording decode for a Linux OBS box: copy the recording to dev1, decode there.
#
# issue 1425 (owner ROZHODNUTÉ issuecomment-7187562277, 26.11.2026): the full-path E2E's recording
# decode + recording-verdict run on dev1, NEVER on a rig box. This script used to run recording-verdict
# ON the box (issue 1351 strih-snv, issue 1462 stream-snv) — a multi-core QR/pixel sweep beside the
# live OBS. Now:
#   1. the box's recording is copied to a run-private dev1 dir under --local-out-dir, PACED
#      (scp -l, REC_PULL_RATE_MBIT default 200 Mbit/s) and verified: size + sha256 against the box
#      (scripts/lib/rec-pull-paced.sh; the box's sha256sum runs at its idle-priority prefix,
#      LOWPRIO_SNIPPET). The box runs no ffmpeg and no recording-verdict;
#   2. the forwarded recording-verdict arguments (everything after `--`, unchanged at the callers)
#      run LOCALLY with the dev1 binary --verdict-bin, their box paths moved to dev1: the recording ->
#      the copy, `--out <box dir>/<name>` -> <local-out-dir>/<name> (the partial and its #186
#      `<partial>-pixels` dir land where the callers read them, byte-compatible), any other path
#      under --out-dir (the pushed marker log / switch schedule) -> fetched back once;
#   3. the dev1 copy is removed on every exit (an EXIT trap), the verdict's own failure included.
# Bytes, seconds, rate, the sha match and the decode time are logged.
#
# Usage (unchanged at every caller; --remote-bin is accepted and ignored):
#   recording-verdict-on-strih-snv.sh --strih-rec '/srv/_REC/2026-11-26 09-24-08.mkv' \
#       --verdict-bin <dev1 recording-verdict> --out-dir /home/newlevel/verdict-out \
#       --local-out-dir <run dir> \
#       -- --extract-partial strih --strih '/srv/_REC/2026-11-26 09-24-08.mkv' --capture-fps 30 \
#          --out /home/newlevel/verdict-out/strih-partial-12345.json
#
# Env: STRIH_LX_BOX / STRIH_USER / STRIH_PW — the box (default 10.77.9.202 / newlevel / newlevel);
#      ONBOX_LABEL (the log tag, default strih-snv); REC_PULL_RATE_MBIT / REC_PULL_TIMEOUT.
set -euo pipefail

# shellcheck source=scripts/lib/rec-pull-paced.sh
. "$(dirname "${BASH_SOURCE[0]}")/lib/rec-pull-paced.sh"

# Pure-string function so a unit test can source the script and assert the command is well-formed
# (%q-quoted, no unescaped argument) WITHOUT touching the network. RUST_LOG=info so the decode
# progress is visible in the output (the agent's liveness signal). Since issue 1425 it is the dev1
# command line printed before the local decode.
build_onstrihlx_command() {
  local exe="$1"; shift
  printf 'env RUST_LOG=info %q' "$exe"
  local a
  for a in "$@"; do
    printf ' %q' "$a"
  done
  printf '\n'
}

# strih_lx_lowprio_prefix <sysfs_root> — issue 1351. The canonical (pure, network-free) encoding of
# the low-priority launch prefix the box uses for its side of the recording copy (issue 1425: the
# sha256 read; the decode itself runs on dev1 since then): always `nice -n 19`, plus `taskset -c <range>` onto the Intel hybrid E-cores
# when this box exposes <sysfs_root>/devices/cpu_atom/cpus as a non-empty range. A multi-core
# QR/pixel sweep over a 1080p60 5-min recording at nice 0 on the P-cores starves OBS's live NDI
# receiver decode threads (the post-run relock storms of issue 1354); the E-cores + idle priority
# keep it off the P-cores OBS runs on. A non-hybrid box (no cpu_atom) just runs deprioritised.
#   <root>/devices/cpu_atom/cpus present + non-empty range -> `nice -n 19 taskset -c <range>`
#   absent / empty / whitespace / unreadable               -> `nice -n 19`  (never `taskset -c ""`)
# Fixture-driven so a unit test can point <sysfs_root> at a fake tree. STEP 2 in main() is the
# remote-shell REPLICA of this same contract (the range is read on the BOX so a replacement
# notebook resolves its own cores); this pure function is what
# tests/recording_verdict_on_strih_lx_lowprio.rs pins.
# issue 1387: when the box carries the baseline's background runner the decode runs through it instead:
# rig-background.slice on the CPUs OBS does not use (AMD has no cpu_atom, so the E-core pin fenced
# nothing there), idle priority included. <runner> is the second argument (test seam).
# issue 1425 (finding 7178516822): on a box with housekeeping CPUs that slice is the ONE housekeeping
# task CPU, which also carries rig-NIC IRQs (design 7119179275): the decode got one CPU (~55 min) beside the
# RX softirq. There (`/etc/strih-housekeeping-cpus.conf` non-empty, written by setup-strih only when the box
# fact is not `none`) it runs on the OBS mask `/etc/strih-isolated-cpus.conf` instead (the placement the
# program-audio sampler took, strih_program_audio_cpus) under SCHED_IDLE + nice 19 + ionice idle (new here),
# never in the slice. A mask that is not a cpulist (N or N-M, comma separated, ASCII digits) keeps the choice
# below. <etc_root> is the third argument (test seam, default /).
strih_lx_lowprio_prefix() {
  local sysfs_root="${1:-/sys}" runner="${2:-/usr/local/sbin/obs-box-background-run}" etc_root="${3:-}"
  local cpus_file="${sysfs_root%/}/devices/cpu_atom/cpus"
  local cpus="" mask=""
  if [ -s "${etc_root%/}/etc/strih-housekeeping-cpus.conf" ]; then
    mask="$(head -n 1 "${etc_root%/}/etc/strih-isolated-cpus.conf" 2>/dev/null | tr -d '[:space:]' || true)"
    if printf '%s' "$mask" | grep -Eqx '[0123456789]+(-[0123456789]+)?(,[0123456789]+(-[0123456789]+)?)*'; then
      printf 'nice -n 19 ionice -c 3 chrt --idle 0 taskset -c %s' "$mask"
      return 0
    fi
  fi
  if [ -x "$runner" ] && sudo -n -l "$runner" >/dev/null 2>&1; then
    printf 'sudo -n %s' "$runner"
    return 0
  fi
  if [ -r "$cpus_file" ]; then
    cpus="$(tr -d '[:space:]' < "$cpus_file" 2>/dev/null || true)"
  fi
  if [ -n "$cpus" ]; then
    printf 'nice -n 19 taskset -c %s' "$cpus"
  else
    printf 'nice -n 19'
  fi
}

# Parse flags + run the decode for real. Wrapped in a function so SOURCING the script (a unit test
# calling build_onstrihlx_command / strih_lx_lowprio_prefix) does NOT trigger arg-parsing / ssh
# against the sourcing shell's $@.
main() {
  local STRIH_LX_BOX="${STRIH_LX_BOX:-10.77.9.202}"
  local STRIH_USER="${STRIH_USER:-newlevel}"
  local STRIH_PW="${STRIH_PW:-newlevel}"
  local VERDICT_BIN=""                                 # the dev1 recording-verdict the E2E downloaded
  local OUT_DIR="/home/newlevel/verdict-out"           # the box dir the callers name in --out
  local LOCAL_OUT_DIR="."                              # dev1-side dir the partial is written into
  local STRIH_REC=""
  local LABEL="${ONBOX_LABEL:-strih-snv}"                # issue 1462: the box name in the log lines
  # Everything after `--` is passed to recording-verdict (on dev1), its box paths rewritten.
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      # --skip-if-exists <partial-path>: if the partial JSON from a PREVIOUS run already exists on
      # dev1 (durable state, #281), skip re-decode entirely so a re-dispatched worker is idempotent.
      --skip-if-exists)
        if [ -f "$2" ]; then
          echo "SKIP: ${LABEL} partial already exists at $2 — skipping re-decode (#281)"
          return 0
        fi
        shift 2 ;;
      --strih-rec)     STRIH_REC="$2"; shift 2 ;;
      --rec)           STRIH_REC="$2"; shift 2 ;;   # issue 1462: the box-neutral spelling
      --verdict-bin)   VERDICT_BIN="$2"; shift 2 ;;
      --remote-bin)    shift 2 ;;                   # issue 1425: no binary goes to the box any more
      --out-dir)       OUT_DIR="$2"; shift 2 ;;
      --local-out-dir) LOCAL_OUT_DIR="$2"; shift 2 ;;
      --)              shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  if [ -z "$STRIH_REC" ]; then
    echo "ERROR: --strih-rec / --rec <recording path as it lives on ${LABEL}> is required" >&2
    exit 2
  fi
  if [ -z "$VERDICT_BIN" ] || [ ! -x "$VERDICT_BIN" ]; then
    echo "ERROR: --verdict-bin <dev1 recording-verdict> is required and must be executable (got '${VERDICT_BIN}'); the decode runs on dev1 (issue 1425)" >&2
    exit 2
  fi
  command -v sshpass >/dev/null 2>&1 || {
    echo "ERROR: sshpass not found — needed to copy the recording from ${LABEL} (issue 1351)." >&2
    exit 1
  }

  local -a SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=10)
  local TARGET="${STRIH_USER}@${STRIH_LX_BOX}"
  mkdir -p "$LOCAL_OUT_DIR"

  # STEP 1 (issue 1425): copy the recording to a run-private dev1 dir, PACED + sha256-verified
  # (scripts/lib/rec-pull-paced.sh). The copy is removed on EVERY exit, the verdict's own failure
  # included: dev1 keeps no recording. The box's sha256sum runs behind LOWPRIO_SNIPPET, the box's own
  # idle-priority prefix (the remote-shell replica of strih_lx_lowprio_prefix, single-quoted so the
  # box resolves its own placement); the box runs no decode.
  local LOWPRIO_SNIPPET COPY_DIR LOCAL_REC
  LOWPRIO_SNIPPET='LP="nice -n 19"; if [ -s /sys/devices/cpu_atom/cpus ]; then LP="$LP taskset -c $(cat /sys/devices/cpu_atom/cpus)"; fi; R=/usr/local/sbin/obs-box-background-run; if [ -x "$R" ] && sudo -n -l "$R" >/dev/null 2>&1; then LP="sudo -n $R"; fi; M="$(head -n 1 /etc/strih-isolated-cpus.conf 2>/dev/null | tr -d "[:space:]")"; if [ -s /etc/strih-housekeeping-cpus.conf ] && printf %s "$M" | grep -Eqx "[0123456789]+(-[0123456789]+)?(,[0123456789]+(-[0123456789]+)?)*"; then LP="nice -n 19 ionice -c 3 chrt --idle 0 taskset -c $M"; fi;'
  COPY_DIR="$(mktemp -d "${LOCAL_OUT_DIR%/}/.rec-copy-${LABEL}.XXXXXX")"
  # shellcheck disable=SC2064  # the dir is fixed now, on purpose
  trap "rm -rf -- '$COPY_DIR'" EXIT
  LOCAL_REC="$COPY_DIR/$(basename -- "$STRIH_REC")"
  rec_pull_paced "$STRIH_USER" "$STRIH_PW" "$STRIH_LX_BOX" "$STRIH_REC" "$LOCAL_REC" "$LABEL" "$LOWPRIO_SNIPPET" || exit 1

  # STEP 2: the forwarded recording-verdict args with every box path moved to dev1: the recording
  # -> the verified copy; `--out <box dir>/<name>` -> <local-out-dir>/<name> (so the partial and its
  # #186 `<partial>-pixels` dir land exactly where the callers read them); any other path under the
  # box out dir (the [8/8b-pre] marker log / switch schedule) -> fetched back once into the copy dir.
  local -a LOCAL_ARGS=()
  local i a prev="" local_partial=""
  for ((i = 0; i < ${#PASS_ARGS[@]}; i++)); do
    a="${PASS_ARGS[$i]}"
    if [ "$prev" = "--out" ]; then
      local_partial="${LOCAL_OUT_DIR%/}/$(basename -- "$a")"
      a="$local_partial"
    elif [ "$a" = "$STRIH_REC" ]; then
      a="$LOCAL_REC"
    elif [ "${a#"${OUT_DIR%/}"/}" != "$a" ]; then
      local fetched
      fetched="$COPY_DIR/inputs/$(basename -- "$a")"
      mkdir -p "$COPY_DIR/inputs"
      if ! sshpass -p "$STRIH_PW" scp -q "${SSH_OPTS[@]}" "${TARGET}:${a}" "$fetched" 2>/dev/null; then
        echo "ERROR: [recording-verdict-on-${LABEL}] could not fetch the decode input ${a} from ${LABEL}" >&2
        exit 1
      fi
      echo "[recording-verdict-on-${LABEL}] decode input ${a} -> ${fetched}"
      a="$fetched"
    fi
    LOCAL_ARGS+=("$a")
    prev="${PASS_ARGS[$i]}"
  done
  if [ -z "$local_partial" ]; then
    echo "WARNING: no --out <partial.json> found in the forwarded args — the decode writes no partial." >&2
  fi

  # STEP 3: the SAME recording-verdict run, on dev1, at nice 19 (a dev1 run dir, never a rig box).
  local CMD t0 t1
  CMD="$(build_onstrihlx_command "$VERDICT_BIN" "${LOCAL_ARGS[@]}")"
  echo "[recording-verdict-on-${LABEL}] decoding on dev1 (issue 1425, no decode on ${LABEL}): nice -n 19 $CMD"
  t0="$(date +%s)"
  env RUST_LOG=info nice -n 19 "$VERDICT_BIN" "${LOCAL_ARGS[@]}"
  t1="$(date +%s)"
  echo "[recording-verdict-on-${LABEL}] decode time on dev1: $((t1 - t0)) s"
  echo "[recording-verdict-on-${LABEL}] done: partial at ${local_partial:-<none>} (the dev1 recording copy is removed on exit)"
}

# Run main only when EXECUTED, not when SOURCED (so a test can source + call
# build_onstrihlx_command without main parsing the sourcing shell's args or touching the network).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
