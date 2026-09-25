#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no side effects at source time); the caller (recording-e2e.sh) runs under set -euo pipefail.
#
# scripts/lib/camera-test-settings.sh -- issue 1371: the E2E [0/8] test-camera shutter/ISO
# enforce, the THIN TRANSPORT half. Owner, 25.9.2026: "shutter a iso boli na zlych hodnotach tak
# tie si mas uz ty vediet pri teste skontrolovat a nastavit kedze uz mas shading pripojenie".
#
# The ONE test camera (a BMPCC, fed through the HDMI splitter into every cambox) is reachable over
# USB-PTP with gphoto2 on whichever cambox has its USB-C (the bkshading relay path, issue 808 --
# never Bluetooth). recording-e2e.sh calls camera_test_settings_enforce right AFTER the issue-808
# relay pause (so exactly one gphoto2 user exists) and after the temporary relay-restore trap (so an
# abort here still restores the relays). This lib only moves bytes: ssh + sysfs presence + gphoto2
# get/set. EVERY decision (which keys, the baseline diff, the set list, the read-back grade, the
# presence/ack/pinned matrix) lives in the pure scripts/camera_test_settings.py (pytest Tier-0).
#
# Flow:
#   1. baseline status (scripts/camera-test-baseline.json): pinned = iso + d002 both have values.
#   2. resolve the box holding the camera: a sysfs idVendor=1edb (Blackmagic) scan over the
#      candidate boxes -- the relay-paused source cambox + cam2 -- first hit wins. No PTP session
#      is opened just to look.
#   3. decide(present, acked, pinned) -- acked = `testcam` named in CAMBOX_OFFLINE_ACK /
#      rig-fleet.txt (the existing scripts/lib/cambox-offline-ack.sh mechanism).
#   4. unverified-* -> a LOUD report-only UNVERIFIED (never a silent pass), return 0.
#      abort-*      -> a named ERROR, exit 1.
#      enforce      -> ONE gphoto2 read session, the plan, and when something differs: the issue-1271
#                      rig-busy guard, ONE gphoto2 --set-config session, ONE read-back session, grade;
#                      a key that does not read back aborts. Nothing is restored after the run: the
#                      baseline IS the test state.
#
# Test seams: CAMERA_TEST_BASELINE (baseline path), CAMERA_TEST_SETTINGS_SSH_TIMEOUT (per-ssh bound).

_CTS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$_CTS_LIB_DIR/cambox-offline-ack.sh"
# shellcheck source=scripts/lib/stray-session-check.sh
. "$_CTS_LIB_DIR/stray-session-check.sh"

# The name the test camera is acked under in CAMBOX_OFFLINE_ACK / rig-fleet.txt, e.g.
#   CAMBOX_OFFLINE_ACK="testcam:usb-c-unplugged-until-1350"
camera_test_settings_ack_name() { printf '%s\n' testcam; }

# Blackmagic Design's USB vendor id (usb.ids: "1edb  Blackmagic design").
camera_test_settings_usb_vendor() { printf '%s\n' 1edb; }

camera_test_settings_presence_marker() { printf '%s\n' CTS_USB; }

# camera_test_settings_baseline_path SCRIPTS_DIR
camera_test_settings_baseline_path() {
  printf '%s\n' "${CAMERA_TEST_BASELINE:-$1/camera-test-baseline.json}"
}

# Remote text: prints `CTS_USB:1` when a Blackmagic USB device is enumerated on the box, else
# `CTS_USB:0`. Reads sysfs only (no gphoto2, no PTP session).
camera_test_settings_presence_cmds() {
  local vendor marker
  vendor="$(camera_test_settings_usb_vendor)"
  marker="$(camera_test_settings_presence_marker)"
  cat <<PRESENCE
_cts_usb=0
for _cts_d in /sys/bus/usb/devices/*; do
  [ -r "\$_cts_d/idVendor" ] || continue
  if [ "\$(cat "\$_cts_d/idVendor" 2>/dev/null)" = $vendor ]; then _cts_usb=1; fi
done
echo "$marker:\$_cts_usb"
PRESENCE
}

# camera_test_settings_parse_presence OUTPUT -> 1 | 0 | unknown (no marker line = the probe did
# not run: ssh failed / box unreachable). Pure.
camera_test_settings_parse_presence() {
  local marker line
  marker="$(camera_test_settings_presence_marker)"
  line="$(printf '%s\n' "${1:-}" | grep -E "^${marker}:[01]\$" | tail -1)" || true
  case "$line" in
    *:1) printf '%s\n' 1 ;;
    *:0) printf '%s\n' 0 ;;
    *) printf '%s\n' unknown ;;
  esac
}

# camera_test_settings_ssh IP PW CMD -> the remote stdout (stderr passes through to the run log).
camera_test_settings_ssh() {
  local ip="$1" pw="$2" cmd="$3"
  timeout "${CAMERA_TEST_SETTINGS_SSH_TIMEOUT:-40}" sshpass -p "$pw" ssh -o StrictHostKeyChecking=no \
    -o ConnectTimeout=8 root@"$ip" "$cmd"
}

# Remote text for ONE gphoto2 session with the given (already validated, plain-token) argv.
camera_test_settings_gphoto2_cmd() {
  printf 'timeout 20 gphoto2 %s\n' "$*"
}

_cts_prefix() { sed 's/^/    /'; }

# camera_test_settings_enforce SCRIPTS_DIR STRIH STREAM CAM_PW LABEL=IP [LABEL=IP ...]
# Returns 0 on enforced-ok / report-only UNVERIFIED; exits 1 on every abort (call it as a BARE
# statement so the exit propagates, like stray_session_check_assert).
camera_test_settings_enforce() {
  local here="$1" strih="$2" stream="$3" pw="$4"
  shift 4
  local py baseline ack status rc pinned spec label ip out p
  local found_label="" found_ip="" unread="" checked="" present=0 acked=0 action
  py="$here/camera_test_settings.py"
  baseline="$(camera_test_settings_baseline_path "$here")"
  ack="$(camera_test_settings_ack_name)"
  echo "[0/8] test-camera shutter/ISO enforce over the bkshading USB path (issue 1371; baseline $baseline)"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "ERROR: issue 1371: python3 not found -- the test-camera settings step cannot decide anything; refusing to run blind." >&2
    exit 1
  fi

  rc=0
  status="$(python3 "$py" status --baseline "$baseline")" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "ERROR: issue 1371: the test-camera baseline $baseline is invalid (see above) -- fix the file, never run with an unreadable baseline." >&2
    exit 1
  fi
  pinned=0
  [ "$status" = pinned ] && pinned=1

  for spec in "$@"; do
    label="${spec%%=*}"
    ip="${spec#*=}"
    checked="${checked:+$checked, }$label ($ip)"
    out="$(camera_test_settings_ssh "$ip" "$pw" "$(camera_test_settings_presence_cmds)" 2>/dev/null)" || true
    p="$(camera_test_settings_parse_presence "$out")"
    case "$p" in
      1)
        found_label="$label"
        found_ip="$ip"
        break
        ;;
      0) ;;
      *) unread="${unread:+$unread, }$label ($ip)" ;;
    esac
  done
  [ -n "$found_ip" ] && present=1
  cambox_offline_ack_is_acked "$ack" && acked=1

  rc=0
  action="$(python3 "$py" decide --present "$present" --acked "$acked" --pinned "$pinned")" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$action" ]; then
    echo "ERROR: issue 1371: the test-camera decision failed (rc=$rc) -- refusing to run blind." >&2
    exit 1
  fi
  echo "    camera on USB: $([ "$present" = 1 ] && echo "yes, $found_label ($found_ip)" || echo "NO (checked $checked${unread:+; UNREADABLE: $unread})"); baseline: $status; ack '$ack': $([ "$acked" = 1 ] && echo yes || echo no) -> $action"

  case "$action" in
    unverified-unpinned)
      echo "::warning title=issue 1371 test-camera settings UNVERIFIED::the test camera is not on USB and the baseline is not pinned yet -- shutter/ISO NOT checked this run"
      echo "    UNVERIFIED (issue 1371): the test camera's shutter/ISO were NOT read or set this run."
      echo "    The camera is not on USB of: $checked${unread:+ (UNREADABLE: $unread)}; the baseline $baseline has iso/d002 = null."
      echo "    Report-only until the supervisor pins the baseline (issue 1350: plug the BMPCC USB-C into the source cambox, then pin iso + d002)."
      echo "    Once iso/d002 are pinned, an absent camera ABORTS the run."
      return 0
      ;;
    unverified-acked)
      cambox_offline_ack_note "$ack"
      echo "::warning title=issue 1371 test-camera settings UNVERIFIED::the test camera is acked offline ($(cambox_offline_ack_reason "$ack")) -- shutter/ISO NOT checked this run"
      echo "    UNVERIFIED (issue 1371): the test camera is operator-acknowledged offline; its shutter/ISO were NOT read or set this run."
      return 0
      ;;
    abort-absent)
      echo "ERROR: issue 1371: the test camera is NOT on USB (no idVendor $(camera_test_settings_usb_vendor) on: $checked${unread:+; UNREADABLE: $unread}) but the baseline is pinned -- the run cannot vouch for the camera's shutter/ISO." >&2
      echo "    Plug the BMPCC USB-C into the source cambox, or ack it offline: CAMBOX_OFFLINE_ACK=$ack:<reason> (or a '$ack:<reason>' line in rig-fleet.txt)." >&2
      exit 1
      ;;
    abort-stale-ack)
      cambox_offline_ack_stale_message "$ack" >&2
      exit 1
      ;;
    abort-unpinned | enforce) ;;
    *)
      echo "ERROR: issue 1371: unknown test-camera action '$action' -- refusing to run blind." >&2
      exit 1
      ;;
  esac

  local read_args raw plan setargs readback grade
  read_args="$(python3 "$py" read-args)"
  rc=0
  raw="$(camera_test_settings_ssh "$found_ip" "$pw" "$(camera_test_settings_gphoto2_cmd "$read_args")")" || rc=$?
  if [ "$action" = abort-unpinned ]; then
    local suggestion
    rc=0
    suggestion="$(python3 "$py" suggest <<<"$raw")" || rc=$?
    echo "ERROR: issue 1371: the test camera IS on USB ($found_label, $found_ip) but the baseline $baseline is not pinned (iso/d002 null) -- refusing to run without a pinned exposure." >&2
    if [ "$rc" -eq 0 ]; then
      echo "    The camera reads now (pin these only after the owner confirms the camera is set right):" >&2
      echo "    $suggestion" >&2
    else
      echo "    The camera read also failed (gphoto2 output unreadable), so no values can be suggested." >&2
    fi
    exit 1
  fi

  rc=0
  plan="$(python3 "$py" plan --baseline "$baseline" <<<"$raw")" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "ERROR: issue 1371: the test camera is on USB ($found_label, $found_ip) but its gphoto2 read failed or was unreadable (rc=$rc) -- refusing to run on an unknown exposure." >&2
    exit 1
  fi
  printf '%s\n' "$plan" | grep -v '^SETARGS ' | _cts_prefix
  setargs="$(printf '%s\n' "$plan" | sed -n 's/^SETARGS //p')"
  if [ -z "$setargs" ]; then
    echo "    ok: the test camera is already at the baseline -- nothing set"
    return 0
  fi

  # issue 1271: a camera --set-config is a rig mutation -- the shared read-only rig-busy guard runs
  # IMMEDIATELY before it (bare statement, its exit 1 propagates).
  stray_session_check_assert "$here" "$strih" "$stream" "the test-camera shutter/ISO set"
  rc=0
  camera_test_settings_ssh "$found_ip" "$pw" "$(camera_test_settings_gphoto2_cmd "$setargs")" >/dev/null || rc=$?
  [ "$rc" -eq 0 ] || echo "    WARNING: issue 1371: gphoto2 --set-config exited rc=$rc -- the read-back decides" >&2

  rc=0
  readback="$(camera_test_settings_ssh "$found_ip" "$pw" "$(camera_test_settings_gphoto2_cmd "$read_args")")" || rc=$?
  rc=0
  grade="$(python3 "$py" grade --baseline "$baseline" <<<"$readback")" || rc=$?
  printf '%s\n' "$grade" | _cts_prefix
  case "$rc" in
    0)
      echo "    ok: the test camera is set to the baseline and read back (issue 1371)"
      return 0
      ;;
    5)
      echo "ERROR: issue 1371: a test-camera setting did NOT read back after --set-config (see MISMATCH above) -- refusing to run on a camera that ignores the baseline." >&2
      exit 1
      ;;
    *)
      echo "ERROR: issue 1371: the test-camera read-back failed or was unreadable (rc=$rc) -- refusing to run on an unknown exposure." >&2
      exit 1
      ;;
  esac
}
