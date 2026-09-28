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
#   { "obs_version":"32.2.0", "distroav_version":"6.2.1", "ndi_runtime":"6.3.2.0",
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
# shellcheck source=scripts/lib/obs-box-baseline-win.sh
. "$HERE/lib/obs-box-baseline-win.sh"
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

# state_json_value FILE KEY -> the string value of "KEY":"<value>" in the flat JSON state object
# FILE, or "" if absent/unreadable/no such file. #826 — generalized out of what used to be the
# #756-only `genlock_build_sha_from_state` (behavior-preserving refactor: every existing caller/test
# of that name keeps working, now implemented as a one-line call here) so every single-key facet
# added since (obs_installs, port4455_owner_path, ...) reuses ONE tolerant parser instead of a new
# copy-pasted grep|sed each time. Same tolerant flat-JSON parse as compare_args_from_state: match
# `"KEY": "<value>"`, unescape \\ -> \ and \" -> ", take the first match only.
state_json_value() {
  local file="$1" key="$2"
  [ -f "$file" ] || return 0
  local pair val
  pair="$(grep -oE "\"${key}\"[[:space:]]*:[[:space:]]*\"(\\\\.|[^\"\\\\])*\"" "$file" 2>/dev/null | head -1)"
  [ -z "$pair" ] && return 0
  val="$(printf '%s' "$pair" | sed -E 's/^"[^"]*"[[:space:]]*:[[:space:]]*"(.*)"$/\1/')"
  val="${val//\\\\/\\}"
  val="${val//\\\"/\"}"
  printf '%s' "$val"
}

# genlock_build_sha_from_state FILE -> the #756 `genlock_build_sha` value from the flat JSON state
# object FILE, or "" if absent/unreadable. This key is NOT a drift-guard --compare key (it is not
# emitted by compare_args_from_state above and never fed to drift-guard --compare) — it is read
# separately here and handed to the CROSS-BOX parity engine (genlock_build_parity_report). "" when
# the box's state has no such key yet -- ENFORCED (#758): the parity engine's OWN "<2 read SHAs"
# branch now returns UNKNOWN (a real gate-blocking condition), so an un-upgraded/unread box's
# bundle-state-server is itself flagged, never silently skipped.
genlock_build_sha_from_state() {
  state_json_value "$1" genlock_build_sha
}

# --- #826: strih OBS-identity machine-check facet — PURE verdict functions -------------------
# The four acceptance verdicts (obs_installs / port4455_identity / obs_process_count /
# startup_chain) + the DEFAULT_OBS_INSTALL_EXE / _WORKDIR / DEFAULT_STARTUP_SHORTCUT pins live in
# their own lib (moved verbatim, issue 1377 -- the 1000-line file budget), with the facet's row
# function vig_row_obs_identity that main() calls per box (issue 1384).
# shellcheck source=scripts/lib/version-integrity-obs-identity.sh
. "$HERE/lib/version-integrity-obs-identity.sh"

# --- genlock vendor-pin report-only ALARM (#1137, #1292) — PURE verdict + git range helpers ----
# vendor_pin_range_log / vendor_pin_ahead_log / vendor_pin_on_dev + genlock_vendor_pin_verdict live
# in their own lib (moved verbatim, issue 1377); the range helpers wrap the shared
# scripts/lib/vendor-range.sh that drift-guard.sh uses too (issue 1384).
# shellcheck source=scripts/lib/version-integrity-vendor-pin.sh
. "$HERE/lib/version-integrity-vendor-pin.sh"

# --- main()'s row functions (issue 1384) -------------------------------------------------------
# Each per-facet row block of main() is a named vig_row_* function: vig_row_obs_identity (in the
# obs-identity lib), vig_row_vendor_pin (in the vendor-pin lib), vig_row_genlock_parity (below the
# source-guard, in this file), and the rows without a family of their own -- vig_row_box_engine,
# vig_row_imag_bytes, vig_row_report_only_boxes -- in this lib, whose header states the contract
# every row follows.
# shellcheck source=scripts/lib/version-integrity-rows.sh
. "$HERE/lib/version-integrity-rows.sh"

# --- source-guard: when sourced (the unit tests), stop here --------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# The cross-box genlock-parity engine (#756) lives in drift-guard.sh; source it so the FLOW below
# can call genlock_build_parity_report directly (drift-guard's own source-guard returns before its
# main, so this pulls in its pure functions only). Sourced AFTER our source-guard, so the gate's own
# unit tests (which source THIS file for its pure parsers) never pull the engine in.
# shellcheck source=/dev/null
. "$DRIFT_GUARD"

# imag_bytes_verdict LABEL MANIFEST CSV -> #1082 imag .so BYTE parity. For each `path=sha` in CSV (the
# imag box's DEPLOYED libobs.so.30 / distroav.so / libobs-opengl.so.30 sha256s, gathered over ssh by
# recording-e2e.sh via scripts/lib/manifest-autosource.sh), resolve the AUTHORITATIVE sha for that
# EXACT manifest path via drift-guard's manifest_sha_for_path (the linux-.so resolver -- #122's
# manifest_sha_for_component knows only the Windows obs.dll/distroav.dll basenames) and compare. A
# TARGETED per-.so compare, NOT the whole-bundle drift_check_all_files walk, so a partial 3-file
# gather never flips the gate UNKNOWN for the ~1600 files it did not hash.
#
# ENFORCED (#758-shape, #1100): an absent CSV or MANIFEST is a gate-blocking UNKNOWN (returns 11), so
# a live gather/auto-source failure REFUSES the run rather than silently passing -- the live imag
# gather is deployed + verified on the rig (imag_so_bytes OK on a green E2E, obs-genlock bundle at
# /usr/lib). Present + all match -> OK (0); any mismatch -> DRIFT (20)
# naming the .so + box; a path absent from the manifest -> UNKNOWN (11, never a false clean). Defined
# below the source-guard because it calls manifest_sha_for_path (drift-guard) -- tested end-to-end via
# the gate subprocess (tests/version_integrity_gate.rs), the same path the #770 byte facet uses.
imag_bytes_verdict() {
  local label="$1" manifest="$2" csv="$3"
  if [ -z "$csv" ] || [ -z "$manifest" ]; then
    printf '  %-22s UNKNOWN  (%s byte gather/manifest not supplied -- #1100 ENFORCED, every box must report its .so bytes)\n' "imag_so_bytes" "$label"
    return 11
  fi
  if [ ! -f "$manifest" ]; then
    printf '  %-22s UNKNOWN  (%s manifest %s not readable)\n' "imag_so_bytes" "$label" "$manifest"
    return 11
  fi
  local OLDIFS="$IFS"; IFS=','
  # shellcheck disable=SC2206
  local -a entries=($csv)
  IFS="$OLDIFS"
  local entry path sha exp drift=0 unknown=0 ok=0 total=0
  for entry in "${entries[@]}"; do
    entry="${entry#"${entry%%[![:space:]]*}"}"; entry="${entry%"${entry##*[![:space:]]}"}"
    [ -z "$entry" ] && continue
    path="${entry%%=*}"; sha="${entry#*=}"
    total=$((total + 1))
    exp="$(manifest_sha_for_path "$manifest" "$path")"
    if [ -z "$exp" ]; then
      printf '  %-22s UNKNOWN  (%s: %s not listed in the manifest -- byte parity unverifiable)\n' "imag_so_bytes" "$label" "$path"
      unknown=$((unknown + 1))
    elif [ "$sha" = "$exp" ]; then
      printf '  %-22s OK       (%s: %s matches the manifest)\n' "imag_so_bytes" "$label" "${path##*/}"
      ok=$((ok + 1))
    else
      printf '  %-22s DRIFT    (%s: %s bytes differ -- expected %s, deployed %s)\n' "imag_so_bytes" "$label" "$path" "$exp" "$sha"
      drift=$((drift + 1))
    fi
  done
  if [ "$total" -eq 0 ]; then
    printf '  %-22s UNKNOWN  (%s byte CSV empty -- #1100 ENFORCED)\n' "imag_so_bytes" "$label"
    return 11
  fi
  [ "$drift" -gt 0 ] && return 20
  [ "$unknown" -gt 0 ] && return 11
  return 0
}

# vig_row_genlock_parity IMAG_ACKED_OFFLINE STRIH_LINUX -> the cross-box genlock-build parity row
# (issue 756/949), moved verbatim out of main() (issue 1384). It is defined HERE rather than in a
# scripts/lib/ facet lib because tests/drift_guard.rs reads THIS file's text for the two
# consumed-paths calls below that carry the --strih-linux flag (issue 1372 review), and because,
# like imag_bytes_verdict above, it calls drift-guard functions. Besides the two arguments it
# READS main()'s win_state and genlock_sha arrays and APPENDS one LABEL=SHA reading per box to
# main()'s parity_args array (bash cannot pass two arrays in, so these three stay main()'s, by
# dynamic scope); vig_row_vendor_pin then pins those readings.
# It follows the vig_row_* contract in the header of scripts/lib/version-integrity-rows.sh.
vig_row_genlock_parity() {
  local imag_acked_offline="$1" strih_linux="$2"
  local entry file
  # #756 — CROSS-BOX genlock-build PARITY: every fleet box must run ONE deployed genlock build. This
  # catches the stale-imag skew the per-box origin/main ref-compare (drift-guard --check-imag) misses
  # during a long-lived dev train (#530/#756: imag ran a stale lineage, segfaulted, wedged the GPU).
  # Gather each box's live GENLOCK_BUILD_SHA.txt: from every --win-state box's state JSON (served by
  # its bundle-state-server) + every --genlock-sha LABEL=SHA supplied directly (imag, read over ssh
  # by recording-e2e.sh). ENFORCED (#758): the parity engine is fail-closed (an unread box, OR fewer
  # than 2 read peers, is UNKNOWN — a REAL gate-blocking condition, never a silent skip).
  local ge gname gsha
  for entry in "${win_state[@]}"; do
    gname="${entry%%=*}"; file="${entry#*=}"
    gsha=""
    [ -n "$file" ] && [ -s "$file" ] && gsha="$(genlock_build_sha_from_state "$file")"
    parity_args+=("${gname}=${gsha}")
  done
  for ge in "${genlock_sha[@]}"; do
    # #1164 -- imag acked offline: drop its genlock-sha entry so the parity certifies the remaining
    # fleet (strih+stream) instead of UNKNOWN-refusing on the physically-absent, acked box. Defense
    # in depth -- the acked call site (recording-e2e.sh) already omits --genlock-sha imag=... entirely.
    if [ -n "$imag_acked_offline" ] && [ "${ge%%=*}" = "imag" ]; then continue; fi
    parity_args+=("$ge")
  done
  # #756/#758 — ENFORCED (no longer opt-in/dormant, per the user's explicit escalation after
  # today's imag stale-build incident): the parity engine ALWAYS runs now, unconditionally — its
  # OWN "fewer than 2 read peers" branch already returns UNKNOWN (11), which this case statement
  # already treats as a gate-blocking condition exactly like every other facet's UNKNOWN. The old
  # `nonempty -ge 2` gate existed ONLY to skip calling the engine at all while the fleet's
  # bundle-state-servers were still being upgraded (#756 rollout) -- that rollout is complete
  # (strih+stream+imag all report genlock_build_sha as of 2026-07-14 ~21:40), so a box that fails
  # to report one now is itself a REAL, actionable gap (a stale/unread bundle-state-server), never
  # a reason to silently skip the whole facet.
  #
  # #949 — a Windows-only vendor/av-sync-dock/** change advances strih/stream's deployed
  # GENLOCK_BUILD_SHA.txt to a SHA imag's OWN build trigger (linux-genlock.yml, which deliberately
  # excludes vendor/av-sync-dock/**) can never be built at -- even though imag's actual built
  # bytes never changed. A raw-string mismatch is therefore NOT proof of a real skew by itself;
  # before handing the raw LABEL=SHA readings to the (still string-comparing) engine, resolve every
  # PAIR of boxes reporting a non-empty, DIFFERENT-string SHA into a real git content check, scoped
  # to the INTERSECTION of the two boxes' own consumed vendor paths (genlock_parity_consumed_paths)
  # -- an empty `git diff` there means the label mismatch is cosmetic, and an `EQUIV=labelA:labelB`
  # marker is appended so the engine treats that ONE pair as in parity. A pair whose diff is
  # NON-empty, or whose SHA cannot be resolved at all (fail-closed -- never a silent pass), gets NO
  # marker and still DRIFTs exactly as before #949. Boxes already byte-identical need no git call
  # at all (the engine's own fast path). Carries BOTH EQUIV= markers (pair proven content-
  # identical) and DIFF= markers (pair genuinely differs -- names the actual paths so the DRIFT
  # message stays actionable) -- genlock_build_parity_report tells them apart by prefix.
  local -a equiv_args=()
  local -a __ep_a=() __ep_b=()
  local pi pj la sa lb sb
  local any_mismatch=0
  for ((pi = 0; pi < ${#parity_args[@]}; pi++)); do
    for ((pj = pi + 1; pj < ${#parity_args[@]}; pj++)); do
      sa="${parity_args[$pi]#*=}"
      sb="${parity_args[$pj]#*=}"
      if [ -n "$sa" ] && [ -n "$sb" ] && [ "$sa" != "$sb" ]; then
        any_mismatch=1
      fi
    done
  done
  if [ "$any_mismatch" -eq 1 ]; then
    local repo_root=""
    repo_root="$(cd "$HERE/.." 2>/dev/null && pwd)" || repo_root=""
    if [ -z "$repo_root" ]; then
      echo "WARN: could not resolve version-integrity-gate.sh's own repo root -- skipping #949 genlock parity content-equivalence check (a label-only mismatch will DRIFT even if the content is identical)" >&2
    else
      timeout 15 git -C "$repo_root" fetch origin --quiet 2>/dev/null \
        || echo "WARN: git fetch origin failed (or timed out) -- #949 genlock parity content-check may see a stale origin (a genuinely new SHA may fail to resolve and DRIFT)" >&2
      local pth pb found_p
      for ((pi = 0; pi < ${#parity_args[@]}; pi++)); do
        for ((pj = pi + 1; pj < ${#parity_args[@]}; pj++)); do
          la="${parity_args[$pi]%%=*}"; sa="${parity_args[$pi]#*=}"
          lb="${parity_args[$pj]%%=*}"; sb="${parity_args[$pj]#*=}"
          [ -z "$sa" ] || [ -z "$sb" ] && continue
          [ "$sa" = "$sb" ] && continue
          __ep_a=()
          while IFS= read -r pth; do [ -n "$pth" ] && __ep_a+=("$pth"); done \
            < <(genlock_parity_consumed_paths "$la" "$strih_linux")
          __ep_b=()
          while IFS= read -r pth; do [ -n "$pth" ] && __ep_b+=("$pth"); done \
            < <(genlock_parity_consumed_paths "$lb" "$strih_linux")
          local -a inter=()
          for pth in "${__ep_a[@]}"; do
            found_p=0
            for pb in "${__ep_b[@]}"; do [ "$pb" = "$pth" ] && found_p=1 && break; done
            [ "$found_p" -eq 1 ] && inter+=("$pth")
          done
          if [ "${#inter[@]}" -eq 0 ]; then
            continue
          fi
          if genlock_parity_equivalent "$repo_root" "$sa" "$sb" "${inter[@]}"; then
            equiv_args+=("EQUIV=${la}:${lb}")
          else
            # #949: not equivalent (a real diff, or an unresolvable sha). Try to name the ACTUAL
            # differing paths so a genuine DRIFT is actionable, not just "two opaque SHAs differ" —
            # empty output here (unresolvable sha) simply means no DIFF= marker is added, and the
            # DRIFT message falls back to its pre-#949 wording.
            local diff_paths=""
            diff_paths="$(genlock_parity_diff_paths "$repo_root" "$sa" "$sb" "${inter[@]}" \
              | paste -sd, - 2>/dev/null || true)"
            if [ -n "$diff_paths" ]; then
              equiv_args+=("DIFF=${la}:${lb}:${diff_paths}")
            fi
          fi
        done
      done
    fi
  fi
  echo "  -- cross-box genlock parity (#756/#949, ENFORCED) --"
  local prc=0 parity_out=""
  parity_out="$(genlock_build_parity_report "${parity_args[@]}" "${equiv_args[@]}")" || prc=$?
  printf '%s\n' "$parity_out" | sed 's/^/    /'
  case "$prc" in
    0)  ok=$((ok + 1)) ;;
    20) bad=$((bad + 1)) ;;
    11) unknown=$((unknown + 1)); unknown_boxes+=("genlock_parity") ;;
    *)  echo "    !! genlock_build_parity_report exited ${prc} (engine error)" >&2; bad=$((bad + 1)) ;;
  esac
  return 0
}

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
  version-integrity-gate.sh [--readme PATH] [--manifest PATH] [--alt-manifest PATH] --win-state NAME=FILE [...]

Options:
  --readme PATH     pinned-set source (default ${DEFAULT_README}); threaded to drift-guard --compare.
  --manifest PATH   the build-under-test BUNDLE_MANIFEST.json — when set, applied to every box that
                    does not already carry a manifest= in its state (activates the BUILD-SHA facet).
  --alt-manifest PATH  #1346 -- the OTHER Windows workflow's BUNDLE_MANIFEST.json of the SAME build
                    (the full windows-genlock bundle beside the fast obs.dll-only --manifest). The
                    two builds' obs.dll bytes differ, so a box is OK on EITHER entry (the line
                    names which) and DRIFT on neither. Applied wherever --manifest is; given alone
                    it is judged exactly like a lone --manifest (fail-closed).
  --win-state N=FILE  a box N whose observed drift-guard stack state JSON the caller wrote to FILE
                    (this gate has no headless ssh gather of its own; the win-* MCP holder
                    pre-fetches it -- #701 proved plain scp/ssh reaches strih/stream, not migrated
                    here). Repeatable. A box with no
                    file is UNKNOWN -> the gate refuses.
  --win-state-report-only N=FILE  #1296 -- a box whose observed stack is PRINTED as an
                    informational row but NEVER blocks (never enters the pass/fail roll-up).
                    RESOLUME-SNV (a traveling CG box, not a measured [0/8] source) uses this so it
                    is version-surfaced without ever refusing the run. Repeatable.
  --imag-manifest PATH  #1082 -- the CI-authoritative linux BUNDLE_MANIFEST.json for imag's build,
                    against which imag's DEPLOYED .so bytes are compared. ENFORCED (#1100).
  --imag-bytes LABEL=path=sha,...  #1082 -- imag's DEPLOYED libobs.so.30 / distroav.so /
                    libobs-opengl.so.30 sha256s (gathered over ssh; imag is not a --win-state box).
                    ENFORCED (#1100): absent -> the imag byte facet is UNKNOWN -> the gate refuses.
  --imag-acked-offline REASON  #1164 -- imag is physically absent and operator-acked offline
                    (rig-fleet.txt \`imag:REASON\`, issue 1013). SKIPS the imag .so byte facet (a
                    loud SKIPPED line, counted OK, never UNKNOWN) and drops any \`imag\`-labelled
                    --genlock-sha entry from the cross-box parity (which then certifies strih+stream).
                    WITHOUT this flag an absent imag is still fail-closed UNKNOWN (the #1100 default).
  --strih-linux  issue 1351 follow-up -- strih is the Linux notebook (strih-lx) after the M4
                    cut-over, so it reports none of the Windows-only version-integrity facets (the
                    #826 OBS-identity set: obs_installs/startup_chain/port4455_identity/
                    obs_process_count, plus the Windows distroav_dll_paths scan + the Windows
                    ndi_runtime facet). SKIPS all of them on the box named \`strih\` (a loud
                    SKIPPED line each, counted OK, never UNKNOWN) while KEEPING the
                    platform-agnostic facets a Linux strih's bundle-state DOES serve
                    (obs_dll_sha256 / distroav_dll_sha256 / genlock_capability / genlock_build_sha
                    parity), so strih's actual genlock build stays verified. WITHOUT this flag
                    strih is graded exactly as before (Windows-shaped, byte-identical).
  --win-baseline N=FILE  issue 1357 -- REPORT-ONLY rows: box N's raw Windows baseline gather
                    (scripts/win-baseline-check.sh --out-dir), graded per item, NEVER counted.

Exit: 0 = every box matches the pinned set (proceed), 20 = a box DRIFTED (REFUSED),
11 = a box UNKNOWN/unread (INCOMPLETE, not clean), 1 = usage error.
EOF
}

main() {
  local readme="$DEFAULT_README" manifest=""
  # #1346 -- the ALTERNATE Windows bundle manifest of the SAME build (the full windows-genlock bundle
  # beside the fast obs.dll-only --manifest): the two workflows' obs.dll bytes differ, so drift-guard
  # accepts either entry. Threaded to each box exactly where --manifest is (alt_manifest=).
  local alt_manifest=""
  local -a win_state=()
  # #756 — extra box genlock-build SHAs supplied directly (LABEL=SHA), for boxes not gated via
  # --win-state (imag-nb is SSH-reachable, so recording-e2e.sh reads its GENLOCK_BUILD_SHA.txt and
  # passes it here). Repeatable. Combined with the SHAs read out of each --win-state file for the
  # CROSS-BOX parity assert.
  local -a genlock_sha=()
  # #1082 -- imag (Linux) .so BYTE parity: a linux BUNDLE_MANIFEST for imag's build + imag's DEPLOYED
  # .so sha256s (LABEL=path=sha,...), gathered over ssh (imag is NOT a --win-state bundle-state box).
  # Both ENFORCED (#758-shape, #1100): absent -> the facet is UNKNOWN -> the gate refuses.
  local imag_manifest="" imag_bytes=""
  # #1164 -- imag acked-offline (physically absent, operator-acked in rig-fleet.txt, issue 1013).
  # When set to the ack REASON, the imag .so byte facet is SKIPPED (a loud line, counted ok, never
  # UNKNOWN) and any --genlock-sha entry labelled exactly `imag` is dropped from the cross-box parity
  # (which then certifies the remaining fleet strih+stream). WITHOUT this flag the gate is
  # byte-identical to before -- an absent imag is still fail-closed UNKNOWN(11) (the #1100 contract).
  local imag_acked_offline=""
  # issue 1351 follow-up -- --strih-linux: strih is the Linux notebook (strih-lx); skip its
  # Windows-only facets (the #826 OBS-identity set + distroav_dll_paths + ndi_runtime) as a loud
  # SKIPPED (counted ok), keep the platform-agnostic byte/capability/parity facets. Mirrors the
  # --imag-acked-offline shape above. WITHOUT this flag strih is graded byte-identical to before.
  local strih_linux=0
  # #1296 -- REPORT-ONLY boxes (NAME=FILE, same state-JSON shape as --win-state): a box whose
  # observed stack is PRINTED as an informational row but NEVER enters the bad/unknown/ok roll-up,
  # so it can never block the run. RESOLUME-SNV uses this: it is a TRAVELING CG box, NOT a measured
  # source in the cam->strih->stream recording path, so it stays OUT of the [0/8] blocking set
  # (targets.md: "Not in the E2E [0/8] version gate") while still surfacing its genlock build + OBS
  # identity in the gate output. Repeatable. A report-only box with no/empty file prints an
  # "unread (report-only)" row and still never blocks.
  local -a win_state_report_only=()
  local -a win_baseline=()   # issue 1357 -- report-only Windows baseline gathers (NAME=FILE)
  while [ $# -gt 0 ]; do
    case "$1" in
      --readme)             shift; readme="${1:-}" ;;
      --manifest)           shift; manifest="${1:-}" ;;
      --alt-manifest)       shift; alt_manifest="${1:-}" ;;
      --win-state)          shift; win_state+=("${1:-}") ;;
      --win-state-report-only) shift; win_state_report_only+=("${1:-}") ;;
      --win-baseline)       shift; win_baseline+=("${1:-}") ;;
      --genlock-sha)        shift; genlock_sha+=("${1:-}") ;;
      --imag-manifest)      shift; imag_manifest="${1:-}" ;;
      --imag-bytes)         shift; imag_bytes="${1:-}" ;;
      --imag-acked-offline) shift; imag_acked_offline="${1:-}" ;;
      --strih-linux)        strih_linux=1 ;;
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

  local bad=0 unknown=0 ok=0 entry name file
  local -a unknown_boxes=()
  for entry in "${win_state[@]}"; do
    name="${entry%%=*}"; file="${entry#*=}"
    # issue 1351 follow-up: --strih-linux only ever applies to the box literally named "strih" --
    # a future box also passed the flag by mistake stays Windows-graded (no other box is affected).
    local is_strih_linux=0
    if [ "$strih_linux" = 1 ] && [ "$name" = "strih" ]; then
      is_strih_linux=1
    fi
    if [ -z "$file" ] || [ ! -s "$file" ]; then
      printf '  %-14s UNKNOWN  (no state file %s — win-* MCP fetch missing)\n' "$name" "${file:-<none>}"
      unknown=$((unknown + 1)); unknown_boxes+=("$name"); continue
    fi
    # The per-box rows (issue 1384: each a named function in its facet lib, same order, same
    # output): the drift-guard engine compare, then the issue-826 OBS-identity rows.
    vig_row_box_engine "$name" "$file" "$readme" "$manifest" "$alt_manifest" "$is_strih_linux"
    vig_row_obs_identity "$name" "$file" "$readme" "$is_strih_linux"
  done

  # The fleet rows, in the original order. parity_args (LABEL=SHA per box) is filled by the parity
  # row and read by the vendor-pin alarm; the row functions update ok / bad / unknown /
  # unknown_boxes above (see the vig_row_* contract in the file header of
  # scripts/lib/version-integrity-rows.sh).
  local -a parity_args=()
  vig_row_genlock_parity "$imag_acked_offline" "$strih_linux"
  vig_row_imag_bytes "$imag_acked_offline" "$imag_bytes" "$imag_manifest"
  vig_row_vendor_pin "${parity_args[@]}"
  vig_row_report_only_boxes "${win_state_report_only[@]}"

  # issue 1357 -- REPORT-ONLY Windows OBS-box baseline rows (the lib renders them; never counted).
  [ "${#win_baseline[@]}" -eq 0 ] || win_baseline_report_rows "${win_baseline[@]}"

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
