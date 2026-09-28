#!/usr/bin/env bash
# airuleset:script-ok source-only lib (row functions only, no top-level statements) -- the sourcing gate owns strict mode; set -euo pipefail here would leak into the sourcing shell (ci-testing-gotchas)
# scripts/lib/version-integrity-rows.sh -- the row functions of scripts/version-integrity-gate.sh's
# main() that have no facet family of their own (issue 1384: main() was ~500 lines, over the
# ~300-line function budget). Each row block was moved out of main() VERBATIM; the gate sources this
# lib BEFORE its source-guard, beside the two facet libs of issue 1377
# (version-integrity-obs-identity.sh, version-integrity-vendor-pin.sh), so sourcing the gate still
# defines every function. No behaviour change: main() calls the rows in the original order and every
# line of output is byte-identical (proof recipe: .claude/rules/version-integrity-gate.md).
#
# The vig_row_* contract (every row function, here and in the facet libs + the gate):
#   - main() calls it as a bare statement, once per box (the per-box rows) or once per run (the
#     fleet rows), in the order main() lists them.
#   - It takes the values it needs as arguments where bash allows it. A row that needs two of
#     main()'s arrays (vig_row_genlock_parity) reads them from main()'s scope and says so.
#   - A COUNTED row updates main()'s roll-up counters ok / bad / unknown and the unknown_boxes array
#     through bash dynamic scoping: those four names are main()'s locals and are NEVER declared
#     local in a row function, so the increments land in main(). A REPORT-ONLY row (the vendor-pin
#     alarm, the report-only boxes) never touches them.
#   - The one other shared output: vig_row_genlock_parity APPENDS one LABEL=SHA reading per box to
#     main()'s parity_args array, which main() declares (`local -a parity_args=()`) right before the
#     call, and then passes to vig_row_vendor_pin as its arguments. A row that needs it never
#     declares it local either.
#   - It prints its rows to stdout (and its SCREAM / engine-error lines to stderr) exactly as the
#     inline block did, and always returns 0, so main()'s set -euo pipefail never stops on it.
#   - It may call functions the gate defines after sourcing drift-guard.sh (below its source-guard):
#     those exist by the time main() runs. A test that only SOURCES the gate therefore gets the row
#     functions defined but must not call them.

# vig_row_box_engine NAME FILE README MANIFEST ALT_MANIFEST IS_STRIH_LINUX -> the per-box
# drift-guard --compare row (issue 123) for one --win-state box whose state FILE is readable:
# builds the engine's arg vector from the box's observed state, threads the global --manifest /
# --alt-manifest (issue 1346) and the issue-1351 strih_linux key, runs the engine and indents
# its output. Moved verbatim out of the gate's main() (issue 1384).
# It follows the vig_row_* contract in the header of scripts/lib/version-integrity-rows.sh.
vig_row_box_engine() {
  local name="$1" file="$2" readme="$3" manifest="$4" alt_manifest="$5" is_strih_linux="$6"
  local -a compare_args
  local rc
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
  if [ "$has_manifest" -eq 0 ] && [ -n "$alt_manifest" ]; then
    compare_args+=("alt_manifest=${alt_manifest}")
  fi
  # issue 1351 follow-up: tell drift-guard's engine to SKIP the Windows-only ndi_runtime +
  # distroav_dll_paths mandatory facets for a Linux strih (loud SKIPPED, counted ok) while every
  # other compare_observed facet (incl. the manifest-gated obs_dll_sha256/distroav_dll_sha256/
  # genlock_capability byte facets) runs exactly as it would for any other box.
  if [ "$is_strih_linux" = 1 ]; then
    compare_args+=("strih_linux=1")
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
  return 0
}

# vig_row_imag_bytes IMAG_ACKED_OFFLINE IMAG_BYTES IMAG_MANIFEST -> the imag .so byte-parity row
# (issue 1082/1100, the issue-1164 acked-offline SKIP), moved verbatim out of the gate's main()
# (issue 1384). IMAG_BYTES is the raw --imag-bytes LABEL=path=sha,... value (may be empty).
# It follows the vig_row_* contract in the header of scripts/lib/version-integrity-rows.sh.
vig_row_imag_bytes() {
  local imag_acked_offline="$1" imag_bytes="$2" imag_manifest="$3"
  # #1082/#1100 -- imag (Linux) .so BYTE parity facet: compare imag's DEPLOYED libobs.so.30 /
  # distroav.so / libobs-opengl.so.30 sha256s (--imag-bytes, gathered over ssh) against the
  # CI-authoritative linux BUNDLE_MANIFEST for imag's build (--imag-manifest, auto-sourced per box by
  # recording-e2e.sh). This closes the byte-parity gap #770 left for imag (its bytes had NO path into
  # the gate -- only its marker). ENFORCED (#758-shape, #1100): the facet runs UNCONDITIONALLY and an
  # absent gather/manifest is a gate-blocking UNKNOWN (11), never the old silent DORMANT skip -- the
  # live imag gather is deployed + verified on the rig (imag_so_bytes OK on a green E2E). Same
  # 756->758 second step #1067 applied to port4455_identity. (The WINDOWS obs.dll/distroav.dll byte
  # enforcement -- removing recording-e2e.sh's manifest-autosource opt-in guard -- stays staged until
  # the bundle-state-server byte gather is redeployed to strih+stream; see #1100.)
  echo "  -- imag .so byte parity (#1082/#1100, enforced) --"
  if [ -n "$imag_acked_offline" ]; then
    # #1164 -- imag physically absent + operator-acked offline (rig-fleet.txt `imag:...`, issue 1013).
    # SKIP the .so byte facet with a LOUD, greppable line instead of the #1100 UNKNOWN(11) refuse --
    # counted ok (never unknown), never a silent pass (the whole imag leg is a NAMED partial this run,
    # exactly like every other imag_leg_skip_note site). The #1100 fail-closed default is untouched:
    # this branch runs ONLY when the operator explicitly acked imag offline.
    printf '  %-22s SKIPPED  (imag acked offline: %s -- issue-1013 leg skip; facet not judged)\n' \
      "imag_so_bytes" "$imag_acked_offline" | sed 's/^/    /'
    ok=$((ok + 1))
  else
    local ib_label="imag" ib_csv=""
    if [ -n "$imag_bytes" ]; then ib_label="${imag_bytes%%=*}"; ib_csv="${imag_bytes#*=}"; fi
    local ib_out="" ibrc=0
    ib_out="$(imag_bytes_verdict "${ib_label:-imag}" "$imag_manifest" "$ib_csv")" || ibrc=$?
    printf '%s\n' "$ib_out" | sed 's/^/    /'
    case "$ibrc" in
      0)  ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)); unknown_boxes+=("imag:so_bytes") ;;
      *)  echo "    !! imag_bytes_verdict exited ${ibrc} (unexpected)" >&2; bad=$((bad + 1)) ;;
    esac
  fi
  return 0
}

# vig_row_report_only_boxes NAME=FILE... -> the issue-1296 report-only box rows (RESOLUME-SNV):
# one informational line per --win-state-report-only box, never counted. Moved verbatim out of
# the gate's main() (issue 1384); prints nothing for no arguments and always returns 0.
vig_row_report_only_boxes() {
  local -a win_state_report_only=("$@")
  # #1296 — REPORT-ONLY boxes (RESOLUME-SNV): surface each one's observed genlock build + OBS
  # identity as an informational row, but NEVER touch bad/unknown/ok, so a report-only box can
  # never block the run. This is deliberately OUTSIDE the [0/8] blocking set (targets.md: resolume
  # is a traveling CG box, not a measured cam->strih->stream source). An unread/empty state file
  # prints an "unread (report-only)" row and still never blocks.
  if [ "${#win_state_report_only[@]}" -gt 0 ]; then
    echo
    echo "  -- report-only boxes (#1296: surfaced, NEVER gate the run) --"
    local ro_entry ro_name ro_file ro_sha ro_obs ro_port4455
    for ro_entry in "${win_state_report_only[@]}"; do
      ro_name="${ro_entry%%=*}"; ro_file="${ro_entry#*=}"
      if [ -z "$ro_file" ] || [ ! -s "$ro_file" ]; then
        printf '  %-14s report-only  (unread — no state file %s; does NOT block)\n' "$ro_name" "${ro_file:-<none>}"
        continue
      fi
      ro_sha="$(genlock_build_sha_from_state "$ro_file")"
      ro_obs="$(state_json_value "$ro_file" obs_process_count)"
      ro_port4455="$(state_json_value "$ro_file" port4455_owner_version)"
      printf '  %-14s report-only  genlock_build_sha=%s obs64=%s obs_version=%s (does NOT block)\n' \
        "$ro_name" "${ro_sha:-n/a}" "${ro_obs:-n/a}" "${ro_port4455:-n/a}"
    done
  fi
  return 0
}
