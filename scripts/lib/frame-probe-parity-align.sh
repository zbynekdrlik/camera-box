#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors scripts/lib/camera-box-parity-align.sh / cambox-offline-ack.sh (no
# top-level `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/frame-probe-parity-align.sh -- pre-gate auto-align of the cam2 STEADY-STATE painter
# (/usr/local/bin/frame-probe, run by cam2-painter.service) to THIS run's candidate build, so the
# deployed painter never silently LAGS the current build between dev->main merges (issue 1138). The
# EXACT frame-probe sibling of camera-box-parity-align.sh's cambox_parity_align_before_gate (issue
# 1202): the E2E itself keeps cam2's deployed painter on the candidate EVERY run instead of the
# painter drifting until a manual redeploy.
#
# WHY (root cause). frame-probe is auto-deployed ONLY at dev->main merge (ci.yml `deploy-fleet` is
# `if github.ref == 'refs/heads/main'`); between merges `dev` advances but the deployed painter
# stays on the last-main build. recording-e2e.sh's report-only pin then SCREAMS a lag every run but
# nothing redeploys it -- exactly the live 2026-08-29 incident (deployed 510da513aac7 LAGGED, hand-
# redeployed f42c66917455): a stale painter emits an UNCOMPENSATED QPSK A/V marker (the phantom
# -92 ms video leg) and paints no issue-1196 aux tick. This lib closes the gap: BEFORE the gate,
# when cam2's deployed frame-probe != the candidate, it deploys the candidate to
# /usr/local/bin/frame-probe (via deploy-fleet.sh --frame-probe, with the issue-892 enable-state-
# preserving lifecycle) so pin + deploy advance together (orphan-PROOF, the camera-box shape).
#
# SOURCE OF TRUTH = the CLEAN CI `probe-tools-linux-amd64` artifact (what ci.yml deploy-fleet
# actually ships), NOT $PROBE_BIN_DIR/frame-probe (which full-path-e2e.yml builds LOCALLY on dev1 --
# a byte-different sha for the same source). Downloading the CI artifact makes the sha compare exact
# (both sides = the same artifact bytes) and is the architecturally-correct deploy source. It is
# also fetched at [0/8] because $PROBE_BIN_DIR is not built until [1/8] (the same build-order trap
# camera-box-parity-align.sh documents).
#
# BEST-EFFORT. frame_probe_parity_align_before_gate ALWAYS returns 0: it is a pre-step, never a gate.
# A failed/partial align just leaves the (report-only) [1/8] pin to SCREAM loudly, naming the fix.
# SKIPPED under --no-main-pin (CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN=1) operator soak -- never realign
# over a build an operator deliberately deployed.
_FPPA_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$_FPPA_HERE/cambox-offline-ack.sh"

# --- PURE decision (no network, no SSH -- unit-tested by sourcing this file) -------------------

# frame_probe_align_action CANDIDATE_SHA ENTRY... -> prints ONE verdict word and returns 0. ENTRY is
# "name=sha" (sha may be empty -- an unread box). Consults CAMBOX_OFFLINE_ACK
# (cambox_offline_ack_is_acked) exactly as the gate does: an acked-offline box is EXCLUDED. Verdicts:
#   NOCANDIDATE  candidate sha empty                  -> no align; the report-only pin decides
#   UNKNOWN      >=1 active box unread (empty sha)     -> no align, fail closed (never align blindly)
#   NOACTIVE     no active (non-acked) box remained    -> nothing to align
#   OK           every active box already == candidate -> no align
#   ALIGN        every active box read AND != candidate-> deploy the candidate
# UNKNOWN takes precedence (an unread box is never safely aligned). frame-probe is cam2-only in
# practice, so ENTRY is a single cam2 pair -- the list shape mirrors cambox_align_action for reuse.
# DIVERGENCE from cambox_align_action (harmless for the cam2-only list, documented for a future
# multi-box caller): a MIXED list (one box on-candidate, another stale) here returns ALIGN and the
# orchestrator re-pushes to ALL active names (the on-candidate box gets a byte-identical re-push),
# whereas cambox_align_action returns MIXED and REFUSES. For frame-probe there is no split-brain to
# investigate (a stale sibling painter simply wants the candidate too), so ALIGN-all is correct; a
# genuine multi-box caller that must not re-push a matching box should deploy only the mismatches.
frame_probe_align_action() {
  local candidate="$1"
  shift
  if [ -z "$candidate" ]; then
    printf 'NOCANDIDATE\n'
    return 0
  fi
  local entry name sha any=0 unknown=0 all_match=1
  for entry in "$@"; do
    name="${entry%%=*}"
    sha="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      continue
    fi
    if [ -z "$sha" ]; then
      unknown=1
      continue
    fi
    any=1
    if [ "$sha" != "$candidate" ]; then
      all_match=0
    fi
  done
  if [ "$unknown" -eq 1 ]; then
    printf 'UNKNOWN\n'
    return 0
  fi
  if [ "$any" -eq 0 ]; then
    printf 'NOACTIVE\n'
    return 0
  fi
  if [ "$all_match" -eq 1 ]; then
    printf 'OK\n'
    return 0
  fi
  printf 'ALIGN\n'
  return 0
}

# frame_probe_align_candidate_version -> THIS run's candidate camera-box version = the
# [workspace.package] version in Cargo.toml (the SAME value recording-e2e.sh passes to the gate as
# --candidate-pin). This is the VERSION-GUARD reference (the fetched CI probe-tools artifact must
# report this version) -- NOT the pin sha. Override via FRAME_PROBE_ALIGN_CANDIDATE /
# FRAME_PROBE_ALIGN_CARGO_TOML. "" if unresolvable.
frame_probe_align_candidate_version() {
  if [ -n "${FRAME_PROBE_ALIGN_CANDIDATE:-}" ]; then
    printf '%s' "$FRAME_PROBE_ALIGN_CANDIDATE"
    return 0
  fi
  local cargo="${FRAME_PROBE_ALIGN_CARGO_TOML:-$_FPPA_HERE/../../Cargo.toml}"
  [ -f "$cargo" ] || {
    printf ''
    return 0
  }
  grep -m1 -oE '^version = "[^"]+"' "$cargo" 2>/dev/null \
    | sed -E 's/^version = "([^"]+)"$/\1/' || true
}

# frame_probe_align_binary_version BIN -> the camera-box version a binary reports (`BIN --version`
# last field), "" if unreadable. Used to VERSION-GUARD the fetched probe-tools artifact via its
# co-located camera-box-probe (frame-probe itself has no --version -- issue 1138 premise).
frame_probe_align_binary_version() {
  local bin="$1"
  "$bin" --version 2>/dev/null | awk '{print $NF}' | head -1 || true
}

# frame_probe_align_sha_of BIN -> sha256 (lowercase hex) of a local binary, "" if unreadable.
frame_probe_align_sha_of() {
  local bin="$1"
  [ -f "$bin" ] || { printf ''; return 0; }
  sha256sum "$bin" 2>/dev/null | awk '{print $1}' | tr '[:upper:]' '[:lower:]' || true
}

# --- impure read + deploy (seam-overridable; exercised end-to-end via the seams) --------------

# frame_probe_align_read_sha NAME TARGET -> the box's /usr/local/bin/frame-probe sha256 (lowercase
# hex) over ssh. "" if unreachable/absent (UNKNOWN downstream). Overridable per-node via the SAME
# seam the gate report uses -- FRAME_PROBE_GATE_SHA_<NAME> (NAME uppercased, "-" -> "_") -- so a
# test drives the align AND the [1/8] pin report identically. Mirrors read_frame_probe_sha in
# camera-box-version-gate.sh (a deliberate copy -- this lib must NOT source the gate, whose
# top-level `set -euo pipefail` would leak into recording-e2e.sh).
frame_probe_align_read_sha() {
  local name="$1" target="${2:-}" var out
  var="FRAME_PROBE_GATE_SHA_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    # review 1138: validate the seam value through the SAME 64-hex filter as the SSH path, so a
    # malformed override reads as UNKNOWN (empty) rather than a spurious sha (align + gate symmetry).
    printf '%s' "${!var}" | grep -oiE '^[0-9a-f]{64}$' | head -1 | tr '[:upper:]' '[:lower:]' || true
    return 0
  fi
  [ -n "$target" ] || { printf ''; return 0; }
  # review 1138: use the SAME ssh password env as the gate's read_frame_probe_sha
  # (CAMERA_BOX_VERSION_GATE_SSH_PASS) -- the extra CAM_PW fallback the first cut carried would let
  # the align READ under a non-default CAM_PW while the [1/8] pin (gate) reads "" -> a spurious
  # UNVERIFIED alarm every run. The DEPLOY path (frame_probe_align_deploy) keeps CAM_PW: it drives
  # deploy-fleet.sh, whose own convention is SSH_PASS=${CAM_PW:-newlevel}.
  out="$(sshpass -p "${CAMERA_BOX_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CAMERA_BOX_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" 'sha256sum /usr/local/bin/frame-probe 2>/dev/null' 2>/dev/null || true)"
  printf '%s' "$out" | awk '{print $1}' | grep -oiE '^[0-9a-f]{64}$' | head -1 | tr '[:upper:]' '[:lower:]' || true
}

# frame_probe_align_resolve_ci_bin -> print the PATH to a VERSION-GUARDED clean CI frame-probe (the
# candidate build), or "" (dormant NOCANDIDATE downstream). Downloads probe-tools-linux-amd64 from
# the newest successful ci.yml run on the candidate branch and VERSION-GUARDS it: the artifact's
# co-located camera-box-probe --version MUST equal the Cargo.toml candidate -- else the candidate's
# own ci.yml has not published yet, so we do NOT align a stale build (self-heals once it completes),
# exactly like cambox_align_deploy's guard. The gh-download path creates a mktemp dir with the
# FRAME_PROBE_ALIGN_DIST_PREFIX name so the [1/8] pin can still read the returned frame-probe path
# for the rest of the run; this function runs inside `$(...)` (command substitution), so its
# `_FPPA_DIST` assignment does NOT escape to the caller -- the dir is therefore reclaimed by the
# AGE-BOUNDED sweep at the orchestrator's entry (frame_probe_parity_align_before_gate), not by the
# caller. The caller-supplied FRAME_PROBE_ALIGN_ARTIFACT_DIR (tests) is NEVER swept (different name).
#
# Seams: FRAME_PROBE_ALIGN_ARTIFACT_DIR (a pre-fetched probe-tools dir -> skip gh, still guarded);
# FRAME_PROBE_ALIGN_REPO / FRAME_PROBE_ALIGN_CI_BRANCH (artifact source, default zbynekdrlik/camera-box
# / dev); FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD=1 (tests with a fixture bin that has no --version).
FRAME_PROBE_ALIGN_DIST_PREFIX="${TMPDIR:-/tmp}/frame-probe-align-ci"
_FPPA_DIST=""
frame_probe_align_resolve_ci_bin() {
  local candidate dir="" fp cbp ver
  candidate="$(frame_probe_align_candidate_version)"
  if [ -n "${FRAME_PROBE_ALIGN_ARTIFACT_DIR:-}" ]; then
    dir="$FRAME_PROBE_ALIGN_ARTIFACT_DIR"
  else
    command -v gh >/dev/null 2>&1 || {
      echo "[0/8] frame-probe parity auto-align (#1138): gh CLI unavailable -- cannot source the candidate frame-probe artifact; the report-only pin below decides." >&2
      printf ''
      return 0
    }
    local repo="${FRAME_PROBE_ALIGN_REPO:-${REPO:-zbynekdrlik/camera-box}}"
    local branch="${FRAME_PROBE_ALIGN_CI_BRANCH:-dev}"
    local run_id
    run_id="$(gh run list --repo "$repo" --branch "$branch" --workflow ci.yml --status success --limit 1 --json databaseId -q '.[0].databaseId' 2>/dev/null || true)"
    if [ -z "$run_id" ]; then
      echo "[0/8] frame-probe parity auto-align (#1138): no successful ci.yml run on '$branch' to source probe-tools-linux-amd64; the report-only pin below decides." >&2
      printf ''
      return 0
    fi
    _FPPA_DIST="$(mktemp -d "${FRAME_PROBE_ALIGN_DIST_PREFIX}.XXXXXX")"
    dir="$_FPPA_DIST"
    if ! gh run download "$run_id" --repo "$repo" -n probe-tools-linux-amd64 --dir "$dir" >/dev/null 2>&1; then
      echo "[0/8] frame-probe parity auto-align (#1138): could not download probe-tools-linux-amd64 from ci.yml run $run_id; the report-only pin below decides." >&2
      printf ''
      return 0
    fi
  fi
  fp="$dir/frame-probe"
  if [ ! -f "$fp" ]; then
    echo "[0/8] frame-probe parity auto-align (#1138): candidate frame-probe missing in artifact ('$fp'); the report-only pin below decides." >&2
    printf ''
    return 0
  fi
  chmod +x "$fp" 2>/dev/null || true
  # VERSION-GUARD via the co-located camera-box-probe (frame-probe has no --version). Skippable for
  # tests whose fixture has no --version.
  if [ "${FRAME_PROBE_ALIGN_SKIP_VERSION_GUARD:-0}" != "1" ]; then
    cbp="$dir/camera-box-probe"
    if [ ! -x "$cbp" ]; then
      echo "[0/8] frame-probe parity auto-align (#1138): probe-tools artifact has no camera-box-probe to version-guard against; the report-only pin below decides." >&2
      printf ''
      return 0
    fi
    ver="$(frame_probe_align_binary_version "$cbp")"
    # review 1138: an UNRESOLVABLE candidate version ("") must REFUSE, not pass vacuously ("" == "").
    # The align DECISION keys on the sha, so unlike the 1202 sibling (whose deploy is unreachable
    # once the version is NOCANDIDATE) an empty candidate here would otherwise disable the guard
    # entirely and deploy an artifact whose version was never verified -- "never align blindly".
    if [ -z "$candidate" ] || [ "$ver" != "$candidate" ]; then
      echo "[0/8] frame-probe parity auto-align (#1138): candidate version=${candidate:-<unresolved>}, newest ci.yml probe build=${ver:-<unreadable>} -- NOT deploying (candidate unresolved, or its own ci.yml build not published yet; self-heals once it completes)." >&2
      printf ''
      return 0
    fi
  fi
  printf '%s' "$fp"
}

# frame_probe_align_deploy CI_BIN NAMES -> deploy the candidate frame-probe to /usr/local/bin/
# frame-probe across the space-separated cam NAMES (cam2 in practice), via deploy-fleet.sh's
# frame-probe-only mode (--frame-probe WITHOUT --binary -> the issue-892 enable-state-preserving
# painter deploy only, never a camera-box fleet deploy). Returns deploy-fleet's exit.
#
# Seams: FRAME_PROBE_ALIGN_DEPLOY_CMD (full override, run instead of the real deploy, with
# CAMERA_SET / FRAME_PROBE_ALIGN_CI_BIN exported); FRAME_PROBE_ALIGN_DEPLOY_FLEET (deploy-fleet.sh
# path; default the real sibling).
frame_probe_align_deploy() {
  local ci_bin="$1" names="$2"
  if [ -n "${FRAME_PROBE_ALIGN_DEPLOY_CMD:-}" ]; then
    CAMERA_SET="$names" FRAME_PROBE_ALIGN_CI_BIN="$ci_bin" bash -c "$FRAME_PROBE_ALIGN_DEPLOY_CMD"
    return $?
  fi
  local deploy_fleet="${FRAME_PROBE_ALIGN_DEPLOY_FLEET:-$_FPPA_HERE/../deploy-fleet.sh}"
  local rc=0
  CAMERA_SET="$names" SSH_PASS="${CAM_PW:-newlevel}" \
    "$deploy_fleet" --frame-probe "$ci_bin" || rc=$?
  return "$rc"
}

# frame_probe_parity_align_before_gate NODE_LIST -> the orchestrator recording-e2e.sh calls at [0/8].
# NODE_LIST is a space-separated "name=user@target" string (cam2 painter in practice). Resolves the
# candidate CI frame-probe, reads each box's deployed sha, decides via frame_probe_align_action, and
# on ALIGN deploys the candidate + logs. EXPORTS FRAME_PROBE_ALIGN_CI_BIN (the fetched CI frame-probe)
# so the [1/8] report pins against the TRUE source of truth (the CI artifact) rather than the dev1
# local build. ALWAYS returns 0 (the report-only pin is the loud signal). SKIPPED under --no-main-pin.
frame_probe_parity_align_before_gate() {
  local node_list="${1:-}"
  if [ "${CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN:-0}" = "1" ]; then
    echo "[0/8] frame-probe parity auto-align (#1138): SKIPPED -- --no-main-pin operator soak (never realign over a deliberately-deployed painter)."
    return 0
  fi
  # review 1138: bound the /tmp leak of the gh-downloaded probe-tools artifact. resolve_ci_bin runs
  # in `$(...)`, so its mktemp dir can't be reclaimed by this caller; instead sweep AGE-STALE
  # (>2h, well past any single E2E run) prior dirs here at entry -- the current run's dir survives
  # through the [1/8] pin and is reclaimed by the NEXT run's sweep. Age-bounded (not "all") so a
  # concurrent run's fresh dir is never removed mid-flight. Best-effort; a caller-supplied
  # FRAME_PROBE_ALIGN_ARTIFACT_DIR is a different name and is never matched/swept.
  find "$(dirname "$FRAME_PROBE_ALIGN_DIST_PREFIX")" -maxdepth 1 -type d \
    -name "$(basename "$FRAME_PROBE_ALIGN_DIST_PREFIX").*" -mmin +120 \
    -exec rm -rf {} + 2>/dev/null || true
  local ci_bin candidate_sha
  ci_bin="$(frame_probe_align_resolve_ci_bin)"
  if [ -z "$ci_bin" ]; then
    echo "[0/8] frame-probe parity auto-align (#1138): no candidate frame-probe resolved -- the report-only pin below decides."
    return 0
  fi
  candidate_sha="$(frame_probe_align_sha_of "$ci_bin")"
  # Export for the [1/8] pin: it now pins the deployed painter against the SAME CI artifact bytes.
  export FRAME_PROBE_ALIGN_CI_BIN="$ci_bin"

  local -a entries=() names=()
  local pair name target sha
  set -f
  # shellcheck disable=SC2086
  for pair in $node_list; do
    name="${pair%%=*}"
    target="${pair#*=}"
    sha="$(frame_probe_align_read_sha "$name" "$target")"
    entries+=("${name}=${sha}")
    cambox_offline_ack_is_acked "$name" && continue
    names+=("$name")
  done
  set +f
  if [ "${#entries[@]}" -eq 0 ]; then
    echo "[0/8] frame-probe parity auto-align (#1138): no painter node in the list -- nothing to align; the report-only pin below decides."
    return 0
  fi
  local action
  action="$(frame_probe_align_action "$candidate_sha" "${entries[@]}")"
  case "$action" in
    ALIGN)
      echo "[0/8] frame-probe parity auto-align (#1138): deployed painter LAGS the candidate (sha ${candidate_sha:0:12}) -- deploying it to /usr/local/bin/frame-probe before the run (${names[*]})."
      if frame_probe_align_deploy "$ci_bin" "${names[*]}"; then
        echo "[0/8] frame-probe parity auto-align (#1138): deploy complete -- cam2's painter is now on the candidate build; the report-only pin below confirms."
      else
        echo "[0/8] frame-probe parity auto-align (#1138): WARNING -- deploy did NOT complete cleanly; the report-only pin below SCREAMS the residual lag + the manual fix (deploy-fleet.sh --frame-probe)." >&2
      fi
      ;;
    OK)
      echo "[0/8] frame-probe parity auto-align (#1138): deployed painter already on the candidate (sha ${candidate_sha:0:12}) -- no deploy."
      ;;
    NOACTIVE)
      echo "[0/8] frame-probe parity auto-align (#1138): no active painter node remained (acked-offline) -- nothing to align."
      ;;
    UNKNOWN)
      echo "[0/8] frame-probe parity auto-align (#1138): painter frame-probe sha unread -- no auto-align; the report-only pin below fails LOUD (report-only)." >&2
      ;;
    NOCANDIDATE)
      echo "[0/8] frame-probe parity auto-align (#1138): no candidate sha resolvable -- skipping align; the report-only pin below decides." >&2
      ;;
  esac
  return 0
}
