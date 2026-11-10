#!/usr/bin/env bash
# airuleset:script-ok source-only lib (function definitions only, no side effects at source time); the caller (recording-e2e.sh) runs under set -euo pipefail.
#
# scripts/lib/camera-test-settings.sh -- issue 1371: the E2E [0/8] test-camera shutter/ISO
# enforce, the THIN TRANSPORT half. Owner, 25.9.2026: "shutter a iso boli na zlych hodnotach tak
# tie si mas uz ty vediet pri teste skontrolovat a nastavit kedze uz mas shading pripojenie".
#
# The ONE test camera (a BMPCC, fed through the HDMI splitter into every cambox) is reachable on
# whichever cambox has its USB-C. Since issue 1452 (shading#6, owner 9.11.2026) it is driven over the
# official Blackmagic REST API on the camera's own USB-C network link (CDC-NCM), the way the shading
# v0.2.x relay does it: `curl` ON the cambox to http://<camera>/control/api/v1/<endpoint>, the camera
# found the relay's way (the cdc_ncm link of a vendor-1edb device, the DHCP lease's SERVER_ADDRESS,
# else the other host of the link's /30). gphoto2/PTP is gone. recording-e2e.sh calls
# camera_test_settings_enforce right AFTER the issue-808 relay pause (so the relay cannot write the
# camera at the same time) and after the temporary relay-restore trap (so an abort here still restores
# the relays). This lib only moves bytes: ssh + sysfs presence + curl get/put. EVERY decision (which
# endpoints, the baseline diff, the PUT list, the read-back grade, the presence/ack/pinned matrix)
# lives in the pure scripts/camera_test_settings.py (pytest Tier-0).
#
# Flow:
#   1. baseline status (scripts/camera-test-baseline.json): pinned = iso + a shutter unit have values.
#   2. resolve the box holding the camera: a sysfs idVendor=1edb (Blackmagic) scan over the
#      candidate boxes -- the relay-paused source cambox + cam2 -- first hit wins. No PTP session
#      is opened just to look.
#   3. decide(present, acked, pinned) -- acked = `testcam` named in CAMBOX_OFFLINE_ACK /
#      rig-fleet.txt (the existing scripts/lib/cambox-offline-ack.sh mechanism).
#   4. unverified-* -> a LOUD report-only UNVERIFIED (never a silent pass), return 0.
#      abort-*      -> a named ERROR, exit 1.
#      enforce      -> ONE REST read session, the plan, and when something differs: the issue-1271
#                      rig-busy guard, ONE PUT session, ONE read-back session, grade;
#                      a key that does not read back aborts. Nothing is restored after the run: the
#                      baseline IS the test state.
#
# The PRODUCTION exposure (owner, 26.9.2026: "ked vypina sa development tak ze aj vratis iso a
# uzavierku naspat"): right before the FIRST --set-config of a development period, enforce stores
# the values it just read (the owner's ISO + shutter) in ~/.camera-box/camera-prod-exposure.json on
# the runner -- only when no snapshot is waiting, so later runs never overwrite it. rig-mode.sh's
# EVENT switch calls camera_test_settings_restore (below): relay stopped, ONE read, the issue-1271
# guard, ONE set, ONE read-back, then the snapshot is moved aside as consumed. A failure there is
# loud and keeps the snapshot, but NEVER aborts the EVENT switch.
#
# Test seams: CAMERA_TEST_BASELINE (baseline path), CAMERA_TEST_SETTINGS_SSH_TIMEOUT (per-ssh bound),
# CAMERA_PROD_EXPOSURE_SNAPSHOT (snapshot path, resolved by camera_test_settings.py snapshot-path).

_CTS_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cambox-offline-ack.sh
. "$_CTS_LIB_DIR/cambox-offline-ack.sh"
# shellcheck source=scripts/lib/stray-session-check.sh
. "$_CTS_LIB_DIR/stray-session-check.sh"
# shellcheck source=scripts/lib/bkshading-relay-runtime.sh
. "$_CTS_LIB_DIR/bkshading-relay-runtime.sh"
# shellcheck source=scripts/lib/event-mode-discord-confirm.sh
. "$_CTS_LIB_DIR/event-mode-discord-confirm.sh"   # event_mode_discord_note_add, the ONE note writer (issue 1407)

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
# `CTS_USB:0`. Reads sysfs only (no network request to the camera).
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

# Remote exit codes of the session checks below.
camera_test_settings_rc_no_link() { printf '%s\n' 95; }
camera_test_settings_rc_no_curl() { printf '%s\n' 96; }
camera_test_settings_rc_relay_active() { printf '%s\n' 97; }

# The endpoints a GET may name (camera_test_settings.py READ_ENDPOINTS), checked before the text is built.
camera_test_settings_get_ok() {
  case "${1:-}" in
    video/iso | video/shutter | video/whiteBalance | video/whiteBalanceTint | lens/iris | system/format) return 0 ;;
    *) return 1 ;;
  esac
}

# The endpoints a PUT may name (the enforceable ones), and the shape of its JSON body: ONE numeric
# field. Both are checked here before the text is built, so a body can never carry shell syntax.
camera_test_settings_put_ok() {
  case "${1:-}" in
    video/iso | video/shutter | video/whiteBalance | video/whiteBalanceTint | lens/iris) ;;
    *) return 1 ;;
  esac
  [[ "${2:-}" =~ ^\{\"[abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ]+\":-?[0123456789]+(\.[0123456789]+)?\}$ ]]
}

# Remote text for ONE REST session: `get EP...` (one `CTS_GET <ep> <http code> <body>` line each) or
# `put EP|JSON...` (one `CTS_PUT <ep> <http code>` line each). Before any request, in the same
# command:
#   - the relay unit must be really stopped (issue-808 pause): only inactive / failed / unknown (or no
#     systemctl answer) pass, anything else exits 97 -- a running relay would write the camera too;
#   - curl must exist (exit 96);
#   - the camera's address: the cdc_ncm link of a vendor-1edb USB device, the lease's SERVER_ADDRESS
#     (else ROUTER), else the other host of the link's /30 -- exit 95 when there is no such link or
#     no address (no camera link file, the camera's Web media manager off, the link not up yet).
# Every request is bounded (`curl -m 5`), and the line names the camera found (`CTS_CAMERA`).
# shellcheck disable=SC2016  # the $(...) / $_cts_* are REMOTE shell text, expanded on the box
camera_test_settings_rest_cmd() {
  local mode="${1:?get|put}" unit spec ep body
  shift
  unit="$(bkshading_relay_unit_name)"
  # issue 1452 (owner 10.11.2026: the REST relay runs in every mode, the E2E pause leaves it on): on a
  # box whose relay is always on, the session stops the relay only for its own requests and the EXIT
  # trap starts it again whatever the session ends with, so its writes never race the relay's.
  if [ -n "${CTS_FOUND_LABEL:-}" ] && bkshading_relay_box_always_on "$CTS_FOUND_LABEL"; then
    printf '_cts_hold=0; if systemctl is-active --quiet %s 2>/dev/null; then systemctl stop %s 2>/dev/null && _cts_hold=1; fi; ' "$unit" "$unit"
    printf 'trap '"'"'[ "$_cts_hold" = 1 ] && systemctl start %s 2>/dev/null; echo "CTS_RELAY_HOLD $_cts_hold"'"'"' EXIT; ' "$unit"
  fi
  printf '_cts_rs="$(systemctl is-active %s 2>/dev/null)"; case "$_cts_rs" in inactive|failed|unknown|"") ;; *) echo "CTS_RELAY_ACTIVE $_cts_rs"; exit %s;; esac; ' \
    "$unit" "$(camera_test_settings_rc_relay_active)"
  printf 'command -v curl >/dev/null 2>&1 || { echo CTS_NO_CURL; exit %s; }; ' "$(camera_test_settings_rc_no_curl)"
  printf '_cts_net=%q; _cts_lease=%q; ' "${CAMERA_TEST_SETTINGS_NET_SYSFS:-/sys/class/net}" "${CAMERA_TEST_SETTINGS_LEASES_DIR:-/run/systemd/netif/leases}"
  printf '%s' '_cts_cam=""; _cts_if=""; for _cts_n in "$_cts_net"/*; do '
  printf '%s' '_cts_d="$(readlink -f "$_cts_n/device/driver" 2>/dev/null)"; [ "${_cts_d##*/}" = cdc_ncm ] || continue; '
  printf '%s' '[ "$(cat "$(readlink -f "$_cts_n/device" 2>/dev/null)/../idVendor" 2>/dev/null)" = 1edb ] || continue; '
  printf '%s' '_cts_if="${_cts_n##*/}"; _cts_ix="$(cat "$_cts_n/ifindex" 2>/dev/null)"; '
  printf '%s' '_cts_cam="$(sed -n "s/^SERVER_ADDRESS=//p" "$_cts_lease/$_cts_ix" 2>/dev/null | head -n 1)"; '
  printf '%s' '[ -n "$_cts_cam" ] || _cts_cam="$(sed -n "s/^ROUTER=//p" "$_cts_lease/$_cts_ix" 2>/dev/null | head -n 1 | cut -d" " -f1)"; '
  printf '%s' 'if [ -z "$_cts_cam" ]; then _cts_a="$(ip -4 -o addr show dev "$_cts_if" 2>/dev/null | awk "{print \$4}" | head -n 1)"; '
  printf '%s' 'case "$_cts_a" in */30) _cts_ip="${_cts_a%/30}"; _cts_l="${_cts_ip##*.}"; _cts_b=$((_cts_l / 4 * 4)); '
  printf '%s' 'if [ $((_cts_l - _cts_b)) = 1 ]; then _cts_p=$((_cts_b + 2)); else _cts_p=$((_cts_b + 1)); fi; _cts_cam="${_cts_ip%.*}.$_cts_p";; esac; fi; '
  printf '%s' 'break; done; '
  printf '[ -n "$_cts_cam" ] || { echo "CTS_NO_LINK ${_cts_if:-none}"; exit %s; }; ' "$(camera_test_settings_rc_no_link)"
  printf '%s' 'echo "CTS_CAMERA $_cts_if $_cts_cam"; _cts_t="$(mktemp)"; '
  for spec in "$@"; do
    if [ "$mode" = get ]; then
      camera_test_settings_get_ok "$spec" || { echo "camera_test_settings_rest_cmd: refused GET '$spec'" >&2; return 1; }
      printf '_cts_c="$(curl -s -m 5 -o "$_cts_t" -w "%%{http_code}" "http://$_cts_cam/control/api/v1/%s")"; echo "CTS_GET %s ${_cts_c:-000} $(tr -d "\\n\\r" <"$_cts_t" 2>/dev/null)"; : >"$_cts_t"; ' "$spec" "$spec"
    else
      ep="${spec%%|*}"
      body="${spec#*|}"
      camera_test_settings_put_ok "$ep" "$body" || { echo "camera_test_settings_rest_cmd: refused PUT '$spec'" >&2; return 1; }
      printf '_cts_c="$(curl -s -m 5 -o /dev/null -w "%%{http_code}" -X PUT -H "Content-Type: application/json" -d '"'"'%s'"'"' "http://$_cts_cam/control/api/v1/%s")"; echo "CTS_PUT %s ${_cts_c:-000}"; ' "$body" "$ep" "$ep"
    fi
  done
  printf '%s\n' 'rm -f "$_cts_t"'
}

# camera_test_settings_transport_abort RC LABEL IP WHAT [CONSEQUENCE] -> exits 1 with a message naming
# the cause of a failed ssh + REST session. Only called for a non-zero RC. CONSEQUENCE (default: the
# E2E's refusal) ends the generic transport message.
camera_test_settings_transport_abort() {
  local rc="$1" label="$2" ip="$3" what="$4" cause
  local consequence="${5:-refusing to run on an unknown exposure}"
  case "$rc" in
    "$(camera_test_settings_rc_relay_active)")
      echo "ERROR: issue 1371: bkshading-relay is still active on $label ($ip) -- the issue-808 relay pause did not take, so the $what would race the relay's own camera writes; refusing." >&2
      exit 1
      ;;
    "$(camera_test_settings_rc_no_curl)")
      echo "ERROR: issue 1452: curl is not available on $label ($ip) -- the camera's REST API cannot be reached, so the $what is refused." >&2
      exit 1
      ;;
    "$(camera_test_settings_rc_no_link)")
      echo "ERROR: issue 1452: the test camera is on USB ($label, $ip) but has no network link to it -- no cdc_ncm link of the camera with an address (the camera link file, verify-device.sh (bb); the camera's Blackmagic Camera Setup -> Network access -> Web media manager ON; firmware >= 8.6 on a Pocket 6K / 9.8 on a Pocket 4K) -- $consequence." >&2
      exit 1
      ;;
    124) cause="timed out" ;;
    127) cause="command not found on the box" ;;
    255) cause="ssh failed" ;;
    *) cause="curl or ssh error" ;;
  esac
  echo "ERROR: issue 1371: the test camera is on USB ($label, $ip) but its REST $what failed: transport rc=$rc ($cause) -- $consequence." >&2
  exit 1
}

_cts_prefix() { sed 's/^/    /'; }

# _cts_find_camera PW LABEL=IP [...] -> sets CTS_FOUND_LABEL / CTS_FOUND_IP (empty = not on USB of
# any candidate), CTS_CHECKED (every candidate, for the log) and CTS_UNREAD (the candidates whose
# presence probe did not answer). The sysfs probe only, first hit wins.
_cts_find_camera() {
  local pw="$1" spec label ip out p
  shift
  CTS_FOUND_LABEL=""
  CTS_FOUND_IP=""
  CTS_CHECKED=""
  CTS_UNREAD=""
  for spec in "$@"; do
    label="${spec%%=*}"
    ip="${spec#*=}"
    CTS_CHECKED="${CTS_CHECKED:+$CTS_CHECKED, }$label ($ip)"
    out="$(camera_test_settings_ssh "$ip" "$pw" "$(camera_test_settings_presence_cmds)" 2>/dev/null)" || true
    p="$(camera_test_settings_parse_presence "$out")"
    case "$p" in
      1)
        CTS_FOUND_LABEL="$label"
        CTS_FOUND_IP="$ip"
        return 0
        ;;
      0) ;;
      *) CTS_UNREAD="${CTS_UNREAD:+$CTS_UNREAD, }$label ($ip)" ;;
    esac
  done
  return 0
}

# camera_test_settings_enforce SCRIPTS_DIR STRIH STREAM CAM_PW LABEL=IP [LABEL=IP ...]
# Returns 0 on enforced-ok / report-only UNVERIFIED; exits 1 on every abort (call it as a BARE
# statement so the exit propagates, like stray_session_check_assert).
camera_test_settings_enforce() {
  local here="$1" strih="$2" stream="$3" pw="$4"
  # issue 1482: the rig's direct feed (cam2's HDMI straight into the splitter) has no test camera in the path
  if declare -F rig_test_feed_skips_test_camera >/dev/null && rig_test_feed_skips_test_camera; then
    return 0
  fi
  shift 4
  local py baseline ack status rc pinned
  local found_label="" found_ip="" unread="" checked="" present=0 acked=0 action
  py="$here/camera_test_settings.py"
  baseline="$(camera_test_settings_baseline_path "$here")"
  ack="$(camera_test_settings_ack_name)"
  echo "[0/8] test-camera shutter/ISO enforce over the camera's REST API on its USB-C link (issues 1371 + 1452; baseline $baseline)"
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

  _cts_find_camera "$pw" "$@"
  found_label="$CTS_FOUND_LABEL"
  found_ip="$CTS_FOUND_IP"
  checked="$CTS_CHECKED"
  unread="$CTS_UNREAD"
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
      echo "    The camera is not on USB of: $checked${unread:+ (UNREADABLE: $unread)}; the baseline $baseline has no iso + shutter pinned."
      echo "    Report-only until the supervisor pins the baseline (plug the BMPCC USB-C into the source cambox, then pin iso + the shutter)."
      echo "    Once iso + the shutter are pinned, an absent camera ABORTS the run."
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

  local read_eps raw plan setreqs readback grade read_rc
  rc=0
  read_eps="$(python3 "$py" read-endpoints)" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$read_eps" ]; then
    echo "ERROR: issue 1371: could not build the REST read endpoints (rc=$rc) -- refusing to run blind." >&2
    exit 1
  fi
  read_rc=0
  # shellcheck disable=SC2086  # read_eps = the words of camera_test_settings.py READ_ENDPOINTS, each checked by camera_test_settings_get_ok
  raw="$(camera_test_settings_ssh "$found_ip" "$pw" "$(camera_test_settings_rest_cmd get $read_eps)")" || read_rc=$?
  if [ "$action" = abort-unpinned ]; then
    local suggestion
    echo "ERROR: issue 1371: the test camera IS on USB ($found_label, $found_ip) but the baseline $baseline is not pinned (iso + a shutter unit null) -- refusing to run without a pinned exposure." >&2
    [ "$read_rc" -eq 0 ] || camera_test_settings_transport_abort "$read_rc" "$found_label" "$found_ip" read
    rc=0
    suggestion="$(python3 "$py" suggest <<<"$raw")" || rc=$?
    if [ "$rc" -eq 0 ]; then
      echo "    The camera reads now (pin these only after the owner confirms the camera is set right):" >&2
      echo "    $suggestion" >&2
    else
      echo "    The camera read was unreadable, so no values can be suggested." >&2
    fi
    exit 1
  fi
  [ "$read_rc" -eq 0 ] || camera_test_settings_transport_abort "$read_rc" "$found_label" "$found_ip" read

  printf '%s\n' "$raw" | { grep '^CTS_CAMERA ' || true; } | _cts_prefix
  rc=0
  plan="$(python3 "$py" plan --baseline "$baseline" <<<"$raw")" || rc=$?
  if [ "$rc" -eq 8 ]; then
    printf '%s\n' "$plan" | _cts_prefix
    echo "ERROR: issue 1452: the test camera reports its shutter in a unit the baseline $baseline does not pin (see above) -- refusing to guess a conversion." >&2
    exit 1
  fi
  if [ "$rc" -ne 0 ]; then
    echo "ERROR: issue 1371: the test camera is on USB ($found_label, $found_ip) but its REST read was unreadable (decision rc=$rc; video/iso + video/shutter must answer 200) -- refusing to run on an unknown exposure." >&2
    exit 1
  fi
  printf '%s\n' "$plan" | { grep -v '^SETREQ ' || true; } | _cts_prefix
  setreqs="$(_cts_requests "$plan" SETREQ)"
  if [ -z "$setreqs" ]; then
    echo "    ok: the test camera is already at the baseline -- nothing set"
    return 0
  fi

  # issue 1271: a camera --set-config is a rig mutation -- the shared read-only rig-busy guard runs
  # IMMEDIATELY before it (bare statement, its exit 1 propagates).
  stray_session_check_assert "$here" "$strih" "$stream" "the test-camera shutter/ISO set"
  # The owner's production exposure: stored from the values just read, BEFORE the camera changes,
  # only when no snapshot is waiting (the EVENT switch restores + consumes it). No record = no set.
  # The python output is captured first, so its exit code never depends on the caller's pipefail.
  local snap snap_out
  rc=0
  snap="$(python3 "$py" snapshot-path)" || rc=$?
  if [ "$rc" -eq 0 ] && [ -n "$snap" ]; then
    snap_out="$(python3 "$py" snapshot --baseline "$baseline" --snapshot "$snap" --box "$found_label" <<<"$raw")" || rc=$?
    [ -z "${snap_out:-}" ] || printf '%s\n' "$snap_out" | _cts_prefix
  fi
  if [ "$rc" -ne 0 ] || [ -z "$snap" ]; then
    echo "ERROR: issue 1371: the test camera's production exposure could not be stored (${snap:-no snapshot path}, rc=$rc, see above) -- refusing to overwrite the owner's ISO/shutter without a record the EVENT switch can restore." >&2
    exit 1
  fi
  readback="$(_cts_set_and_readback "$found_ip" "$found_label" "$pw" "$setreqs" "$read_eps" \
    "refusing to run on an unknown exposure")" || exit 1
  rc=0
  grade="$(python3 "$py" grade --baseline "$baseline" <<<"$readback")" || rc=$?
  printf '%s\n' "$grade" | _cts_prefix
  case "$rc" in
    0)
      echo "    ok: the test camera is set to the baseline and read back (issue 1371)"
      return 0
      ;;
    5)
      echo "ERROR: issue 1371: a test-camera setting did NOT read back after its PUT (see MISMATCH above) -- refusing to run on a camera that ignores the baseline." >&2
      exit 1
      ;;
    *)
      echo "ERROR: issue 1371: the test-camera read-back output was unreadable (decision rc=$rc) -- refusing to run on an unknown exposure." >&2
      exit 1
      ;;
  esac
}

# _cts_requests PLAN TAG -> the plan's `TAG <endpoint> <json>` lines as `endpoint|json` words (one per
# line), the form camera_test_settings_rest_cmd put takes.
_cts_requests() {
  printf '%s\n' "${1:-}" | sed -n "s/^$2 \([^ ]*\) \(.*\)$/\1|\2/p"
}

# _cts_set_and_readback IP LABEL PW SETREQS READ_EPS CONSEQUENCE -> ONE PUT session, then ONE read-back
# session; prints the read-back text. Shared by the E2E enforce and the EVENT restore, so both handle
# every transport code the same way. The session refusals (95/96/97) and a failed read-back exit 1
# through camera_test_settings_transport_abort (CONSEQUENCE ends the message); a PUT the camera answers
# with anything but 2xx, or a failed PUT session, is only a WARNING, because the read-back decides. Call
# it as `x="$(_cts_set_and_readback ...)" || exit 1` -- it runs in the command-substitution subshell.
_cts_set_and_readback() {
  local ip="$1" label="$2" pw="$3" setreqs="$4" read_eps="$5" consequence="$6" rc=0 out readback
  local -a reqs=()
  local putcmd
  mapfile -t reqs <<<"$setreqs"
  if ! putcmd="$(camera_test_settings_rest_cmd put "${reqs[@]}")"; then
    echo "ERROR: issue 1452: a planned PUT was refused before it was sent (see above) -- $consequence." >&2
    exit 1
  fi
  out="$(camera_test_settings_ssh "$ip" "$pw" "$putcmd")" || rc=$?
  case "$rc" in
    0) ;;
    "$(camera_test_settings_rc_relay_active)" | "$(camera_test_settings_rc_no_curl)" | "$(camera_test_settings_rc_no_link)")
      camera_test_settings_transport_abort "$rc" "$label" "$ip" set "$consequence"
      ;;
    *) echo "    WARNING: issue 1452: the PUT session exited rc=$rc -- the read-back decides" >&2 ;;
  esac
  printf '%s\n' "$out" | { grep '^CTS_PUT ' || true; } | while read -r _t ep code; do
    case "$code" in
      2??) ;;
      *) echo "    WARNING: issue 1452: PUT $ep answered HTTP $code -- the read-back decides" >&2 ;;
    esac
  done
  rc=0
  # shellcheck disable=SC2086  # read_eps = the words of camera_test_settings.py READ_ENDPOINTS, each checked by camera_test_settings_get_ok
  readback="$(camera_test_settings_ssh "$ip" "$pw" "$(camera_test_settings_rest_cmd get $read_eps)")" || rc=$?
  [ "$rc" -eq 0 ] || camera_test_settings_transport_abort "$rc" "$label" "$ip" read-back "$consequence"
  printf '%s\n' "$readback"
}

# ---------------------------------------------------------------------------------------------
# the EVENT-switch restore of the production exposure (called from rig-mode.sh's EVENT path)
# ---------------------------------------------------------------------------------------------

# The operator's manual way out, named in every restore failure: set the camera by hand AND move
# the snapshot aside, or the next EVENT switch writes the stale values over the owner's new ones.
camera_test_settings_manual_consume_hint() {
  printf '%s\n' "python3 scripts/camera_test_settings.py consume"
}

# Remote text: stop the relay on the camera box before the restore's REST sessions. The relay is
# normally already stopped (TEST mode stopped+disabled it); this covers a rig that ran an E2E
# outside TEST mode. The EVENT switch starts + enables the relay right after the restore.
camera_test_settings_relay_stop_cmds() {
  printf 'systemctl stop %s 2>/dev/null || true\n' "$(bkshading_relay_unit_name)"
}

# Exit code of the restore subshell when the camera IS back on its production values but the
# snapshot could not be moved aside (restored, yet the next EVENT switch would restore it again).
camera_test_settings_rc_unconsumed() { printf '%s\n' 22; }

# _cts_consume PY SNAP -> moves the snapshot aside as consumed (which also clears a restore-failed
# marker); exits 22 when that fails.
_cts_consume() {
  local py="$1" snap="$2" moved rc=0
  moved="$(python3 "$py" consume --snapshot "$snap")" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "WARNING: issue 1371: the production exposure is back on the camera, but the snapshot $snap could not be moved aside (rc=$rc, see above) -- move it by hand: $(camera_test_settings_manual_consume_hint), or the next EVENT switch restores it again." >&2
    exit "$(camera_test_settings_rc_unconsumed)"
  fi
  echo "    snapshot moved aside: $moved"
}

# _cts_restore_apply HERE STRIH STREAM PW SNAP LABEL=IP [...] -> run ONLY in a subshell (every
# failure is an `exit`, the shared transport/guard helpers exit too). exit 0 = restored + read back
# + consumed; 21 = the camera already had the production values (consumed, nothing set); 22 =
# restored but not consumed; anything else = NOT restored, the snapshot stays.
_cts_restore_apply() {
  local here="$1" strih="$2" stream="$3" pw="$4" snap="$5"
  shift 5
  local py read_eps raw plan setreqs readback grade rc read_rc
  local consequence="the production exposure was NOT restored"
  py="$here/camera_test_settings.py"
  _cts_find_camera "$pw" "$@"
  if [ -z "$CTS_FOUND_IP" ]; then
    echo "ERROR: issue 1371: the test camera is NOT on USB (no idVendor $(camera_test_settings_usb_vendor) on: $CTS_CHECKED${CTS_UNREAD:+; UNREADABLE: $CTS_UNREAD}) -- $consequence." >&2
    exit 1
  fi
  echo "    camera on USB: $CTS_FOUND_LABEL ($CTS_FOUND_IP)"
  # issue 1271: the relay stop and the camera's REST session are rig mutations too -- the shared
  # read-only rig-busy guard runs BEFORE them (its exit 1 ends this subshell, never the EVENT switch),
  # and again right before the set.
  stray_session_check_assert "$here" "$strih" "$stream" "the test-camera production exposure read"
  camera_test_settings_ssh "$CTS_FOUND_IP" "$pw" "$(camera_test_settings_relay_stop_cmds)" >/dev/null 2>&1 || true

  rc=0
  read_eps="$(python3 "$py" read-endpoints)" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$read_eps" ]; then
    echo "ERROR: issue 1371: could not build the REST read endpoints (rc=$rc) -- $consequence." >&2
    exit 1
  fi
  read_rc=0
  # shellcheck disable=SC2086  # read_eps = the words of camera_test_settings.py READ_ENDPOINTS, each checked by camera_test_settings_get_ok
  raw="$(camera_test_settings_ssh "$CTS_FOUND_IP" "$pw" "$(camera_test_settings_rest_cmd get $read_eps)")" || read_rc=$?
  [ "$read_rc" -eq 0 ] || camera_test_settings_transport_abort "$read_rc" "$CTS_FOUND_LABEL" "$CTS_FOUND_IP" read "$consequence"

  rc=0
  plan="$(python3 "$py" restore-plan --snapshot "$snap" <<<"$raw")" || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "ERROR: issue 1371: the restore could not be planned (decision rc=$rc: 3 = unreadable camera output, 7 = invalid snapshot, 8 = the camera reports its shutter in another unit than the snapshot) -- $consequence." >&2
    exit 1
  fi
  printf '%s\n' "$plan" | { grep -v '^RESTOREREQ ' || true; } | _cts_prefix
  setreqs="$(_cts_requests "$plan" RESTOREREQ)"
  if [ -z "$setreqs" ]; then
    _cts_consume "$py" "$snap"
    exit 21
  fi

  stray_session_check_assert "$here" "$strih" "$stream" "the test-camera production exposure restore"
  readback="$(_cts_set_and_readback "$CTS_FOUND_IP" "$CTS_FOUND_LABEL" "$pw" "$setreqs" "$read_eps" \
    "$consequence")" || exit 1
  rc=0
  grade="$(python3 "$py" restore-grade --snapshot "$snap" <<<"$readback")" || rc=$?
  printf '%s\n' "$grade" | _cts_prefix
  case "$rc" in
    0) ;;
    5)
      echo "ERROR: issue 1371: a production setting did NOT read back after its PUT (see MISMATCH above) -- $consequence." >&2
      exit 1
      ;;
    *)
      echo "ERROR: issue 1371: the restore read-back was unreadable (decision rc=$rc) -- $consequence." >&2
      exit 1
      ;;
  esac
  _cts_consume "$py" "$snap"
  exit 0
}

# _cts_restore_failed PY SNAP REASON -> the LOUD not-restored report (run log + a ::warning
# annotation), plus the restore-failed marker next to the snapshot for the handover check.
_cts_restore_failed() {
  local py="$1" snap="${2:-}" reason="$3"
  CTS_RESTORE_OUTCOME=failed
  echo "::warning title=issue 1371 production exposure NOT restored::the test camera's production ISO/shutter were NOT restored ($reason) -- production would start on the TEST exposure"
  echo "WARNING: issue 1371: the test camera's production ISO/shutter were NOT restored ($reason). Production would start on the TEST exposure. The snapshot ${snap:-(no path)} stays: run scripts/rig-mode.sh event again once the camera is on USB and the rig is not on air, or set the camera by hand AND move the snapshot aside: $(camera_test_settings_manual_consume_hint)." >&2
  if [ -n "$snap" ] && [ -e "$snap" ]; then
    python3 "$py" restore-failed --snapshot "$snap" --reason "$reason" >/dev/null 2>&1 || true
  fi
}

# camera_test_settings_restore SCRIPTS_DIR STRIH STREAM CAM_PW LABEL=IP [LABEL=IP ...]
# The rig-mode EVENT switch: put the production exposure the E2E snapshotted back on the test
# camera. NEVER exits (the whole camera part runs in a subshell); returns 0 when restored / nothing
# was pending, 1 otherwise (a loud named line; a not-restored snapshot is kept for a retry). Sets
# CTS_RESTORE_OUTCOME (none | restored | already | restored-unconsumed | failed) +
# CTS_RESTORE_SUMMARY for the Discord note.
camera_test_settings_restore() {
  local here="$1" strih="$2" stream="$3" pw="$4"
  shift 4
  local py snap rc=0
  py="$here/camera_test_settings.py"
  CTS_RESTORE_OUTCOME=failed
  CTS_RESTORE_SUMMARY=""
  echo "[exposure] issue 1371: restore the test camera's production ISO + shutter (snapshotted by the E2E before its first set) while the relay is still stopped"
  snap="$(python3 "$py" snapshot-path)" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$snap" ]; then
    _cts_restore_failed "$py" "" "the snapshot path could not be resolved, rc=$rc"
    return 1
  fi
  if [ ! -e "$snap" ]; then
    CTS_RESTORE_OUTCOME=none
    echo "    no production exposure snapshot pending ($snap): the E2E did not change the test camera since the last EVENT switch -- nothing to restore"
    return 0
  fi
  rc=0
  CTS_RESTORE_SUMMARY="$(python3 "$py" restore-status --snapshot "$snap")" || rc=$?
  if [ "$rc" -ne 0 ]; then
    CTS_RESTORE_SUMMARY=""
    _cts_restore_failed "$py" "$snap" "the snapshot is invalid, see above"
    return 1
  fi
  echo "    pending production exposure: $CTS_RESTORE_SUMMARY"
  rc=0
  # rig-mode.sh keeps the OBS WebSocket password in OBS_WS_PASSWORD; the shared rig-busy guard
  # reads OBS_PASSWORD. Hand it over inside the subshell only (an auth-enabled OBS would otherwise
  # fail the guard's read and let it fail OPEN).
  (
    OBS_PASSWORD="${OBS_PASSWORD:-${OBS_WS_PASSWORD:-}}"
    _cts_restore_apply "$here" "$strih" "$stream" "$pw" "$snap" "$@"
  ) || rc=$?
  case "$rc" in
    0)
      CTS_RESTORE_OUTCOME=restored
      echo "    ok: the test camera is back on its production exposure and read back (issue 1371): $CTS_RESTORE_SUMMARY"
      return 0
      ;;
    21)
      CTS_RESTORE_OUTCOME=already
      echo "    ok: the test camera already had its production exposure -- nothing set (issue 1371): $CTS_RESTORE_SUMMARY"
      return 0
      ;;
    "$(camera_test_settings_rc_unconsumed)")
      CTS_RESTORE_OUTCOME=restored-unconsumed
      echo "::warning title=issue 1371 production exposure snapshot not moved aside::the camera is back on its production exposure, but move the snapshot aside by hand: $(camera_test_settings_manual_consume_hint)"
      return 1
      ;;
    *)
      _cts_restore_failed "$py" "$snap" "rc=$rc, see above"
      return 1
      ;;
  esac
}

# camera_test_settings_restore_discord_note MSG_FILE -> add the restore outcome to the EVENT
# Discord confirmation (the owner reads THAT on the phone). A failure goes ON TOP of the message,
# a success at the end; nothing for `none`. The phone line never names a command (the owner cannot
# run it from the phone) -- the commands are in the run log. The writing is the ONE shared
# event_mode_discord_note_add (scripts/lib/event-mode-discord-confirm.sh, sourced above): the
# prepend is moved over only when complete, else appended, so the confirmation body is never lost.
# Never fails.
camera_test_settings_restore_discord_note() {
  local msg="${1:-}" line="" where=end
  [ -n "$msg" ] && [ -f "$msg" ] || return 0
  case "${CTS_RESTORE_OUTCOME:-none}" in
    restored) line="✅ Testovacia kamera: produkčná expozícia vrátená (${CTS_RESTORE_SUMMARY:-})." ;;
    already) line="✅ Testovacia kamera: produkčná expozícia už sedela (${CTS_RESTORE_SUMMARY:-})." ;;
    restored-unconsumed) line="✅ Testovacia kamera: produkčná expozícia vrátená (${CTS_RESTORE_SUMMARY:-}), ale snímku treba odložiť — napíš Claudovi." ;;
    failed)
      line="⚠️ Testovacia kamera: produkčná expozícia (ISO/uzávierka) sa NEVRÁTILA${CTS_RESTORE_SUMMARY:+ (čakala: $CTS_RESTORE_SUMMARY)} — kamera môže ostať na testovacej expozícii. Napíš Claudovi, nech ju vráti, keď je kamera na USB a rig nevysiela."
      where=top
      ;;
    *) return 0 ;;
  esac
  event_mode_discord_note_add "$msg" "$line" "$where"
  return 0
}
