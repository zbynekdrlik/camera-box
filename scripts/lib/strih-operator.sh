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
#                        rtl8852bu read DOWN 00:00:00:00:00:00). Graded on the strih role (setup-strih
#                        step 11 passes keep-bluetooth to the kiosk, pinned by a test); a NOTE on the
#                        stream role (owner 13.10.2026: "na stream-pp netreba bluethooth ani na stream
#                        snv, iba na strih-snv treba");
#       (stream-deck)    the deck on USB and at least one Satellite surface, read through
#                        scripts/strih_satellite_watch.py (its USB ids, its sysfs reader, its
#                        /api/surfaces parser: the 30 s watch and verify read the deck one way); NOTE
#                        when no deck is plugged in, and on a role without a Satellite (step 16 gate);
#       (mv-click)       [BasicWindow] MultiviewMouseSwitch true in the operator's OBS user.ini, read
#                        the way libobs config_get_bool reads it; an absent key FAILs as not seeded;
#       (studio-mode)    GetStudioModeEnabled over the OBS websocket (no auth on the strih OBS); NOTE
#                        while OBS is not running;
#       (ws-seed)        the obs-websocket config step 7 writes: enabled, :4455, auth_required false;
#       (qt6-svg)        the package qt6-svg-plugins installed with both of its plugins (verify item 21
#                        grades the icon engine only);
#       (operator-input) X lists a real keyboard and a real pointer (never the XTEST devices, never the
#                        ACPI buttons every notebook lists as keyboards);
#       (panel-gpu)      `xrandr --listproviders` on :0: every provider after Provider 0 (the X screen's
#                        GPU) that lists `Sink Output` (the reverse-PRIME panel sink) must be named
#                        `modesetting`, the Xorg DDX the stable Intel notebook ran it on. Any other name
#                        is a vendor DDX: strih-snv's AMD 780M sink ran xf86-video-amdgpu and its panel
#                        commit waited forever on an NVIDIA fence (11/12.10.2026, ROZHODNUTE 6132283178)
#                        -- FAIL naming the DDX package the shared baseline removes; NOTE with
#                        one provider; a read that fails is a hung X;
#       (drm-hang)       this boot's kernel log (`journalctl -k -b 0`): a hung task (`blocked for more
#                        than`) whose trace within STRIH_OPERATOR_KLOG_TRACE_LINES lines (before the next
#                        `INFO: task`) has a `commit_work+0x` or `drm_atomic_helper_wait_for_fences+0x`
#                        frame, or a DRM `*ERROR*` commit timeout (`flip_done` / `hw_done` / `commit wait`
#                        timed out, amdgpu_dm `Waiting for fences timed out`, nvidia-drm `Flip event
#                        timeout`), FAILs naming its first time stamp (the 11.10 hang sat 18 h in the log
#                        before anyone saw the frozen panel); an unreadable log, or one with no kernel
#                        line (`-- No entries --` for a user outside adm/systemd-journal), FAILs;
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
# (python3), STRIH_OPERATOR_XRANDR (a file read instead of `xrandr --listproviders` on :0),
# STRIH_OPERATOR_KLOG (a file read instead of `journalctl -k -b 0 --no-pager`),
# COMPANION_SATELLITE_REST_URL (strih-provision.sh); `dpkg-query`, `sudo`, `xrandr` and `journalctl` are
# looked up on PATH; STRIH_NIC_IFACE overrides the rig NIC like everywhere else.

# The subnet rule + the rig-NIC verdict live in the NIC lib (verify item 36).
if ! declare -F strih_nic_rig_subnet_others >/dev/null; then
  # shellcheck source=scripts/lib/strih-nic-driver.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-nic-driver.sh"
fi
# The ONE "run on the kiosk display :0 as the operator" helper lives in the session-apps lib.
if ! declare -F strih_kiosk_x_run >/dev/null; then
  # shellcheck source=scripts/lib/strih-session-apps.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-session-apps.sh"
fi
# The operator keyboard (step 16g + the keyboard row) lives in its own lib (issue 1441).
if ! declare -F strih_operator_keyboard_install >/dev/null; then
  # shellcheck source=scripts/lib/strih-keyboard.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-keyboard.sh"
fi

# issue 1419: + strih-snv's four [BasicWindow] keys (owner 13.10.2026, the full strih-snv OBS setup).
STRIH_OPERATOR_INI_KEYS=(SaveProjectors=true ProjectorAlwaysOnTop=false MultiviewMouseSwitch=true
  CloseExistingProjectors=true WarnBeforeStartingStream=false WarnBeforeStoppingStream=false
  WarnBeforeStoppingRecord=false)
STRIH_OPERATOR_QT_SVG_PACKAGE=qt6-svg-plugins
# The ACPI / system "keyboards" X lists on every notebook (and a keyboard's media-key half): never the
# operator's keyboard. Matched against the lower-cased device name.
STRIH_OPERATOR_NOT_A_KEYBOARD='power button|sleep button|video bus|lid switch|hid events|hotkeys|extra buttons|pc speaker|consumer control|system control'
# (panel-gpu): the amdgpu Xorg DDX package the shared baseline removes (obs_box_amdgpu_ddx_package,
# test-pinned equal).
STRIH_OPERATOR_AMDGPU_DDX_PKG=xserver-xorg-video-amdgpu
# (drm-hang): how many lines after a hung-task line its trace may run (the 11.10 trace: 20 lines).
STRIH_OPERATOR_KLOG_TRACE_LINES=25

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
import configparser, os, sys, tempfile
path = sys.argv[1]
if os.path.islink(path):
    # setup-strih runs as root: never write or chown through a link the desktop user could point anywhere.
    sys.stderr.write("%s is a symlink -- refusing to write it\n" % path)
    sys.exit(4)
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
# A temp file in the same directory renamed over the name: a reader never sees half a file, and the
# rename replaces a link instead of writing through it (a symlink was refused above). The mode is kept.
mode = os.stat(path).st_mode & 0o777 if os.path.exists(path) else 0o644
fd, tmp = tempfile.mkstemp(prefix=".user.ini.", dir=os.path.dirname(os.path.abspath(path)))
try:
    with os.fdopen(fd, "w") as fh:
        cp.write(fh, space_around_delimiters=False)
    os.chmod(tmp, mode)
    os.replace(tmp, path)
except BaseException:
    os.unlink(tmp)
    raise
PY
  then
    chown -h "$user":"$user" "$ini" 2>/dev/null || true
    echo "  seeded [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini}"
  else
    echo "  WARN: could not seed [BasicWindow] ${STRIH_OPERATOR_INI_KEYS[*]} in ${ini} (left untouched: unparseable, or a symlink, above) -- fix the file and re-run setup-strih.sh"
  fi
  return 0
}

# strih_operator_nic_isolate -> take every OTHER wired NetworkManager profile off the rig subnet (see the
# header). rc 1 + a stderr line when nmcli cannot list or change a profile; rc 0 otherwise (a rig NIC or
# rig address that cannot be read is a WARN: nothing is touched blind).
strih_operator_nic_isolate() {
  local ip addrs nic pfx others ifcs="" covered=" " rows u t dev ifn name ac a on_rig touched=0 ifc addr
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
    ifn="$(_strih_operator_nmcli -g connection.interface-name connection show "$u" </dev/null 2>/dev/null || true)"
    [ "$ifn" != "$nic" ] || continue
    for a in $(_strih_operator_nmcli -g ipv4.addresses connection show "$u" </dev/null 2>/dev/null | tr ',' ' ' || true); do
      if strih_nic_on_subnet "$ip" "$pfx" "$a"; then on_rig=1; fi
    done
    name="$(_strih_operator_nmcli -g connection.id connection show "$u" </dev/null 2>/dev/null || true)"
    if [ "$on_rig" = 0 ]; then
      # An inactive DHCP profile has no address to place: the 11.10.2026 onboard profile was one, and it
      # landed on the rig subnet when a cable went in. Named, never changed blind.
      if [ -z "$dev" ] && [ "$(_strih_operator_nmcli -g ipv4.method connection show "$u" </dev/null 2>/dev/null || true)" = auto ]; then
        echo "  WARN: '${name}' (${u}) is an inactive DHCP wired profile -- its subnet is unknown until a cable goes in; if it lands on the rig subnet verify item 36 FAILs (nmcli connection modify <name> connection.autoconnect no keeps it down)"
      fi
      continue
    fi
    ac="$(_strih_operator_nmcli -g connection.autoconnect connection show "$u" </dev/null 2>/dev/null || true)"
    if [ "$ac" != no ]; then
      _strih_operator_nmcli connection modify "$u" connection.autoconnect no </dev/null || {
        echo "strih-operator: nmcli could not set connection.autoconnect no on '${name}' (${u})" >&2; return 1; }
      echo "  '${name}' (${u}): connection.autoconnect no -- it is on the rig subnet ${ip}/${pfx} next to the rig NIC ${nic}"
      touched=1
    fi
    if [ -n "$dev" ]; then
      _strih_operator_nmcli connection down "$u" </dev/null >/dev/null || {
        echo "strih-operator: nmcli could not bring '${name}' (${u}) down on ${dev}" >&2; return 1; }
      echo "  '${name}' (${u}): brought down on ${dev} (it carried the rig subnet next to ${nic})"
      touched=1
    fi
  done <<<"$rows"
  for ifc in $ifcs; do
    case "$covered" in *" ${ifc} "*) ;; *)
      echo "  WARN: ${ifc} carries an address on the rig subnet ${ip}/${pfx} but no NetworkManager connection runs on it -- take it down by hand (verify item 36 FAILs it)" ;;
    esac
  done
  if [ "$touched" = 0 ]; then
    echo "  no other wired profile on the rig subnet ${ip}/${pfx} next to ${nic} needed a change -- unchanged"
  fi
  return 0
}

# strih_operator_hdmi_shows_multiview DRM_JSON -> rc 0 when the fixed HDMI output
# (~/.camera-box/drm-output.json, issue 1346) is enabled and shows the built-in Multiview, else 1
# (missing / unreadable / disabled / the Program view).
strih_operator_hdmi_shows_multiview() {
  local f="${1-}"
  [ -n "$f" ] && [ -f "$f" ] || return 1
  python3 - "$f" <<'PY'
import json, sys
try:
    with open(sys.argv[1]) as fh:
        d = json.load(fh)
except (OSError, ValueError):
    sys.exit(1)
sys.exit(0 if isinstance(d, dict) and d.get("enabled") is True and d.get("view") == "multiview" else 1)
PY
}

# strih_operator_projector_seed COLLECTION DESKTOP_USER [DRM_JSON] -> issue 1419: the fullscreen
# Multiview projector seeded into an EMPTY saved_projectors list of the box's collection
# (scripts/strih_projector_seed.py, operator-wins), only while OBS is stopped. Skipped when the HDMI
# output already shows the Multiview (owner 13.10.2026, strih-pp: a window next to it is a duplicate).
# WARN-only: a missing collection is named, never fatal.
strih_operator_projector_seed() {
  local coll="${1:?collection path required}" user="${2:?desktop user required}" drm="${3-}"
  if strih_operator_hdmi_shows_multiview "$drm"; then
    echo "  SKIP: the Multiview is on the HDMI output (${drm}: view multiview) -- no projector window seeded"
    return 0
  fi
  if strih_operator_obs_running; then
    echo "  WARN: OBS is running -- it rewrites ${coll} on exit, so the Multiview projector was NOT seeded; stop strih-obs.service and re-run setup-strih.sh (issue 1419)"
    return 0
  fi
  if python3 "$(dirname "${BASH_SOURCE[0]}")/../strih_projector_seed.py" --collection "$coll"; then
    chown -h "$user":"$user" "$coll" 2>/dev/null || true
  else
    echo "  WARN: the Multiview projector could not be seeded into ${coll} (above) -- port the collection first (issue 1419)"
  fi
  return 0
}

# strih_operator_install USER_INI DESKTOP_USER [COLLECTION] -> setup-strih step 16g: the user.ini seed
# (WARN-only), the Multiview projector seed when COLLECTION is given (WARN-only, issue 1419) and the
# rig-subnet isolation (rc 1 on an nmcli failure).
strih_operator_install() {
  local ini="${1:?user.ini path required}" user="${2:?desktop user required}" coll="${3:-}"
  strih_operator_user_ini_seed "$ini" "$user"
  local uhome=""
  uhome="$(getent passwd "$user" | cut -d: -f6)"
  if [ -n "$coll" ]; then strih_operator_projector_seed "$coll" "$user" "${uhome}/.camera-box/drm-output.json"; fi
  strih_operator_keyboard_install "$user" "$uhome" ||
    echo "  WARN: the keyboard file $(strih_operator_keyboard_file) could not be written -- verify-strih (keyboard) FAILs until it is"
  strih_operator_nic_isolate
}

# --- verify-strih item 43 --------------------------------------------------------------------------

# _strih_operator_on_x USER HOME SECONDS CMD... -> CMD on the kiosk display :0 as USER, bounded
# (strih_kiosk_x_run, the session-apps lib).
_strih_operator_on_x() {
  local user="$1" home="$2" t="$3"
  shift 3
  strih_kiosk_x_run "${STRIH_OPERATOR_EUID:-$(id -u)}" "$user" "$home" "$t" "$@"
}

strih_operator_xinput_tool() { printf '%s' "${STRIH_OPERATOR_XINPUT:-xinput}"; }

# strih_operator_xinput USER HOME ARGS... -> `xinput ARGS...` on :0; nothing when X does not answer.
strih_operator_xinput() {
  local user="$1" home="$2"
  shift 2
  _strih_operator_on_x "$user" "$home" 10 "$(strih_operator_xinput_tool)" "$@" 2>/dev/null || true
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
# Enabled` 0 / 1, `none` without the property, `unread` when `xinput list-props` did not answer) -> ONE row.
strih_operator_tap_verdict() {
  local name v on="" off="" nolib="" unread=""
  while IFS='|' read -r name v; do
    [ -n "$name" ] || continue
    case "$v" in
      1) on="${on}${on:+, }${name}" ;;
      0) off="${off}${off:+, }${name}" ;;
      unread) unread="${unread}${unread:+, }${name}" ;;
      *) case "${name,,}" in *touchpad*) nolib="${nolib}${nolib:+, }${name}" ;; esac ;;
    esac
  done
  if [ -n "$off" ] || [ -n "$nolib" ] || [ -n "$unread" ]; then
    printf 'FAIL|(tap-to-click)%s%s%s -- a tap does not click: re-run setup-strih.sh step 11 (30-touchpad-tap.conf) and restart the X session; live: xinput set-prop <device> "libinput Tapping Enabled" 1\n' \
      "${off:+ tap-to-click OFF on ${off} (libinput Tapping Enabled=0)}" \
      "${nolib:+ ${nolib} not driven by libinput (no Tapping property)}" \
      "${unread:+ xinput list-props did not answer for ${unread} (X not answering?)}"
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
    if [ -z "$props" ]; then
      printf '%s|unread\n' "$name"
      continue
    fi
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

# strih_operator_ini_verdict USER_INI -> ONE row (operator-ini, issue 1419): every STRIH_OPERATOR_INI_KEYS
# key=value present in [BasicWindow] (booleans compared case-insensitively, the way OBS writes them).
strih_operator_ini_verdict() {
  local ini="${1:?user.ini path required}" v
  v="$(python3 - "$ini" "${STRIH_OPERATOR_INI_KEYS[@]}" <<'PY' 2>/dev/null || true
import configparser, sys
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
try:
    if not cp.read(sys.argv[1]):
        print("FAIL|the file is missing")
        sys.exit(0)
except Exception:
    print("FAIL|the file is unreadable")
    sys.exit(0)
bad = []
for kv in sys.argv[2:]:
    k, want = kv.split("=", 1)
    got = cp.get("BasicWindow", k, fallback=None)
    if got is None or got.lower() != want.lower():
        bad.append("%s=%s (want %s)" % (k, got, want))
print(("FAIL|" + ", ".join(bad)) if bad else "OK|%d keys seeded" % (len(sys.argv) - 2))
PY
)"
  case "${v%%|*}" in
    OK) printf 'OK|(operator-ini) [BasicWindow] %s in %s\n' "${v#*|}" "$ini" ;;
    *) printf 'FAIL|(operator-ini) [BasicWindow] %s in %s -- stop OBS and re-run setup-strih.sh step 16g (issue 1419)\n' "${v#*|}" "$ini" ;;
  esac
}

# strih_operator_bt_verdict HCICONFIG_TEXT [ADAPTER] -> ONE row: hci0 UP with a non-zero BD address.
# Report-only on every role since the owner's 16.10.2026 ruling ("ok nemusis to riesit netreba mys"):
# a dead or unreadable controller is a NOTE naming the state, never a FAIL.
# ADAPTER (1/0, default 1) = the kernel registered a Bluetooth adapter (/sys/class/bluetooth/hci*).
# A box with none (strih-pp: a desktop with a USB receiver mouse, 12.10.2026) has nothing to pair: NOTE.
strih_operator_bt_verdict() {
  local text="${1-}" adapter="${2:-1}" addr bad=""
  if [ -z "$text" ] && [ "$adapter" = 0 ]; then
    printf 'NOTE|(bluetooth) no Bluetooth adapter on this box -- not graded; the operator needs a USB (receiver) mouse\n'
    return 0
  fi
  if [ -z "$text" ]; then
    printf 'NOTE|(bluetooth) no hci0 Bluetooth controller (hciconfig hci0 answered nothing) -- report-only, the operator needs no Bluetooth mouse (owner 16.10.2026)\n'
    return 0
  fi
  addr="$(sed -n 's/.*BD Address: \([0123456789ABCDEFabcdef:]*\).*/\1/p' <<<"$text")"
  addr="${addr%%$'\n'*}"
  if ! grep -Eq '^[[:space:]]+UP( |$)' <<<"$text"; then bad="DOWN"; fi
  case "$addr" in '' | 00:00:00:00:00:00) bad="${bad}${bad:+, }BD address ${addr:-unreadable}" ;; esac
  if [ -n "$bad" ]; then
    printf 'NOTE|(bluetooth) hci0 %s -- the Bluetooth controller does not work (strih-snv rtl8852bu ROM v3: no eco-4 Linux firmware upstream, issue 1418); report-only, the operator needs no Bluetooth mouse (owner 16.10.2026)\n' "$bad"
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

# strih_operator_mvclick_verdict USER_INI -> ONE row: [BasicWindow] MultiviewMouseSwitch is true the way
# libobs config_get_bool reads it (util/config-file.c: `true` in any case, or a non-zero strtoull of the
# value -- base 16 after a leading 0x, else base 10; `on` / `yes` read FALSE). An absent key is OBS's
# built-in true, but it FAILs as "not seeded": step 16g pins it, so a restored false can never hide.
strih_operator_mvclick_verdict() {
  local ini="${1:?user.ini path required}" v
  v="$(python3 - "$ini" <<'PY' 2>/dev/null || true
import configparser, re, sys
cp = configparser.RawConfigParser(strict=False)
cp.optionxform = str
try:
    if not cp.read(sys.argv[1]):
        print("missing|the file is missing")
        sys.exit(0)
except Exception:
    print("unread|the file is unreadable")
    sys.exit(0)
if not cp.has_option("BasicWindow", "MultiviewMouseSwitch"):
    print("absent|the key is not seeded")
    sys.exit(0)
v = cp.get("BasicWindow", "MultiviewMouseSwitch")
def obs_bool(s):
    if s.lower() == "true":
        return True
    base = 10
    if s.startswith("0x"):
        base, s = 16, s[2:]
    m = re.match(r"\s*[+-]?" + (r"(?:0[xX])?([0-9a-fA-F]*)" if base == 16 else r"([0-9]*)"), s)
    digits = m.group(1) if m else ""
    return bool(digits) and int(digits, base) != 0
print("%s|MultiviewMouseSwitch=%s" % ("true" if obs_bool(v) else "false", v))
PY
)"
  case "${v%%|*}" in
    true) printf 'OK|(mv-click) [BasicWindow] %s in %s -- a Multiview click cuts\n' "${v#*|}" "$ini" ;;
    *) printf 'FAIL|(mv-click) [BasicWindow] %s in %s -- MultiviewMouseSwitch not true or not seeded: clicking a Multiview tile may not cut; stop OBS and re-run setup-strih.sh step 16g (issue 1418)\n' "${v#*|}" "$ini" ;;
  esac
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

# strih_operator_xrandr_read USER HOME -> `xrandr --listproviders` of display :0 (bounded 10 s);
# rc = the read's own rc (124 = timed out, 127 = no xrandr).
strih_operator_xrandr_read() {
  local user="$1" home="$2"
  if [ -n "${STRIH_OPERATOR_XRANDR:-}" ]; then
    cat -- "$STRIH_OPERATOR_XRANDR" 2>/dev/null
    return
  fi
  _strih_operator_on_x "$user" "$home" 10 xrandr --listproviders 2>/dev/null
}

# strih_operator_xrandr_providers (stdin = `xrandr --listproviders`) -> `INDEX|SINK|OUTPUTS|NAME` per
# provider: SINK 1 when its capabilities list `Sink Output`, OUTPUTS its `outputs:` count (empty when
# the line has none), NAME the DDX's provider name (`modesetting`, or a vendor DDX's own name).
strih_operator_xrandr_providers() {
  LC_ALL=C awk '
    /^Provider [0123456789]+:/ {
      n = $2; sub(/:$/, "", n)
      sink = (index($0, "Sink Output") > 0) ? 1 : 0
      out = ""
      if (match($0, / outputs: [0123456789]+/)) out = substr($0, RSTART + 10, RLENGTH - 10)
      name = ""; i = index($0, " name:")
      if (i > 0) name = substr($0, i + 6)
      print n "|" sink "|" out "|" name
    }' || true
}

# strih_operator_panel_gpu_verdict XRANDR_TEXT READ_RC -> ONE row: every reverse-PRIME display sink runs
# on the modesetting DDX (the shared baseline removes STRIH_OPERATOR_AMDGPU_DDX_PKG to put it there).
strih_operator_panel_gpu_verdict() {
  local text="${1-}" rc="${2:-0}" rows count n sink out name src="" sinks="" bad=""
  case "$rc" in
    0) ;;
    124 | 137)
      printf 'FAIL|(panel-gpu) xrandr --listproviders on :0 did not answer within 10 s -- the X server does not answer clients (the 12.10.2026 hang)\n'
      return 0 ;;
    127)
      printf 'FAIL|(panel-gpu) not graded: xrandr is not installed (package x11-xserver-utils) -- the GPU providers of :0 cannot be read\n'
      return 0 ;;
    *)
      printf 'FAIL|(panel-gpu) xrandr --listproviders on :0 failed (rc %s) -- X is down or does not answer clients (the 12.10.2026 hang)\n' "$rc"
      return 0 ;;
  esac
  rows="$(strih_operator_xrandr_providers <<<"$text")"
  count=0
  while IFS='|' read -r n sink out name; do
    [ -n "$n" ] || continue
    count=$((count + 1))
    if [ "$n" = 0 ]; then src="$name"; continue; fi
    [ "$sink" = 1 ] || continue
    sinks="${sinks}${sinks:+, }${name:-an unnamed provider}"
    [ "$name" = modesetting ] || bad="${bad}${bad:+; }${name:-an unnamed provider} (Provider ${n}, ${out:-?} outputs)"
  done <<<"$rows"
  if [ "$count" = 0 ]; then
    printf 'FAIL|(panel-gpu) xrandr --listproviders on :0 listed no GPU provider -- X is down or does not answer clients (the 12.10.2026 hang)\n'
  elif [ -n "$bad" ]; then
    printf 'FAIL|(panel-gpu) the reverse-PRIME panel sink %s runs on its vendor DDX, not modesetting, behind the X screen'"'"'s GPU %s: its panel commits wait on %s fences, and on the amdgpu DDX one never signalled (the strih-snv 11/12.10.2026 freeze) -- re-run setup-strih.sh step 11 and restart X: the shared baseline removes %s, whose own 10-amdgpu.conf beats any OutputClass override; a sink on another vendor DDX needs that DDX package removed the same way\n' \
      "$bad" "${src:-Provider 0}" "${src:-Provider 0}" "$STRIH_OPERATOR_AMDGPU_DDX_PKG"
  elif [ "$count" = 1 ]; then
    printf 'NOTE|(panel-gpu) one GPU provider (%s), no PRIME -- not graded\n' "${src:-Provider 0}"
  elif [ -n "$sinks" ]; then
    printf 'OK|(panel-gpu) %s drives the X screen; the reverse-PRIME panel sink runs on the modesetting DDX\n' "${src:-Provider 0}"
  else
    printf 'OK|(panel-gpu) %s GPU providers, no reverse-PRIME display sink: %s drives the X screen\n' "$count" "${src:-Provider 0}"
  fi
}

# strih_operator_klog_read -> this boot's kernel log (`journalctl -k -b 0 --no-pager`, bounded 30 s);
# rc = the read's own rc.
strih_operator_klog_read() {
  if [ -n "${STRIH_OPERATOR_KLOG:-}" ]; then
    cat -- "$STRIH_OPERATOR_KLOG" 2>/dev/null
    return
  fi
  timeout 30 journalctl -k -b 0 --no-pager 2>/dev/null
}

# strih_operator_klog_drm_hang (stdin = a kernel log) -> `STAMP|task|DESC` for the first hung task whose
# trace waits in a DRM commit, or `STAMP|flip|MESSAGE` for the first DRM `*ERROR*` commit timeout (the
# header's forms), whichever the log shows first; nothing when neither is there. STAMP is the short-format
# `Mon DD HH:MM:SS` (else the line's first field, e.g. short-iso), empty when the line has no `kernel:`.
strih_operator_klog_drm_hang() {
  LC_ALL=C awk -v win="$STRIH_OPERATOR_KLOG_TRACE_LINES" '
    function stamp(l,   i, p, f) {
      i = index(l, " kernel:")
      if (i == 0) return ""
      p = substr(l, 1, i - 1)
      if (match(p, /^[ABCDEFGHIJKLMNOPQRSTUVWXYZ][abcdefghijklmnopqrstuvwxyz][abcdefghijklmnopqrstuvwxyz] +[0123456789]+ [0123456789][0123456789]:[0123456789][0123456789]:[0123456789][0123456789]/))
        return substr(p, RSTART, RLENGTH)
      split(p, f, " ")
      return f[1]
    }
    function message(l,   i) {
      i = index(l, " kernel: ")
      return (i > 0) ? substr(l, i + 9) : l
    }
    /INFO: task .* blocked for more than/ {
      pend = 1; n = 0; ts = stamp($0)
      d = substr($0, index($0, "INFO: task ") + 11); sub(/\.[ \t]*$/, "", d)
      next
    }
    /\[drm[^*]*\*ERROR\*.*((flip_done|hw_done|commit wait) timed out|Waiting for fences timed out|Flip event timeout)/ {
      print stamp($0) "|flip|" message($0); exit
    }
    pend {
      if ($0 ~ /INFO: task /) pend = 0
      else if (++n > win + 0) pend = 0
      else if ($0 ~ /[ \t](commit_work|drm_atomic_helper_wait_for_fences)\+0x/) { print ts "|task|" d; exit }
    }' || true
}

# strih_operator_drm_hang_verdict KLOG_TEXT READ_RC -> ONE row: no hung display commit in this boot.
strih_operator_drm_hang_verdict() {
  local text="${1-}" rc="${2:-0}" hit ts kind d
  if [ "$rc" != 0 ] || [ -z "$text" ]; then
    printf 'FAIL|(drm-hang) this boot'"'"'s kernel log could not be read (journalctl -k -b 0 --no-pager: rc %s, %s) -- a hung display commit cannot be ruled out\n' \
      "$rc" "$([ -n "$text" ] && echo "partial output" || echo "no output")"
    return 0
  fi
  # A boot's kernel log always holds kernel lines (`Linux version` at least). journalctl run by a user
  # outside adm/systemd-journal exits 0 and prints only `-- No entries --`: that is a blind read.
  if ! grep -q 'kernel:' <<<"$text"; then
    printf 'FAIL|(drm-hang) this boot'"'"'s kernel log shows no kernel line (journalctl -k -b 0 printed: %s) -- run verify-strih as root (or a member of adm/systemd-journal); a hung display commit cannot be ruled out\n' \
      "$(head -n 1 <<<"$text")"
    return 0
  fi
  hit="$(strih_operator_klog_drm_hang <<<"$text")"
  if [ -z "$hit" ]; then
    printf 'OK|(drm-hang) no hung display commit and no flip_done timeout in this boot'"'"'s kernel log\n'
    return 0
  fi
  IFS='|' read -r ts kind d <<<"$hit"
  ts="${ts:-an unknown time}"
  if [ "$kind" = task ]; then
    printf 'FAIL|(drm-hang) a display commit hung in this boot, first at %s: task %s, its trace waits in a DRM commit (commit_work / drm_atomic_helper_wait_for_fences) -- the panel freezes while OBS keeps rendering (the 11/12.10.2026 freeze); only a reboot clears it, and (panel-gpu) names the cure\n' "$ts" "$d"
  else
    printf 'FAIL|(drm-hang) a display flip timed out in this boot, first at %s: %s -- the panel freezes while OBS keeps rendering (the 11/12.10.2026 freeze); only a reboot clears it, and (panel-gpu) names the cure\n' "$ts" "$d"
  fi
}

# strih_operator_grade_rows REPO USER_HOME DESKTOP_USER -> verify-strih rows `OK|` / `NOTE|` / `FAIL|text`
# (strih_operator_row_count of them, in the header's order). Read-only; always rc 0.
strih_operator_grade_rows() {
  local repo="${1:?repo root required}" home="${2:?user home required}" user="${3:?desktop user required}"
  local xl slaves bt bt_adapter=1 why deck dk_usb dk_surf dk_ids dk_names obs_up=0 gap d1 d2
  local xr="" xr_rc=0 kl="" kl_rc=0
  xl="$(strih_operator_xinput "$user" "$home" list --short)"
  slaves="$(strih_operator_xinput_slaves <<<"$xl")"
  if ! command -v "$(strih_operator_xinput_tool)" >/dev/null 2>&1; then
    printf 'FAIL|(tap-to-click) not graded: xinput is not installed -- re-run setup-strih.sh step 11 (the kiosk installs it)\n'
  elif [ -z "$xl" ]; then
    printf 'FAIL|(tap-to-click) not graded: xinput on :0 answered nothing -- X is down or does not answer clients\n'
  else
    _strih_operator_tap_lines "$user" "$home" "$slaves" | strih_operator_tap_verdict
  fi
  bt="$(strih_operator_bt_read)"
  if [ "$(strih_box_role 2>/dev/null || echo strih)" = stream ]; then
    printf 'NOTE|(bluetooth) not graded: the stream role needs no Bluetooth (owner 13.10.2026: only strih-snv)\n'
  elif [ "$bt" = '<no-hciconfig>' ]; then
    printf 'NOTE|(bluetooth) hciconfig is not installed (package bluez) -- the Bluetooth controller cannot be read; report-only (owner 16.10.2026)\n'
  else
    if compgen -G "$(strih_operator_sysfs)/class/bluetooth/hci*" >/dev/null; then bt_adapter=1; else bt_adapter=0; fi
    strih_operator_bt_verdict "$bt" "$bt_adapter"
  fi
  if why="$(obs_box_step_applies 16)"; then
    deck="$(strih_operator_deck_read "$repo")"
    IFS='|' read -r dk_usb dk_surf dk_ids dk_names <<<"$deck"
    strih_operator_deck_verdict "$dk_usb" "$dk_surf" "$dk_ids" "$dk_names"
  else
    printf 'NOTE|(stream-deck) not graded: %s\n' "$why"
  fi
  strih_operator_mvclick_verdict "${home}/.config/obs-studio/user.ini"
  strih_operator_ini_verdict "${home}/.config/obs-studio/user.ini"
  if strih_operator_obs_running; then obs_up=1; fi
  strih_operator_studio_verdict "$(strih_operator_studio_read)" "$obs_up"
  strih_operator_ws_seed_verdict "${home}/.config/obs-studio/plugin_config/obs-websocket/config.json"
  strih_operator_qtsvg_verdict "${STRIH_OPERATOR_LIBDIR:-/usr/lib/x86_64-linux-gnu}"
  if ! command -v "$(strih_operator_xinput_tool)" >/dev/null 2>&1; then
    printf 'FAIL|(operator-input) not graded: xinput is not installed -- re-run setup-strih.sh step 11 (the kiosk installs it)\n'
  elif [ -z "$xl" ]; then
    printf 'FAIL|(operator-input) xinput on :0 answered nothing -- X is down or does not answer clients (the 12.10.2026 hang)\n'
  else
    strih_operator_input_verdict <<<"$slaves"
  fi
  if xr="$(strih_operator_xrandr_read "$user" "$home")"; then xr_rc=0; else xr_rc=$?; fi
  strih_operator_panel_gpu_verdict "$xr" "$xr_rc"
  if kl="$(strih_operator_klog_read)"; then kl_rc=0; else kl_rc=$?; fi
  strih_operator_drm_hang_verdict "$kl" "$kl_rc"
  strih_operator_keyboard_row "$user" "$home"
  if [ "$obs_up" = 0 ]; then
    printf 'NOTE|(operator-panel) not graded: OBS is not running\n'
    printf 'NOTE|(mv-projector) not graded: OBS is not running\n'
    return 0
  fi
  gap="${STRIH_OPERATOR_GRAB_GAP_S:-4}"
  d1="$(strih_operator_grab "$user" "$home")"
  sleep "$gap"
  d2="$(strih_operator_grab "$user" "$home")"
  strih_operator_panel_verdict "$d1" "$d2" "$gap"
  # issue 1418 (16.10.2026): the stream role has one screen carrying the stream OBS and the Restreamer
  # dashboard; a fullscreen Multiview projector would cover them, so the row is not graded there.
  if [ "$(strih_box_role 2>/dev/null || echo strih)" = stream ]; then
    printf 'NOTE|(mv-projector) not graded: the stream role has no Multiview projector (one screen, it would cover the stream OBS and the Restreamer dashboard)\n'
    return 0
  fi
  local hdmi_mv=0
  if strih_operator_hdmi_shows_multiview "${home}/.camera-box/drm-output.json"; then hdmi_mv=1; fi
  strih_operator_projector_verdict "$(_strih_operator_on_x "$user" "$home" 10 \
    "${STRIH_OPERATOR_WMCTRL:-wmctrl}" -lx 2>/dev/null || true)" "$hdmi_mv"
  return 0
}

# strih_operator_projector_verdict WMCTRL_LX_TEXT [HDMI_MV] -> ONE row (mv-projector, issue 1419): the
# operator's Multiview projector window is open on :0 while OBS runs (strih-snv's saved fullscreen
# Multiview). HDMI_MV=1 (the HDMI output shows the Multiview, owner 13.10.2026) needs no window: OK,
# and a window next to it is a NOTE (a duplicate the operator may close).
strih_operator_projector_verdict() {
  local text="${1-}" hdmi_mv="${2:-0}"
  if [ "$hdmi_mv" = 1 ]; then
    if grep -q 'Projector - Multiview' <<<"$text"; then
      printf 'NOTE|(mv-projector) the Multiview is on the HDMI output AND a projector window is open -- a duplicate, close the window (owner 13.10.2026)\n'
    else
      printf 'OK|(mv-projector) the Multiview is on the HDMI output (drm-output view multiview); no projector window needed\n'
    fi
    return 0
  fi
  if grep -q 'Projector - Multiview' <<<"$text"; then
    printf 'OK|(mv-projector) the Multiview projector window is open on :0\n'
  else
    printf 'FAIL|(mv-projector) no "Projector - Multiview" window on :0 -- the operator has no Multiview; stop OBS and re-run setup-strih.sh step 16g (it seeds the saved projector, issue 1419)\n'
  fi
}

strih_operator_row_count() { printf '14'; }

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
