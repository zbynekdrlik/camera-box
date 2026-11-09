#!/usr/bin/env bash
# scripts/bkshading-deploy-relay.sh — deploy the pinned bkshading RELAY release to a cambox (808 M3).
# Full header below `set -euo pipefail` (kept early for pre-write-script-check.sh).
set -euo pipefail

# ---------------------------------------------------------------------------------------------
# WHY: scripts/bkshading-provision-relay.sh installs the systemd unit + gphoto2 + env (enable-only)
# and leaves the relay binary to this script. It takes the relay of the pinned shading release
# (issue 1487, scripts/shading-release.pin, scripts/lib/shading-release.sh: the exact tag, sha256 proved
# against the pin and the release's .sha256, no fallback) and places it on a cambox at
# /usr/local/bin/bkshading-relay.
#
# It mirrors the PROVEN deploy idiom in scripts/deploy-fleet.sh: the verified binary, then the
# read-only-root swap cycle `remount,rw -> scp -> chmod +x -> sha256 byte-verify -> remount,ro`.
# ROOT MODE (issue 808, slice B): the deploy READS the target's own root (`findmnt -no OPTIONS /`)
# and runs that cycle only on a read-only root -- every cambox, and a handheld SBC once its
# read-only fstab is live. A read-write root (an SBC before its first read-only reboot) is never
# remounted; an unreadable root refuses before the box is touched. One path for cambox and SBC.
# Pure decisions (the ENABLE-ONLY invariant, the sha-match verdict, the relay + root state)
# live in scripts/lib/bkshading-deploy-runtime.sh so they are Tier-0 unit-testable without a rig.
#
# RELAY STATE (issue 808, 25.9.2026): the deploy reads the relay's state first. An ACTIVE relay is
# STOPPED before the swap and started again after the ro remount (its PREVIOUS state is restored);
# a stopped relay (the TEST-mode default, issue 1311) stays stopped — the deploy never STARTS a relay
# that was not running and never `enable`s it (enable-state is setup-device.sh / rig-mode.sh's job).
# Swapping under a running relay left the replaced binary deleted-but-open, the final `remount,ro`
# failed "busy", and the old script swallowed it (cam6/cam7 root stayed read-WRITE); the ro close is
# now the ONE shared verified close (scripts/lib/ro-window.sh, issue 1407): the root mode is READ,
# and a root left writable FAILS LOUD naming the writers + the deleted-but-open holders, and the
# relay is NOT started again on it. USB / USB-Ethernet transports only —
# no wireless-pairing transport (owner hard rule). Per approval-scope.md the binary deploy + the ro-root remount it
# performs are the standing-approved WORK — this script does NOT ask permission and does NOT gate on
# "is it off-air"; the operator who runs it guards live timing. It does NOT reboot the host.
#
# Usage:  scripts/bkshading-deploy-relay.sh --host <ip> [--box <name>] [--arch amd64|arm64]
#                                           [--binary <path>] [--dry-run] [--force-live]
#   --host <ip>       (required) the cambox/SBC to deploy the relay to (e.g. 10.77.9.201).
#   --box <name>      issue 1452: the box's name (camN, or the SBC's hostname) -- picks ITS pin from
#                     scripts/shading-release-boxes.txt (the REST relay v0.2.x only where its camera
#                     serves REST); none = the fleet default pin. Before anything changes, the box's own
#                     hostname is read: a box whose name maps to ANOTHER pin is refused (a forgotten
#                     --box), never deployed the wrong release.
#   --arch <a>        target arch of the release asset: `amd64` (default; cambox) or `arm64`
#                     (SBC/handheld zero-class arm64 SBC, issue 808 SBC milestone).
#                     The remount cycle follows the target's own root mode, never a flag.
#   --binary <path>   an already-fetched relay binary (deploy-fleet.sh fetches once for the whole
#                     fleet). Its sha256 must equal the pin: any other bytes are refused.
#   --dry-run         print the plan and touch no box (no ssh/scp; without --binary it still
#                     fetches the release asset, to show its sha256).
#   (exit 6)          the rig lease is held by a live E2E (issue 1371): no deploy, no bypass. A lease
#                     held by THIS run itself (RIG_LEASE_REPO/RIG_LEASE_RUN_ID, else GITHUB_REPOSITORY/
#                     GITHUB_RUN_ID; issue 1452, the post-merge deploy job) is ours and goes through.
#                     BKSHADING_DEPLOY_LEASE_RESOURCE=<atom> (deploy-fleet --venue sets <venue>-cams):
#                     a holder whose declared issue-1450 `resources` do not name it goes through too.
#   --force-live      BYPASS the rig-busy guard and deploy even while a broadcast is live. Supervisor
#                     override ONLY, logged loudly — a relay deploy/restart during live production can
#                     fork-wedge the cambox (gphoto2 PTP on the shared xHCI bus, 2026-09-13 #1229).
#   -h | --help       show this header.
# SBC/handheld example: scripts/bkshading-deploy-relay.sh --host <sbc> --arch arm64
#
# Env: SSH_PASS (default newlevel), STRIH_HOST/STREAM_HOST (rig-busy OBS-WS hosts, default
#      10.77.9.202/.204), OBS_PASSWORD (default ""). Overridable for Tier-0 tests (inject fakes):
#      BKSHADING_DEPLOY_SSH, BKSHADING_DEPLOY_SCP, BKSHADING_DEPLOY_SSHPASS_PREFIX,
#      BKSHADING_DEPLOY_OBS_PHASE2_DIR (dir holding obs_phase2.py for the rig-busy guard).
#
# Exit codes: 0 = relay deployed + byte-verified AND the root back in its previous mode (read-only
# when it was) AND the relay back in its previous state; 1 = a step failed / the root mode or relay
# state could not be read / sha256 mismatch / the ro remount failed (holder named) / the restore
# failed; 2 = bad args. A box never provisioned for the relay: run
# scripts/bkshading-provision-relay.sh --install on a cambox (setup-device.sh does it on a fresh
# install), scripts/bkshading-provision-sbc.sh --install on a handheld SBC.
# ---------------------------------------------------------------------------------------------

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/bkshading-deploy-runtime.sh
. "$HERE/lib/bkshading-deploy-runtime.sh"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$HERE/lib/bkshading-relay-runtime.sh" # bkshading_relay_bin_path() — the ONE relay install path
# shellcheck source=scripts/lib/shading-release.sh
. "$HERE/lib/shading-release.sh" # the pinned shading release (issue 1487)
# shellcheck source=scripts/lib/stray-session-check.sh
. "$HERE/lib/stray-session-check.sh" # stray_session_check_assert() — the ONE shared rig-busy guard
# shellcheck source=scripts/lib/rig-lease.sh
. "$HERE/lib/rig-lease.sh"  # issue 1371: the rig lease an E2E holds while it owns the camboxes
# shellcheck source=scripts/lib/ro-window.sh
. "$HERE/lib/ro-window.sh" # ro_window_close_cmds / ro_window_holders — the ONE verified ro close (issue 1407)
# shellcheck source=scripts/lib/bkshading-camera-link.sh
. "$HERE/lib/bkshading-camera-link.sh" # issue 1452: the camera link file a REST-pin relay needs

RELAY_DEST="$(bkshading_relay_bin_path)"     # /usr/local/bin/bkshading-relay (one source of truth)
RELAY_UNIT="$(bkshading_relay_unit_name)"    # bkshading-relay.service (one source of truth)
RESTORE_ACTION=none                          # issue 808: `start` only when the relay was running before the swap
BOX_DIRTY=0                                  # issue 808: 1 from the relay stop / rw remount until finish_once ran
# Staging path for the ETXTBSY-safe swap: scp lands here (SAME dir → atomic rename), then `mv -f`
# replaces the (possibly RUNNING) relay inode. scp'ing directly onto a running exe fails ETXTBSY
# ("dest open: Failure", 2026-09-13 escalation). $$ is the local PID = a unique per-run stage name.
RELAY_STAGE="${RELAY_DEST}.deploy.$$"
# Rig-busy guard inputs (mirror scripts/rig-busy-gate.sh's own defaults). The guard reuses the
# shared obs_phase2.py rig-busy-check; RIG_BUSY_HERE names the dir holding obs_phase2.py (this
# script's own scripts/ dir), overridable so a Tier-0 test can point it at a fake.
STRIH_HOST="${STRIH_HOST:-10.77.9.202}"
STREAM_HOST="${STREAM_HOST:-10.77.9.204}"
OBS_PASSWORD="${OBS_PASSWORD:-}"
RIG_BUSY_HERE="${BKSHADING_DEPLOY_OBS_PHASE2_DIR:-$HERE}"
SSH_PASS="${SSH_PASS:-newlevel}"
ARCH="amd64"   # default: cambox; --arch arm64 = SBC/handheld relay
RO_ROOT=0      # issue 808: 1 once the target's own root reads read-only (the remount cycle runs)

# Overridable command surfaces (real defaults; the test injects fakes + an empty sshpass prefix).
SSH_BIN="${BKSHADING_DEPLOY_SSH:-ssh}"
SCP_BIN="${BKSHADING_DEPLOY_SCP:-scp}"
# sshpass prefix as an ARRAY (not a word-split string), so a password with whitespace is safe
# (deploy-fleet.sh quotes `-p "$SSH_PASS"`; a word-split string here would not). The env var, when
# SET (even empty — a test), replaces the prefix verbatim: empty -> the fakes run bare (no sshpass);
# unset -> wrap ssh/scp with the fleet password.
if [ -n "${BKSHADING_DEPLOY_SSHPASS_PREFIX+set}" ]; then
  read -r -a SSHPASS_PREFIX <<<"$BKSHADING_DEPLOY_SSHPASS_PREFIX"
else
  SSHPASS_PREFIX=(sshpass -p "$SSH_PASS")
fi

HOST=""
BOX=""
BINARY=""
DRY_RUN=0
FORCE_LIVE=0   # --force-live: bypass the rig-busy guard (supervisor-only, logged loudly)

# require_val: a flag needs a following value; without one, fail with the bad-args exit 2 + a
# message (NOT a bare `shift 2` that aborts under set -e with exit 1 and no diagnostic).
require_val() { [ "$1" -ge 2 ] || { echo "ERROR: $2 requires a value (see --help)" >&2; exit 2; }; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    --host) require_val "$#" --host; HOST="$2"; shift 2 ;;
    --box) require_val "$#" --box; BOX="$2"; shift 2 ;;
    --arch) require_val "$#" --arch; ARCH="$2"; shift 2 ;;
    --binary) require_val "$#" --binary; BINARY="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --force-live) FORCE_LIVE=1; shift ;;
    -h | --help)
      grep -E '^# ' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 2 ;;
  esac
done

if [ -z "$HOST" ]; then
  echo "ERROR: --host <ip> is required. Usage: bkshading-deploy-relay.sh --host <ip> [--arch amd64|arm64] [--binary <path>] [--dry-run]" >&2
  exit 2
fi

# Validate the arch and name its release asset.
case "$ARCH" in
  amd64 | arm64) ;;
  *) echo "ERROR: --arch must be amd64 or arm64 (got: $ARCH)" >&2; exit 2 ;;
esac
ASSET="$(shading_relay_asset_for_arch "$ARCH")"
# issue 1452: the pin of THIS box (scripts/shading-release-boxes.txt). Every lib call below reads it
# through SHADING_RELEASE_PIN_FILE, so the fetch and the two sha256 proofs are the same code for any pin.
PIN_SEAM="${SHADING_RELEASE_PIN_FILE:-}"
PIN_FILE="$(shading_pin_file_for_box "$HERE/.." "$BOX")" || exit 2
SHADING_RELEASE_PIN_FILE="$PIN_FILE"
export SHADING_RELEASE_PIN_FILE

# The provisioning a box needs before its relay runs: the cambox relay canon, or the SBC one.
if [ "$ARCH" = "arm64" ]; then
  PROVISION_HINT="on the SBC run scripts/bkshading-provision-sbc.sh --install (if not yet) + reboot"
else
  PROVISION_HINT="on the box run scripts/bkshading-provision-relay.sh --install --rig-mode <test|event> (if not yet) + reboot"
fi

# Conditional read-only-root swap (issue 808, slice B): RO_ROOT is set from the TARGET's own root
# mode just before the box is touched -- 1 on a read-only root (every cambox, a read-only SBC). No
# ssh remount call at all on a read-write root, so a board before its first read-only reboot is
# never remounted ro (which would be wrong / fail-busy).
maybe_remount_rw() { [ "$RO_ROOT" = 1 ] || return 0; ssh_box "$1" "mount -o remount,rw /"; }

# RESTORE_SESSION is empty on the normal path and `setsid -w` inside the EXIT trap: the trap's restore
# then runs in its own session, so a second Ctrl-C from the terminal cannot reach it (sshpass forwards
# SIGINT to its ssh child even when this shell ignores it).
RESTORE_SESSION=()
ssh_box() { "${RESTORE_SESSION[@]}" "${SSHPASS_PREFIX[@]}" "$SSH_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3 "root@$1" "$2"; }
scp_box() { "${SSHPASS_PREFIX[@]}" "$SCP_BIN" -o StrictHostKeyChecking=no -o ConnectTimeout=10 -o ServerAliveInterval=10 -o ServerAliveCountMax=3 "$2" "root@$1:$3"; }

# issue 808 (cam6/cam7, 25.9.2026): the ro remount used to be swallowed (`2>/dev/null; true`), the
# script printed OK and the box root stayed read-WRITE. issue 1407: the close is now the ONE shared
# verified close (scripts/lib/ro-window.sh), run on the box in ONE ssh call: the ro remount, the root
# mode READ (never the mount exit code alone), and on a root left writable the FAIL lines naming the
# WRITERS (`fuser -vm /` filtered to ACCESS F -- the old `| head -n 40` cut a high-PID writer off
# behind PID 1 and the kernel threads) and the deleted-but-open holders, then `exit 1`. No retry: a
# writer that keeps / busy keeps it busy on every retry (a stopped relay has closed its files once
# `systemctl stop` returned). The box's FAIL lines are printed, then one summary line naming every
# holder (ro_window_holders). Any other rc is the transport (ssh 255, sshpass 5/6): reported as such.
remount_ro_checked() {
  [ "$RO_ROOT" = 1 ] || return 0
  local rc=0 out holders
  out="$(ssh_box "$1" "$(ro_window_close_cmds "issue 1407" "$1" \
    "The relay binary is in place, but the box is left on a writable root, so nothing is started on it." \
    "stop that writer on $1, then put the root back read-only until 'findmnt -no OPTIONS /' reads ro (never leave a cambox root rw).")" 2>&1)" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  if [ "$rc" -ne 1 ]; then
    # The close exits only 0 or 1, so any other rc is the transport (ssh 255, sshpass 5/6).
    echo "ERROR: ssh to $1 FAILED during the ro remount (rc $rc) -- the root may still be read-WRITE;" >&2
    echo "       reach the box and check 'findmnt -no OPTIONS /' (it must say ro) by hand." >&2
    return 1
  fi
  printf '%s\n' "${out:-<no output>}" | sed 's/^/       /' >&2
  holders="$(ro_window_holders "$out")"
  echo "ERROR: the ro remount FAILED on $1 -- the box root stays read-WRITE; holder(s): ${holders:-none named}" >&2
  return 1
}

# Restore the relay's PREVIOUS state after the swap: start it again ONLY when it was active before
# (bkshading_deploy_restore_action). A relay that was stopped (TEST mode, issue 1311) stays stopped.
restore_relay() {
  [ "$RESTORE_ACTION" = start ] || return 0
  if ssh_box "$1" "systemctl start $RELAY_UNIT"; then
    echo "restored: $RELAY_UNIT started again on $1 (it was active before the swap)"
  else
    echo "ERROR: could not start $RELAY_UNIT again on $1 (it was active before the swap) -- start it by hand" >&2
    return 1
  fi
}

# finish_box HOST -> the ro remount (checked) THEN the relay restore; non-zero when either failed.
# issue 1407: a root that does not read ro again (or could not be read) starts NOTHING -- the relay
# stays stopped and says so; it is started by hand once the root is read-only again.
finish_box() {
  if ! remount_ro_checked "$1"; then
    if [ "$RESTORE_ACTION" = start ]; then
      echo "ERROR: leaving $RELAY_UNIT STOPPED on $1 -- its root is not verified read-only (nothing starts on a writable root); start it by hand once the root reads ro" >&2
    fi
    return 1
  fi
  restore_relay "$1"
}

# finish_once -> finish_box for $HOST exactly once, and only after the box was touched (BOX_DIRTY=1
# from the relay stop / rw remount on). Every explicit exit path calls it, and the EXIT trap below
# calls it too, so a Ctrl-C / SIGTERM mid-scp still restores the ro root and the relay's state.
# BOX_DIRTY clears only AFTER finish_box returned; FINISHING marks a restore in progress, so a signal
# landing INSIDE the restore is reported loudly by the EXIT trap instead of being mistaken for "done".
FINISHING=0
finish_once() {
  local rc=0
  [ "$BOX_DIRTY" = 1 ] || return 0
  [ "$FINISHING" = 0 ] || return 1
  FINISHING=1
  finish_box "$HOST" || rc=1
  BOX_DIRTY=0
  FINISHING=0
  return "$rc"
}

# The EXIT trap must survive a dead terminal (a HUP'd ssh session: every write to stderr fails) and a
# second signal: errexit off, signals ignored, every message write guarded.
deploy_on_exit() {
  local rc=$?
  set +e
  trap '' INT TERM HUP PIPE
  if command -v setsid >/dev/null 2>&1; then
    RESTORE_SESSION=(setsid -w)
  else
    echo "WARNING: setsid not found -- a second Ctrl-C can still interrupt the restore on $HOST" >&2 2>/dev/null
  fi
  if [ "$FINISHING" = 1 ]; then
    # The restore itself was interrupted. finish_box is safe to repeat (remount,ro on an ro root and
    # start on an active unit are no-ops), so run it again -- now signal-proof -- and only ask for a
    # hand check if that fails too.
    echo "WARNING: the ro-root/relay restore on $HOST was interrupted -- running it again" >&2 2>/dev/null
    FINISHING=0
    if ! finish_once; then
      echo "ERROR: the ro-root/relay restore on $HOST FAILED after an interruption -- check by hand: 'findmnt -no OPTIONS /' must say $([ "$RO_ROOT" = 1 ] && echo ro || echo rw), 'systemctl is-active $RELAY_UNIT' must match its state before the deploy (${WAS_ACTIVE:-unknown})" >&2 2>/dev/null
    fi
  elif [ "$BOX_DIRTY" = 1 ]; then
    echo "ERROR: deploy interrupted/aborted after the box was touched -- restoring the ro root + the relay state" >&2 2>/dev/null
    finish_once
  fi
  return "$rc"
}
trap deploy_on_exit EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# --- resolve the relay binary: the pinned release, or a --binary carrying exactly its bytes ---
if [ -z "$BINARY" ]; then
  BINARY="$(shading_release_relay "$HERE/.." "$ARCH")" || { echo "ERROR: no verified relay of the pinned shading release (above)" >&2; exit 1; }
fi
[ -f "$BINARY" ] || { echo "ERROR: relay binary not found: $BINARY" >&2; exit 1; }
shading_release_file_ok "$HERE/.." "$ASSET" "$BINARY" || exit 1
chmod +x "$BINARY" 2>/dev/null || true

LOCAL_SHA="$(sha256sum "$BINARY" | awk '{print $1}')"

# --- dry-run: print the plan, touch nothing ---
if [ "$DRY_RUN" -eq 1 ]; then
  STEPS="rig-busy guard  ->  read the relay state + the root mode (findmnt -no OPTIONS /)  ->  stop the relay if active  ->  [read-only root] mount -o remount,rw /  ->  scp (staged $RELAY_STAGE)  ->  chmod +x + atomic mv  ->  sha256 byte-verify  ->  [read-only root] mount -o remount,ro / + the root mode READ (a root left writable FAILS LOUD naming the writers, nothing started)  ->  start the relay again if it was active"
  cat <<PLAN
DRY-RUN — bkshading relay deploy plan:
  host           : $HOST
  arch           : $ARCH (asset $ASSET, shading $(shading_pin_tag "$HERE/.."))
  pin            : ${PIN_FILE##*/} (box '${BOX:-<none: the fleet default>}'; the box's own hostname is checked against it before the swap)
  source binary  : $BINARY (sha256 $LOCAL_SHA)
  deploy target  : root@$HOST:$RELAY_DEST
  steps          : $STEPS
  root mode      : read on the box; a read-only root gets the remount cycle, a read-write root none, an unreadable one refuses before the box is touched
  relay state    : an ACTIVE relay is stopped for the swap and started again after; a stopped relay stays stopped (never started)
  next step      : $PROVISION_HINT
PLAN
  exit 0
fi

# --- rig-lease PREFLIGHT (issue 1371, 14.10.2026): NEVER touch a cambox while an E2E holds the rig
# lease. A live holder (holder.json present, heartbeat fresh, its run not known finished) owns the
# camboxes: it pauses the relay, records, restores. A deploy in that window remounts the root and
# swaps the binary under the run (it happened on cam5 during run 38579117744). --force-live is a
# BROADCAST override only and does NOT bypass this; wait for the release. RIG_LEASE_DIR is the
# shared lib's seam (tests point it at a scratch dir).
# issue 1452: a lease held by THIS run itself (the post-merge deploy job takes it through
# rig-busy-gate.sh before deploy-fleet.sh, which calls this script per box) is ours, not a foreign
# holder: the identity is rig-busy-gate.sh's own (RIG_LEASE_REPO/RIG_LEASE_RUN_ID, else the Actions
# GITHUB_REPOSITORY/GITHUB_RUN_ID); an operator shell has neither and is refused as before.
OWN_LEASE_REPO="${RIG_LEASE_REPO:-${GITHUB_REPOSITORY:-}}"
OWN_LEASE_RUN="${RIG_LEASE_RUN_ID:-${GITHUB_RUN_ID:-}}"
if ! rig_lease_is_stale "${RIG_LEASE_STALE_SECS:-5400}"; then
  if rig_lease_held_by "$OWN_LEASE_REPO" "$OWN_LEASE_RUN"; then
    echo "the rig lease is held by this run itself ($OWN_LEASE_REPO#$OWN_LEASE_RUN) -- proceeding with the relay deploy to $HOST"
  elif [ -n "${BKSHADING_DEPLOY_LEASE_RESOURCE:-}" ] && ! rig_lease_holder_covers "$BKSHADING_DEPLOY_LEASE_RESOURCE"; then
    # issue 1452: deploy-fleet --venue <v> names the target's own cameras (<v>-cams); a holder that
    # declared other resources (issue 1450) does not own this box.
    echo "the rig lease is held by $(rig_lease_holder_summary) for other resources, not $BKSHADING_DEPLOY_LEASE_RESOURCE -- proceeding with the relay deploy to $HOST"
  else
    echo "ERROR: the rig lease is held by $(rig_lease_holder_summary) -- an E2E owns the camboxes; no relay deploy to $HOST until it releases (re-run then). --force-live does not bypass the lease." >&2
    exit 6
  fi
fi

# --- rig-busy PREFLIGHT (2026-09-13 escalation): NEVER deploy a cambox relay while a broadcast may
# be LIVE. A relay deploy DURING live production fork-wedged cam1 (gphoto2 PTP contention with the
# grabber on the single shared xHCI controller cascades into D-state gphoto2 + fork-exhaustion).
# Reuse the SAME shared rig-busy guard recording-e2e.sh uses (obs_phase2.py rig-busy-check via
# stray_session_check_assert) — refuse on a busy rig unless --force-live (supervisor-only, logged
# loudly); the guard fail-OPENs (WARN + proceed) only when NO box is readable, never on a transient.
if [ "$FORCE_LIVE" = 1 ]; then
  echo "WARNING: --force-live — BYPASSING the rig-busy guard for the bkshading relay deploy to $HOST." >&2
  echo "         A relay deploy/restart during a LIVE broadcast can fork-wedge the cambox (2026-09-13 escalation); supervisor override only." >&2
else
  stray_session_check_assert "$RIG_BUSY_HERE" "$STRIH_HOST" "$STREAM_HOST" "the bkshading relay deploy to $HOST"
fi

# --- real deploy: read-only-root swap cycle (mirrors deploy-fleet.sh) ---
if [ "${SSHPASS_PREFIX[0]:-}" = "sshpass" ]; then
  command -v sshpass >/dev/null 2>&1 || { echo "ERROR: sshpass required (apt-get install sshpass)" >&2; exit 1; }
fi

echo "[bkshading-deploy-relay] deploying $BINARY ($ARCH) -> root@$HOST:$RELAY_DEST (staged via $RELAY_STAGE, atomic mv)"

# issue 808 (cam6/cam7, 25.9.2026): swapping the binary under a RUNNING relay left that process
# holding the replaced, deleted-but-open inode, so the final `remount,ro` failed "busy" and the root
# stayed read-WRITE. So: read the relay's state FIRST, STOP it when it is active (the rig-busy guard
# above has already refused a live broadcast), swap, restore ro, and only then start it again --
# restoring the PREVIOUS state, never starting a relay that was stopped (bkshading_deploy_restore_action).
WAS_RC=0
WAS_ACTIVE="$(ssh_box "$HOST" "systemctl is-active $RELAY_UNIT 2>/dev/null || true" 2>/dev/null)" || WAS_RC=$?
WAS_ACTIVE="$(printf '%s' "$WAS_ACTIVE" | tr -d '[:space:]')"
RESTORE_ACTION="$(bkshading_deploy_restore_action "$WAS_ACTIVE")"
if [ "$WAS_RC" -ne 0 ] || [ "$RESTORE_ACTION" = unreadable ]; then
  # Never guess "not running" from a failed read: the relay could be live on the old inode.
  echo "ERROR: could not read $RELAY_UNIT's state on $HOST (ssh rc=$WAS_RC, is-active='${WAS_ACTIVE}') -- nothing changed on the box" >&2
  exit 1
fi
echo "relay state before the swap: $WAS_ACTIVE -> after the swap: $([ "$RESTORE_ACTION" = start ] && echo 'start it again' || echo 'leave it stopped')"

# issue 1452: the box names itself; its own pin must be the one this deploy carries (a cam3 deployed
# without --box would otherwise get the fleet default, a PP box given --box cam3 the REST relay its camera
# cannot serve). An unreadable name keeps the pin --box chose (or the default), and says so.
BOX_SELF="$(ssh_box "$HOST" "uname -n 2>/dev/null || true" 2>/dev/null || true)"
BOX_SELF="$(printf '%s' "$BOX_SELF" | tr -d '[:space:]')"
if [ -n "$BOX_SELF" ]; then
  SELF_PIN="$(SHADING_RELEASE_PIN_FILE="$PIN_SEAM" shading_pin_file_for_box "$HERE/.." "$BOX_SELF")" || exit 1
  if [ "$SELF_PIN" != "$PIN_FILE" ]; then
    echo "ERROR: $HOST names itself '$BOX_SELF', whose pin is ${SELF_PIN##*/}, but this deploy carries ${PIN_FILE##*/} (--box '${BOX:-<none>}') -- pass --box $BOX_SELF; nothing changed on the box (issue 1452)" >&2
    exit 1
  fi
  echo "box $HOST names itself '$BOX_SELF': pin ${PIN_FILE##*/} (issue 1452)"
elif [ -z "$BOX" ]; then
  echo "ERROR: could not read the box's own name on $HOST and no --box was given -- its relay pin cannot be chosen; pass --box <name>; nothing changed on the box (issue 1452)" >&2
  exit 1
else
  echo "NOTE: could not read the box's own name on $HOST -- deploying the pin of --box '$BOX' (${PIN_FILE##*/}) as given (issue 1452)"
fi

# issue 808, slice B: the remount cycle follows the TARGET's own root mode -- read it like the relay
# state above: an unreadable read (ssh failure / empty / unexpected) REFUSES before touching the box,
# never taken for rw (an ro root left rw after the swap) or ro (an rw root forced ro mid-use).
ROOT_RC=0
ROOT_OPTS="$(ssh_box "$HOST" "$(bkshading_deploy_root_opts_cmd)" 2>/dev/null)" || ROOT_RC=$?
ROOT_OPTS="$(printf '%s' "$ROOT_OPTS" | tr -d '[:space:]')"
ROOT_ACTION="$(bkshading_deploy_root_remount_action "$ROOT_OPTS")"
if [ "$ROOT_RC" -ne 0 ] || [ "$ROOT_ACTION" = unreadable ]; then
  echo "ERROR: could not read the root mount options on $HOST (ssh rc=$ROOT_RC, findmnt='${ROOT_OPTS}') -- nothing changed on the box" >&2
  exit 1
fi
[ "$ROOT_ACTION" = remount ] && RO_ROOT=1
echo "root on $HOST: ${ROOT_OPTS%%,*} -> $([ "$RO_ROOT" = 1 ] && echo 'remount rw for the swap, back to ro after' || echo 'read-write, no remount')"
# A cambox (the amd64 relay) always runs read-only (setup-device.sh STEP 18). An rw cambox root is a
# leftover (the 25.9.2026 cam6/cam7 class): the deploy follows the box and leaves it rw, but says so.
if [ "$RO_ROOT" = 0 ] && [ "$ARCH" = amd64 ]; then
  echo "WARNING: the root on $HOST is read-WRITE ($ROOT_OPTS) -- a cambox runs read-only (setup-device.sh STEP 18)." >&2
  echo "         The deploy leaves it as it is; find the writer ('fuser -vm /') and run 'mount -o remount,ro /' on $HOST." >&2
fi
BOX_DIRTY=1
if [ "$RESTORE_ACTION" = start ]; then
  if ! ssh_box "$HOST" "systemctl stop $RELAY_UNIT"; then
    echo "ERROR: could not stop $RELAY_UNIT on $HOST before the swap -- restoring its state" >&2
    finish_once || true
    exit 1
  fi
fi

if ! maybe_remount_rw "$HOST"; then
  echo "ERROR: remount rw / failed on $HOST" >&2
  finish_once || true
  exit 1
fi
# Stage to a temp path in the SAME directory, then atomic `mv -f` over the relay binary. scp'ing
# directly onto a running executable fails ETXTBSY ("dest open: Failure", 2026-09-13); the stage +
# rename(2) also keeps a half-copied file from ever sitting at the real path. On any failure, clean
# up the stage file, restore the ro root AND the relay's previous state (finish_once).
if ! scp_box "$HOST" "$BINARY" "$RELAY_STAGE"; then
  echo "ERROR: scp of relay binary to $HOST failed" >&2
  ssh_box "$HOST" "rm -f $RELAY_STAGE 2>/dev/null || true" || true
  finish_once || true
  exit 1
fi
if ! ssh_box "$HOST" "chmod +x $RELAY_STAGE && mv -f $RELAY_STAGE $RELAY_DEST"; then
  echo "ERROR: staging chmod + atomic mv of the relay binary failed on $HOST" >&2
  ssh_box "$HOST" "rm -f $RELAY_STAGE 2>/dev/null || true" || true
  finish_once || true
  exit 1
fi

# Byte-verify (deploy-from-clean-tree.md Layer 3): a partial scp / stale same-name binary would
# otherwise pass unnoticed. Read the remote sha AND the exec bit BEFORE restoring ro (fresh file):
# scp with no `-p` creates a 0644 file on a FIRST deploy, so the chmod above is the ONLY thing making
# it executable — verify that too, else the unit's ExecStart would fail at reboot.
REMOTE_SHA="$(ssh_box "$HOST" "sha256sum $RELAY_DEST 2>/dev/null | awk '{print \$1}'" || echo "")"
REMOTE_EXEC="$(ssh_box "$HOST" "test -x $RELAY_DEST && echo yes || echo no" 2>/dev/null || echo no)"
VERIFIED=1
if [ "$(bkshading_deploy_sha_match "$LOCAL_SHA" "$REMOTE_SHA")" != "match" ]; then
  echo "ERROR: sha256 mismatch after deploy (local=$LOCAL_SHA remote=${REMOTE_SHA:-<none>}) — deploy NOT verified" >&2
  VERIFIED=0
elif [ "$REMOTE_EXEC" != "yes" ]; then
  echo "ERROR: $RELAY_DEST is not executable on $HOST after deploy — deploy NOT verified" >&2
  VERIFIED=0
fi
# A binary that failed the byte-verify is never started: the relay is left STOPPED (loudly) rather
# than run on unverified bytes -- redeploy, then start it.
if [ "$VERIFIED" = 0 ] && [ "$RESTORE_ACTION" = start ]; then
  echo "ERROR: leaving $RELAY_UNIT STOPPED on $HOST -- the swapped binary is not verified; redeploy, then start it" >&2
  RESTORE_ACTION=none
fi

# The ro remount (checked, FAIL LOUD naming the holder) + the relay restore -- on every verdict.
FINISH_RC=0
finish_once || FINISH_RC=1
[ "$VERIFIED" = 1 ] || exit 1
if [ "$FINISH_RC" -ne 0 ]; then
  echo "ERROR: relay binary swapped + byte-verified on $HOST, but the box was NOT left clean (see above) — deploy FAILED" >&2
  exit 1
fi
echo "OK: relay deployed + byte-verified (executable) on $HOST ($RELAY_DEST, sha256 $LOCAL_SHA)$([ "$RO_ROOT" = 1 ] && echo ', root back to read-only')"

# issue 1452: a REST-pin relay finds the camera through the camera link file; on a cambox it is put in
# place by its own verified ro window (networkd reloaded after the close). The SBC gets it from
# bkshading-provision-sbc.sh --install (its root may still be read-write by design).
if [ "$ARCH" = amd64 ] && [ "$(shading_pin_transport "$HERE/..")" = rest ]; then
  if LINK_OUT="$(ssh_box "$HOST" "$(bkshading_camera_link_apply_program)" 2>&1)"; then
    printf '%s\n' "$LINK_OUT" | sed 's/^/       /'
    echo "OK: the camera link file is in place on $HOST (issue 1452)"
  else
    printf '%s\n' "$LINK_OUT" | sed 's/^/       /' >&2
    echo "ERROR: the relay is deployed, but the camera link file did not land on $HOST (above) -- the REST relay cannot find the camera until it does (issue 1452)" >&2
    exit 1
  fi
fi

# The deploy never STARTS a relay that was not running: the predicate is the single source of truth
# (always `no`); if a future change flips it, that is a RED test, not a silent live start. An active
# relay was restored above (its previous state), which is not a start of a stopped service.
if [ "$(bkshading_deploy_should_start)" = "yes" ]; then
  echo "WARNING: should_start=yes — refusing to start a stopped relay anyway (enable-only invariant)" >&2
elif [ "$RESTORE_ACTION" != start ]; then
  echo "relay was not running: left stopped. Next: $PROVISION_HINT;"
  echo "             it comes up at boot$([ "$ARCH" = arm64 ] || echo ' / rig-mode.sh event')."
fi
