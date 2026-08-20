#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines one function, no top-level statements) — matches the
# sibling scripts/lib/*.sh convention (camera-box-restart-verify.sh, cold-cut-step.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing runs in the CALLER's shell, and
# recording-e2e.sh (the only caller) already sets it.
#
# scripts/lib/qr-align.sh — #1003 floor-3 per-run camera alignment step, invoked as a BLOCKING
# preflight in scripts/recording-e2e.sh (owner rework mandate 2026-08-20: "zarad ten screenshot
# spread check aj s auto-align do e2e"). This is the #675 sourced-helper pattern: the whole runner
# lives here so recording-e2e.sh's own text gains only a banner + one call line, and the static
# anchor tests that read recording-e2e.sh never see this body.
#
# WHAT it does (all the logic is in scripts/qr_align_pins.py — this is only the bash wiring):
#   simultaneous barrier WS GetSourceScreenshot of every on-air strih input (CAMERA_ALIGN_SET, incl.
#   cam4) -> decode the painter dual-QR -> floor-3 pins from the exact gen_ts_ns delivery delta ->
#   apply (read-back verified) -> RE-MEASURE -> exit 0 iff the frame_id spread <= parity tolerance.
#
# BLOCKING, not report-only: unlike the #ci-testing-gotchas #1133 report-only helpers (which must
# always `return 0`), this step MUST fail the run when it cannot align — the owner's acceptance is
# "ABORT the run with a per-camera named reason if it stays misaligned". So qr_align_run returns the
# aligner's exit code; the caller aborts on non-zero. It is guarded so a genuine failure is a CLEAN
# named return, never a bare `set -e` mid-function abort.
#
# DOMAINS: strih per-source pins ONLY. The stream `NDI 2ME PGM` hold (operator A/V domain) and imag
# 3ms floor are never in the align set, so they are never written.

# qr_align_run <host> <password>
#   Runs the floor-3 aligner against strih. Sources default to camera_align_ndi_sources_csv (the
#   caller must already have sourced scripts/camera-set.sh — recording-e2e.sh does). Overridable
#   knobs (all optional): QR_ALIGN_SOURCES (explicit CSV), QR_ALIGN_ROUNDS, QR_ALIGN_MAX_DELTA_MS,
#   QR_ALIGN_PARITY_TOL_IDS, QR_ALIGN_EXTRA_ARGS. Returns the aligner's exit code (0 = aligned /
#   already-aligned; non-zero = could not align — the caller ABORTS the run).
qr_align_run() {
  local host="$1" password="${2:-}"
  local here sources rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"        # scripts/ dir (qr_align_pins.py)
  sources="${QR_ALIGN_SOURCES:-$(camera_align_ndi_sources_csv)}"

  if [ -z "$sources" ]; then
    echo "::error::[qr-align] no align sources — CAMERA_ALIGN_SET is empty and QR_ALIGN_SOURCES unset" >&2
    return 2
  fi

  local -a args=(--host "$host" --password "$password" --sources "$sources" --execute)
  [ -n "${QR_ALIGN_ROUNDS:-}" ]         && args+=(--rounds "$QR_ALIGN_ROUNDS")
  [ -n "${QR_ALIGN_MAX_DELTA_MS:-}" ]   && args+=(--max-delta-ms "$QR_ALIGN_MAX_DELTA_MS")
  [ -n "${QR_ALIGN_PARITY_TOL_IDS:-}" ] && args+=(--parity-tol-ids "$QR_ALIGN_PARITY_TOL_IDS")
  # QR_ALIGN_EXTRA_ARGS is an intentional word-split escape hatch for one-off flags.
  # shellcheck disable=SC2206
  [ -n "${QR_ALIGN_EXTRA_ARGS:-}" ]     && args+=(${QR_ALIGN_EXTRA_ARGS})

  # `|| rc=$?` keeps the caller's `set -e` from aborting before we can print the named reason.
  python3 "$here/qr_align_pins.py" "${args[@]}" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "::error::[qr-align] camera alignment FAILED (rc=$rc) — see the per-camera reason above; the run is ABORTED (owner rework #1003)" >&2
    return "$rc"
  fi
  return 0
}
