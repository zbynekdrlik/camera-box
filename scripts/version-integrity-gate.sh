#!/usr/bin/env bash
#
# version-integrity-gate.sh — the pre-rig-test VERSION-INTEGRITY precondition gate (#123, EPIC #125).
#
# WHY THIS GATE EXISTS (the user's hard requirement, "we can't dev/test on randomly-deployed
# versions"): every rig test (recording-e2e, loopback, the obs phase scripts) measures the
# behaviour of the LIVE strih+stream OBS stack. Those results are ONLY trustworthy when the live
# stack matches the pinned zero-loss SHA set (the versions + critical settings in vendor/README.md
# AND, when a bundle manifest is supplied, the per-component/whole-bundle BUILD SHAs). A test run on
# a drifted / randomly-deployed / STOCK-OBS build is worthless and actively misleading (that is #119:
# a wrong-bytes-right-version build silently shipped and every "it works" claim was false). So this
# gate runs FIRST — ALONGSIDE the DanteSync NTP+PTP gate (#7) — and REFUSES (exits non-zero) on
# DRIFT or UNKNOWN, so the rig is never brought up and no result is trusted on an unverified stack.
#
# It REUSES the unit-tested deterministic engine scripts/drift-guard.sh --compare (tested in
# tests/drift_guard.rs) — it does NOT reinvent any comparison. This script is the FLOW that gathers
# each box's observed stack state and runs the engine per box, then rolls the verdicts up.
#
# BOX ACCESS (this rig): the Windows OBS boxes (strih 10.77.9.202, stream 10.77.9.204) DENY ssh/scp,
# so this script cannot read their live OBS state itself. Mirroring dantesync-gate.sh's --win-status:
# the caller (the autopilot worker / operator, who HAS the win-* MCP) gathers each box's observed
# drift-guard values (the SAME read-only PowerShell reads /drift-guard step 1 does) into a flat JSON
# state file and passes it via --win-state NAME=FILE. recording-e2e.sh (and the other rig-test entry
# scripts) try to FETCH each box's state JSON over the box's standing http.server first
# (fetch_box_state, mirroring fetch_dante_status), falling back to the caller-pre-fetched file. A box
# with NO state file is UNKNOWN -> the gate REFUSES (never a silent pass with the box unverified).
# (dantesync-gate.sh's OWN DanteSync gate no longer uses this file-relay pattern for strih/stream
# — it queries them LIVE over HTTP via --win-http, #648 — but this version-integrity gate still
# does; #123/#119 is unrelated, separate scope.)
#
# State file = a flat JSON object of the drift-guard --compare observed keys for that box, e.g.
#   { "obs_version":"32.1.2", "distroav_version":"6.2.1", "ndi_runtime":"6.3.2.0",
#     "output_fps":"30", "genlock_wall_clock":"1",
#     "ndi_input_latency":"NDI cam5=0,NDI cam1=0,NDI cam3=0",
#     "distroav_dll_paths":"C:\\ProgramData\\obs-studio\\plugins\\distroav\\bin\\64bit\\distroav.dll",
#     "genlock_capability":"…the live genlock marker text…",
#     "manifest":"./gbundle/BUNDLE_MANIFEST.json", "obs_dll_sha256":"…", "distroav_dll_sha256":"…",
#     "bundle_hashes":"relpath=sha,…" }
# Every key is OPTIONAL; any drift-guard key you omit the engine reports UNKNOWN (so the gate refuses)
# — exactly the never-false-clean discipline drift-guard already enforces.
#
# Usage:
#   version-integrity-gate.sh [--readme PATH] [--manifest PATH] \
#       --win-state strih=/tmp/strih-state.json [--win-state stream=/tmp/stream-state.json] ...
#   version-integrity-gate.sh --help
#
# Exit codes: 0 = every box matches the pinned set (rig test may proceed),
#   20 = at least one box DRIFTED (a setting/version/SHA differs — run REFUSED),
#   11 = at least one box UNKNOWN (state unread / a value the engine could not read — incomplete,
#        NOT clean),
#   1  = usage / environment error.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIFT_GUARD="$HERE/drift-guard.sh"
DEFAULT_README="vendor/README.md"

# --- PURE function (no network, no MCP — unit-tested) ---------------------------------------

# compare_args_from_state FILE -> one `key=val` line per drift-guard --compare observed key found in
# the flat JSON state object FILE. Pure text parse (no jq — drift-guard itself parses without jq, and
# the windows-2022 git-bash runner has no jq): the state is a flat `{"key":"val", …}` object on one
# or more lines; each "key":"value" pair becomes `key=value`. JSON string escapes \\ -> \ and \" -> "
# are unescaped so the Windows backslash path and any quote survive. Values may contain spaces, '='
# and ',' (ndi_input_latency = `NDI cam5=0,NDI cam1=0`), so the key/value split is on the JSON
# structure (`"key": "value"`), never on those characters. Only the keys drift-guard accepts are
# emitted; an unknown key in the state is skipped (the engine would WARN-ignore it anyway).
compare_args_from_state() {
  local file="$1"
  [ -f "$file" ] || { echo "compare_args_from_state: no such file: $file" >&2; return 1; }
  # grep every "key": "value" pair (value may contain escaped \" — match up to an UNescaped closing
  # quote: a run of (non-quote | backslash-quote) chars). One pair per output line via grep -o.
  # The known drift-guard --compare observed keys (host is added by the gate, not from the state):
  local keys='obs_version|distroav_version|ndi_runtime|output_fps|genlock_wall_clock|ndi_input_latency|distroav_dll_paths|manifest|obs_dll_sha256|distroav_dll_sha256|genlock_capability|bundle_hashes'
  # Match "<key>" : "<value>" where <value> is any run of (escaped char | non-backslash-non-quote).
  grep -oE "\"(${keys})\"[[:space:]]*:[[:space:]]*\"(\\\\.|[^\"\\\\])*\"" "$file" 2>/dev/null \
  | while IFS= read -r pair; do
      [ -z "$pair" ] && continue
      # key = the first quoted token; val = the second quoted token's contents.
      local key val
      key="$(printf '%s' "$pair" | sed -E 's/^"([^"]*)".*/\1/')"
      # Strip everything up to and including the colon + opening quote, and the trailing quote.
      val="$(printf '%s' "$pair" | sed -E 's/^"[^"]*"[[:space:]]*:[[:space:]]*"(.*)"$/\1/')"
      # Unescape JSON string escapes that matter for these values: \\ -> \ and \" -> ".
      val="${val//\\\\/\\}"
      val="${val//\\\"/\"}"
      printf '%s=%s\n' "$key" "$val"
    done
}

# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ------------------------------------------------

usage() {
  cat <<EOF
version-integrity-gate.sh — pre-rig-test VERSION-INTEGRITY gate (#123, EPIC #125).

REFUSES to let a rig test run unless the LIVE strih+stream stack matches the pinned zero-loss SHA
set (vendor/README.md versions + settings, and the bundle BUILD SHAs when a manifest is supplied).
A test run on a randomly-deployed / drifted / stock-OBS build is worthless (#119) — so this gate
runs FIRST (alongside the DanteSync gate #7) and FAILS FAST on drift or an unverified box.

The Windows boxes deny ssh; the caller (win-* MCP holder) pre-fetches each box's observed stack
state into a flat JSON file (the drift-guard --compare observed keys) and passes it via --win-state.

Usage:
  version-integrity-gate.sh [--readme PATH] [--manifest PATH] --win-state NAME=FILE [...]

Options:
  --readme PATH     pinned-set source (default ${DEFAULT_README}); threaded to drift-guard --compare.
  --manifest PATH   the build-under-test BUNDLE_MANIFEST.json — when set, applied to every box that
                    does not already carry a manifest= in its state (activates the BUILD-SHA facet).
  --win-state N=FILE  a box N whose observed drift-guard stack state JSON the caller wrote to FILE
                    (ssh denied; the win-* MCP holder pre-fetches it). Repeatable. A box with no
                    file is UNKNOWN -> the gate refuses.

Exit: 0 = every box matches the pinned set (proceed), 20 = a box DRIFTED (REFUSED),
11 = a box UNKNOWN/unread (INCOMPLETE, not clean), 1 = usage error.
EOF
}

main() {
  local readme="$DEFAULT_README" manifest=""
  local -a win_state=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --readme)     shift; readme="${1:-}" ;;
      --manifest)   shift; manifest="${1:-}" ;;
      --win-state)  shift; win_state+=("${1:-}") ;;
      -h|--help)    usage; exit 0 ;;
      --*)          echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
      *)            echo "unexpected argument: $1" >&2; usage >&2; exit 1 ;;
    esac
    shift || true
  done

  if [ ! -x "$DRIFT_GUARD" ]; then
    echo "ERROR: drift-guard engine not found/executable: $DRIFT_GUARD" >&2
    exit 1
  fi
  if [ "${#win_state[@]}" -eq 0 ]; then
    echo "ERROR: no box to gate (no --win-state given)." >&2
    echo "The version-integrity gate cannot certify the stack with zero boxes — refusing to pass." >&2
    exit 1
  fi

  echo "== version-integrity-gate (#123): pre-rig-test — live strih+stream stack MUST match the pinned set =="
  echo "   pins from ${readme}; engine = drift-guard.sh --compare; a drifted/unverified box REFUSES the run"

  local bad=0 unknown=0 ok=0 entry name file rc
  local -a compare_args
  local -a unknown_boxes=()
  for entry in "${win_state[@]}"; do
    name="${entry%%=*}"; file="${entry#*=}"
    if [ -z "$file" ] || [ ! -s "$file" ]; then
      printf '  %-14s UNKNOWN  (no state file %s — win-* MCP fetch missing)\n' "$name" "${file:-<none>}"
      unknown=$((unknown + 1)); unknown_boxes+=("$name"); continue
    fi
    echo "  -- ${name} (${file}) --"
    # Build the drift-guard --compare arg vector from the box's observed state.
    compare_args=(--compare "host=${name}" --readme "$readme")
    local has_manifest=0 arg
    while IFS= read -r arg; do
      [ -z "$arg" ] && continue
      [ "${arg%%=*}" = "manifest" ] && has_manifest=1
      compare_args+=("$arg")
    done < <(compare_args_from_state "$file")
    # If a global --manifest was given and the box's state did not carry its own, apply it so the
    # BUILD-SHA / whole-bundle facet runs on every box uniformly.
    if [ "$has_manifest" -eq 0 ] && [ -n "$manifest" ]; then
      compare_args+=("manifest=${manifest}")
    fi
    rc=0
    # Capture the engine's exit code DIRECTLY (no pipe between drift-guard and the status read), THEN
    # indent the buffered output for display. The fail-closed property must NOT depend on `set -o
    # pipefail` staying enabled: a piped `exit 20` to `sed` would otherwise yield pipeline status 0,
    # the `||` would never fire, rc would stay 0, and a DRIFT would be miscounted as OK — a false pass.
    local engine_out=""
    engine_out="$("$DRIFT_GUARD" "${compare_args[@]}" 2>&1)" || rc=$?
    printf '%s\n' "$engine_out" | sed 's/^/    /'
    case "$rc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("$name") ;;
      *)  echo "    !! drift-guard exited ${rc} for ${name} (engine/usage error)" >&2; bad=$((bad + 1)) ;;
    esac
  done

  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} box(es) DRIFTED from the pinned zero-loss set — rig test REFUSED." >&2
    echo "!! A result on a randomly-deployed / drifted / stock build is worthless (#119). Restore the" >&2
    echo "!! pinned build (off-air + user-approved redeploy), re-verify with /drift-guard, then re-run." >&2
    [ "$unknown" -gt 0 ] && echo "!! (${unknown} further box(es) UNKNOWN: ${unknown_boxes[*]} — status also incomplete.)" >&2
    exit 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN: ${unknown_boxes[*]} (state unread / a value not read) — NOT clean." >&2
    echo "!! Every box must report a complete observed stack before the rig test is trusted. (${ok} OK.)" >&2
    exit 11
  fi
  echo "GATE PASS — ${ok} box(es) match the pinned zero-loss set. The live stack is the build we expect; proceed."
  exit 0
}

main "$@"
