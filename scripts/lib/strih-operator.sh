#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the strih operator-surface constants, the setup-strih
# step 16g install and the verify-strih item 43 grader; no top-level statements besides constants) --
# the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. setup-strih.sh / verify-strih.sh set their own.
# Every function checks its own return codes.
#
# scripts/lib/strih-operator.sh -- issue 1418 (owner 12.10.2026, 30 min before the SNV event): the strih
# notebook's X server stopped answering (OBS kept rendering, the panel froze), the Bluetooth mouse could
# not pair, the touchpad did not click, clicking the Multiview did not cut and the Stream Deck was stuck.
# verify-strih had passed: it graded services, OBS, genlock and NDI, never what the OPERATOR touches.
# ROZHODNUTE 6126870183: every hand fix on a notebook lands in setup + verify the same day. Design
# 6127043932 (approach 1): this ONE lib is the operator surface.
#
#   * strih_operator_install -- setup-strih step 16g (as root), every role:
#     - the OBS user.ini [BasicWindow] keys STRIH_OPERATOR_INI_KEYS: SaveProjectors=true +
#       ProjectorAlwaysOnTop=false (issue 1346, moved here from step 7: the operator's laptop Multiview
#       projector re-opens on every launch and never stays above the other windows; the OBS default is
#       SaveProjectors=false, and imag is the opposite: SaveProjectors=false + an autostart re-open hook) and
#       MultiviewMouseSwitch=true (clicking a Multiview tile cuts to that scene; it read false on
#       12.10.2026, most likely from the 11.10 user.ini backup restore). A RawConfigParser upsert that
#       never clobbers an unparseable file, and NEVER while OBS runs: OBS rewrites user.ini from memory
#       on exit, so a key written under a running OBS is lost (a WARN names it; verify grades it);
#     - every OTHER wired NetworkManager profile on the rig subnet: connection.autoconnect no, and down
#       when it is active (issue 1411: the onboard port's DHCP profile on the rig /23 won the routes and
#       the fleet clock broke for ~22 h). The subnet rule is strih_nic_rig_subnet_others (the ONE rule
#       verify item 36's (nic-homed) row uses). The rig NIC's own profile (its active device or its
#       connection.interface-name) is never touched, and nothing is touched while the rig address is
#       not on the rig NIC (the subnet is then unknown). An interface on the rig subnet that no
#       NetworkManager connection runs is named in a WARN (item 36 FAILs it).
#   * strih_operator_grade_rows / _report -- verify-strih item 43, read-only, one row each (FAIL unless
#     noted). The X reads run on display :0 as the operator with their Xauthority (as root through
#     `sudo -u`), each bounded by `timeout`, so a hung X server fails a row instead of hanging verify:
#       (tap-to-click)   the LIVE `libinput Tapping Enabled` = 1 on every touchpad X lists (xinput
#                        list-props): the xorg.conf.d file the baseline row grades can be present while
#                        tapping is off, as on 12.10.2026; a touchpad without the libinput property
#                        FAILs; NOTE when the box has no touchpad;
#       (bluetooth)      hci0 present, UP, non-zero BD address (`hciconfig hci0`; the 12.10.2026
#                        rtl8852bu read DOWN 00:00:00:00:00:00). Graded because the kiosk keeps
#                        bluetooth on these boxes (STRIH_OPERATOR_KIOSK_BLUETOOTH, pinned to the
#                        setup-strih step 11 call by a test);
#       (stream-deck)    the deck on USB and at least one Satellite surface, read through
#                        scripts/strih_satellite_watch.py (its USB ids, its sysfs reader, its
#                        /api/surfaces parser: the 30 s watch and verify read the deck one way); NOTE
#                        when no deck is plugged in, and on a role without a Satellite (step 16 gate);
#       (mv-click)       [BasicWindow] MultiviewMouseSwitch true in the operator's OBS user.ini;
#       (studio-mode)    GetStudioModeEnabled over the OBS websocket (no auth on the strih OBS); NOTE
#                        while OBS is not running;
#       (ws-seed)        the obs-websocket config step 7 writes: enabled, :4455, auth_required false;
#       (qt6-svg)        the package qt6-svg-plugins installed with both of its plugins (verify item 21
#                        grades the icon engine only);
#       (operator-input) X lists a real keyboard and a real pointer (never the XTEST devices, never the
#                        ACPI buttons every notebook lists as keyboards);
#       (operator-panel) two `ffmpeg -f x11grab` frames of :0 a few seconds apart differ while OBS
#                        runs: the 12.10.2026 X hang kept OBS rendering while the panel froze; NOTE
#                        while OBS is not running. A truly still picture (OBS paused on a still scene,
#                        no meters, no clock) would read as frozen.
#     The intercom hub is graded by verify item 16 (an inactive hub FAILs on the strih role).
#
# Test seams (tests/python/test_strih_operator_1418.py): STRIH_OPERATOR_OBS_RUNNING_CMD (`pgrep -x obs`),
# STRIH_OPERATOR_NMCLI (nmcli), STRIH_OPERATOR_IP_ADDR_CMD (`ip -o -4 addr show`), STRIH_OPERATOR_SYSFS
# (/sys), STRIH_OPERATOR_EUID (the running uid), STRIH_OPERATOR_XINPUT (xinput), STRIH_OPERATOR_HCICONFIG
# (hciconfig), STRIH_OPERATOR_STUDIO_CMD (default: a websocket-client read of STRIH_OPERATOR_OBS_WS_URL,
# ws://127.0.0.1:4455), STRIH_OPERATOR_GRAB_CMD (default: ffmpeg x11grab, one md5 per frame),
# STRIH_OPERATOR_GRAB_GAP_S (4), STRIH_OPERATOR_LIBDIR (/usr/lib/x86_64-linux-gnu), STRIH_OPERATOR_PYTHON
# (python3), COMPANION_SATELLITE_REST_URL (strih-provision.sh); `dpkg-query` and `sudo` are looked up on
# PATH; STRIH_NIC_IFACE overrides the rig NIC like everywhere else.

# The subnet rule + the rig-NIC verdict live in the NIC lib (verify item 36).
if ! declare -F strih_nic_rig_subnet_others >/dev/null; then
  # shellcheck source=scripts/lib/strih-nic-driver.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-nic-driver.sh"
fi

STRIH_OPERATOR_INI_KEYS=(SaveProjectors=true ProjectorAlwaysOnTop=false MultiviewMouseSwitch=true)
# What setup-strih step 11 passes obs_box_kiosk (pinned by a test): the kiosk keeps bluetooth, so the
# Bluetooth row is graded.
STRIH_OPERATOR_KIOSK_BLUETOOTH=keep-bluetooth
STRIH_OPERATOR_QT_SVG_PACKAGE=qt6-svg-plugins
# The ACPI / system "keyboards" X lists on every notebook (and a keyboard's media-key half): never the
# operator's keyboard. Matched against the lower-cased device name.
STRIH_OPERATOR_NOT_A_KEYBOARD='power button|sleep button|video bus|lid switch|hid events|hotkeys|extra buttons|pc speaker|consumer control|system control'

strih_operator_sysfs() { printf '%s' "${STRIH_OPERATOR_SYSFS:-/sys}"; }
_strih_operator_nmcli() { "${STRIH_OPERATOR_NMCLI:-nmcli}" "$@"; }

# _strih_operator_cmd VAR DEFAULT -> run the command a seam variable names (word-split), else DEFAULT.
_strih_operator_cmd() {
  local -a cmd
  read -r -a cmd <<<"${1:-$2}"
  "${cmd[@]}"
}

# strih_operator_obs_running -> rc 0 when an OBS process runs.
strih_operator_obs_running() {
  _strih_operator_cmd "${STRIH_OPERATOR_OBS_RUNNING_CMD:-}" "pgrep -x obs" >/dev/null 2>&1
}

# _strih_operator_ip_addrs -> `ip -o -4 addr show` (always rc 0).
_strih_operator_ip_addrs() {
  _strih_operator_cmd "${STRIH_OPERATOR_IP_ADDR_CMD:-}" "ip -o -4 addr show" 2>/dev/null || true
}

# --- setup-strih step 16g --------------------------------------------------------------------------

# strih_operator_user_ini_seed USER_INI DESKTOP_USER -> upsert STRIH_OPERATOR_INI_KEYS into [BasicWindow]
# of USER_INI (owned by DESKTOP_USER afterwards). Never while OBS runs, never over an unparseable file:
# both print a WARN line. Always rc 0 (verify item 43's (mv-click) row grades the result).
strih_operator_user_ini_seed() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}"
  if strih_operator_obs_running; then
    echo "  WARN: OBS is running -- it rewrites ${ini} from memory on exit, so [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} were NOT seeded; stop strih-obs.service and re-run setup-strih.sh (issue 1418)"
    return 0
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "  WARN: python3 absent -- cannot seed [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini}"
    return 0
  fi
  if python3 - "$ini" "${STRIH_OPERATOR_INI_KEYS[@]}" <<'PY'
import configparser, os, sys
path = sys.argv[1]
# strict=False tolerates duplicate keys/sections OBS may write; RawConfigParser avoids % interpolation.
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
if os.path.exists(path):
    try:
        cp.read(path)
    except Exception as e:
        # NEVER clobber a user.ini that cannot be parsed: a rewrite would DISCARD every other OBS setting.
        sys.stderr.write("user.ini parse failed (%s) -- leaving it untouched\n" % e)
        sys.exit(3)
if not cp.has_section("BasicWindow"):
    cp.add_section("BasicWindow")
for kv in sys.argv[2:]:
    k, v = kv.split("=", 1)
    cp.set("BasicWindow", k, v)
with open(path, "w") as fh:
    cp.write(fh, space_around_delimiters=False)
PY
  then
    chown "$user":"$user" "$ini" 2>/dev/null || true
    echo "  seeded [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini}"
  else
    echo "  WARN: could not seed [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini} (left untouched, above) -- fix the file and re-run setup-strih.sh"
  fi
  return 0
}

# strih_operator_nic_isolate -> take every OTHER wired NetworkManager profile off the rig subnet (see the
# header). rc 1 + a stderr line when nmcli cannot list or change a profile; rc 0 otherwise (a rig NIC or
# rig address that cannot be read is a WARN: nothing is touched blind).
strih_operator_nic_isolate() {
  local ip addrs nic pfx others ifcs="" covered=" " rows u t dev ifn name ac a on_rig changed=0 ifc addr
  ip="$(strih_lx_ip)" || return 1
  addrs="$(_strih_operator_ip_addrs)"
  if ! nic="$(strih_lx_rig_nic "$(strih_operator_sysfs)" "$addrs" "$ip" 2>/dev/null)"; then
    echo "  WARN: the rig NIC cannot be resolved -- no other profile taken off the rig subnet (verify item 36 grades it)"
    return 0
  fi
  if ! pfx="$(strih_nic_rig_prefix "$nic" "$ip" "$addrs")"; then
    echo "  WARN: ${ip} is not on the rig NIC ${nic} right now -- the rig subnet is unknown, no other profile touched (verify item 36 grades it)"
    return 0
  fi
  others="$(strih_nic_rig_subnet_others "$nic" "$ip" "$addrs")"
  while read -r ifc addr; do
    if [ -n "$ifc" ]; then ifcs="${ifcs} ${ifc}"; fi
  done <<<"$others"
  if ! rows="$(_strih_operator_nmcli -g UUID,TYPE,DEVICE connection show 2>/dev/null)"; then
    echo "strih-operator: nmcli cannot list the NetworkManager connections" >&2
    return 1
  fi
  while IFS=: read -r u t dev; do
    [ -n "$u" ] && [ "$dev" != "$nic" ] || continue
    on_rig=0
    if [ -n "$dev" ]; then
      case " ${ifcs} " in *" ${dev} "*) on_rig=1; covered="${covered}${dev} " ;; esac
    fi
    if [ "$t" != 802-3-ethernet ]; then
      if [ "$on_rig" = 1 ]; then
        echo "  WARN: ${dev} carries an address on the rig subnet ${ip}/${pfx} through the ${t} connection ${u} -- not a wired profile, left as it is; take it off the rig subnet by hand (verify item 36 FAILs it)"
      fi
      continue
    fi
    ifn="$(_strih_operator_nmcli -g connection.interface-name connection show "$u" 2>/dev/null || true)"
    [ "$ifn" != "$nic" ] || continue
    for a in $(_strih_operator_nmcli -g ipv4.addresses connection show "$u" 2>/dev/null | tr ',' ' ' || true); do
      if strih_nic_on_subnet "$ip" "$pfx" "$a"; then on_rig=1; fi
    done
    [ "$on_rig" = 1 ] || continue
    name="$(_strih_operator_nmcli -g connection.id connection show "$u" 2>/dev/null || true)"
    ac="$(_strih_operator_nmcli -g connection.autoconnect connection show "$u" 2>/dev/null || true)"
    if [ "$ac" != no ]; then
      _strih_operator_nmcli connection modify "$u" connection.autoconnect no || {
        echo "strih-operator: nmcli could not set connection.autoconnect no on '${name}' (${u})" >&2; return 1; }
      echo "  '${name}' (${u}): connection.autoconnect no -- it is on the rig subnet ${ip}/${pfx} next to the rig NIC ${nic}"
      changed=1
    fi
    if [ -n "$dev" ]; then
      _strih_operator_nmcli connection down "$u" >/dev/null || {
        echo "strih-operator: nmcli could not bring '${name}' (${u}) down on ${dev}" >&2; return 1; }
      echo "  '${name}' (${u}): brought down on ${dev} (it carried the rig subnet next to ${nic})"
      changed=1
    fi
  done <<<"$rows"
  for ifc in $ifcs; do
    case "$covered" in *" ${ifc} "*) ;; *)
      echo "  WARN: ${ifc} carries an address on the rig subnet ${ip}/${pfx} but no NetworkManager connection runs on it -- take it down by hand (verify item 36 FAILs it)" ;;
    esac
  done
  if [ "$changed" = 0 ]; then
    echo "  no other wired profile on the rig subnet ${ip}/${pfx} next to ${nic} needed a change -- unchanged"
  fi
  return 0
}

# strih_operator_install USER_INI DESKTOP_USER -> setup-strih step 16g: the user.ini seed (WARN-only)
# and the rig-subnet isolation (rc 1 on an nmcli failure).
strih_operator_install() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}"
  strih_operator_user_ini_seed "$ini" "$user"
  strih_operator_nic_isolate
}

# --- verify-strih item 43 --------------------------------------------------------------------------

# _strih_operator_on_x USER HOME SECONDS CMD... -> CMD on the kiosk display :0 with the operator's
# Xauthority (as root through `sudo -u USER`, else inline), bounded by `timeout SECONDS`.
_strih_operator_on_x() {
  local user="$1" home="$2" t="$3"
  shift 3
  if [ "${STRIH_OPERATOR_EUID:-$(id -u)}" = 0 ]; then
    sudo -u "$user" env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout "$t" "$@"
  else
    env DISPLAY=:0 XAUTHORITY="${home}/.Xauthority" timeout "$t" "$@"
  fi
}

# strih_operator_xinput USER HOME ARGS... -> `xinput ARGS...` on :0; nothing when X does not answer.
strih_operator_xinput() {
  local user="$1" home="$2"
  shift 2
  _strih_operator_on_x "$user" "$home" 10 "${STRIH_OPERATOR_XINPUT:-xinput}" "$@" 2>/dev/null || true
}

# strih_operator_xinput_slaves (stdin = `xinput list --short`) -> `ID|pointer|NAME` / `ID|keyboard|NAME`
# per slave device (masters and floating devices are skipped).
strih_operator_xinput_slaves() {
  LC_ALL=C awk -F'\t' -v arrow="$(printf '\342\206\263')" '
    NF >= 3 && $3 ~ /^\[slave +(pointer|keyboard)/ {
      kind = ($3 ~ /pointer/) ? "pointer" : "keyboard"
      id = $2; sub(/^id=/, "", id)
      name = $1; i = index(name, arrow)
      if (i > 0) name = substr(name, i + length(arrow))
      sub(/^ +/, "", name); sub(/ +$/, "", name)
      print id "|" kind "|" name
    }'
}

# strih_operator_tap_verdict (stdin = `NAME|VALUE` per pointer, VALUE = the live `libinput Tapping
# Enabled` 0 / 1, or `none` without the property) -> ONE row.
strih_operator_tap_verdict() {
  local name v on="" off="" nolib=""
  while IFS='|' read -r name v; do
    [ -n "$name" ] || continue
    case "$v" in
      1) on="${on}${on:+, }${name}" ;;
      0) off="${off}${off:+, }${name}" ;;
      *) case "${name,,}" in *touchpad*) nolib="${nolib}${nolib:+, }${name}" ;; esac ;;
    esac
  done
  if [ -n "$off" ] || [ -n "$nolib" ]; then
    printf 'FAIL|(tap-to-click)%s%s -- a tap does not click: re-run setup-strih.sh step 11 (30-touchpad-tap.conf) and restart the X session; live: xinput set-prop <device> "libinput Tapping Enabled" 1\n' \
      "${off:+ tap-to-click OFF on ${off} (libinput Tapping Enabled=0)}" \
      "${nolib:+ ${nolib} not driven by libinput (no Tapping property)}"
  elif [ -n "$on" ]; then
    printf 'OK|(tap-to-click) libinput Tapping Enabled=1 live on %s\n' "$on"
  else
    printf 'NOTE|(tap-to-click) X lists no touchpad -- not graded\n'
  fi
}

# _strih_operator_tap_lines USER HOME SLAVES -> `NAME|VALUE` per real pointer (strih_operator_tap_verdict).
_strih_operator_tap_lines() {
  local user="$1" home="$2" slaves="$3" id kind name props v
  while IFS='|' read -r id kind name; do
    [ "$kind" = pointer ] || continue
    case "${name,,}" in *xtest*) continue ;; esac
    props="$(strih_operator_xinput "$user" "$home" list-props "$id")"
    v="$(sed -n 's/^[[:space:]]*libinput Tapping Enabled ([0123456789]*):[[:space:]]*\([01]\)[[:space:]]*$/\1/p' <<<"$props")"
    v="${v%%$'\n'*}"
    printf '%s|%s\n' "$name" "${v:-none}"
  done <<<"$slaves"
}

# strih_operator_input_verdict (stdin = strih_operator_xinput_slaves) -> ONE row: a real keyboard AND a
# real pointer (never XTEST, never STRIH_OPERATOR_NOT_A_KEYBOARD).
strih_operator_input_verdict() {
  local id kind name low kb="" ptr="" miss=""
  while IFS='|' read -r id kind name; do
    [ -n "$id" ] || continue
    low="${name,,}"
    case "$low" in *xtest*) continue ;; esac
    if [ "$kind" = keyboard ]; then
      [[ "$low" =~ $STRIH_OPERATOR_NOT_A_KEYBOARD ]] && continue
      kb="${kb}${kb:+, }${name}"
    else
      ptr="${ptr}${ptr:+, }${name}"
    fi
  done
  [ -n "$kb" ] || miss="keyboard"
  [ -n "$ptr" ] || miss="${miss}${miss:+ and no }pointer"
  if [ -n "$miss" ]; then
    printf 'FAIL|(operator-input) X lists no %s the operator can use (only virtual / system devices) -- plug in or pair it, check the USB bus (dmesg)\n' "$miss"
  else
    printf 'OK|(operator-input) X lists keyboard: %s; pointer: %s\n' "$kb" "$ptr"
  fi
}

# strih_operator_bt_verdict HCICONFIG_TEXT -> ONE row: hci0 UP with a non-zero BD address.
strih_operator_bt_verdict() {
  local text="${1-}" addr bad=""
  if [ -z "$text" ]; then
    printf 'FAIL|(bluetooth) no hci0 Bluetooth controller (hciconfig hci0 answered nothing) -- the operator'"'"'s Bluetooth mouse cannot pair\n'
    return 0
  fi
  addr="$(sed -n 's/.*BD Address: \([0123456789ABCDEFabcdef:]*\).*/\1/p' <<<"$text")"
  addr="${addr%%$'\n'*}"
  if ! grep -Eq '^[[:space:]]+UP( |$)' <<<"$text"; then bad="DOWN"; fi
  case "$addr" in '' | 00:00:00:00:00:00) bad="${bad}${bad:+, }BD address ${addr:-unreadable}" ;; esac
  if [ -n "$bad" ]; then
    printf 'FAIL|(bluetooth) hci0 %s -- the Bluetooth controller does not work and the operator'"'"'s Bluetooth mouse cannot pair (the 12.10.2026 rtl8852bu: "Can'"'"'t init device hci0"); journalctl -k | grep -i bluetooth\n' "$bad"
  else
    printf 'OK|(bluetooth) hci0 UP, BD address %s\n' "$addr"
  fi
}

# strih_operator_bt_read -> `hciconfig hci0` (empty when absent; a missing tool prints a marker).
strih_operator_bt_read() {
  local tool="${STRIH_OPERATOR_HCICONFIG:-hciconfig}"
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf '<no-hciconfig>'
    return 0
  fi
  timeout 10 "$tool" hci0 2>/dev/null || true
}

# strih_operator_deck_read REPO -> `DECK|SURFACES|IDS|NAMES`: DECK 1 / 0 / ? (the deck on USB),
# SURFACES the Satellite's open surfaces (? when /api/surfaces does not answer, read only with the deck
# on USB), IDS the deck USB ids, NAMES the surfaces. Read through scripts/strih_satellite_watch.py.
# Empty when python3 or the module fails.
strih_operator_deck_read() {
  local repo="${1:?repo root required}"
  "${STRIH_OPERATOR_PYTHON:-python3}" - "${repo}/scripts" "$(strih_operator_sysfs)/bus/usb/devices" \
    "$(strih_companion_satellite_rest_url)" <<'PY' 2>/dev/null || true
import sys
sys.path.insert(0, sys.argv[1])
import strih_satellite_watch as w
deck = w.deck_on_usb(sys.argv[2], w.STREAM_DECK_USB_IDS)
names = None
if deck:
    raw, _err = w.fetch_json(sys.argv[3] + "/api/surfaces", w.HTTP_TIMEOUT_S)
    names = w.surface_names(raw)
print("|".join(["?" if deck is None else str(int(deck)), "?" if names is None else str(len(names)),
                ",".join(w.STREAM_DECK_USB_IDS), ", ".join(names or ())]))
PY
}

# strih_operator_deck_verdict DECK SURFACES IDS NAMES -> ONE row (strih_operator_deck_read's fields).
strih_operator_deck_verdict() {
  local deck="${1-}" surf="${2-}" ids="${3-}" names="${4-}"
  case "$deck" in
    1) ;;
    0) printf 'NOTE|(stream-deck) no Stream Deck (%s) on USB -- not graded; plug it in before a production\n' "$ids"; return 0 ;;
    '?') printf 'FAIL|(stream-deck) the USB device tree is unreadable -- cannot tell whether the Stream Deck is plugged in\n'; return 0 ;;
    *) printf 'FAIL|(stream-deck) the Stream Deck read did not answer (python3 + scripts/strih_satellite_watch.py)\n'; return 0 ;;
  esac
  case "$surf" in
    '?' | '') printf 'FAIL|(stream-deck) the Stream Deck (%s) is on USB but the Companion Satellite REST /api/surfaces does not answer -- is companion-satellite.service running? (item 39)\n' "$ids" ;;
    0) printf 'FAIL|(stream-deck) the Stream Deck (%s) is on USB but the Satellite lists no surface (the 12.10.2026 stuck deck) -- replug it, or systemctl --user restart companion-satellite\n' "$ids" ;;
    *) printf 'OK|(stream-deck) the Stream Deck is on USB and the Satellite lists %s surface(s) (%s)\n' "$surf" "$names" ;;
  esac
}

# strih_operator_mvclick_verdict USER_INI -> ONE row: [BasicWindow] MultiviewMouseSwitch is true (the
# way libobs config_get_bool reads it: true / on / yes, or a non-zero number).
strih_operator_mvclick_verdict() {
  local ini="${1:?user.ini path required}" v
  v="$(python3 - "$ini" <<'PY' 2>/dev/null || true
import configparser, sys
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
try:
    if not cp.read(sys.argv[1]):
        print("<missing file>")
        sys.exit(0)
except Exception:
    print("<unreadable file>")
    sys.exit(0)
print(cp.get("BasicWindow", "MultiviewMouseSwitch", fallback="<absent>"))
PY
)"
  case "${v,,}" in
    true | on | yes) ;;
    *) if ! [[ "$v" =~ ^-?[0123456789]+$ ]] || [ "$((10#${v#-}))" = 0 ]; then
         printf 'FAIL|(mv-click) [BasicWindow] MultiviewMouseSwitch=%s in %s -- clicking a Multiview tile does not cut; stop OBS and re-run setup-strih.sh step 16g (issue 1418)\n' "${v:-<unread>}" "$ini"
         return 0
       fi ;;
  esac
  printf 'OK|(mv-click) [BasicWindow] MultiviewMouseSwitch=%s in %s -- a Multiview click cuts\n' "$v" "$ini"
}

# strih_operator_studio_read -> true | false | nothing: Studio Mode of the strih OBS over its websocket.
strih_operator_studio_read() {
  local v
  if [ -n "${STRIH_OPERATOR_STUDIO_CMD:-}" ]; then
    v="$(_strih_operator_cmd "$STRIH_OPERATOR_STUDIO_CMD" "" 2>/dev/null || true)"
  else
    v="$(timeout 10 python3 -c 'import json, sys
from websocket import create_connection
ws = create_connection(sys.argv[1], timeout=5)
json.loads(ws.recv())
ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}))
json.loads(ws.recv())
ws.send(json.dumps({"op": 6, "d": {"requestType": "GetStudioModeEnabled", "requestId": "v43"}}))
while True:
    m = json.loads(ws.recv())
    if m.get("op") == 7 and (m.get("d") or {}).get("requestId") == "v43":
        e = (m["d"].get("responseData") or {}).get("studioModeEnabled")
        print("true" if e is True else "false" if e is False else "")
        break
ws.close()' "${STRIH_OPERATOR_OBS_WS_URL:-ws://127.0.0.1:4455}" 2>/dev/null || true)"
  fi
  printf '%s' "${v%%$'\n'*}"
}

# strih_operator_studio_verdict VALUE OBS_UP -> ONE row (OBS_UP 1 = an OBS process runs).
strih_operator_studio_verdict() {
  case "${1-}" in
    true) printf 'OK|(studio-mode) Studio Mode on (GetStudioModeEnabled)\n' ;;
    false) printf 'FAIL|(studio-mode) Studio Mode is OFF -- the preview/program cut flow is gone; strih_scenes.py --bootstrap turns it on at every OBS start (restart strih-obs.service), or turn it on in OBS\n' ;;
    *) if [ "${2-}" = 1 ]; then
         printf 'FAIL|(studio-mode) OBS runs but did not answer GetStudioModeEnabled on :4455 (websocket down, or a password set)\n'
       else
         printf 'NOTE|(studio-mode) not graded: OBS is not running\n'
       fi ;;
  esac
}

# strih_operator_ws_seed_verdict CONFIG_JSON -> ONE row: the obs-websocket config setup-strih step 7
# writes (server_enabled true, server_port 4455, auth_required false) is in place.
strih_operator_ws_seed_verdict() {
  local f="${1:?config path required}" v
  v="$(python3 - "$f" <<'PY' 2>/dev/null || true
import json, sys
try:
    with open(sys.argv[1]) as fh:
        d = json.load(fh)
except FileNotFoundError:
    print("missing")
    sys.exit(0)
except Exception:
    print("unreadable")
    sys.exit(0)
if not isinstance(d, dict):
    print("unreadable")
    sys.exit(0)
bad = []
if d.get("server_enabled") is not True:
    bad.append("server_enabled=%s" % json.dumps(d.get("server_enabled")))
p = d.get("server_port")
if not isinstance(p, int) or isinstance(p, bool) or p != 4455:
    bad.append("server_port=%s" % json.dumps(p))
if d.get("auth_required") is not False:
    bad.append("auth_required=%s" % json.dumps(d.get("auth_required")))
print("ok" if not bad else " ".join(bad))
PY
)"
  if [ "$v" = ok ]; then
    printf 'OK|(ws-seed) %s: server_enabled true, server_port 4455, auth_required false\n' "$f"
  else
    printf 'FAIL|(ws-seed) %s: %s -- every rig tool dials the no-auth :4455 server (tally, scene seeder, E2E); re-run setup-strih.sh step 7\n' "$f" "${v:-unread}"
  fi
}

# strih_operator_qtsvg_verdict LIBDIR -> ONE row: the qt6-svg-plugins package installed with both plugins.
strih_operator_qtsvg_verdict() {
  local libdir="${1:?usr libdir required}" bad="" f
  [ "$(dpkg-query -W -f='${Status}' "$STRIH_OPERATOR_QT_SVG_PACKAGE" 2>/dev/null || true)" = "install ok installed" ] \
    || bad="package ${STRIH_OPERATOR_QT_SVG_PACKAGE} not installed"
  for f in "$(strih_lx_qt6_svg_iconengine_path "$libdir")" "${libdir%/}/qt6/plugins/imageformats/libqsvg.so"; do
    [ -e "$f" ] || bad="${bad}${bad:+; }${f} missing"
  done
  if [ -n "$bad" ]; then
    printf 'FAIL|(qt6-svg) %s -- OBS 32'"'"'s SVG theme icons render blank; apt-get install %s (setup-strih.sh step 4)\n' "$bad" "$STRIH_OPERATOR_QT_SVG_PACKAGE"
  else
    printf 'OK|(qt6-svg) %s installed, icon engine + image format plugins present\n' "$STRIH_OPERATOR_QT_SVG_PACKAGE"
  fi
}

# strih_operator_grab USER HOME -> one digest line of a full frame of display :0 (nothing on failure).
strih_operator_grab() {
  local user="$1" home="$2" v
  if [ -n "${STRIH_OPERATOR_GRAB_CMD:-}" ]; then
    v="$(_strih_operator_cmd "$STRIH_OPERATOR_GRAB_CMD" "" 2>/dev/null || true)"
  else
    v="$(_strih_operator_on_x "$user" "$home" 20 ffmpeg -hide_banner -loglevel error -nostdin \
      -f x11grab -i :0.0 -frames:v 1 -f md5 - 2>/dev/null || true)"
  fi
  printf '%s' "${v%%$'\n'*}"
}

# strih_operator_panel_verdict DIGEST1 DIGEST2 GAP_S -> ONE row: the panel changed between two frames.
strih_operator_panel_verdict() {
  local d1="${1-}" d2="${2-}" gap="${3-}"
  if [ -z "$d1" ] || [ -z "$d2" ]; then
    printf 'FAIL|(operator-panel) ffmpeg x11grab of :0 did not answer within 20 s -- the X server does not answer clients (the 12.10.2026 hang), or ffmpeg is missing\n'
  elif [ "$d1" = "$d2" ]; then
    printf 'FAIL|(operator-panel) two x11grab frames of :0 %s s apart are identical while OBS runs -- the panel is frozen (the 12.10.2026 X hang: OBS rendered, the operator saw a still picture)\n' "$gap"
  else
    printf 'OK|(operator-panel) the panel is updating: two x11grab frames of :0 %s s apart differ while OBS runs\n' "$gap"
  fi
}

# strih_operator_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|` / `NOTE|` / `FAIL|text`
# (strih_operator_row_count of them, in the header's order). Read-only; always rc 0.
strih_operator_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local xl slaves bt why deck dk_usb dk_surf dk_ids dk_names obs_up=0 gap d1 d2
  xl="$(strih_operator_xinput "$user" "$home" list --short)"
  slaves="$(strih_operator_xinput_slaves <<<"$xl")"
  if [ -z "$xl" ]; then
    printf 'FAIL|(tap-to-click) not graded: xinput on :0 answered nothing -- X is down or does not answer clients\n'
  else
    _strih_operator_tap_lines "$user" "$home" "$slaves" | strih_operator_tap_verdict
  fi
  bt="$(strih_operator_bt_read)"
  if [ "$STRIH_OPERATOR_KIOSK_BLUETOOTH" != keep-bluetooth ]; then
    printf 'NOTE|(bluetooth) not graded: the kiosk does not keep bluetooth on this box\n'
  elif [ "$bt" = '<no-hciconfig>' ]; then
    printf 'FAIL|(bluetooth) hciconfig is not installed (package bluez) -- the Bluetooth controller cannot be read\n'
  else
    strih_operator_bt_verdict "$bt"
  fi
  if why="$(obs_box_step_applies 16)"; then
    deck="$(strih_operator_deck_read "$repo")"
    IFS='|' read -r dk_usb dk_surf dk_ids dk_names <<<"$deck"
    strih_operator_deck_verdict "$dk_usb" "$dk_surf" "$dk_ids" "$dk_names"
  else
    printf 'NOTE|(stream-deck) not graded: %s\n' "$why"
  fi
  strih_operator_mvclick_verdict "${home}/.config/obs-studio/user.ini"
  if strih_operator_obs_running; then obs_up=1; fi
  strih_operator_studio_verdict "$(strih_operator_studio_read)" "$obs_up"
  strih_operator_ws_seed_verdict "${home}/.config/obs-studio/plugin_config/obs-websocket/config.json"
  strih_operator_qtsvg_verdict "${STRIH_OPERATOR_LIBDIR:-/usr/lib/x86_64-linux-gnu}"
  if [ -z "$xl" ]; then
    printf 'FAIL|(operator-input) xinput on :0 answered nothing -- X is down or does not answer clients (the 12.10.2026 hang)\n'
  else
    strih_operator_input_verdict <<<"$slaves"
  fi
  if [ "$obs_up" = 0 ]; then
    printf 'NOTE|(operator-panel) not graded: OBS is not running\n'
    return 0
  fi
  gap="${STRIH_OPERATOR_GRAB_GAP_S:-4}"
  d1="$(strih_operator_grab "$user" "$home")"
  sleep "$gap"
  d2="$(strih_operator_grab "$user" "$home")"
  strih_operator_panel_verdict "$d1" "$d2" "$gap"
  return 0
}

strih_operator_row_count() { printf '9'; }

# strih_operator_grade_report REPO USER_HOME DESKTOP_USER -> print the rows through the CALLER's ok / note
# / bad functions (verify-strih). rc 1 unless the grader printed every row.
strih_operator_grade_report() {
  local st d rows=0
  while IFS='|' read -r st d; do
    [ -n "$st" ] || continue
    rows=$((rows + 1))
    case "$st" in
      OK) ok "$d" ;;
      NOTE) note "$d" ;;
      *) bad "$d" ;;
    esac
  done < <(strih_operator_grade_rows "$@")
  [ "$rows" = "$(strih_operator_row_count)" ]
}
