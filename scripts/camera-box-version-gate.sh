#!/usr/bin/env bash
# camera-box-version-gate.sh — camera-box BINARY version gate: PIN-to-origin/main + cross-box parity (#875/#1136).
# See the extended header below for the full rationale/usage; set -e up front per
# script-failure-policy.md.
set -euo pipefail
#
# WHY THIS GATE EXISTS (issue 875, a deliberate follow-up split from issue 862). The camera-box
# binary is deployed CONTINUOUSLY (`1.7.0-dev.NNN` grows on almost every PR), so a box can silently
# fall behind and run objectively different behaviour than the rest of the fleet with nothing
# shouting about it — live 2026-07-29 cam4 ran three builds behind cam1/2/3 and was the ONLY box
# missing the publish-30p fix, found only by hand. The existing preconditions do not catch this:
# dantesync-version-gate.sh (issue 862) checks the dantesync DAEMON version, version-integrity-gate.sh
# (#123) checks the Windows OBS stack — neither ever reads the camera-box app binary's own version.
#
# COMPARISON MODEL — PIN-TO-MAIN (primary) + CROSS-BOX PARITY (supplement), issue 1136. The original
# #875 gate was PARITY-ONLY (no fixed pin), on the premise that a continuously-deployed dev build has
# "no single stable value to pin against". That premise had a hole the owner hit hard (issue 1136): a
# fleet where every box AGREES on an OLD build passes parity while running objectively different
# behaviour than main's release — live, the fleet ran dev.462 for a WEEK while main carried the #1111
# fix, and the parity gate stayed green because the WHOLE fleet was uniformly stale. The fix is a
# MOVING pin: every active box must run the version in origin/main's Cargo.toml. This is NOT the
# spurious-fail-prone FIXED pin the old header rejected — the pin is READ from origin/main, and the
# push-to-main auto-deploy (the ci.yml deploy-fleet job) pushes that SAME binary to the fleet, so the
# pin and the deployed reality move together with no stale-pin window (if the deploy fails, the gate
# SHOULD refuse — that is the point). Relative cross-box parity stays as a SUPPLEMENT: the dormant
# `--no-main-pin` path, for a deliberate pre-merge / operator soak where the fleet is knowingly not
# yet on main. This is exactly the doctrine .claude/rules/early-gate-pin-doctrine.md generalizes — an
# early gate PINS to the expected release and fails CLOSED on UNKNOWN; peer parity is a supplement,
# never a substitute.
#
# WHICH NODES. camera-box runs ONLY on the Linux cam boxes (the NDI senders) — NOT on imag-nb / dev1 /
# strih / stream. So this gate only takes --linux nodes, and the caller derives the active fleet from
# CAMERA_ACTIVE_SET (never a literal cam range — see .claude/rules/camera-active-set.md).
#
# UNAVAILABLE NODES. An unread node NEVER silently passes: it is either read, or explicitly EXCLUDED
# via the SAME CAMBOX_OFFLINE_ACK / rig-fleet.txt mechanism scripts/lib/cambox-offline-ack.sh already
# provides (#758/#827), reused verbatim exactly as issue 862's gate does — never a second mechanism.
#
# VERSION SOURCE. `/usr/local/bin/camera-box --version` prints `camera-box X.Y.Z-dev.NNN` and is read
# over the SAME SSH transport (absolute path, NOT relied on via the remote PATH — mirrors
# scripts/deploy-fleet.sh's own per-box version read).
#
# Usage:
#   camera-box-version-gate.sh [--fleet-file PATH] [--main-pin VERSION | --no-main-pin] \
#       --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 cam3=root@10.77.9.63"
#   camera-box-version-gate.sh --help
#
# Exit codes: 0 = every active box is on the origin/main pin (or, with --no-main-pin, agrees with its
#   peers) — rig test may proceed; 20 = a box is OFF the main pin / peers DISAGREE — REFUSED;
#   11 = a box UNKNOWN (version unread, not excluded) OR the main pin itself is UNREADABLE (fail
#   CLOSED, never a silent pass) — INCOMPLETE, NOT clean; 1 = usage / environment error.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"

DEFAULT_FLEET_FILE="$HERE/../rig-fleet.txt"

# --- PURE functions (no network, no SSH — unit-tested by sourcing this file) ------------------

# camera_box_version_from_version_output TEXT -> the camera-box version found in TEXT (the raw
# stdout of `camera-box --version`, i.e. "camera-box X.Y.Z-dev.NNN"). The LAST match wins — purely
# defensive robustness against an SSH banner/MOTD ever landing ahead of the real line, mirroring
# dantesync_version_from_version_output. "" when TEXT has no match (UNKNOWN downstream — an
# unreachable/unread box, never guessed).
camera_box_version_from_version_output() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -oE 'camera-box [0-9]+\.[0-9]+\.[0-9]+(-[.A-Za-z0-9]+)?' \
    | tail -1 \
    | sed 's/^camera-box //' || true
}

# camera_box_modal_version LIST_NEWLINE -> the MOST FREQUENT version among the non-empty lines in
# LIST_NEWLINE (the relative-parity REFERENCE the per-box verdict compares each box against), "" if
# none. Ties are broken by the lexicographically-SMALLEST version so the result is deterministic —
# the tie-break is presentational only (any disagreement fails the gate regardless of which value is
# named the "majority"). Empty lines (unread boxes) are ignored; they are graded UNKNOWN, never DRIFT.
camera_box_modal_version() {
  local list="$1"
  printf '%s\n' "$list" | sed '/^$/d' | sort | uniq -c \
    | sort -k1,1nr -k2,2 | awk 'NR == 1 { print $2; exit }'
}

# camera_box_version_verdict NAME VERSION MODAL -> prints ONE box->version table row and returns
# 0 OK / 20 DRIFT / 11 UNKNOWN. VERSION empty -> UNKNOWN (never a silent pass on an unread box).
# VERSION non-empty but != MODAL (the fleet majority) -> DRIFT. This is a RELATIVE peer compare, not
# a pin compare (issue 875): MODAL is derived from the fleet itself, there is no fixed expected value.
camera_box_version_verdict() {
  local name="$1" version="$2" modal="$3"
  if [ -z "$version" ]; then
    printf '  %-14s %-16s UNKNOWN  (camera-box version not read)\n' "$name" "-"
    return 11
  fi
  if [ "$version" != "$modal" ]; then
    printf '  %-14s %-16s DRIFT    (peers run %s)\n' "$name" "$version" "$modal"
    return 20
  fi
  printf '  %-14s %-16s OK\n' "$name" "$version"
  return 0
}

# camera_box_fleet_report ENTRY... -> ENTRY is "name=version" (version may be empty — an unread box).
# Consults CAMBOX_OFFLINE_ACK (cambox_offline_ack_is_acked/_reason, scripts/lib/cambox-offline-ack.sh)
# for a knowingly-offline box: reported EXCLUDED with its ack reason, never counted as UNKNOWN/DRIFT
# and never a reason to fail the gate. Computes the fleet's modal version from the READ (non-excluded,
# non-empty) boxes, prints the full box->version table, and returns 0 (every active box agrees) /
# 20 (>=1 box DRIFTED from the majority — the active fleet disagrees) / 11 (>=1 box UNKNOWN, no DRIFT).
camera_box_fleet_report() {
  local entry name version
  echo "== camera-box-version-gate (#875): cross-box camera-box binary version parity — relative, no pin =="
  # First pass: collect the READ (non-excluded, non-empty) versions to compute the majority reference.
  local read_versions=""
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    cambox_offline_ack_is_acked "$name" && continue
    [ -n "$version" ] || continue
    read_versions="${read_versions}${version}"$'\n'
  done
  local modal
  modal="$(camera_box_modal_version "$read_versions")"
  # Second pass: print the table + roll up the verdict.
  local bad=0 unknown=0 ok=0 rc
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      printf '  %-14s %-16s EXCLUDED (acked offline: %s)\n' "$name" "${version:--}" \
        "$(cambox_offline_ack_reason "$name")"
      continue
    fi
    rc=0
    camera_box_version_verdict "$name" "$version" "$modal" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)) ;;
    esac
  done
  echo
  local distinct
  distinct="$(printf '%s' "$read_versions" | sed '/^$/d' | sort -u | tr '\n' ' ')"
  # Name BOTH the DRIFT and UNKNOWN counts in the SAME banner (the issue-862 follow-up lesson): a
  # fleet that is mostly UNREADABLE must never read as a single minor drift.
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: active boxes run DIFFERENT camera-box versions — seen: ${distinct}(peer reference ${modal}); ${unknown} box(es) UNKNOWN — rig test REFUSED." >&2
    echo "!! A box on a different camera-box build runs objectively different behaviour than the rest of the fleet (issue 875 — cam4 once silently lacked the publish-30p fix)." >&2
    echo "!! Redeploy every active box to the SAME camera-box version (scripts/deploy-fleet.sh); fix SSH reachability for any UNKNOWN box; then re-run." >&2
    return 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN (camera-box version not read), 0 box(es) DRIFTED — NOT clean." >&2
    echo "!! Every active box must report its camera-box version before this parity gate is trusted. (${ok} on ${modal}.)" >&2
    return 11
  fi
  if [ "$ok" -eq 0 ]; then
    # No box was actually checked (bad==0 && unknown==0 here), so every listed box was acked-offline
    # -> a vacuous pass. Say so plainly rather than "agree on <none>", which reads like a real result.
    echo "GATE PASS (vacuous) — every listed box is acked-offline; no active box remained to compare."
    return 0
  fi
  echo "GATE PASS — ${ok} active box(es) agree on camera-box ${modal} (any acked-offline box excluded above)."
  return 0
}

# --- PIN-to-main layer (issue 1136) — PURE functions, unit-tested by sourcing ----------------

# camera_box_version_from_cargo_toml TEXT -> the [package] version in a Cargo.toml TEXT (the FIRST
# `version = "X"` line that starts at column 0 — a dependency version is `name = { version = "X" }`
# or lives under a `[dependencies]` table with its own key, never a line START of `version = `, so
# the `^version = "` anchor picks the package version alone). "" when TEXT has no such line (the
# caller then fails CLOSED — an unreadable pin is UNKNOWN, never a silent pass). Mirrors the pure
# parse shape of camera_box_version_from_version_output above.
camera_box_version_from_cargo_toml() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -m1 -oE '^version = "[^"]+"' \
    | sed -E 's/^version = "([^"]+)"$/\1/' || true
}

# camera_box_pin_verdict NAME VERSION PIN -> prints ONE box->version table row and returns
# 0 OK / 20 PIN-DRIFT / 11 UNKNOWN. VERSION empty -> UNKNOWN (never a silent pass on an unread box).
# VERSION non-empty but != PIN -> PIN-DRIFT (the box is NOT on main's release — this catches a single
# drifted box AND a UNIFORMLY-stale fleet, which relative parity alone misses). This is a PIN compare
# (issue 1136), mirroring dantesync_version_verdict — the expected value is main's Cargo.toml, not a
# fleet-derived modal.
camera_box_pin_verdict() {
  local name="$1" version="$2" pin="$3"
  if [ -z "$version" ]; then
    printf '  %-14s %-16s UNKNOWN   (camera-box version not read)\n' "$name" "-"
    return 11
  fi
  if [ "$version" != "$pin" ]; then
    printf '  %-14s %-16s PIN-DRIFT (expected main %s)\n' "$name" "$version" "$pin"
    return 20
  fi
  printf '  %-14s %-16s OK\n' "$name" "$version"
  return 0
}

# camera_box_fleet_candidate_uniform CANDIDATE ENTRY... -> 0 iff EVERY non-acked entry's version
# equals CANDIDATE exactly (an empty/unread version never matches — fail closed), else 1. The pure
# decision behind the #1136-addendum --candidate-pin accept: the pre-merge E2E deploys THIS run's
# merge-candidate build to the fleet to measure it, so a fleet uniformly on that ONE candidate is a
# valid measurement target; anything else (stale, mixed, unread) falls back to the main-pin refusal.
camera_box_fleet_candidate_uniform() {
  local candidate="$1"
  shift
  local entry name version
  [ -n "$candidate" ] || return 1
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then continue; fi
    [ "$version" = "$candidate" ] || return 1
  done
  return 0
}

# camera_box_fleet_report_pinned PIN ENTRY... -> ENTRY is "name=version" (version may be empty — an
# unread box). Grades every active box against PIN (origin/main's camera-box version), honouring the
# SAME CAMBOX_OFFLINE_ACK exclusion as the parity report. Prints the box->version table and returns
# 0 (every active box on the pin) / 20 (>=1 box OFF the pin — the fleet is NOT on main's release) /
# 11 (>=1 box UNKNOWN, none off-pin). Mirrors dantesync_fleet_report (the #862 pin gate) structure.
camera_box_fleet_report_pinned() {
  local pin="$1"
  shift
  local entry name version
  echo "== camera-box-version-gate (#875/#1136): camera-box binary PINNED to origin/main — pin ${pin} =="
  local bad=0 unknown=0 ok=0 rc
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      printf '  %-14s %-16s EXCLUDED (acked offline: %s)\n' "$name" "${version:--}" \
        "$(cambox_offline_ack_reason "$name")"
      continue
    fi
    rc=0
    camera_box_pin_verdict "$name" "$version" "$pin" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)) ;;
    esac
  done
  echo
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} active box(es) are NOT on the pinned main camera-box ${pin}, ${unknown} box(es) UNKNOWN — rig test REFUSED." >&2
    echo "!! A box on a camera-box build other than main's release runs objectively different behaviour than production (issue 1136 — the fleet ran a stale build for a WEEK while main carried the #1111 fix, and the parity-only gate stayed green because the WHOLE fleet was uniformly stale)." >&2
    echo "!! Redeploy every active box to main's camera-box ${pin} (the push-to-main auto-deploy job does this automatically; scripts/deploy-fleet.sh forces it); fix SSH for any UNKNOWN box; then re-run." >&2
    return 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN (camera-box version not read), 0 box(es) off-pin — NOT clean." >&2
    echo "!! Every active box must report its camera-box version before this pin gate is trusted. (${ok} on the pinned ${pin}.)" >&2
    return 11
  fi
  if [ "$ok" -eq 0 ]; then
    echo "GATE PASS (vacuous) — every listed box is acked-offline; no active box remained to compare against the pin ${pin}."
    return 0
  fi
  echo "GATE PASS — ${ok} active box(es) on the pinned main camera-box ${pin} (any acked-offline box excluded above)."
  return 0
}

# --- usage + SSH read (impure; not unit-tested) ----------------------------------------------

usage() {
  cat <<EOF
camera-box-version-gate.sh — cross-box camera-box BINARY version-parity precondition gate (#875).

By DEFAULT (issue 1136) PINS every active cam box's camera-box binary version to origin/main's
Cargo.toml — a UNIFORMLY-stale fleet is REFUSED, not just a single-box drift. REFUSES (non-zero) on
ANY box off the pin, any unread-and-unexcluded box, OR an unreadable pin (fail CLOSED). With
--no-main-pin it falls back to the legacy relative cross-box parity (peers agree) — for a deliberate
pre-merge / operator soak where the fleet is knowingly not yet on main.

Usage:
  camera-box-version-gate.sh [--fleet-file PATH] [--main-pin VERSION | --no-main-pin] \\
      --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 cam3=root@10.77.9.63"

Options:
  --fleet-file PATH default CAMBOX_OFFLINE_ACK source when the env var is unset (default:
                    ${DEFAULT_FLEET_FILE}) — same file recording-e2e.sh's fleet preflight reads.
  --main-pin VERSION  the expected main camera-box version (skips the git read). Also settable via
                    CAMERA_BOX_VERSION_GATE_MAIN_PIN. When unset, the pin is read from
                    \`git show origin/main:Cargo.toml\` (a best-effort \`git fetch origin main\` first).
  --no-main-pin     DISABLE the pin layer — relative cross-box parity only (legacy #875). Also
                    settable via CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN=1. The documented escape for a
                    deliberate pre-merge / operator soak where the fleet is not yet on main; the
                    automatic push:[dev,main] E2E gate NEVER sets it, so it always enforces the pin.
  --candidate-pin VERSION  a SECOND accepted uniform target (issue 1136 addendum): the merge-
                    candidate build THIS CI run built. Accepted ONLY when the whole active fleet is
                    uniformly on it (unread boxes stay fail-closed; stale/mixed fleets stay
                    refused) — the pre-merge bootstrap escape that does not reopen the
                    uniformly-stale hole. Also settable via CAMERA_BOX_VERSION_GATE_CANDIDATE_PIN.
  --linux "N=U@IP ..."  one or more SSH-reachable cam boxes (space-separated "name=user@ip" pairs
                    in ONE argument, mirrors dantesync-version-gate.sh's --linux). Repeatable. Read
                    via \`/usr/local/bin/camera-box --version\` over SSH.
  --frame-probe-only  run ONLY the #1138 frame-probe (cam2 painter) sha-pin over the --linux nodes
                    (skip the camera-box parity read). Report-only (exit 0) unless --frame-probe-hard.
  --frame-probe-hard  (issue 1235) flip --frame-probe-only into a HARD, fail-closed gate: exit
                    non-zero when the deployed painter LAGS the candidate CI build (30) or cannot be
                    verified (31 — painter sha unread, or the candidate sha unresolved). Used by the
                    recording-e2e [1/8] pin now the [0/8] auto-align is rig-proven to keep cam2 current.
  --frame-probe-expected-sha SHA | --frame-probe-expected-bin PATH  the candidate frame-probe sha to
                    pin against (the clean probe-tools CI artifact the [0/8] align fetched + deployed).

Exit: 0 = active boxes on the main pin (or, with --no-main-pin, agree) — proceed. 20 = a box OFF the
  pin / peers DISAGREE (REFUSED). 11 = a box UNKNOWN/unread, or the pin itself unreadable (fail
  CLOSED, INCOMPLETE). 1 = usage error. Under --frame-probe-hard: 30 = painter LAGS the candidate
  (REFUSED), 31 = painter unverifiable / candidate sha unresolved (fail CLOSED).
EOF
}

# read_camera_box_version_output NAME TARGET -> raw `camera-box --version` stdout for NAME (a cam
# box, "user@ip"). Read over SSH via the ABSOLUTE binary path (NOT the remote PATH — mirrors
# deploy-fleet.sh). "" if unreachable/absent (UNKNOWN downstream, never guessed). Overridable
# per-node for tests/offline via CAMERA_BOX_VERSION_GATE_VERSION_<NAME> (NAME uppercased, "-" -> "_")
# — mirrors dantesync-version-gate.sh's DANTESYNC_VERSION_GATE_VERSION_<NAME> fixture seam.
read_camera_box_version_output() {
  local name="$1" target="${2:-}" var
  var="CAMERA_BOX_VERSION_GATE_VERSION_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  [ -n "$target" ] || { printf ''; return 0; }
  sshpass -p "${CAMERA_BOX_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CAMERA_BOX_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    '/usr/local/bin/camera-box --version' 2>/dev/null || true
}

# read_main_cargo_version -> the camera-box package version on origin/main (the PIN), read from
# `git show origin/main:Cargo.toml`. This is a MOVING pin (issue 1136): it advances automatically
# when a version merges to main AND the push-to-main auto-deploy pushes that same binary to the
# fleet, so the pin and the deployed reality move together — no stale-pin spurious-fail window (the
# #875-header objection to a FIXED pin). "" on ANY failure (no git repo / no origin/main / no version
# line) -> the caller fails CLOSED (UNKNOWN=11), never a silent pass.
# Override seams (tests + operator/CI supply):
#   CAMERA_BOX_VERSION_GATE_MAIN_PIN=<version>       use this value directly (skip git)
#   CAMERA_BOX_VERSION_GATE_MAIN_PIN_UNAVAILABLE=1   simulate an unreadable pin (returns "")
read_main_cargo_version() {
  if [ -n "${CAMERA_BOX_VERSION_GATE_MAIN_PIN:-}" ]; then
    printf '%s' "$CAMERA_BOX_VERSION_GATE_MAIN_PIN"
    return 0
  fi
  if [ "${CAMERA_BOX_VERSION_GATE_MAIN_PIN_UNAVAILABLE:-0}" = "1" ]; then
    printf ''
    return 0
  fi
  local root cargo=""
  root="$(git -C "$HERE" rev-parse --show-toplevel 2>/dev/null)" || { printf ''; return 0; }
  # Best-effort refresh so origin/main is current (bounded; a fetch failure is tolerated — we fall
  # back to whatever origin/main ref is already present). Never let the fetch's own exit abort us.
  # Only trust FETCH_HEAD when THIS fetch just succeeded (a stale FETCH_HEAD from an unrelated prior
  # fetch would name the wrong ref); otherwise read the local origin/main ref.
  if git -C "$root" fetch --quiet --depth=1 origin main >/dev/null 2>&1; then
    cargo="$(git -C "$root" show FETCH_HEAD:Cargo.toml 2>/dev/null)" || cargo=""
  fi
  if [ -z "$cargo" ]; then
    cargo="$(git -C "$root" show origin/main:Cargo.toml 2>/dev/null)" || cargo=""
  fi
  [ -n "$cargo" ] || { printf ''; return 0; }
  camera_box_version_from_cargo_toml "$cargo"
}

# --- #1138 frame-probe (cam2 painter) sha-pin — REPORT-ONLY, dormant unless an expected sha is
# supplied ------------------------------------------------------------------------------------
#
# /usr/local/bin/frame-probe (the cam2 painter that paints /dev/fb0 + emits the QPSK marker) was
# UNPINNABLE (no --version -- it is probe-gated CI-built code) and in NO gate, and is NOT auto-
# deployed (only setup-device.sh provisioning installs it) -- so a stale painter's staleness was
# detected by NOTHING (the .claude/rules/early-gate-pin-doctrine.md "frame-probe UNPINNABLE" row).
# This section pins it by sha256 (the #1118 recording-verdict-on-imag sha-compare pattern, which
# needs no on-box --version): compare each active cam box's DEPLOYED frame-probe sha256 against an
# EXPECTED sha (the current CI probe-tools-linux-amd64 build). It is REPORT-ONLY (SCREAMS but never
# flips this gate's exit) AND DORMANT unless an expected sha is supplied (--frame-probe-expected-sha
# / --frame-probe-expected-bin / FRAME_PROBE_EXPECTED_SHA) -- so it is a no-op with no behaviour
# change unless an expected sha is supplied. This `frame_probe_pin_report` REMAINS report-only and is
# still the right shape for the [0/8] full-parity SUPPLEMENT + the --no-main-pin operator soak. The
# report-only->hard TWO-STEP has since LANDED (issue 1235): frame-probe now HAS a fleet auto-deploy
# (the [0/8] `frame_probe_parity_align_before_gate`, issue 1138, deploys the candidate painter every
# run), so the E2E [1/8] pin runs the HARD sibling `frame_probe_pin_gate` (two functions below) via
# --frame-probe-hard and REFUSES a lagging/unverifiable painter. The painter's correctness is ALSO
# gated functionally ([0/8] optical non-black #901 + marker CSV growth).

# frame_probe_pin_verdict NAME DEPLOYED_SHA EXPECTED_SHA -> one row + code (mirrors the #1118
# onimag_upload_decision / dantesync_tray_verdict sha compare).
#   DEPLOYED_SHA empty  -> UNKNOWN (31): frame-probe sha unread on the box (fail-closed-LOUD)
#   EXPECTED_SHA empty   -> UNKNOWN (31): the current-build expected sha could not be resolved
#   DEPLOYED != EXPECTED -> ALARM   (30): the deployed painter LAGS the current CI build (orphan)
#   else                 -> OK      (0):  the deployed painter matches the current build
# Prints to STDOUT (tests capture it); the report runner adds a stderr SCREAM banner on ALARM/UNKNOWN.
frame_probe_pin_verdict() {
  local name="$1" deployed="$2" expected="$3"
  if [ -z "$deployed" ]; then
    printf '  %-14s %-16s UNKNOWN   (frame-probe sha256 not read on the box -- fail-closed)\n' "$name" "-"
    return 31
  fi
  if [ -z "$expected" ]; then
    printf '  %-14s %-16s UNKNOWN   (current-build frame-probe sha unresolved -- cannot verify)\n' "$name" "${deployed:0:12}"
    return 31
  fi
  if [ "$deployed" != "$expected" ]; then
    printf '  %-14s %-16s ALARM     (deployed frame-probe LAGS the current build -- expected %s, redeploy the painter)\n' \
      "$name" "${deployed:0:12}" "${expected:0:12}"
    return 30
  fi
  printf '  %-14s %-16s OK        (frame-probe matches the current build)\n' "$name" "${deployed:0:12}"
  return 0
}

# resolve_frame_probe_expected_sha EXPECTED_SHA EXPECTED_BIN -> the expected frame-probe sha256
# (lowercase hex): EXPECTED_SHA wins; else sha256 of the local EXPECTED_BIN (the current CI build);
# else "" (the whole section stays dormant). Best-effort, never fails the caller.
resolve_frame_probe_expected_sha() {
  local expected_sha="${1:-}" expected_bin="${2:-}"
  if [ -n "$expected_sha" ]; then
    printf '%s' "$expected_sha" | tr '[:upper:]' '[:lower:]'
    return 0
  fi
  if [ -n "$expected_bin" ] && [ -f "$expected_bin" ]; then
    sha256sum "$expected_bin" 2>/dev/null | awk '{print $1}' | tr '[:upper:]' '[:lower:]' || true
  fi
}

# read_frame_probe_sha NAME TARGET -> the deployed /usr/local/bin/frame-probe sha256 (lowercase hex)
# for a cam box, over ssh. "" if unreachable/absent (UNKNOWN downstream). Override per-node for
# tests/offline via FRAME_PROBE_GATE_SHA_<NAME> (NAME uppercased, "-" -> "_").
read_frame_probe_sha() {
  local name="$1" target="${2:-}" var out
  var="FRAME_PROBE_GATE_SHA_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    printf '%s' "${!var}" | tr '[:upper:]' '[:lower:]'
    return 0
  fi
  [ -n "$target" ] || { printf ''; return 0; }
  out="$(sshpass -p "${CAMERA_BOX_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CAMERA_BOX_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    'sha256sum /usr/local/bin/frame-probe 2>/dev/null' 2>/dev/null || true)"
  printf '%s' "$out" | awk '{print $1}' | grep -oiE '^[0-9a-f]{64}$' | head -1 | tr '[:upper:]' '[:lower:]' || true
}

# frame_probe_pin_report EXPECTED_SHA PAIR... -> REPORT-ONLY: for each "name=target" PAIR, read the
# deployed frame-probe sha and grade it against EXPECTED_SHA. DORMANT (prints nothing, returns 0)
# when EXPECTED_SHA is empty. Never touches the caller's rc / the gate exit; a lagging/unread
# painter SCREAMS a table row + a stderr banner. Honours the SAME CAMBOX_OFFLINE_ACK exclusion.
frame_probe_pin_report() {
  local expected="$1"; shift
  [ -n "$expected" ] || return 0
  echo "-- frame-probe (cam2 painter) sha-pin (#1138, report-only, ALARM never blocks) --"
  local pair name target dep frc
  for pair in "$@"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    cambox_offline_ack_is_acked "$name" && continue
    dep="$(read_frame_probe_sha "$name" "$target")"
    frc=0
    frame_probe_pin_verdict "$name" "$dep" "$expected" || frc=$?
    case "$frc" in
      30) echo "!! FRAME-PROBE PIN ALARM: ${name} /usr/local/bin/frame-probe LAGS the current CI build -- redeploy the painter (report-only, does NOT block this run)." >&2 ;;
      31) echo "!! FRAME-PROBE PIN ALARM: could not verify ${name} /usr/local/bin/frame-probe against the current build (report-only)." >&2 ;;
    esac
  done
}

# frame_probe_pin_gate EXPECTED_SHA PAIR... -> HARD, fail-closed sibling of frame_probe_pin_report
# (issue 1235). Same per-node grading (reuses frame_probe_pin_verdict / read_frame_probe_sha and the
# SAME CAMBOX_OFFLINE_ACK exclusion) but it PROPAGATES the worst verdict rc instead of swallowing it,
# and it REFUSES (does NOT go dormant) on an EMPTY expected sha -- "couldn't verify" is a failure, per
# .claude/rules/early-gate-pin-doctrine.md, never a silent pass. Return codes reuse the verdict codes:
#   30 = ALARM   (a deployed painter LAGS the candidate CI build)
#   31 = UNKNOWN (a painter sha unread on the box, OR the candidate CI sha unresolved)
#    0 = OK      (every read painter matches the candidate)
# UNKNOWN (31) takes precedence over ALARM (30) -- "couldn't verify" is the strongest refuse (both
# codes REFUSE the run; in practice this gate is single-node/cam2, so the fold is only ever one code).
# WHY it can flip hard now: the [0/8] frame-probe auto-align (frame_probe_parity_align_before_gate) deploys
# the candidate painter to cam2 every E2E run, so a residual lag means a real deploy failure, not the
# perpetual-noise a pin-without-a-deploy would be -- rig-proven on the first green 7-cam series (the
# active deploy path + this pin's OK observed end-to-end). set -e safe: the acked-offline test uses the
# `A && continue` shape (A is never the final && command, so its failure does not trip set -e), exactly
# as frame_probe_pin_report does; the caller captures the return with `|| rc=$?`.
frame_probe_pin_gate() {
  local expected="$1"; shift
  # NB: the banner must NOT contain the bare words a per-node verdict row prints (ALARM / UNKNOWN),
  # or a test asserting "no verdict row" (an acked-offline box) would match the banner instead (#1235
  # review). It also keeps the positive ALARM/UNKNOWN row assertions honest (they can only be
  # satisfied by a real verdict row, never the banner).
  echo "-- frame-probe (cam2 painter) sha-pin (#1235, HARD gate, fail-closed on an unverified or lagging painter) --"
  local worst=0
  if [ -z "$expected" ]; then
    printf '  %-14s %-16s UNKNOWN   (candidate CI frame-probe sha unresolved -- the [0/8] align could not source probe-tools; cannot verify -- REFUSING)\n' "cam2" "-"
    echo "!! FRAME-PROBE PIN FAIL: candidate CI frame-probe sha unresolved (the [0/8] auto-align could not source probe-tools-linux-amd64) -- cannot verify the painter is current; REFUSING the run (fail-closed, issue 1235). Self-heals once ci.yml publishes the candidate / gh recovers; re-run then." >&2
    return 31
  fi
  local pair name target dep frc
  for pair in "$@"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    cambox_offline_ack_is_acked "$name" && continue
    dep="$(read_frame_probe_sha "$name" "$target")"
    frc=0
    frame_probe_pin_verdict "$name" "$dep" "$expected" || frc=$?
    case "$frc" in
      30)
        echo "!! FRAME-PROBE PIN FAIL: ${name} /usr/local/bin/frame-probe LAGS the candidate CI build -- the painter did not advance with the fleet; REFUSING the run (redeploy via deploy-fleet.sh --frame-probe, issue 1235)." >&2
        [ "$worst" -lt 30 ] && worst=30
        ;;
      31)
        echo "!! FRAME-PROBE PIN FAIL: could not verify ${name} /usr/local/bin/frame-probe against the candidate CI build (painter sha unread) -- REFUSING the run (fail-closed, issue 1235)." >&2
        worst=31
        ;;
    esac
  done
  return "$worst"
}

# --- source-guard: when sourced (the unit tests), stop here -----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ----------------------------------------------------
main() {
  local fleet_file="$DEFAULT_FLEET_FILE"
  local main_pin_opt=""
  local candidate_pin="${CAMERA_BOX_VERSION_GATE_CANDIDATE_PIN:-}"
  local no_main_pin="${CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN:-0}"
  # #1138 frame-probe sha-pin (report-only, dormant unless one of these is supplied).
  local fp_expected_sha="${FRAME_PROBE_EXPECTED_SHA:-}" fp_expected_bin=""
  # #1138 --frame-probe-only: run ONLY the report-only frame-probe sha-pin (skip the camera-box
  # parity read) and exit 0. recording-e2e engages this from [1/8], where the current-build
  # frame-probe exists (it is built at [1/8], AFTER the [0/8] camera-box parity gate — so the
  # expected bin cannot be wired into that earlier call, and re-running the whole parity gate would
  # print a confusing second table). cam2-scoped by its caller (frame-probe lives only on cam2).
  local frame_probe_only=0
  # #1235 --frame-probe-hard: flip the (otherwise report-only) --frame-probe-only mode into a HARD,
  # fail-closed gate that exits non-zero on a lagging/unverifiable painter (see frame_probe_pin_gate).
  local frame_probe_hard=0
  local -a linux_raw=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --fleet-file) shift; fleet_file="${1:-}" ;;
      --main-pin) shift; main_pin_opt="${1:-}" ;;
      --candidate-pin) shift; candidate_pin="${1:-}" ;;
      --no-main-pin) no_main_pin=1 ;;
      --frame-probe-expected-sha) shift; fp_expected_sha="${1:-}" ;;
      --frame-probe-expected-bin) shift; fp_expected_bin="${1:-}" ;;
      --frame-probe-only) frame_probe_only=1 ;;
      --frame-probe-hard) frame_probe_hard=1 ;;
      --linux) shift; linux_raw+=("${1:-}") ;;
      -h | --help)
        usage
        exit 0
        ;;
      --*)
        echo "unknown option: $1" >&2
        usage >&2
        exit 1
        ;;
      *)
        echo "unexpected argument: $1" >&2
        usage >&2
        exit 1
        ;;
    esac
    shift || true
  done

  # #1235: --frame-probe-hard hardens ONLY the --frame-probe-only pin (the full parity gate has its
  # OWN exit code); on the full invocation it would be a silent no-op, so reject the combination loud.
  if [ "$frame_probe_hard" = "1" ] && [ "$frame_probe_only" != "1" ]; then
    echo "ERROR: --frame-probe-hard requires --frame-probe-only (it hardens the frame-probe sha-pin, not the camera-box parity gate)." >&2
    usage >&2
    exit 1
  fi

  local -a linux_pairs=()
  local raw
  set -f
  for raw in "${linux_raw[@]}"; do
    # shellcheck disable=SC2206
    linux_pairs+=($raw)
  done
  set +f

  if [ "${#linux_pairs[@]}" -eq 0 ]; then
    echo "ERROR: no node to gate (--linux is empty)." >&2
    echo "The camera-box version-parity gate cannot certify the fleet with zero nodes — refusing to pass." >&2
    exit 1
  fi

  CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$fleet_file")"
  export CAMBOX_OFFLINE_ACK

  # #1138 --frame-probe-only: run ONLY the report-only frame-probe sha-pin over the given nodes and
  # exit 0 (report-only ALWAYS — a lagging painter SCREAMS but never flips the exit). Skips the
  # camera-box parity read entirely, so no camera-box --version fixture / origin/main pin is needed.
  # This is how recording-e2e engages the DORMANT report from [1/8] (where the current-build
  # frame-probe exists), cam2-scoped, without re-running the [0/8] parity gate.
  if [ "$frame_probe_only" = "1" ]; then
    local fp_only_expected
    fp_only_expected="$(resolve_frame_probe_expected_sha "$fp_expected_sha" "$fp_expected_bin")"
    # #1235: --frame-probe-hard flips this from report-only (always exit 0) to a fail-closed HARD gate
    # that exits non-zero on a lagging/unverifiable painter. Report-only stays the default so the
    # (dormant) [0/8] full-parity supplement and the --no-main-pin operator soak are byte-unchanged.
    if [ "$frame_probe_hard" = "1" ]; then
      local fp_rc=0
      frame_probe_pin_gate "$fp_only_expected" "${linux_pairs[@]}" || fp_rc=$?
      exit "$fp_rc"
    fi
    frame_probe_pin_report "$fp_only_expected" "${linux_pairs[@]}"
    exit 0
  fi

  local -a entries=()
  local pair name target version
  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(camera_box_version_from_version_output "$(read_camera_box_version_output "$name" "$target")")"
    entries+=("${name}=${version}")
  done

  # --- pin layer (issue 1136): PIN-to-origin/main is the DEFAULT; --no-main-pin drops to the legacy
  # relative-parity supplement. An explicit --main-pin / CAMERA_BOX_VERSION_GATE_MAIN_PIN wins over
  # the git read (read_main_cargo_version honours the same env seam). An unreadable pin fails CLOSED
  # (exit 11) — a fleet must NEVER run on a random/stale build unverified (the owner's hard rule).
  # #1138: resolve the expected frame-probe sha ONCE (dormant "" unless supplied).
  local fp_expected
  fp_expected="$(resolve_frame_probe_expected_sha "$fp_expected_sha" "$fp_expected_bin")"

  local rc=0
  if [ "$no_main_pin" = "1" ]; then
    echo "[camera-box-version-gate] PIN layer DISABLED (--no-main-pin) — relative cross-box parity only (#875)." >&2
    camera_box_fleet_report "${entries[@]}" || rc=$?
    frame_probe_pin_report "$fp_expected" "${linux_pairs[@]}"
    exit "$rc"
  fi

  local pin
  if [ -n "$main_pin_opt" ]; then
    pin="$main_pin_opt"
  else
    pin="$(read_main_cargo_version)"
  fi
  if [ -z "$pin" ]; then
    echo "ERROR: [camera-box-version-gate] could not determine the origin/main camera-box pin (Cargo.toml unreadable) — failing CLOSED (issue 1136)." >&2
    echo "The pin gate MUST know main's expected camera-box version; a fleet must NOT run on a random/stale build unverified. Run inside a git checkout with origin/main reachable, or pass --main-pin <version> / --no-main-pin (the documented escape)." >&2
    exit 11
  fi
  camera_box_fleet_report_pinned "$pin" "${entries[@]}" || rc=$?
  # --- #1136 addendum: candidate-pin accept — the pre-merge bootstrap escape that does NOT
  # reopen the uniformly-stale hole. Only an off-pin refusal (20) is reconsidered, and ONLY when
  # the whole active fleet is uniformly on the ONE named candidate (this run's own merge-candidate
  # build). UNKNOWN (11) stays fail-closed; a stale or mixed fleet stays refused.
  if [ "$rc" -eq 20 ] && [ -n "$candidate_pin" ] && [ "$candidate_pin" != "$pin" ]; then
    if camera_box_fleet_candidate_uniform "$candidate_pin" "${entries[@]}"; then
      echo "GATE PASS — active fleet is uniformly on the CANDIDATE build ${candidate_pin} (this run's merge candidate; main pin ${pin} not yet advanced — issue 1136 pre-merge accept)."
      rc=0
    fi
  fi
  frame_probe_pin_report "$fp_expected" "${linux_pairs[@]}"
  exit "$rc"
}

main "$@"
