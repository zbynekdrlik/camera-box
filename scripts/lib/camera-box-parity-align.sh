#!/usr/bin/env bash
# airuleset:script-ok source-only lib (functions only; sourced into a caller that owns its own
# shell options) -- mirrors the scripts/lib/cbox-burn-log-persist.sh / cambox-offline-ack.sh
# convention (no top-level `set -euo pipefail`: a sourced lib must never mutate the caller's opts).
#
# scripts/lib/camera-box-parity-align.sh -- pre-gate auto-align of the active cam fleet to THIS
# run's candidate camera-box build, so the [0/8] camera-box version-parity gate's existing
# --candidate-pin accept passes WITHOUT a manual deploy-fleet on the version-parity treadmill
# (issue 1202).
#
# WHY (root cause). camera-box-version-gate.sh (#875/#1136) reads each active box's
# /usr/local/bin/camera-box --version and PINS it to origin/main, with a --candidate-pin ACCEPT
# that passes only when the whole active fleet is uniformly on THIS run's candidate build. During
# active dev, origin/main lags dev by dozens of builds (live killed run 32883434208: pin dev.481,
# fleet cam3 dev.550, candidate dev.551), so the candidate-pin accept is the only passing path --
# but each dev commit bumps the candidate, leaving the fleet one build behind (candidate-1).
# recording-e2e.sh [2/8]/[2b/8] DO scp the candidate binary to each box, but only to a transient
# /tmp/camera-box-burn-* path run via systemd-run -- they never write /usr/local/bin/camera-box
# (what the gate reads) and run AFTER the [0/8] gate. So the gate refuses every run until a manual
# deploy-fleet. This lib closes the ORDER gap: BEFORE the gate, when the fleet is uniformly on ONE
# stale build != candidate, it deploys the candidate to /usr/local/bin/camera-box fleet-wide (via
# the existing deploy-fleet.sh), so the gate's own candidate-pin accept then passes.
#
# MIXED-FLEET PROTECTION IS PRESERVED. Only the ALIGN verdict (every active box read AND uniform on
# ONE version != candidate) authorises a deploy. Versions differing BETWEEN boxes (MIXED) or any
# unread box (UNKNOWN) -> NO deploy -> the UNTOUCHED gate refuses exactly as before. So the align is
# a best-effort pre-step; the version-parity gate stays the single authority.
_CBPA_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$_CBPA_HERE/cambox-offline-ack.sh"

# --- PURE decision (no network, no SSH -- unit-tested by sourcing this file) -------------------

# cambox_align_action CANDIDATE ENTRY... -> prints ONE verdict word and returns 0. ENTRY is
# "name=version" (version may be empty -- an unread box). Consults CAMBOX_OFFLINE_ACK
# (cambox_offline_ack_is_acked) exactly as the gate does: an acked-offline box is EXCLUDED from the
# decision. Verdicts:
#   NOCANDIDATE  candidate empty                     -> no align; the gate decides
#   UNKNOWN      >=1 active box unread (empty version)-> no align, fail closed (a deploy would
#                                                        target an unreachable box); the gate=11
#   NOACTIVE     no active (non-acked) box remained   -> nothing to align; the gate vacuous-passes
#   MIXED        active boxes DISAGREE                 -> no align (mixed protection); the gate=20
#   OK           every active box already == candidate -> no align; the gate passes via candidate-pin
#   ALIGN        every active box read AND uniform on ONE version != candidate -> deploy the candidate
# UNKNOWN takes precedence over MIXED (an unread box is never safely aligned regardless).
cambox_align_action() {
  local candidate="$1"
  shift
  if [ -z "$candidate" ]; then
    printf 'NOCANDIDATE\n'
    return 0
  fi
  local entry name version first="" have_first=0 uniform=1 any=0 unknown=0
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      continue
    fi
    if [ -z "$version" ]; then
      unknown=1
      continue
    fi
    any=1
    if [ "$have_first" -eq 0 ]; then
      first="$version"
      have_first=1
    elif [ "$version" != "$first" ]; then
      uniform=0
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
  if [ "$uniform" -eq 0 ]; then
    printf 'MIXED\n'
    return 0
  fi
  if [ "$first" = "$candidate" ]; then
    printf 'OK\n'
    return 0
  fi
  printf 'ALIGN\n'
  return 0
}

# cambox_align_version_from_output TEXT -> the camera-box version in TEXT (raw `camera-box
# --version` stdout). LAST match wins (defensive against a leading SSH banner), "" if none.
# Mirrors camera-box-version-gate.sh's own parser so the align + the gate read identically.
cambox_align_version_from_output() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -oE 'camera-box [0-9]+\.[0-9]+\.[0-9]+(-[.A-Za-z0-9]+)?' \
    | tail -1 \
    | sed 's/^camera-box //' || true
}

# cambox_align_candidate_version -> THIS run's candidate camera-box version = the [workspace.package]
# version in Cargo.toml (the SAME value recording-e2e.sh passes to the gate as --candidate-pin).
# Override for tests via CAMBOX_ALIGN_CANDIDATE / CAMBOX_ALIGN_CARGO_TOML. "" if unresolvable
# (the orchestrator then no-ops with NOCANDIDATE; never a silent wrong align).
cambox_align_candidate_version() {
  if [ -n "${CAMBOX_ALIGN_CANDIDATE:-}" ]; then
    printf '%s' "$CAMBOX_ALIGN_CANDIDATE"
    return 0
  fi
  local cargo="${CAMBOX_ALIGN_CARGO_TOML:-$_CBPA_HERE/../../Cargo.toml}"
  [ -f "$cargo" ] || {
    printf ''
    return 0
  }
  grep -m1 -oE '^version = "[^"]+"' "$cargo" 2>/dev/null \
    | sed -E 's/^version = "([^"]+)"$/\1/' || true
}

# --- impure read + deploy (not unit-tested for their SSH/deploy side; the orchestrator IS exercised
# end-to-end via the CAMERA_BOX_VERSION_GATE_VERSION_<NAME> read seam + CAMBOX_ALIGN_DEPLOY_CMD) ---

# cambox_align_read_version NAME TARGET -> the box's /usr/local/bin/camera-box version, over SSH via
# the ABSOLUTE binary path. "" if unreachable/absent (UNKNOWN downstream, never guessed). Overridable
# per-node for tests/offline via the SAME seam the gate uses -- CAMERA_BOX_VERSION_GATE_VERSION_<NAME>
# (NAME uppercased, "-" -> "_") pointing at a file holding raw `camera-box --version` output -- so a
# test drives the align and the gate identically.
cambox_align_read_version() {
  local name="$1" target="${2:-}" var
  var="CAMERA_BOX_VERSION_GATE_VERSION_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cambox_align_version_from_output "$(cat "${!var}" 2>/dev/null || true)"
    return 0
  fi
  [ -n "$target" ] || {
    printf ''
    return 0
  }
  cambox_align_version_from_output "$(sshpass -p "${CAM_PW:-${CAMERA_BOX_VERSION_GATE_SSH_PASS:-newlevel}}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${CAMERA_BOX_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" '/usr/local/bin/camera-box --version' 2>/dev/null || true)"
}

# cambox_align_deploy CANDIDATE NAMES -> deploy THIS run's candidate binary to /usr/local/bin/
# camera-box across the space-separated cam NAMES, via the existing deploy-fleet.sh (its full
# stop->rw->scp->byte-verify->start->version-verify->genlock-emit cycle; it REFUSES on any
# mismatch). Returns deploy-fleet's exit. Reuses $PROBE_BIN_DIR/camera-box -- this run's candidate
# build already in hand (version-identical to the Cargo.toml candidate; the #174 burn is
# runtime-gated OFF in production, so it is behaviourally identical to the clean binary with the
# burn env unset) -- so no second artifact download/build; the next push-to-main auto-deploy
# overwrites /usr/local/bin/camera-box with the clean camera-box-linux-amd64. Test seam:
# CAMBOX_ALIGN_DEPLOY_CMD (run instead of the real deploy, with CAMERA_SET / CAMBOX_ALIGN_CANDIDATE
# exported); binary override: CAMBOX_ALIGN_BINARY.
cambox_align_deploy() {
  local candidate="$1" names="$2"
  if [ -n "${CAMBOX_ALIGN_DEPLOY_CMD:-}" ]; then
    CAMERA_SET="$names" CAMBOX_ALIGN_CANDIDATE="$candidate" bash -c "$CAMBOX_ALIGN_DEPLOY_CMD"
    return $?
  fi
  local binary="${CAMBOX_ALIGN_BINARY:-${PROBE_BIN_DIR:-target/release}/camera-box}"
  if [ ! -x "$binary" ]; then
    echo "[0/8] camera-box parity auto-align (#1202): candidate binary '$binary' missing/not executable -- cannot align; the gate below decides." >&2
    return 1
  fi
  CAMERA_SET="$names" SSH_PASS="${CAM_PW:-newlevel}" \
    "$_CBPA_HERE/../deploy-fleet.sh" --binary "$binary"
}

# cambox_parity_align_before_gate NODE_LIST -> the orchestrator recording-e2e.sh calls right before
# the [0/8] camera-box version-parity gate. NODE_LIST is the SAME space-separated "name=user@target"
# string it passes to the gate as --linux. Reads each box's version, decides via cambox_align_action
# against the Cargo.toml candidate, and on ALIGN deploys the candidate + logs. ALWAYS returns 0 --
# the version-parity gate that runs immediately after is the authority (a failed/partial align just
# leaves the gate to REFUSE loudly, unchanged). SKIPPED under the --no-main-pin operator soak (the
# gate is in relative-parity mode; never auto-realign over a build an operator deliberately deployed).
cambox_parity_align_before_gate() {
  local node_list="${1:-}"
  if [ "${CAMERA_BOX_VERSION_GATE_NO_MAIN_PIN:-0}" = "1" ]; then
    echo "[0/8] camera-box parity auto-align (#1202): SKIPPED -- --no-main-pin operator soak (gate in relative-parity mode; never auto-realign over a deliberately-deployed build)."
    return 0
  fi
  local candidate
  candidate="$(cambox_align_candidate_version)"
  local -a entries=() names=()
  local pair name target version
  # NODE_LIST is intentionally word-split into "name=target" pairs (same shape the gate consumes).
  # shellcheck disable=SC2086
  for pair in $node_list; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(cambox_align_read_version "$name" "$target")"
    entries+=("${name}=${version}")
    names+=("$name")
  done
  if [ "${#entries[@]}" -eq 0 ]; then
    echo "[0/8] camera-box parity auto-align (#1202): no cam nodes in the list -- nothing to align; the gate below decides."
    return 0
  fi
  local action
  action="$(cambox_align_action "$candidate" "${entries[@]}")"
  case "$action" in
    ALIGN)
      echo "[0/8] camera-box parity auto-align (#1202): active fleet uniformly on a stale build, candidate ${candidate} -- deploying the candidate to /usr/local/bin/camera-box before the gate (${names[*]})."
      if cambox_align_deploy "$candidate" "${names[*]}"; then
        echo "[0/8] camera-box parity auto-align (#1202): deploy complete -- the version-parity gate below now sees the fleet on ${candidate}."
      else
        echo "[0/8] camera-box parity auto-align (#1202): WARNING -- deploy did NOT complete cleanly; the version-parity gate below is the authority and will REFUSE if the fleet is not on ${candidate}." >&2
      fi
      ;;
    OK)
      echo "[0/8] camera-box parity auto-align (#1202): active fleet already on the candidate ${candidate} -- no deploy."
      ;;
    MIXED | NOACTIVE)
      echo "[0/8] camera-box parity auto-align (#1202): no auto-align (${action}) -- versions differ BETWEEN boxes or no active box remained; the version-parity gate below decides (mixed fleets stay REFUSED)."
      ;;
    UNKNOWN)
      echo "[0/8] camera-box parity auto-align (#1202): no auto-align -- a box's camera-box version is unread; the version-parity gate below fails CLOSED."
      ;;
    NOCANDIDATE)
      echo "[0/8] camera-box parity auto-align (#1202): no candidate version resolvable from Cargo.toml -- skipping align; the gate below decides." >&2
      ;;
  esac
  return 0
}
