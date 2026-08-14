#!/usr/bin/env bash
# scripts/lib/e2e-failure-stage.sh — #844: compose the failed-gate Discord alert content, naming the
# STAGE the full-path E2E run actually failed in, derived from its DURABLE on-runner artifacts.
#
# Sourced by .github/workflows/full-path-e2e.yml's `if: failure()` alert step. PURE + side-effect
# free: it reads files under a run's OUTDIR and echoes ONE line of Discord `content` — no ssh, no
# network, no writes — so it is safe to source from the workflow and to unit-test directly
# (tests/harness_e2e_failure_stage_844.rs drives it via `bash -c '. "$LIB"; e2e_failure_stage_content …'`).
#
# WHY (#844): the alert used to hardcode "cam2→cam1→strih→stream frame-loss/latency gate breached"
# on EVERY job failure, including a [0/8] preflight abort that never recorded a frame — naming a
# measurement that was never taken. The verdict-<RUN_ID>.json the #703 fail-closed guard already
# trusts is the ground truth for "was a verdict actually produced?" — never a mutable in-process
# phase file that a killed run could leave stale (the very bug class this ticket family is about).
#
# Source-only: defines one pure function, runs nothing on its own (no main, no ssh).

# e2e_failure_stage_content OUTDIR RUN_ID SHA_SHORT RUN_URL -> one line of Discord `content`.
# Emits a "frame-loss/latency breach" claim ONLY for a genuinely failing verdict JSON; every other
# path states plainly that no verdict/measurement was produced and names the stage the run reached.
e2e_failure_stage_content() {
  local outdir="$1" run_id="$2" sha="$3" url="$4"
  local head="🔴 **camera-box recording-based full-path E2E FAILED** (${sha})"

  # No run id / no OUTDIR name at all: the harness died before it even had a run identity.
  # (A run_id that IS set but whose OUTDIR is absent falls THROUGH to the artifact checks below,
  # which correctly land it in the "aborted before a recording was captured" stage — a missing dir
  # matches no glob and holds no verdict, so it is honestly a pre-recording abort, not "startup".)
  if [ -z "$run_id" ] || [ -z "$outdir" ]; then
    printf '%s — aborted at startup: no run artifacts were produced; no frame-loss measurement was taken — %s\n' \
      "$head" "$url"
    return 0
  fi

  local verdict="$outdir/verdict-${run_id}.json"
  if [ -f "$verdict" ]; then
    local overall
    overall="$(jq -r '.overall_pass' "$verdict" 2>/dev/null || printf 'unknown')"
    case "$overall" in
      true)
        printf '%s — the VERDICT stage zero-loss/A/V verdict itself PASSED; a later gate step failed (not a frame-loss regression) — %s\n' \
          "$head" "$url"
        ;;
      false)
        printf '%s — VERDICT stage: cam2→cam1→strih→stream zero-loss/A/V verdict FAILED — a genuine frame-loss/latency breach — %s\n' \
          "$head" "$url"
        ;;
      *)
        printf '%s — VERDICT stage: verdict JSON present but unreadable (overall_pass=%s); no trustworthy frame-loss verdict — %s\n' \
          "$head" "$overall" "$url"
        ;;
    esac
    return 0
  fi

  # No verdict JSON. Did the run get as far as capturing recordings/painter ground-truth to dev1?
  # (strih-*.mkv / stream-*.mp4 / painter-*.csv are downloaded at [7/8], BEFORE the [8/8] verdict.)
  local f captured=0
  for f in "$outdir"/strih-*.mkv "$outdir"/stream-*.mp4 "$outdir"/painter-*.csv; do
    if [ -s "$f" ]; then captured=1; break; fi
  done

  if [ "$captured" = 1 ]; then
    printf '%s — aborted at the DECODE/VERDICT stage: recordings were captured but NO verdict was produced; no frame-loss verdict was reached — %s\n' \
      "$head" "$url"
  else
    printf '%s — aborted BEFORE a recording was captured (preflight / deploy / record setup); no frame-loss measurement was taken — %s\n' \
      "$head" "$url"
  fi
  return 0
}
