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
#   knobs (all optional): QR_ALIGN_SOURCES (explicit CSV), QR_ALIGN_MAX_DELTA_MS,
#   QR_ALIGN_PARITY_TOL_IDS, QR_ALIGN_EXTRA_ARGS, and the #1160 stable-tail bounds QR_ALIGN_ROUNDS
#   (→ the --max-measure-rounds cap) + QR_ALIGN_BUDGET_S (→ --measure-budget-s, the ~150 s wall-clock
#   bound, #1161). The budget is INTERNAL to qr_align_pins.py, so this step needs NO outer `timeout`
#   and recording-e2e.sh is untouched. Returns the aligner's exit code (0 = aligned / already-aligned;
#   non-zero = could not align — the caller ABORTS the run).
qr_align_run() {
  local host="$1" password="${2:-}"
  local here sources rc=0
  here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"        # scripts/ dir (qr_align_pins.py)
  # Default to the align set MINUS any acked-offline box (#1003 review 🟡): a temporarily wedged/
  # acked on-air camera must not abort the whole run just because it cannot decode a painter QR.
  sources="${QR_ALIGN_SOURCES:-$(camera_align_ndi_sources_excluding_csv "${PREFLIGHT_EXCLUDED_CAMS:-}")}"

  if [ -z "$sources" ]; then
    echo "::error::[qr-align] no align sources — CAMERA_ALIGN_SET is empty and QR_ALIGN_SOURCES unset" >&2
    return 2
  fi

  # #1161 TWO-PHASE reset + audit -> each source's TRUE arrival transport floor, so qr_align_pins.py
  # can pin each FASTER camera ABOVE its floor (a pin below the floor is structurally inert —
  # latency = max(pin, transport)). WHY two-phase: the strih genlock audit `latency_ms +
  # mean_head_skew_ms` is the PRESENT AGE (max(pin, transport)); it equals the TRUE transport only
  # while the pin sits BELOW it. Pins PERSIST across runs, so a prior aligned run leaves them elevated
  # and a naive audit would read pin-HELD ages, not transports (review 🔴). PHASE 0: reset every align
  # pin to the floor; settle so the genlock sheds DOWN to the transport; then fetch the audit scoped
  # to ONLY the post-settle log lines (the [4g/8] Correction-2 line-count discipline — never a blind
  # -Tail that averages latency_ms across two pin regimes, review 🟡). The win_ssh_run calls are
  # `timeout`-bounded (win-ssh-exec.sh's own doc: the caller must bound it; review 🔵). Best-effort
  # throughout: any hiccup (standalone call with no win_ssh_run/PROBE_BIN_DIR/OUTDIR, reset failure,
  # unreachable log, no audit lines) skips --jitter-json and qr_align_pins.py falls back to the
  # inert-prone floor+delta plan with its own loud warning. All on-air strih inputs sit on the
  # always-active Multiview grid, so the audit fires for them continuously — no preview cycling.
  # Override with QR_ALIGN_JITTER_JSON (an explicit pre-computed path) for a manual run or a test;
  # QR_ALIGN_RESET_SETTLE_S (shed) / QR_ALIGN_AUDIT_WINDOW_S (clean-sample accrual) tune the waits.
  local jitter_json="${QR_ALIGN_JITTER_JSON:-}"
  if [ -z "$jitter_json" ] && [ -n "${STRIH_USER:-}" ] && [ -n "${PROBE_BIN_DIR:-}" ] \
      && [ -n "${OUTDIR:-}" ] && command -v win_ssh_run >/dev/null 2>&1; then
    local _log="$OUTDIR/qr-align-strih-${RUN_ID:-$$}.log"
    local _jj="$OUTDIR/qr-align-jitter-${RUN_ID:-$$}.json"
    local _settle="${QR_ALIGN_RESET_SETTLE_S:-15}" _window="${QR_ALIGN_AUDIT_WINDOW_S:-12}"
    local _newest='Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1'
    local _rrc=0
    # PHASE 0: reset every align pin to the floor (so the audit reads TRUE transports), then settle.
    timeout 120 python3 "$here/qr_align_pins.py" --host "$host" --password "$password" \
      --sources "$sources" --reset-to-floor >&2 || _rrc=$?
    if [ "$_rrc" -eq 0 ]; then
      sleep "$_settle"
      # Mark the log length AFTER the shed, then let clean post-settle audit lines accrue, then fetch
      # ONLY those lines (win_ssh_run re-sourced in a timeout-bounded subshell; the PS command rides
      # an env var to avoid nested-quoting hazards).
      local _start
      # `|| true` guards the substitution (review 🔵) -- a bare invocation under set -e would
      # otherwise abort mid-function on an ssh/pipefail failure.
      _start="$(_qa_ps="(Get-Content ($_newest)).Count" timeout 60 bash -c \
        '. "$0/lib/win-ssh-exec.sh"; win_ssh_run "$1" "$2" "$3" "$_qa_ps"' \
        "$here" "$STRIH_USER" "$password" "$host" 2>/dev/null | tr -d '[:space:]' || true)"
      case "$_start" in
        ''|*[!0-9]*)
          # A failed/garbled count read must NOT degrade to `-Skip 0` = the WHOLE OBS log: that
          # re-mixes the PRE-reset pin-held audit lines the two-phase reset exists to separate,
          # producing regime-mixed garbage floors (review 🟡). Skip the fetch -> floor+delta fallback.
          echo "WARNING: [qr-align] #1161 could not read the post-settle log line count (ssh flake/timeout); skipping the floor-aware audit to avoid a regime-mixed whole-log fetch — falling back to floor+delta." >&2
          ;;
        *)
          sleep "$_window"
          if _qa_ps="Get-Content ($_newest) | Select-Object -Skip $_start" timeout 120 bash -c \
              '. "$0/lib/win-ssh-exec.sh"; win_ssh_run "$1" "$2" "$3" "$_qa_ps"' \
              "$here" "$STRIH_USER" "$password" "$host" > "$_log" 2>/dev/null && [ -s "$_log" ] \
              && "$PROBE_BIN_DIR/genlock-jitter-report" --file "$_log" --json > "$_jj" 2>/dev/null \
              && [ -s "$_jj" ]; then
            jitter_json="$_jj"
            echo "[qr-align] #1161 post-reset arrival-floor audit fetched -> $_jj (floor-aware plan enabled)" >&2
          else
            echo "WARNING: [qr-align] #1161 could not fetch the post-reset strih genlock audit; the plan falls back to the inert-prone floor+delta — see qr_align_pins.py's own warning." >&2
          fi
          ;;
      esac
    else
      echo "WARNING: [qr-align] #1161 pin reset-to-floor failed (rc=$_rrc); skipping the floor-aware audit — the plan falls back to floor+delta." >&2
    fi
  fi

  local -a args=(--host "$host" --password "$password" --sources "$sources" --execute)
  [ -n "$jitter_json" ] && args+=(--jitter-json "$jitter_json")
  # #1209: persist any UNDECODABLE align screenshot's PNG into the run dir, so a reproducible
  # [4i/8align] abort (e.g. cam3 mostly undecodable) can be root-caused from the actual pixels.
  # OUTDIR is recording-e2e's run dir; absent for a standalone/manual call, where persistence is
  # simply off (qr_align_pins.py treats a missing --screenshot-dir as disabled = byte-identical gate).
  [ -n "${OUTDIR:-}" ] && args+=(--screenshot-dir "$OUTDIR")
  # #1160: the measure phase is dynamic (measure-to-a-stable-tail), so QR_ALIGN_ROUNDS is now the
  # hard round CAP, and QR_ALIGN_BUDGET_S the wall-clock bound.
  [ -n "${QR_ALIGN_ROUNDS:-}" ]         && args+=(--max-measure-rounds "$QR_ALIGN_ROUNDS")
  [ -n "${QR_ALIGN_BUDGET_S:-}" ]       && args+=(--measure-budget-s "$QR_ALIGN_BUDGET_S")
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
