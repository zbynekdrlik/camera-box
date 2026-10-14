#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the dantesync install functions, no top-level
# statements besides constants) -- the scripts/lib/*.sh convention: sourcing runs in the CALLER's
# shell, so strict mode here would leak into it. setup-strih.sh sets its own.
#
# scripts/lib/dantesync-install.sh -- issue 1412 (gap G5): put dantesync on a FRESH Linux OBS box.
#
# WHY: setup-strih.sh step 2 wrote the dantesync UNIT but never the binary nor /etc/dantesync/config.json
# -- strih-snv got both by hand. A fresh box (strih-pp) therefore came up with no clock discipline:
# strih_dantesync_install takes its `absent` branch and never starts the daemon. Built from
# setup-imag.sh step 3 (the binary download + the role config), with two deliberate differences:
#   * the binary is the PINNED release (DANTESYNC_VERSION_PIN, read from scripts/dantesync-version-gate.sh,
#     the ONE pin the fleet is graded against), never `releases/latest`;
#   * nothing that already exists is touched. An installed binary is kept (a new one arrives only
#     through dantesync-fleet-upgrade.sh, the canaried path -- .claude/rules/dantesync-fleet-upgrade.md)
#     and an existing config is the box's own (strih-snv, the fleet DATE master, is never rewritten:
#     a config change on the master takes effect at its next restart, which steps the fleet date).
#
# The config is the role's canonical shape (scripts/dantesync-canonical-config.json): `server` = the
# `ntp-master` role (ntp_server_mode on, an internet NTP upstream for the date), `client` = the `video`
# role (ntp_server_mode off; the upstream is on the unit's --ntp-server command line). Both allow ONE
# grandmaster (the box fact STRIH_DANTE_GM) and run clock_discipline=ptp_phase_lock.
#
# Path seams (tests): DANTESYNC_INSTALL_BIN (default STRIH_DANTESYNC_BIN, the path
# strih_dantesync_install's restart decision reads), DANTESYNC_INSTALL_CONFIG, DANTESYNC_RELEASE_BASE,
# DANTESYNC_VERSION_PIN (overrides the gate's pin), DANTESYNC_NTP_UPSTREAM (the master's NTP upstream),
# DANTESYNC_INSTALL_TRAY_BIN + DANTESYNC_INSTALL_TRAY_RELEASE_FILE (the Linux tray, issue 1429).
#
# Issue 1429 (owner 14.10.2026: the clock state as a tray icon on the Linux boxes too, like on Windows):
# dantesync_install_tray installs the PINNED release's `dantesync-tray-linux-amd64` (first shipped in
# v1.18.0, dantesync issue 132). It runs as the session app dantesync-tray.service
# (scripts/lib/strih-session-apps.sh, setup-strih step 16d). Unlike the daemon, the tray FOLLOWS the pin:
# an installed tray whose sha256 differs from the pinned release's is replaced (a tray is not
# clock-critical, and one pin keeps it on the daemon's version). The install records `v<pin> <sha256>` in
# a release file, so verify-strih grades a tray BEHIND the pin with no network read
# (dantesync_install_tray_state).

# The internet NTP upstream a `server` (NTP-master) box dates the fleet from: the value strih-snv's own
# config carries (Cloudflare time). DANTESYNC_NTP_UPSTREAM overrides it.
DANTESYNC_INSTALL_DEFAULT_NTP_UPSTREAM=162.159.200.1
# The Linux tray: where it is installed (the path dantesync-tray.service runs), and the first release
# that ships it.
DANTESYNC_TRAY_DEFAULT_BIN=/usr/local/bin/dantesync-tray
DANTESYNC_TRAY_FIRST_VERSION=1.18.0
# The release record the install writes next to the binary's install: `v<version> <sha256>`.
DANTESYNC_TRAY_DEFAULT_RELEASE_FILE=/usr/local/share/dantesync-tray/release

# dantesync_install_pinned_version -> the dantesync version the fleet is pinned to: DANTESYNC_VERSION_PIN
# when set, else the default the version gate declares (one source, never a second literal). rc 1 + a
# stderr line when neither yields an X.Y.Z.
dantesync_install_pinned_version() {
  local v="${DANTESYNC_VERSION_PIN:-}" gate
  if [ -z "$v" ]; then
    gate="$(dirname "${BASH_SOURCE[0]}")/../dantesync-version-gate.sh"
    v="$(sed -n 's/^DANTESYNC_VERSION_PIN="\${DANTESYNC_VERSION_PIN:-\([0-9.]*\)}"$/\1/p' "$gate" 2>/dev/null | head -1 || true)"
  fi
  if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "dantesync-install: no pinned dantesync version (DANTESYNC_VERSION_PIN / scripts/dantesync-version-gate.sh)" >&2
    return 1
  fi
  printf '%s' "$v"
}

# dantesync_install_release_url VERSION -> the linux-amd64 release asset URL of VERSION.
dantesync_install_release_url() {
  local v="${1:?version required}"
  printf '%s/v%s/dantesync-linux-amd64' \
    "${DANTESYNC_RELEASE_BASE:-https://github.com/zbynekdrlik/dantesync/releases/download}" "$v"
}

# dantesync_install_config_json ROLE GM [NTP_UPSTREAM] -> the role's /etc/dantesync/config.json text.
# ROLE server|client, GM = the one allowed PTP grandmaster. rc 1 on an unknown role, an empty GM, or a
# value that is not a plain host (it is embedded in JSON).
dantesync_install_config_json() {
  local role="${1:-}" gm="${2:-}" up="${3:-${DANTESYNC_NTP_UPSTREAM:-$DANTESYNC_INSTALL_DEFAULT_NTP_UPSTREAM}}"
  local hostre='^[A-Za-z0-9][A-Za-z0-9.-]*$' ntp_line="" master=false
  [[ "$gm" =~ $hostre ]] || { echo "dantesync-install: grandmaster '${gm}' is not a host name / address" >&2; return 1; }
  case "$role" in
    server)
      [[ "$up" =~ $hostre ]] || { echo "dantesync-install: NTP upstream '${up}' is not a host name / address" >&2; return 1; }
      ntp_line="  \"ntp_server\": \"${up}\","
      master=true ;;
    client) ;;
    *) echo "dantesync-install: role '${role}' must be server or client" >&2; return 1 ;;
  esac
  printf '{\n'
  printf '  "_ntp_server_examples": "sk.pool.ntp.org, europe.pool.ntp.org, time.google.com, time.cloudflare.com",\n'
  printf '  "http_status": {\n    "enabled": true,\n    "port": 8898\n  },\n'
  [ -z "$ntp_line" ] || printf '%s\n' "$ntp_line"
  printf '  "ntp_server_mode": {\n    "enabled": %s,\n    "max_step_us": 100000,\n    "port": 123,\n    "stratum": 3\n  },\n' "$master"
  printf '  "system": {\n    "gm_allowlist": [\n      "%s"\n    ],\n    "clock_discipline": "ptp_phase_lock"\n  }\n}\n' "$gm"
}

# dantesync_install_binary -> install the pinned dantesync binary at DANTESYNC_INSTALL_BIN
# (/usr/local/bin/dantesync) when NO binary is there; an existing one is kept whatever its version
# (dantesync-fleet-upgrade.sh owns upgrades). Downloads the binary + its release `.sha256` to a temp
# dir, verifies the sha256 BEFORE anything runs it (the fleet upgrader's order), checks `--version`
# reports the pin, then `install -m 0755` (rename into place). rc 1 + a stderr line on any failure.
dantesync_install_binary() {
  local bin="${DANTESYNC_INSTALL_BIN:-${STRIH_DANTESYNC_BIN:-/usr/local/bin/dantesync}}" pin url tmp got want have
  if [ -x "$bin" ]; then
    echo "  dantesync binary present (${bin}) -- kept (upgrades go through dantesync-fleet-upgrade.sh)"
    return 0
  fi
  pin="$(dantesync_install_pinned_version)" || return 1
  url="$(dantesync_install_release_url "$pin")"
  tmp="$(mktemp -d)" || { echo "dantesync-install: mktemp failed" >&2; return 1; }
  if ! curl -fsSL "$url" -o "${tmp}/dantesync" || ! curl -fsSL "${url}.sha256" -o "${tmp}/dantesync.sha256"; then
    rm -rf "$tmp"
    echo "dantesync-install: download of the pinned dantesync v${pin} or its .sha256 failed (${url})" >&2
    return 1
  fi
  want="$(awk 'NR==1{print $1}' "${tmp}/dantesync.sha256" 2>/dev/null || true)"
  have="$(sha256sum "${tmp}/dantesync" 2>/dev/null | awk '{print $1}' || true)"
  if [ -z "$want" ] || [ "$want" != "$have" ]; then
    rm -rf "$tmp"
    echo "dantesync-install: SHA256 MISMATCH for dantesync v${pin} (release '${want}', downloaded '${have}') -- refusing to run or install it" >&2
    return 1
  fi
  chmod 0755 "${tmp}/dantesync"
  got="$("${tmp}/dantesync" --version 2>/dev/null | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | tail -1 || true)"
  if [ "$got" != "$pin" ]; then
    rm -rf "$tmp"
    echo "dantesync-install: the downloaded binary reports version '${got}', not the pin ${pin} -- refusing to install it" >&2
    return 1
  fi
  install -D -m 0755 "${tmp}/dantesync" "$bin" || { rm -rf "$tmp"; echo "dantesync-install: could not install ${bin}" >&2; return 1; }
  rm -rf "$tmp"
  echo "  installed dantesync v${pin} -> ${bin}"
}

# dantesync_install_config ROLE GM -> write the role config at DANTESYNC_INSTALL_CONFIG
# (/etc/dantesync/config.json) when NONE exists (0644, temp + rename). An existing config is left
# untouched and named. rc 1 on a render or write failure.
dantesync_install_config() {
  local role="${1:-}" gm="${2:-}" cfg="${DANTESYNC_INSTALL_CONFIG:-/etc/dantesync/config.json}" text tmp
  if [ -e "$cfg" ]; then
    echo "  ${cfg} present -- the box's own dantesync config is kept (never rewritten here)"
    return 0
  fi
  text="$(dantesync_install_config_json "$role" "$gm")" || return 1
  install -d -m 0755 "$(dirname "$cfg")" || { echo "dantesync-install: could not create $(dirname "$cfg")" >&2; return 1; }
  tmp="$(mktemp "$(dirname "$cfg")/.config.json.XXXXXX")" || { echo "dantesync-install: mktemp in $(dirname "$cfg") failed" >&2; return 1; }
  if ! printf '%s\n' "$text" > "$tmp" || ! chmod 0644 "$tmp" || ! mv -f "$tmp" "$cfg"; then
    rm -f "$tmp"
    echo "dantesync-install: could not write ${cfg}" >&2
    return 1
  fi
  echo "  wrote ${cfg} (role ${role}, gm_allowlist ${gm}, clock_discipline ptp_phase_lock)"
}

# --- the Linux tray (issue 1429) ---------------------------------------------------------------------

# dantesync_install_tray_bin -> the installed tray path (DANTESYNC_INSTALL_TRAY_BIN, the ONE seam the
# install and the verify-strih grader both read; default /usr/local/bin/dantesync-tray).
dantesync_install_tray_bin() { printf '%s' "${DANTESYNC_INSTALL_TRAY_BIN:-$DANTESYNC_TRAY_DEFAULT_BIN}"; }

# dantesync_install_tray_release_file -> the tray's release record (DANTESYNC_INSTALL_TRAY_RELEASE_FILE).
dantesync_install_tray_release_file() {
  printf '%s' "${DANTESYNC_INSTALL_TRAY_RELEASE_FILE:-$DANTESYNC_TRAY_DEFAULT_RELEASE_FILE}"
}

# dantesync_install_tray_record VERSION SHA256 -> write `v<VERSION> <SHA256>` to the release record (0644,
# temp + rename; an unchanged record is not rewritten). rc 1 + a stderr line on a failed write.
dantesync_install_tray_record() {
  local file line dir tmp
  file="$(dantesync_install_tray_release_file)"
  line="v${1:?version required} ${2:?sha256 required}"
  [ "$(head -n 1 "$file" 2>/dev/null || true)" = "$line" ] && return 0
  dir="$(dirname "$file")"
  install -d -m 0755 "$dir" || { echo "dantesync-install: could not create ${dir}" >&2; return 1; }
  tmp="$(mktemp "${dir}/.release.XXXXXX")" || { echo "dantesync-install: mktemp in ${dir} failed" >&2; return 1; }
  if ! printf '%s\n' "$line" > "$tmp" || ! chmod 0644 "$tmp" || ! mv -f "$tmp" "$file"; then
    rm -f "$tmp"
    echo "dantesync-install: could not write the tray release record ${file}" >&2
    return 1
  fi
}

# dantesync_install_tray_state [PIN] -> ONE token, rc 0 iff ok, read with NO network (verify-strih's
# (dantesync-tray) row): `absent` (no executable tray), `stale:<why>` (no release record, the record names
# another version than the pin -- a pin bump setup has not followed --, or the binary's sha256 is not the
# recorded release's -- replaced by hand), `ok`. PIN defaults to dantesync_install_pinned_version.
dantesync_install_tray_state() {
  local bin file pin rec rv rs have
  bin="$(dantesync_install_tray_bin)"
  file="$(dantesync_install_tray_release_file)"
  [ -x "$bin" ] || { printf 'absent'; return 1; }
  pin="${1-}"
  if [ -z "$pin" ]; then
    pin="$(dantesync_install_pinned_version 2>/dev/null)" || { printf 'stale:pin-unreadable'; return 1; }
  fi
  rec="$(head -n 1 "$file" 2>/dev/null || true)"
  [ -n "$rec" ] || { printf 'stale:no-release-record'; return 1; }
  read -r rv rs _ <<<"$rec"
  [ "$rv" = "v${pin}" ] || { printf 'stale:installed-%s-pin-v%s' "$rv" "$pin"; return 1; }
  have="$(sha256sum "$bin" 2>/dev/null | awk '{print $1}' || true)"
  [ -n "$have" ] && [ "$have" = "$rs" ] || { printf 'stale:sha256-is-not-the-v%s-release' "$pin"; return 1; }
  printf 'ok'
}

# dantesync_install_tray_url VERSION -> the Linux tray release asset URL of VERSION.
dantesync_install_tray_url() {
  local v="${1:?version required}"
  printf '%s/v%s/dantesync-tray-linux-amd64' \
    "${DANTESYNC_RELEASE_BASE:-https://github.com/zbynekdrlik/dantesync/releases/download}" "$v"
}

# dantesync_install_version_ge A B -> rc 0 iff the X.Y.Z version A >= B, compared number by number (1.100.0
# is above 1.18.0); rc 1 when below; rc 2 when either is not X.Y.Z. The digit sets are spelled out: a
# bracket range follows the locale (issue 1302).
dantesync_install_version_ge() {
  local a="${1-}" b="${2-}" re='^[0123456789]+\.[0123456789]+\.[0123456789]+$' i
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

# dantesync_install_tray -> install the pinned release's Linux tray at dantesync_install_tray_bin.
#   * a pin below DANTESYNC_TRAY_FIRST_VERSION has no Linux tray asset: a named WARN, nothing downloaded,
#     rc 0 (the tray arrives with the fleet roll to 1.18.0+ and the DANTESYNC_VERSION_PIN bump);
#   * else the release `.sha256` first: an installed, executable tray with that sha256 is kept (its mtime
#     unchanged, which verify's process check compares against); otherwise the tray is downloaded to a
#     temp dir, its sha256 verified BEFORE anything is installed, then installed 0755 as a sibling
#     `<bin>.new.<pid>` and renamed over the name (a running tray keeps the inode it runs, and a failed copy
#     never leaves a half-written or 0600 tray under the name); the release record follows;
#   * a download failure, a malformed `.sha256` or a sha256 mismatch: rc 1 + a stderr line, nothing
#     installed (an installed tray is left as it is).
dantesync_install_tray() {
  local bin pin url tmp want have cur
  bin="$(dantesync_install_tray_bin)"
  pin="$(dantesync_install_pinned_version)" || return 1
  if ! dantesync_install_version_ge "$pin" "$DANTESYNC_TRAY_FIRST_VERSION"; then
    echo "  WARN: dantesync v${pin} ships no Linux tray (dantesync-tray-linux-amd64 first in v${DANTESYNC_TRAY_FIRST_VERSION}) -- ${bin} not installed; it arrives with the fleet roll to v${DANTESYNC_TRAY_FIRST_VERSION}+ and the DANTESYNC_VERSION_PIN bump"
    return 0
  fi
  url="$(dantesync_install_tray_url "$pin")"
  tmp="$(mktemp -d)" || { echo "dantesync-install: mktemp failed" >&2; return 1; }
  if ! curl -fsSL "${url}.sha256" -o "${tmp}/dantesync-tray.sha256"; then
    rm -rf "$tmp"
    echo "dantesync-install: download of the dantesync v${pin} tray .sha256 failed (${url}.sha256)" >&2
    return 1
  fi
  want="$(awk 'NR==1{print tolower($1)}' "${tmp}/dantesync-tray.sha256" 2>/dev/null || true)"
  if ! [[ "$want" =~ ^[0123456789abcdef]{64}$ ]]; then
    rm -rf "$tmp"
    echo "dantesync-install: the release .sha256 of the dantesync v${pin} tray is not a sha256 ('${want}') -- refusing to install it" >&2
    return 1
  fi
  cur="$(sha256sum "$bin" 2>/dev/null | awk '{print $1}' || true)"
  if [ -x "$bin" ] && [ "$cur" = "$want" ]; then
    rm -rf "$tmp"
    echo "  dantesync tray v${pin} present (${bin}, sha256 matches the release) -- kept"
    dantesync_install_tray_record "$pin" "$want"
    return
  fi
  if ! curl -fsSL "$url" -o "${tmp}/dantesync-tray"; then
    rm -rf "$tmp"
    echo "dantesync-install: download of the dantesync v${pin} tray failed (${url})" >&2
    return 1
  fi
  have="$(sha256sum "${tmp}/dantesync-tray" 2>/dev/null | awk '{print $1}' || true)"
  if [ "$want" != "$have" ]; then
    rm -rf "$tmp"
    echo "dantesync-install: SHA256 MISMATCH for the dantesync v${pin} tray (release '${want}', downloaded '${have}') -- refusing to install it" >&2
    return 1
  fi
  if ! install -D -m 0755 "${tmp}/dantesync-tray" "${bin}.new.$$" || ! mv -f "${bin}.new.$$" "$bin"; then
    rm -f "${bin}.new.$$"
    rm -rf "$tmp"
    echo "dantesync-install: could not install ${bin}" >&2
    return 1
  fi
  rm -rf "$tmp"
  if [ -n "$cur" ]; then
    echo "  replaced the dantesync tray with v${pin} -> ${bin} (the installed sha256 ${cur:0:12}... differed)"
  else
    echo "  installed the dantesync tray v${pin} -> ${bin}"
  fi
  dantesync_install_tray_record "$pin" "$want"
}

# dantesync_install ROLE GM -> the binary, then the config (both only when absent). rc 1 on a failure.
dantesync_install() {
  dantesync_install_binary || return 1
  dantesync_install_config "${1:-}" "${2:-}"
}
