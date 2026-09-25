#!/usr/bin/env bash
# recording-verdict-on-resolume.sh — issue 1302: decode the cg OBS recording IN PLACE on RESOLUME-SNV
# and bring back ONLY the small partial JSON (+ its pixel proofs). See the extended header below.
set -euo pipefail

# WHY (issue 1302): the CG_CHAIN=1 E2E profile used to scp the cg OBS recording to dev1 and decode it
# there inside the [8/8d] merge. dev1 is a small Tier-0 box: the release E2E of 25.9.2026 (run
# 36115692830) spent 29 min in that one decode and hit the 75-min job timeout, so CG_CHAIN was set
# back to 0. This script moves the decode to the box that already holds the recording, exactly the
# way recording-verdict-on-stream.sh --execute does it for the stream box:
#   STEP 0  preflight: ffmpeg + ffprobe must be reachable (fail loud BY NAME, never a decode that
#           dies half way with an opaque error). RESOLUME-SNV keeps ffmpeg under C:\ffmpeg\<build>\bin
#           but not on PATH, so every session first prepends the first ffmpeg.exe found under
#           RESOLUME_FFMPEG_ROOT (default C:\ffmpeg; empty = rely on PATH). Then any decode of the
#           same exe still running from an earlier run is stopped, and a stale partial of the same
#           name is deleted.
#   STEP 1  deploy recording-verdict.exe behind the issue-1118 sha256 VERSION GATE
#           (scripts/lib/verdict-upload-gate.sh, shared with the imag + strih-lx extracts): an
#           identical on-box binary is reused, a stale or absent one is replaced.
#   STEP 2  run `recording-verdict.exe --extract-partial cg --cg <recording> --out <partial>` ON the
#           box, at the issue-1260 BelowNormal PriorityClass (build_onbox_command, reused from
#           recording-verdict-on-stream.sh) so it never starves the live obs64 / Arena processes.
#   STEP 3  pull back the partial JSON and, when present, its `<partial>-pixels` dir.
#
# Transport: plain session-agnostic OpenSSH (win-ssh-exec.sh). Nothing here needs the interactive
# desktop session, so ssh is the right channel (.claude/rules/win-ssh-vs-mcp.md): a file copy, a
# headless CLI decode and a file download. This script always EXECUTES (no plan-print mode): the
# harness only calls it in the executing gate (E2E_EXECUTE_VERDICT=1).
#
# Usage:
#   RESOLUME_BOX=<ip> RESOLUME_USER=<u> RESOLUME_PW=<pw> recording-verdict-on-resolume.sh \
#       --verdict-exe-local <dev1 path to the CI-built recording-verdict.exe> \
#       --local-out-dir <dev1 dir to pull results into> \
#       [--verdict-exe 'C:\camera-box\recording-verdict.exe'] [--out-dir 'C:\camera-box\verdict-out'] \
#       [--force-upload] \
#       -- --extract-partial cg --cg '<recording on the box>' --out '<box partial path>'
#   RESOLUME_BOX=… RESOLUME_USER=… RESOLUME_PW=… recording-verdict-on-resolume.sh --stop-decode \
#       [--verdict-exe '<exe>']   # stop a decode of that exe still running on the box (best-effort)
#
# Env: RESOLUME_BOX (default resolume.lan), RESOLUME_USER / RESOLUME_PW (REQUIRED — the harness
# passes the cg-chain credentials, scripts/lib/cg-chain-e2e.sh cg_chain_user / cg_chain_pw),
# RESOLUME_FFMPEG_ROOT (default C:\ffmpeg).

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/win-ssh-exec.sh
. "$HERE/lib/win-ssh-exec.sh"
# shellcheck source=scripts/lib/verdict-upload-gate.sh
. "$HERE/lib/verdict-upload-gate.sh"
# build_onbox_command (the issue-1260 PriorityClass + PowerShell quoting) is reused verbatim from the
# stream extract, never re-implemented; sourcing it does not run its main().
# shellcheck source=scripts/recording-verdict-on-stream.sh
. "$HERE/recording-verdict-on-stream.sh"

# onresolume_ps_quote <text> — a PowerShell double-quoted literal of <text> (an embedded " is
# doubled). Pure.
onresolume_ps_quote() {
  printf '"%s"' "${1//\"/\"\"}"
}

# PowerShell exit-code rule every builder below respects: `powershell -EncodedCommand` exits 1 when
# the LAST statement failed — and `-ErrorAction SilentlyContinue` only hides the message, `$?` is
# still false. So a statement whose failure is expected (nothing to stop, nothing to delete) is
# either guarded (`if (Test-Path …)`) or never the last one.

# onresolume_ffmpeg_path_ps <root> — the PowerShell prefix that puts the first ffmpeg.exe found under
# <root> at the front of PATH for this session. RESOLUME-SNV has ffmpeg under C:\ffmpeg\<build>\bin
# but NOT on PATH (read 25.9.2026), and recording-verdict.exe shells out to ffmpeg/ffprobe. The
# build dir is found, never hard-coded, so an ffmpeg upgrade on the box needs no change here. Empty
# <root> = no prefix. Ends with "; " so it prefixes the next statement. Pure.
onresolume_ffmpeg_path_ps() {
  [ -n "${1:-}" ] || return 0
  # shellcheck disable=SC2016  # PowerShell $f / $env:Path must NOT expand in bash
  printf '$f = Get-ChildItem -LiteralPath %s -Filter ffmpeg.exe -Recurse -ErrorAction SilentlyContinue | Select-Object -First 1; if ($f) { $env:Path = $f.DirectoryName + ";" + $env:Path }; ' \
    "$(onresolume_ps_quote "$1")"
}

# onresolume_tool_preflight_ps — the PowerShell text that exits 3 and names every missing decode
# tool (ffmpeg / ffprobe) the on-box recording-verdict.exe shells out to. Pure.
onresolume_tool_preflight_ps() {
  # shellcheck disable=SC2016  # single-quoted PowerShell; $m / $_ must NOT expand in bash
  printf '%s' '$m = @("ffmpeg","ffprobe") | Where-Object { -not (Get-Command $_ -ErrorAction SilentlyContinue) }; if ($m) { Write-Output ("MISSING-TOOL: " + ($m -join ",")); exit 3 }'
}

# onresolume_stop_decode_ps <exe> — the PowerShell statement that stops any recording-verdict process
# running from exactly <exe> (an earlier run's decode left behind by a grace overrun or an aborted
# harness). Only that one binary path is matched — never OBS, Arena or anything else on the box. It
# also frees the exe for the STEP 1 re-upload. Ends with "; ". Pure.
onresolume_stop_decode_ps() {
  local exe="$1" name
  name="$(win_ssh_basename "$exe")"
  name="${name%.exe}"
  # shellcheck disable=SC2016  # PowerShell $_ must NOT expand in bash
  printf 'Get-Process -Name %s -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq %s } | Stop-Process -Force; ' \
    "$(onresolume_ps_quote "$name")" "$(onresolume_ps_quote "$exe")"
}

# onresolume_prepare_ps <out-dir> <partial> <exe> — the PowerShell text that stops a leftover decode
# of <exe>, creates the box-local output dir and deletes any earlier partial + `<partial>-pixels` dir
# of the same name, so a failed decode can never leave an OLD partial for STEP 3 to pull back. The
# deletes are Test-Path-guarded (nothing to delete = success) and a delete that FAILS is the last
# statement, so it fails the step loudly. Pure.
onresolume_prepare_ps() {
  local out_dir="$1" partial="$2" exe="$3"
  onresolume_stop_decode_ps "$exe"
  # shellcheck disable=SC2016  # PowerShell $p must NOT expand in bash
  printf 'New-Item -ItemType Directory -Force -Path %s | Out-Null; foreach ($p in @(%s, %s)) { if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force } }' \
    "$(onresolume_ps_quote "$out_dir")" "$(onresolume_ps_quote "$partial")" \
    "$(onresolume_ps_quote "${partial%.json}-pixels")"
}

# onresolume_sha_probe_ps <exe> — the PowerShell text that prints the lower-case sha256 of <exe>,
# or nothing when it is absent (the sha256 gate's `present` input). Pure.
onresolume_sha_probe_ps() {
  local q
  q="$(onresolume_ps_quote "$1")"
  printf 'if (Test-Path -LiteralPath %s -PathType Leaf) { (Get-FileHash -Algorithm SHA256 -LiteralPath %s).Hash.ToLower() }' "$q" "$q"
}

main() {
  local RESOLUME_BOX="${RESOLUME_BOX:-resolume.lan}"
  local RESOLUME_USER="${RESOLUME_USER:-}"
  local RESOLUME_PW="${RESOLUME_PW:-}"
  local VERDICT_EXE='C:\camera-box\recording-verdict.exe'
  local OUT_DIR='C:\camera-box\verdict-out'
  local FFMPEG_ROOT="${RESOLUME_FFMPEG_ROOT-C:\\ffmpeg}"
  local VERDICT_EXE_LOCAL="" LOCAL_OUT_DIR="" FORCE_UPLOAD=0 STOP_DECODE=0
  local -a PASS_ARGS=()
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --verdict-exe)        VERDICT_EXE="$2"; shift 2 ;;
      --out-dir)            OUT_DIR="$2"; shift 2 ;;
      --verdict-exe-local)  VERDICT_EXE_LOCAL="$2"; shift 2 ;;
      --local-out-dir)      LOCAL_OUT_DIR="$2"; shift 2 ;;
      --force-upload)       FORCE_UPLOAD=1; shift 1 ;;
      --stop-decode)        STOP_DECODE=1; shift 1 ;;
      --)                   shift; PASS_ARGS=("$@"); break ;;
      *) echo "unknown arg: $1" >&2; exit 2 ;;
    esac
  done

  # --stop-decode: only stop a decode of VERDICT_EXE still running on the box (the harness calls this
  # when it gives up on the cg leg, so the decode never keeps running next to the live Arena/cg OBS).
  # Best-effort: nothing running is success, an unreachable box is reported, never fatal.
  if [ "$STOP_DECODE" = 1 ]; then
    if [ -z "$RESOLUME_USER" ] || [ -z "$RESOLUME_PW" ]; then
      echo "ERROR: RESOLUME_USER / RESOLUME_PW are not set — the caller passes the cg-chain credentials" >&2
      exit 2
    fi
    if win_ssh_run "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$(onresolume_stop_decode_ps "$VERDICT_EXE")exit 0"; then
      echo "[recording-verdict-on-resolume] stopped any decode of $VERDICT_EXE on ${RESOLUME_BOX}"
    else
      echo "[recording-verdict-on-resolume] WARNING: could not reach ${RESOLUME_BOX} to stop the decode of $VERDICT_EXE" >&2
    fi
    return 0
  fi

  local OUT_PARTIAL="" i
  for ((i = 0; i + 1 < ${#PASS_ARGS[@]}; i++)); do
    if [ "${PASS_ARGS[$i]}" = "--out" ]; then
      OUT_PARTIAL="${PASS_ARGS[$((i + 1))]}"
      break
    fi
  done
  if [ -z "$OUT_PARTIAL" ]; then
    echo "ERROR: needs a --out <partial.json> inside the forwarded args" >&2
    exit 2
  fi
  if [ -z "$LOCAL_OUT_DIR" ]; then
    echo "ERROR: needs --local-out-dir <dev1 dir to pull results into>" >&2
    exit 2
  fi
  if [ -z "$VERDICT_EXE_LOCAL" ] || [ ! -f "$VERDICT_EXE_LOCAL" ]; then
    echo "ERROR: needs --verdict-exe-local <the CI-built recording-verdict.exe on dev1> (got '$VERDICT_EXE_LOCAL')" >&2
    exit 2
  fi
  if [ -z "$RESOLUME_USER" ] || [ -z "$RESOLUME_PW" ]; then
    echo "ERROR: RESOLUME_USER / RESOLUME_PW are not set — the caller passes the cg-chain credentials" >&2
    exit 2
  fi
  command -v sshpass >/dev/null 2>&1 || {
    echo "ERROR: sshpass not found — needed to ssh/scp into RESOLUME-SNV." >&2
    exit 1
  }
  local PIXELS_DIR="${OUT_PARTIAL%.json}-pixels"

  local FFMPEG_PS
  FFMPEG_PS="$(onresolume_ffmpeg_path_ps "$FFMPEG_ROOT")"
  echo "[recording-verdict-on-resolume] STEP 0: decode-tool preflight on ${RESOLUME_BOX} (ffmpeg searched under '${FFMPEG_ROOT}')"
  win_ssh_run "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "${FFMPEG_PS}$(onresolume_tool_preflight_ps)"
  win_ssh_run "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$(onresolume_prepare_ps "$OUT_DIR" "$OUT_PARTIAL" "$VERDICT_EXE")"

  local local_sha remote_sha present=0 decision
  local_sha="$(sha256sum "$VERDICT_EXE_LOCAL" 2>/dev/null | cut -d' ' -f1)" || local_sha=""
  remote_sha="$(win_ssh_run "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" \
    "$(onresolume_sha_probe_ps "$VERDICT_EXE")" 2>/dev/null | tr -dc '0-9a-f')" || remote_sha=""
  [ -n "$remote_sha" ] && present=1
  decision="$(verdict_upload_decision "$FORCE_UPLOAD" "$present" "$local_sha" "$remote_sha")"
  if [ "$decision" = "upload" ]; then
    echo "[recording-verdict-on-resolume] STEP 1: deploying $VERDICT_EXE_LOCAL -> ${RESOLUME_BOX}:${VERDICT_EXE} (issue 1118 version gate: absent, stale or forced)"
    win_ssh_upload "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$VERDICT_EXE_LOCAL" "$VERDICT_EXE"
  else
    echo "[recording-verdict-on-resolume] STEP 1: $VERDICT_EXE already on ${RESOLUME_BOX} with the identical sha256 — upload skipped (issue 1118 version gate)"
  fi

  local ONBOX_CMD
  # The decode runs in its own ssh session, so ffmpeg goes on PATH again for it.
  ONBOX_CMD="${FFMPEG_PS}$(build_onbox_command "$VERDICT_EXE" "${PASS_ARGS[@]}")"
  echo "[recording-verdict-on-resolume] STEP 2: decoding ON ${RESOLUME_BOX}: $ONBOX_CMD"
  win_ssh_run "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$ONBOX_CMD"

  mkdir -p "$LOCAL_OUT_DIR"
  local local_partial
  local_partial="$LOCAL_OUT_DIR/$(win_ssh_basename "$OUT_PARTIAL")"
  echo "[recording-verdict-on-resolume] STEP 3: pulling back $OUT_PARTIAL -> $local_partial"
  win_ssh_download "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$OUT_PARTIAL" "$local_partial"
  if win_ssh_path_exists "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$PIXELS_DIR"; then
    echo "[recording-verdict-on-resolume] pulling back the pixel proofs $PIXELS_DIR -> $LOCAL_OUT_DIR/"
    win_ssh_download_dir "$RESOLUME_USER" "$RESOLUME_PW" "$RESOLUME_BOX" "$PIXELS_DIR" "$LOCAL_OUT_DIR/"
  else
    echo "[recording-verdict-on-resolume] no pixel-proof dir on the box — nothing was flagged"
  fi
  echo "[recording-verdict-on-resolume] done: partial at $local_partial (the cg recording stayed on RESOLUME-SNV)"
}

# Run main only when EXECUTED, never when SOURCED (a test sources the pure builders).
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  main "$@"
fi
