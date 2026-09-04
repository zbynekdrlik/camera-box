#!/usr/bin/env bash
# dantesync-version-gate.sh — fleet-wide dantesync VERSION-PARITY precondition gate (#862).
# See the extended header comment below for the full rationale/usage; set -e up front per
# script-failure-policy.md.
set -euo pipefail
#
# WHY THIS GATE EXISTS (the user's hard, repeated requirement: "nechcem vidieť rozídené verzie" —
# a mixed/stale fleet must never be discoverable only by eye or by post-mortem of a failed run).
# The existing preconditions (dantesync-gate.sh's NTP+PTP check, #7; version-integrity-gate.sh's
# OBS/DistroAV/NDI pinned-set check, #123) both measure LIVE BEHAVIOUR of a daemon/build — neither
# ever checks the dantesync DAEMON's OWN VERSION. So a fleet running a pre-#53-burst-filter
# dantesync (#836/#851: a strictly WORSE measurement instrument — coin-flip individual NTP
# samples instead of a filtered median) passes both existing gates cleanly and can still silently
# corrupt every downstream cross-node latency/timestamp number this harness produces.
#
# This is a SEPARATE script from version-integrity-gate.sh (which is scoped to the Windows
# strih/stream OBS stack only) because dantesync ALSO runs on every Linux cam box AND on dev1
# itself — the control box that RUNS this very gate. dev1 is checked exactly like every other
# node (#862 point 2: the harness's own host is never exempt just because it is convenient).
#
# COMPARISON MODEL (#862 point 3): every node's observed version is compared against ONE PINNED
# expected version (DANTESYNC_VERSION_PIN) — NOT merely "do the boxes agree with each other". A
# fleet that uniformly agrees on a STALE version must still FAIL; this is a pin check, not a
# parity-between-peers check (contrast scripts/drift-guard.sh's genlock_build_sha CROSS-BOX
# parity, which genuinely has no fixed pin because every build's SHA is unique).
#
# UNAVAILABLE NODES (#862 point 4): an unread node NEVER silently passes. It is either read, or
# explicitly EXCLUDED via the SAME CAMBOX_OFFLINE_ACK / rig-fleet.txt mechanism
# scripts/lib/cambox-offline-ack.sh already provides for an offline cambox (#758/#827) — reused
# here verbatim, never a second exclusion mechanism invented for this gate.
#
# VERSION SOURCE (#862 follow-up, 2026-07-30): the ORIGINAL version of this gate assumed dantesync
# has no readable version on Windows and read a startup log/journal line instead
# (`journalctl -u dantesync` on Linux, the Windows service log via bundle-state). BOTH sources
# turned out empty on the real fleet — `journalctl -u dantesync` never actually carries a version
# line on Linux, and the strih/stream bundle-state servers deployed at the time never picked up
# the new `dantesync_version` key — so the gate hard-blocked every E2E run at [0/8] with 7 of 8
# boxes UNKNOWN (see the #862 supervisor-verification comment). Live re-check found the actual
# answer simpler: `dantesync --version` prints `dantesync X.Y.Z` and answers on EVERY platform —
# Linux, Windows, and dev1 itself — over the SAME SSH transport this repo already uses elsewhere
# (drift-guard.sh's `gather_and_check_imag`, dantesync-gate.sh's Linux journal reads). One uniform
# reader (`read_dantesync_version_output`, below) now covers every node kind; the bundle-state
# coupling and the journal/log parser are gone (see .claude/rules/dantesync-version-reading.md for
# the corrected read-path notes, and scripts/bundle_state_gather.py /
# scripts/bundle-state-server.py for the reverted #862 additions).
#
# Usage:
#   dantesync-version-gate.sh [--pin VERSION] [--fleet-file PATH] \
#       --linux "cam1=root@10.77.9.61 cam2=root@10.77.9.62 imag-nb=newlevel@10.77.9.182" \
#       --local dev1 \
#       --win "strih=newlevel@10.77.9.202 stream=newlevel@10.77.9.204"
#   dantesync-version-gate.sh --help
#
# Exit codes: 0 = every node matches the pin (or is knowingly excluded) — rig test may proceed,
#   20 = at least one node DRIFTED (version present but wrong) — REFUSED,
#   11 = at least one node UNKNOWN (version unread, not excluded) — INCOMPLETE, NOT clean,
#   1  = usage / environment error.

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$HERE/lib/cambox-offline-ack.sh"

DEFAULT_FLEET_FILE="$HERE/../rig-fleet.txt"
# The fleet-wide pin (#862 point 3). Bumped 2026-07-30 (follow-up fix) to the supervisor's
# fleet-convergence target — a deliberate, bump-on-purpose value in the SAME spirit as
# verify-device.sh's NDI_VERSION_PIN: whoever executes a dantesync fleet upgrade (#851) bumps this
# alongside the deploy, so the gate never silently drifts to "whatever happens to be out".
# Bumped 2026-08-11: fleet rolled to v1.8.30 (dantesync NTP server-mode self-discipline fix —
# the master no longer free-runs at oscillator error; verified 8/8 boxes on the rollout day).
# Bumped 2026-08-11 (same day, evening): fleet rolled to v1.8.32 (master sawtooth collapse,
# dantesync issue 71 — ramp-aware agreement + 10s cadence + small-offset fast lane; strih canary
# read residual 0us on 13/13 consecutive samples, PTP LOCK held; verified 8/8 boxes).
# Bumped 2026-08-12: fleet rolled to v1.8.41 (dantesync issue 83, PR 84/86 — PTP-locked master
# defers its UTC-phase step to a 2500us deadband, additively reporting it via ntp_deadband_us;
# camera-box's own gate change for this is issue 1021; verified 8/8 boxes -- strih, stream,
# cam1-4, imag-nb, dev1).
# Bumped 2026-08-16: fleet rolled to v1.8.42 (dantesync PR 89 — GM-source allowlist drops a
# foreign-subnet grandmaster before adoption; camera-box issue 1073. Empty allowlist = unchanged
# last-writer-wins, so non-stream boxes are behaviorally identical; stream gets gm_allowlist).
# Bumped 2026-08-16 (same day, evening): fleet rolled to v1.8.43 (dantesync PR 90 — multi-homed
# PTP capture interface selection by trusted GM subnet; stream locked to the RIG grandmaster
# 10.77.9.184 for the first time — the live-event stutter root cause, issue 1073 forensics).
# Bumped 2026-09-03: fleet rolled to v1.8.53 (dantesync#109 — the pcap IGMP-join socket no longer
# binds UDP 319/320, so a Dante Virtual Soundcard ptp.exe on the same host (stream) can lock to the
# GM; canary stream verified 14:3xZ, rest of the fleet rolled after the owner's production window).
DANTESYNC_VERSION_PIN="${DANTESYNC_VERSION_PIN:-1.8.53}"

# --- PURE functions (no network, no SSH — unit-tested by sourcing this file) ------------------

# dantesync_version_from_version_output TEXT -> the dantesync version found in TEXT, which is the
# raw stdout of a `dantesync --version` invocation ("dantesync X.Y.Z" — confirmed live 2026-07-30
# on every managed platform: Linux console, Windows service exe, and dev1 itself). The LAST match
# wins, never the first — purely defensive robustness against SSH banner/MOTD noise ever landing
# ahead of the real line, mirroring the "freshest wins" discipline this gate's now-removed
# journal-based reader used for the same reason. "" when TEXT has no match (UNKNOWN downstream —
# unreachable/unread, never guessed).
dantesync_version_from_version_output() {
  local text="$1"
  printf '%s\n' "$text" \
    | grep -oE 'dantesync [0-9]+\.[0-9]+\.[0-9]+' \
    | tail -1 \
    | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' || true
}

# dantesync_version_verdict NAME VERSION PIN -> prints ONE box->version table row and returns
# 0 OK / 20 DRIFT / 11 UNKNOWN. VERSION empty -> UNKNOWN (never a silent pass on an unread node).
# VERSION non-empty but != PIN -> DRIFT — this is a PIN compare, not a peer-agreement compare, so
# a uniformly-stale fleet fails exactly as loudly as a mixed one (#862 point 3).
dantesync_version_verdict() {
  local name="$1" version="$2" pin="$3"
  if [ -z "$version" ]; then
    printf '  %-14s %-12s UNKNOWN  (dantesync version not read)\n' "$name" "-"
    return 11
  fi
  if [ "$version" != "$pin" ]; then
    printf '  %-14s %-12s DRIFT    (expected %s)\n' "$name" "$version" "$pin"
    return 20
  fi
  printf '  %-14s %-12s OK\n' "$name" "$version"
  return 0
}

# dantesync_fleet_report PIN ENTRY... -> ENTRY is "name=version" (version may be empty — an
# unread node). Consults CAMBOX_OFFLINE_ACK (cambox_offline_ack_is_acked/_reason,
# scripts/lib/cambox-offline-ack.sh) for a knowingly-offline node: reported EXCLUDED with its ack
# reason, never counted as UNKNOWN/DRIFT and never a reason to fail the gate (#862 point 4). Prints
# the full box->version table (#862 point 5) and returns 0 (every non-excluded node OK) /
# 20 (>=1 DRIFT) / 11 (>=1 UNKNOWN, no DRIFT).
dantesync_fleet_report() {
  local pin="$1"
  shift
  echo "== dantesync-version-gate (#862): fleet-wide dantesync version parity — pin ${pin} =="
  local bad=0 unknown=0 ok=0 entry name version rc
  for entry in "$@"; do
    name="${entry%%=*}"
    version="${entry#*=}"
    if cambox_offline_ack_is_acked "$name"; then
      printf '  %-14s %-12s EXCLUDED (acked offline: %s)\n' "$name" "${version:--}" \
        "$(cambox_offline_ack_reason "$name")"
      continue
    fi
    rc=0
    dantesync_version_verdict "$name" "$version" "$pin" || rc=$?
    case "$rc" in
      0) ok=$((ok + 1)) ;;
      20) bad=$((bad + 1)) ;;
      11) unknown=$((unknown + 1)) ;;
    esac
  done
  echo
  # #862 follow-up: name BOTH the DRIFT and UNKNOWN counts in the SAME top banner line, never
  # UNKNOWN-as-an-aside — a fleet that is mostly UNREADABLE (7 of 8 boxes, the live incident this
  # fixes) must never read as "1 box(es) run a dantesync version other than the pinned", which
  # undersells an almost-total read failure as a single minor drift.
  if [ "$bad" -gt 0 ]; then
    echo "!! GATE FAILED: ${bad} box(es) DRIFTED from the pinned ${pin}, ${unknown} box(es) UNKNOWN (version not read) — rig test REFUSED." >&2
    echo "!! A drifted clock daemon can measure the offset worse (#851) and hide behind an otherwise-healthy run." >&2
    echo "!! Upgrade the box(es) named DRIFT above to ${pin}; fix SSH/dantesync reachability for the box(es) named UNKNOWN; then re-run." >&2
    return 20
  fi
  if [ "$unknown" -gt 0 ]; then
    echo "!! GATE INCOMPLETE: ${unknown} box(es) UNKNOWN (dantesync version not read), 0 box(es) DRIFTED — NOT clean." >&2
    echo "!! Every managed node must report its dantesync version before this gate is trusted. (${ok} OK.)" >&2
    return 11
  fi
  echo "GATE PASS — ${ok} box(es) on the pinned dantesync ${pin} (any acked-offline box excluded above)."
  return 0
}

# --- #1139 REPORT-ONLY orphan alarms: tray sha-pin + pin-vs-newest-release lag -----------------
#
# The daemon gate above is a HARD version pin. Two orphan holes the doctrine
# (.claude/rules/early-gate-pin-doctrine.md) names remain, both REPORT-ONLY here (they SCREAM but
# never flip the gate exit -- see each function's rationale):
#   1. dantesync-tray.exe (strih/stream) is in NO gate at all -- the daemon can roll while the tray
#      lags (live 2026-08-20: deployed tray sha != the pinned release asset). The tray is a
#      GUI-subsystem app with NO console --version (verified live: empty over ssh) and no PE version
#      resource, so it is pinned by sha256 against the release's dantesync-tray-windows-amd64.exe
#      asset (the #1118 sha-compare pattern), NOT by --version.
#   2. DANTESYNC_VERSION_PIN is hand-bumped, so it can sit BEHIND the newest published dantesync gh
#      release (an orphan release). A lag ALARM compares the pin against the newest release tag.
# Both are ALARM-report, not hard-gate: the tray is a cosmetic status GUI that plays NO part in the
# clock discipline (a stale tray does not corrupt any measurement), and the daemon roll is a
# deliberate canary rollout (a lag = "a release is waiting to be rolled", not "the rig is broken
# now") -- so a hard block on every E2E over either would be too blunt (the doctrine's own call for
# the canary case). But both SCREAM on every run so an orphan can never sit silently. The documented
# two-step upgrade to a hard-gate: once the tray is folded into the fleet-upgrade roll (advancing
# with the daemon), flip the tray ALARM into the gate's DRIFT roll-up.

# dantesync_tray_verdict NAME DEPLOYED_SHA EXPECTED_SHA -> one tray-row verdict + return code.
#   DEPLOYED_SHA empty  -> UNKNOWN (31): tray sha unread on the box (fail-closed-LOUD)
#   EXPECTED_SHA empty   -> UNKNOWN (31): the pinned release's tray asset sha could not be resolved
#   DEPLOYED != EXPECTED -> ALARM   (30): the deployed tray LAGS the pinned release (orphan)
#   else                 -> OK      (0):  the deployed tray matches the pinned release asset
# Prints to STDOUT (tests capture it); main() adds a stderr SCREAM banner on ALARM/UNKNOWN.
dantesync_tray_verdict() {
  local name="$1" deployed="$2" expected="$3"
  if [ -z "$deployed" ]; then
    printf '  %-14s %-18s UNKNOWN  (dantesync-tray.exe sha256 not read on the box -- fail-closed)\n' "$name" "-"
    return 31
  fi
  if [ -z "$expected" ]; then
    printf '  %-14s %-18s UNKNOWN  (pinned release tray-asset sha256 unresolved -- cannot verify)\n' "$name" "${deployed:0:12}"
    return 31
  fi
  if [ "$deployed" != "$expected" ]; then
    printf '  %-14s %-18s ALARM    (deployed tray LAGS the pinned release -- expected %s, redeploy the tray)\n' \
      "$name" "${deployed:0:12}" "${expected:0:12}"
    return 30
  fi
  printf '  %-14s %-18s OK       (tray matches the pinned release asset)\n' "$name" "${deployed:0:12}"
  return 0
}

# dantesync_pin_lag_verdict PIN NEWEST -> one pin-lag verdict + return code.
#   NEWEST empty     -> UNKNOWN (33): the newest published dantesync release could not be resolved
#   PIN != NEWEST    -> LAG     (32): the fixed pin sits behind the newest published release (orphan)
#   else             -> OK      (0):  the pin IS the newest published release
# NEWEST/PIN are compared as bare X.Y.Z strings (the release tag's leading "v" is stripped by the
# caller). A pin can never legitimately EXCEED the newest published release, so any difference is a
# lag ("a release is waiting to be rolled"). Prints to STDOUT; main() adds a stderr SCREAM banner.
dantesync_pin_lag_verdict() {
  local pin="$1" newest="$2"
  if [ -z "$newest" ]; then
    printf '  %-32s UNKNOWN  (newest dantesync gh release unresolved -- lag unverifiable)\n' "pin_vs_newest_release"
    return 33
  fi
  if [ "$pin" != "$newest" ]; then
    printf '  %-32s LAG      (pin %s is behind the newest published release %s -- roll the fleet + bump the pin)\n' \
      "pin_vs_newest_release" "$pin" "$newest"
    return 32
  fi
  printf '  %-32s OK       (pin %s is the newest published release)\n' "pin_vs_newest_release" "$pin"
  return 0
}

# --- source-guard: when sourced (the unit tests), stop here -----------------------------------
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# --- flow (executed only when run directly) ----------------------------------------------------

usage() {
  cat <<EOF
dantesync-version-gate.sh — fleet-wide dantesync VERSION-PARITY precondition gate (#862).

Compares every managed node's dantesync version against ONE pinned expected version
(DANTESYNC_VERSION_PIN, default ${DANTESYNC_VERSION_PIN}). REFUSES (non-zero) on any drifted or
unread-and-unexcluded node, printing a box->version table.

Usage:
  dantesync-version-gate.sh [--pin VERSION] [--fleet-file PATH] \\
      --linux "name=user@ip ..." [--local name ...] [--win "name=user@ip ..."]

Options:
  --pin VERSION     the fleet-pinned expected dantesync version (default \$DANTESYNC_VERSION_PIN).
  --fleet-file PATH default CAMBOX_OFFLINE_ACK source when the env var is unset (default:
                    ${DEFAULT_FLEET_FILE}) — same file recording-e2e.sh's fleet preflight reads.
  --linux "N=U@IP ..."  one or more SSH-reachable Linux nodes (space-separated "name=user@ip"
                    pairs in ONE argument, mirrors dantesync-gate.sh's --linux). Repeatable. Read
                    via \`dantesync --version\` over SSH.
  --local NAME      a node read LOCALLY (no ssh) — dev1, the box running this gate. Repeatable.
                    Read via \`dantesync --version\` directly.
  --win "N=U@IP ..."  one or more SSH-reachable Windows nodes (strih/stream), same
                    space-separated "name=user@ip" shape as --linux. Repeatable. Read via the
                    dantesync SERVICE exe's full path over SSH (not on Windows PATH).

Exit: 0 = every node on the pin (or excluded) — proceed. 20 = a node DRIFTED (REFUSED).
  11 = a node UNKNOWN/unread (INCOMPLETE, not clean). 1 = usage error.
EOF
}

# The dantesync SERVICE exe's install path on a Windows node — NOT on PATH there (unlike Linux,
# where `dantesync` is on PATH at /usr/local/bin on every managed node incl. dev1, confirmed live
# 2026-07-30). Overridable for a differently-installed box.
DANTESYNC_VERSION_GATE_WIN_EXE="${DANTESYNC_VERSION_GATE_WIN_EXE:-C:\\Program Files\\DanteSync\\dantesync.exe}"

# read_dantesync_version_output NAME TARGET WIN -> raw `dantesync --version` stdout for NAME.
# TARGET empty -> LOCAL read (dev1, the box running this gate — no ssh, mirrors
# clock-offset-painter-gate.sh's dev1-is-local convention). TARGET set ("user@ip") -> SSH read;
# WIN=1 runs the Windows service exe's full quoted path (confirmed live: OpenSSH-for-Windows
# executes it via cmd.exe directly, no PowerShell wrapper needed); WIN unset/0 runs the bare
# `dantesync --version` (on PATH on every Linux node incl. dev1). "" if unreachable/absent
# (UNKNOWN downstream, never guessed). Overridable per-node for tests/offline via
# DANTESYNC_VERSION_GATE_VERSION_<NAME> (NAME uppercased, "-" -> "_") — mirrors
# dantesync-gate.sh's DANTESYNC_GATE_LINUX_JOURNAL_<NAME> fixture-injection seam.
read_dantesync_version_output() {
  local name="$1" target="${2:-}" win="${3:-0}" var cmd
  var="DANTESYNC_VERSION_GATE_VERSION_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    cat "${!var}" 2>/dev/null || true
    return 0
  fi
  if [ -z "$target" ]; then
    # --local nodes are always the Linux box running this gate itself (dev1) -- WIN never applies.
    dantesync --version 2>/dev/null || true
    return 0
  fi
  if [ "$win" = "1" ]; then
    cmd="\"${DANTESYNC_VERSION_GATE_WIN_EXE}\" --version"
  else
    cmd="dantesync --version"
  fi
  sshpass -p "${DANTESYNC_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${DANTESYNC_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    "$cmd" 2>/dev/null || true
}

# --- #1139 impure readers (below the source-guard; never unit-tested directly) -----------------

# The dantesync-tray.exe install path on a Windows node (same dir as the daemon exe). Overridable.
DANTESYNC_TRAY_GATE_WIN_EXE="${DANTESYNC_TRAY_GATE_WIN_EXE:-C:\\Program Files\\DanteSync\\dantesync-tray.exe}"

# read_dantesync_tray_sha NAME TARGET -> the deployed dantesync-tray.exe sha256 (lowercase hex) for
# a Windows node, read via `certutil -hashfile ... SHA256` over ssh (a session-agnostic file read).
# "" if unreachable/absent (UNKNOWN downstream, never guessed). Override per-node for tests/offline
# via DANTESYNC_TRAY_SHA_<NAME> (NAME uppercased, "-" -> "_") -- mirrors the version reader's seam.
read_dantesync_tray_sha() {
  local name="$1" target="${2:-}" var out
  var="DANTESYNC_TRAY_SHA_$(printf '%s' "$name" | tr '[:lower:]-' '[:upper:]_')"
  if [ -n "${!var:-}" ]; then
    printf '%s' "${!var}" | tr '[:upper:]' '[:lower:]'
    return 0
  fi
  [ -n "$target" ] || { printf ''; return 0; }
  out="$(sshpass -p "${DANTESYNC_VERSION_GATE_SSH_PASS:-newlevel}" ssh \
    -o StrictHostKeyChecking=no -o BatchMode=no \
    -o "ConnectTimeout=${DANTESYNC_VERSION_GATE_SSH_TIMEOUT:-8}" \
    "$target" \
    "certutil -hashfile \"${DANTESYNC_TRAY_GATE_WIN_EXE}\" SHA256" 2>/dev/null || true)"
  # certutil prints a header + the 64-hex hash line + a trailer; grab the first 64-hex token.
  printf '%s' "$out" | grep -oiE '[0-9a-f]{64}' | head -1 | tr '[:upper:]' '[:lower:]' || true
}

# read_dantesync_tray_expected_sha PIN -> the expected tray sha256 (lowercase hex) = the sha from
# the v{PIN} dantesync gh release's `dantesync-tray-windows-amd64.exe.sha256` asset. Best-effort:
# "" on any failure (UNKNOWN downstream, never a false OK). Override for tests via
# DANTESYNC_TRAY_EXPECTED_SHA.
read_dantesync_tray_expected_sha() {
  local pin="$1" tmp asset
  if [ -n "${DANTESYNC_TRAY_EXPECTED_SHA:-}" ]; then
    printf '%s' "$DANTESYNC_TRAY_EXPECTED_SHA" | tr '[:upper:]' '[:lower:]'
    return 0
  fi
  command -v gh >/dev/null 2>&1 || { printf ''; return 0; }
  tmp="$(mktemp -d 2>/dev/null || true)"
  [ -n "$tmp" ] || { printf ''; return 0; }
  asset="${DANTESYNC_TRAY_ASSET_SHA256:-dantesync-tray-windows-amd64.exe.sha256}"
  if gh release download "v${pin}" --repo "${DANTESYNC_RELEASE_REPO:-zbynekdrlik/dantesync}" \
      -p "$asset" -D "$tmp" --clobber >/dev/null 2>&1 && [ -f "$tmp/$asset" ]; then
    awk '{print $1; exit}' "$tmp/$asset" | tr '[:upper:]' '[:lower:]' || true
  fi
  rm -rf "$tmp" 2>/dev/null || true
}

# read_dantesync_newest_release -> the newest published dantesync gh release version (bare X.Y.Z,
# leading "v" stripped). Best-effort: "" on any failure (UNKNOWN downstream). Override for tests via
# DANTESYNC_NEWEST_RELEASE.
read_dantesync_newest_release() {
  if [ -n "${DANTESYNC_NEWEST_RELEASE:-}" ]; then
    printf '%s' "$DANTESYNC_NEWEST_RELEASE" | sed 's/^v//'
    return 0
  fi
  command -v gh >/dev/null 2>&1 || { printf ''; return 0; }
  gh release list --repo "${DANTESYNC_RELEASE_REPO:-zbynekdrlik/dantesync}" --limit 1 \
    --json tagName --jq '.[0].tagName' 2>/dev/null | sed 's/^v//' || true
}

main() {
  local pin="$DANTESYNC_VERSION_PIN" fleet_file="$DEFAULT_FLEET_FILE"
  local -a linux_raw=() local_names=() win_raw=()
  while [ $# -gt 0 ]; do
    case "$1" in
      --pin) shift; pin="${1:-}" ;;
      --fleet-file) shift; fleet_file="${1:-}" ;;
      --linux) shift; linux_raw+=("${1:-}") ;;
      --local) shift; local_names+=("${1:-}") ;;
      --win) shift; win_raw+=("${1:-}") ;;
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

  local -a linux_pairs=() win_pairs=()
  local raw
  set -f
  for raw in "${linux_raw[@]}"; do
    # shellcheck disable=SC2206
    linux_pairs+=($raw)
  done
  for raw in "${win_raw[@]}"; do
    # shellcheck disable=SC2206
    win_pairs+=($raw)
  done
  set +f

  if [ "${#linux_pairs[@]}" -eq 0 ] && [ "${#local_names[@]}" -eq 0 ] && [ "${#win_pairs[@]}" -eq 0 ]; then
    echo "ERROR: no node to gate (--linux, --local and --win are all empty)." >&2
    echo "The dantesync version-parity gate cannot certify the fleet with zero nodes — refusing to pass." >&2
    exit 1
  fi

  CAMBOX_OFFLINE_ACK="$(cambox_offline_ack_effective "${CAMBOX_OFFLINE_ACK:-}" "$fleet_file")"
  export CAMBOX_OFFLINE_ACK

  local -a entries=()
  local pair name target version

  for pair in "${linux_pairs[@]}"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(dantesync_version_from_version_output "$(read_dantesync_version_output "$name" "$target" 0)")"
    entries+=("${name}=${version}")
  done

  for name in "${local_names[@]}"; do
    [ -z "$name" ] && continue
    version="$(dantesync_version_from_version_output "$(read_dantesync_version_output "$name" "" 0)")"
    entries+=("${name}=${version}")
  done

  for pair in "${win_pairs[@]}"; do
    name="${pair%%=*}"
    target="${pair#*=}"
    version="$(dantesync_version_from_version_output "$(read_dantesync_version_output "$name" "$target" 1)")"
    entries+=("${name}=${version}")
  done

  local rc=0
  dantesync_fleet_report "$pin" "${entries[@]}" || rc=$?

  # #1139 -- REPORT-ONLY orphan alarms. These SCREAM (a table row + a stderr banner) but NEVER touch
  # rc / the gate exit (see the pure-verdict headers for the ALARM-not-hard-gate justification): the
  # tray is a cosmetic status GUI (no clock role), and the pin lag reflects a deliberate canary
  # rollout, so blocking every E2E over either is too blunt -- but an orphan must never sit silently.
  echo
  echo "-- dantesync-tray.exe sha-pin (#1139, report-only, ALARM never blocks) --"
  local texp tpair tname ttarget tdep tvrc
  texp="$(read_dantesync_tray_expected_sha "$pin")"
  for tpair in "${win_pairs[@]}"; do
    tname="${tpair%%=*}"
    ttarget="${tpair#*=}"
    cambox_offline_ack_is_acked "$tname" && continue
    tdep="$(read_dantesync_tray_sha "$tname" "$ttarget")"
    tvrc=0
    dantesync_tray_verdict "$tname" "$tdep" "$texp" || tvrc=$?
    case "$tvrc" in
      30) echo "!! DANTESYNC-TRAY ALARM: ${tname} dantesync-tray.exe LAGS the pinned release v${pin} -- redeploy the tray (report-only, does NOT block this run)." >&2 ;;
      31) echo "!! DANTESYNC-TRAY ALARM: could not verify ${tname} dantesync-tray.exe against the pinned release v${pin} (report-only)." >&2 ;;
    esac
  done

  echo
  echo "-- dantesync pin vs newest published release (#1139, report-only, LAG never blocks) --"
  local newest lvrc
  newest="$(read_dantesync_newest_release)"
  lvrc=0
  dantesync_pin_lag_verdict "$pin" "$newest" || lvrc=$?
  case "$lvrc" in
    32) echo "!! DANTESYNC-PIN-LAG ALARM: pin ${pin} is behind the newest published release ${newest} -- roll the fleet + bump DANTESYNC_VERSION_PIN (report-only, does NOT block this run)." >&2 ;;
    33) echo "!! DANTESYNC-PIN-LAG ALARM: could not resolve the newest dantesync gh release to check pin lag (report-only)." >&2 ;;
  esac

  exit "$rc"
}

main "$@"
