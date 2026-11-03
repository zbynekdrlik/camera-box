#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no other top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
# Every function checks its own return codes (a caller runs it in an `||` list, where errexit is off).
#
# scripts/lib/dtray.sh -- issue 1485: dstack's shared tray `dtray` on every Linux OBS workstation
# (strih-snv, stream-snv, strih-pp, stream-pp; the dsync tray's set, issue 1429). dtray shows every dstack
# device of the PC (the intercom hub's Dante device, the strih OBS's dante_input) in the tint2 systray. It
# runs as the session app dtray.service (scripts/lib/strih-session-apps.sh: setup-strih step 16d installs,
# verify-strih items 37-40 grade).
#
# ONE pin, scripts/dtray-release.pin: the tag, the asset, its sha256 (the release's own) and the first
# dstack version whose devices dtray can see (DTRAY_DEVICE_FIRST_DSTACK). The binary is fetched from that
# exact tag only, proved against the pin AND the release's `.sha256`, never another build (no fallback). A
# release record `<tag> <sha256>` lets verify grade a dtray behind the pin with no network read.
#
# Expected devices: `/etc/dstack/devices/<name>.device` (empty; dtray reads only the name) for each Dante
# device the box's facts declare (STRIH_DANTE_DEVICES). The names we wrote are recorded, so a device the
# facts no longer declare is removed and a marker someone else wrote is never touched.
#
# The `dtray --print` grade: a device a dstack below DTRAY_DEVICE_FIRST_DSTACK runs is not discoverable, so
# while the pinned intercom OR obs release (INTERCOM_RELEASE_DSTACK_VERSION / OBS_RELEASE_DSTACK_VERSION
# in their pins) is below it, an expected device dtray reports "expected, not running" is a NOTE; once both
# are at or above it, a FAIL. The threshold follows the pins, never a date.
#
#   dtray_install HOME            setup-strih step 16d (the binary + record; removes ~/.config/autostart/dtray.desktop)
#   dtray_state                   -> absent | stale:<why> | ok (no network; verify's unit row)
#   dtray_devices_apply           setup-strih step 16d (the markers)
#   dtray_print_states EXP...     (stdin = `dtray --print`) -> `present|not-running|unlisted <name>` (pure)
#   dtray_devices_row             -> ONE verify row `OK|...` / `NOTE|...` / `FAIL|...`
#
# Seams: DTRAY_PIN_FILE, DTRAY_RELEASE_BASE, DTRAY_BIN, DTRAY_RELEASE_FILE, DTRAY_DEVICES_DIR,
# DTRAY_DEVICES_RECORD, DTRAY_DANTE_DEVICES (the fact STRIH_DANTE_DEVICES), DTRAY_INTERCOM_PIN_FILE,
# DTRAY_OBS_PIN_FILE, DTRAY_PRINT_TIMEOUT; `curl` and `timeout` from PATH.

DTRAY_LIB_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DTRAY_DEFAULT_BIN=/usr/local/bin/dtray
DTRAY_DEFAULT_SHARE=/usr/local/share/dtray
DTRAY_DEFAULT_DEVICES_DIR=/etc/dstack/devices
# dtray --print waits ~3.5 s for discovery; the bound only catches a hang.
DTRAY_DEFAULT_PRINT_TIMEOUT=15
DTRAY_NOT_RUNNING="expected, not running"

dtray_bin() { printf '%s' "${DTRAY_BIN:-$DTRAY_DEFAULT_BIN}"; }
dtray_release_file() { printf '%s' "${DTRAY_RELEASE_FILE:-${DTRAY_DEFAULT_SHARE}/release}"; }
dtray_devices_dir() { printf '%s' "${DTRAY_DEVICES_DIR:-$DTRAY_DEFAULT_DEVICES_DIR}"; }
dtray_devices_record() { printf '%s' "${DTRAY_DEVICES_RECORD:-${DTRAY_DEFAULT_SHARE}/devices}"; }

# _dtray_field FILE KEY -> the value of KEY in FILE (empty when absent). Always rc 0.
_dtray_field() { sed -n "s/^${2}=//p" "$1" 2>/dev/null | head -n 1 || true; }

# dtray_pin KEY -> the value of KEY in the dtray pin, validated: TAG vX.Y.Z, ASSET a plain file name,
# SHA256 64 lower-case hex, DEVICE_FIRST_DSTACK vX.Y.Z. rc 1 + a stderr line otherwise.
dtray_pin() {
  local key="${1:?key required}" file v re
  file="${DTRAY_PIN_FILE:-${DTRAY_LIB_ROOT}/scripts/dtray-release.pin}"
  v="$(_dtray_field "$file" "DTRAY_${key}")"
  case "$key" in
    RELEASE_TAG|DEVICE_FIRST_DSTACK) re='^v[0123456789]+\.[0123456789]+\.[0123456789]+$' ;;
    RELEASE_ASSET) re='^[-.0123456789A-Za-z_]+$' ;;
    RELEASE_SHA256) re='^[0123456789abcdef]{64}$' ;;
    *) echo "dtray: unknown pin key ${key}" >&2; return 1 ;;
  esac
  [[ "$v" =~ $re ]] || { echo "dtray: no valid DTRAY_${key} in ${file} ('${v}')" >&2; return 1; }
  printf '%s' "$v"
}

# dtray_release_url -> the pinned asset's URL (its `.sha256` is the same URL + .sha256).
dtray_release_url() {
  local tag asset
  tag="$(dtray_pin RELEASE_TAG)" || return 1
  asset="$(dtray_pin RELEASE_ASSET)" || return 1
  printf '%s/%s/%s' "${DTRAY_RELEASE_BASE:-https://github.com/zbynekdrlik/dstack/releases/download}" "$tag" "$asset"
}

# _dtray_sha FILE -> its sha256 (empty when unreadable). Always rc 0.
_dtray_sha() { sha256sum "$1" 2>/dev/null | awk '{print $1}' || true; }

# _dtray_write_file FILE TEXT -> write TEXT (0644, temp + rename) unless FILE already holds it. rc 1 + stderr.
_dtray_write_file() {
  local file="$1" text="$2" dir tmp
  [ -f "$file" ] && [ "$(cat "$file" 2>/dev/null)" = "$text" ] && return 0
  dir="$(dirname "$file")"
  install -d -m 0755 "$dir" || { echo "dtray: could not create ${dir}" >&2; return 1; }
  tmp="$(mktemp "${dir}/.dtray.XXXXXX")" || { echo "dtray: mktemp in ${dir} failed" >&2; return 1; }
  if ! printf '%s\n' "$text" > "$tmp" || ! chmod 0644 "$tmp" || ! mv -f "$tmp" "$file"; then
    rm -f "$tmp"
    echo "dtray: could not write ${file}" >&2
    return 1
  fi
}

# dtray_install HOME -> the pinned dtray at dtray_bin + its release record; removes HOME's XDG autostart
# entry of dtray (`dtray --autostart on` writes one; the unit is the ONE start path).
#   * the release `.sha256` first: it must name the pin's sha256, else nothing is downloaded;
#   * an installed, executable dtray with that sha256 is kept (its mtime unchanged: verify's process check
#     compares against it); else the asset is downloaded to a temp dir, proved against the pin, installed
#     0755 as `<bin>.new.<pid>` and renamed over the name (a running tray keeps its inode);
#   * any failure: rc 1 + a stderr line, nothing installed (an installed dtray is left as it is).
dtray_install() {
  local home="${1:?user home required}" bin tag want url tmp said cur have desktop
  desktop="${home}/.config/autostart/dtray.desktop"
  if [ -e "$desktop" ] || [ -L "$desktop" ]; then
    rm -f "$desktop" || { echo "dtray: could not remove ${desktop}" >&2; return 1; }
    echo "  removed ~/.config/autostart/dtray.desktop (dtray starts from dtray.service, never the XDG autostart)"
  fi
  bin="$(dtray_bin)"
  tag="$(dtray_pin RELEASE_TAG)" || return 1
  want="$(dtray_pin RELEASE_SHA256)" || return 1
  url="$(dtray_release_url)" || return 1
  tmp="$(mktemp -d)" || { echo "dtray: mktemp failed" >&2; return 1; }
  if ! curl -fsSL "${url}.sha256" -o "${tmp}/dtray.sha256"; then
    rm -rf "$tmp"
    echo "dtray: download of the dtray ${tag} .sha256 failed (${url}.sha256)" >&2
    return 1
  fi
  said="$(awk 'NR==1{print tolower($1)}' "${tmp}/dtray.sha256" 2>/dev/null || true)"
  if [ "$said" != "$want" ]; then
    rm -rf "$tmp"
    echo "dtray: the release .sha256 of dtray ${tag} says '${said}', the pin says '${want}' -- refusing it" >&2
    return 1
  fi
  cur="$(_dtray_sha "$bin")"
  if [ -x "$bin" ] && [ "$cur" = "$want" ]; then
    rm -rf "$tmp"
    echo "  dtray ${tag} present (${bin}, sha256 matches the pin) -- kept"
    _dtray_write_file "$(dtray_release_file)" "${tag} ${want}"
    return
  fi
  if ! curl -fsSL "$url" -o "${tmp}/dtray"; then
    rm -rf "$tmp"
    echo "dtray: download of dtray ${tag} failed (${url})" >&2
    return 1
  fi
  have="$(_dtray_sha "${tmp}/dtray")"
  if [ "$have" != "$want" ]; then
    rm -rf "$tmp"
    echo "dtray: SHA256 MISMATCH for dtray ${tag} (pin '${want}', downloaded '${have}') -- refusing to install it" >&2
    return 1
  fi
  if ! install -D -m 0755 "${tmp}/dtray" "${bin}.new.$$" || ! mv -f "${bin}.new.$$" "$bin"; then
    rm -f "${bin}.new.$$"
    rm -rf "$tmp"
    echo "dtray: could not install ${bin}" >&2
    return 1
  fi
  rm -rf "$tmp"
  if [ -n "$cur" ]; then
    echo "  replaced dtray with ${tag} -> ${bin} (the installed sha256 ${cur:0:12}... differed)"
  else
    echo "  installed dtray ${tag} -> ${bin}"
  fi
  _dtray_write_file "$(dtray_release_file)" "${tag} ${want}"
}

# dtray_state -> ONE token, rc 0 iff ok, read with NO network: `absent` (no executable dtray),
# `stale:<why>` (no release record, the record names another tag than the pin, or the binary is not the
# pinned sha256), `ok`.
dtray_state() {
  local bin rec rt rs tag want have
  bin="$(dtray_bin)"
  [ -x "$bin" ] || { printf 'absent'; return 1; }
  tag="$(dtray_pin RELEASE_TAG 2>/dev/null)" || { printf 'stale:pin-unreadable'; return 1; }
  want="$(dtray_pin RELEASE_SHA256 2>/dev/null)" || { printf 'stale:pin-unreadable'; return 1; }
  rec="$(head -n 1 "$(dtray_release_file)" 2>/dev/null || true)"
  [ -n "$rec" ] || { printf 'stale:no-release-record'; return 1; }
  read -r rt rs _ <<<"$rec"
  [ "$rt" = "$tag" ] || { printf 'stale:installed-%s-pin-%s' "$rt" "$tag"; return 1; }
  have="$(_dtray_sha "$bin")"
  [ -n "$have" ] && [ "$have" = "$want" ] && [ "$rs" = "$want" ] \
    || { printf 'stale:sha256-is-not-the-%s-release' "$tag"; return 1; }
  printf 'ok'
}

# --- the expected devices ----------------------------------------------------------------------------

# dtray_expected_devices -> the Dante device names the box declares (STRIH_DANTE_DEVICES: `none` or
# `name=arc:cmc:flows:rx ...`), one per line; nothing on a box without one. rc 1 when unreadable.
dtray_expected_devices() {
  local spec d
  if [ -n "${DTRAY_DANTE_DEVICES+set}" ]; then
    spec="$DTRAY_DANTE_DEVICES"
  elif declare -F strih_lx_dante_devices >/dev/null; then
    spec="$(strih_lx_dante_devices 2>/dev/null)" || return 1
  else
    return 1
  fi
  [ -n "$spec" ] || return 1
  [ "$spec" = none ] && return 0
  for d in $spec; do
    printf '%s\n' "${d%%=*}"
  done
}

# _dtray_recorded -> the marker names we wrote (the devices record), one per line. Always rc 0.
_dtray_recorded() { grep -v '^[[:space:]]*$' "$(dtray_devices_record)" 2>/dev/null || true; }

# _dtray_listed NAME LIST -> rc 0 iff NAME is a line of LIST.
_dtray_listed() { [ -n "$2" ] && printf '%s\n' "$2" | grep -qxF -- "$1"; }

# dtray_devices_apply -> each declared device's marker (created empty 0644 when missing), each recorded
# marker no longer declared removed, the record rewritten to the declared names that are ours. A marker we
# did not create is never removed. rc 1 + a stderr line on an unreadable fact or a failed write.
dtray_devices_apply() {
  local dir want old n f keep="" made=0 gone=0 what=""
  dir="$(dtray_devices_dir)"
  want="$(dtray_expected_devices)" || { echo "dtray: the box facts are unreadable (STRIH_DANTE_DEVICES) -- no device marker written" >&2; return 1; }
  old="$(_dtray_recorded)"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    f="${dir}/${n}.device"
    if [ -e "$f" ]; then
      _dtray_listed "$n" "$old" && keep="${keep}${n}"$'\n'
      continue
    fi
    if ! install -d -m 0755 "$dir" || ! install -m 0644 /dev/null "$f"; then
      echo "dtray: could not write ${f}" >&2
      return 1
    fi
    keep="${keep}${n}"$'\n'
    made=$((made + 1))
  done <<<"$want"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    _dtray_listed "$n" "$want" && continue
    rm -f "${dir}/${n}.device" || { echo "dtray: could not remove ${dir}/${n}.device" >&2; return 1; }
    gone=$((gone + 1))
  done <<<"$old"
  if [ -n "$keep" ]; then
    _dtray_write_file "$(dtray_devices_record)" "${keep%$'\n'}" || return 1
  elif [ -e "$(dtray_devices_record)" ]; then
    rm -f "$(dtray_devices_record)" || { echo "dtray: could not remove $(dtray_devices_record)" >&2; return 1; }
  fi
  [ "$made" = 0 ] || what="${what}, ${made} marker(s) written"
  [ "$gone" = 0 ] || what="${what}, ${gone} of ours no longer declared deleted"
  if [ -n "$want" ]; then
    echo "  dtray expected devices in ${dir}: ${want//$'\n'/ }${what}"
  else
    echo "  dtray expected devices: none (no Dante device on this box)${what}"
  fi
}

# --- the dtray --print grade ---------------------------------------------------------------------------

# dtray_version_ge A B -> rc 0 iff vX.Y.Z A >= B (number by number); rc 2 when either is not vX.Y.Z.
dtray_version_ge() {
  local a="${1#v}" b="${2#v}" re='^[0123456789]+\.[0123456789]+\.[0123456789]+$' i
  local -a x y
  [[ "$a" =~ $re ]] && [[ "$b" =~ $re ]] || return 2
  IFS=. read -r -a x <<<"$a"
  IFS=. read -r -a y <<<"$b"
  for i in 0 1 2; do
    if ((10#${x[i]} > 10#${y[i]})); then return 0; fi
    if ((10#${x[i]} < 10#${y[i]})); then return 1; fi
  done
  return 0
}

# dtray_device_shows -> rc 0 iff both pinned dstack users (the intercom hub, the obs release) run a dstack at
# or above DTRAY_DEVICE_FIRST_DSTACK; always prints `intercom <v> obs <v> first <v>` (`?` = unreadable). An
# unreadable version reads as below (a device dtray misses is then a NOTE naming it, never a quiet OK).
dtray_device_shows() {
  local first ic obs rc=0
  first="$(dtray_pin DEVICE_FIRST_DSTACK 2>/dev/null || echo '?')"
  ic="$(_dtray_field "${DTRAY_INTERCOM_PIN_FILE:-${DTRAY_LIB_ROOT}/scripts/intercom-release.pin}" INTERCOM_RELEASE_DSTACK_VERSION)"
  obs="$(_dtray_field "${DTRAY_OBS_PIN_FILE:-${DTRAY_LIB_ROOT}/scripts/obs-release.pin}" OBS_RELEASE_DSTACK_VERSION)"
  dtray_version_ge "${ic:-?}" "$first" || rc=1
  dtray_version_ge "${obs:-?}" "$first" || rc=1
  printf 'intercom %s obs %s first %s' "${ic:-?}" "${obs:-?}" "$first"
  return "$rc"
}

# dtray_print_states NAME... (stdin = `dtray --print`) -> one line per NAME: `present NAME` (a status line
# of it), `not-running NAME` (dtray's "expected, not running" line), `unlisted NAME` (no line at all: dtray
# never read its marker). Names compare case aside, as dtray does. Reads all of stdin. Always rc 0.
dtray_print_states() {
  local text n
  text="$(cat)"
  for n in "$@"; do
    printf '%s\n' "$text" | awk -v n="$n" -v nr="$DTRAY_NOT_RUNNING" '
      BEGIN { st = "unlisted"; ln = tolower(n) ": " }
      tolower(substr($0, 1, length(ln))) == ln {
        rest = substr($0, length(ln) + 1)
        if (rest == nr) { if (st == "unlisted") st = "not-running" } else st = "present"
      }
      END { print st " " n }'
  done
  return 0
}

# dtray_devices_row -> ONE verify-strih row: the declared markers in place and no stale marker of ours,
# then on a Dante box `dtray --print` (bounded) listing every declared device. Always rc 0.
dtray_devices_row() {
  local dir want old n missing="" stale="" out rc=0 st shown bad="" note="" present=""
  local -a dt_names=()
  dir="$(dtray_devices_dir)"
  if ! want="$(dtray_expected_devices)"; then
    printf 'FAIL|(dtray-devices) the box facts are unreadable (STRIH_DANTE_DEVICES) -- load the box facts (setup/verify-strih --box <name>)\n'
    return 0
  fi
  old="$(_dtray_recorded)"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    dt_names+=("$n")
    [ -f "${dir}/${n}.device" ] || missing="${missing} ${n}"
  done <<<"$want"
  while IFS= read -r n; do
    [ -n "$n" ] || continue
    _dtray_listed "$n" "$want" || { [ -e "${dir}/${n}.device" ] && stale="${stale} ${n}"; }
  done <<<"$old"
  if [ -n "$missing" ] || [ -n "$stale" ]; then
    printf 'FAIL|(dtray-devices) device markers in %s: missing [%s ] stale of ours [%s ] -- re-run setup-strih.sh step 16d\n' \
      "$dir" "$missing" "$stale"
    return 0
  fi
  if [ "${#dt_names[@]}" = 0 ]; then
    printf 'OK|(dtray-devices) no Dante device on this box (STRIH_DANTE_DEVICES none): no device marker, none of ours left\n'
    return 0
  fi
  out="$(timeout "${DTRAY_PRINT_TIMEOUT:-$DTRAY_DEFAULT_PRINT_TIMEOUT}" "$(dtray_bin)" --print 2>/dev/null)" || rc=$?
  if [ "$rc" != 0 ]; then
    # shellcheck disable=SC2016  # the backticks quote the command in the row text, nothing expands
    printf 'FAIL|(dtray-devices) `dtray --print` failed (rc %s%s) -- read it by hand: %s --print\n' "$rc" \
      "$([ "$rc" = 124 ] && printf ', no answer in %s s' "${DTRAY_PRINT_TIMEOUT:-$DTRAY_DEFAULT_PRINT_TIMEOUT}")" "$(dtray_bin)"
    return 0
  fi
  shown="$(dtray_device_shows)" && shown="ok:${shown}" || shown="below:${shown}"
  while read -r st n; do
    case "$st" in
      present) present="${present} ${n}" ;;
      not-running) if [ "${shown%%:*}" = ok ]; then bad="${bad} ${n}(not running)"; else note="${note} ${n}"; fi ;;
      *) bad="${bad} ${n}(not listed)" ;;
    esac
  done < <(printf '%s\n' "$out" | dtray_print_states "${dt_names[@]}")
  if [ -n "$bad" ]; then
    printf 'FAIL|(dtray-devices) dtray --print misses expected device(s):%s (shows:%s; dstack %s) -- dtray reads %s; check the device runs (dstack >= the pin first)\n' \
      "$bad" "${present:- none}" "${shown#*:}" "$dir"
  elif [ -n "$note" ]; then
    printf 'NOTE|(dtray-devices) markers in place; dtray --print reports%s "%s": the pinned dstack is below the first that dtray sees (%s) -- shows once both pins are at or above it (shows:%s)\n' \
      "$note" "$DTRAY_NOT_RUNNING" "${shown#*:}" "${present:- none}"
  else
    printf 'OK|(dtray-devices) markers in place and dtray --print shows every expected device:%s\n' "$present"
  fi
  return 0
}
