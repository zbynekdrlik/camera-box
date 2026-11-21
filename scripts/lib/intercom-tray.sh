#!/usr/bin/env bash
# airuleset:script-ok source-only lib (constants + functions, no other top-level statements) -- the
# scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak into it.
# Every function checks its own return codes (a caller runs it in an `||` list, where errexit is off).
#
# scripts/lib/intercom-tray.sh -- issue 1542 (intercom#34, owner 19.11.2026: the cutters' headphone level
# sliders opened from a tray icon in the bottom bar): the intercom project's `intercom-tray` on the STRIH
# role (strih-snv, strih-pp; never the stream role, whose hub is moved aside). Its menu opens the hub's
# loopback-only level page (127.0.0.1:8791) in Firefox. It runs as the session app intercom-tray.service
# (scripts/lib/strih-session-apps.sh: setup-strih step 16d installs, verify-strih grades), like dtray.
#
# The binary is the 7th member of the PINNED intercom hub bundle (scripts/intercom-release.pin,
# scripts/lib/strih-intercom-release.sh): `strih_intercom_release_ensure` proves the tarball against the
# pin and the release's .sha256 and extracts it to <tree>/intercom-release/<tag>/, and the tray is
# installed from THAT directory only -- never another build, no fallback. A release record
# `<tag> <sha256>` lets verify grade a tray behind the pin with no network read.
#
#   intercom_tray_install ROOT    setup-strih step 16d (the binary + its record)
#   intercom_tray_state ROOT      -> absent | stale:<why> | ok (no network; verify's unit row)
#
# Seams: INTERCOM_TRAY_BIN, INTERCOM_TRAY_RELEASE_FILE; the bundle's own seams (STRIH_INTERCOM_PIN_FILE,
# STRIH_INTERCOM_RELEASE_URL, STRIH_INTERCOM_TARBALL_SRC).

if ! declare -F strih_intercom_release_ensure >/dev/null; then
  # shellcheck source=scripts/lib/strih-intercom-release.sh
  . "$(dirname "${BASH_SOURCE[0]}")/strih-intercom-release.sh"
fi

INTERCOM_TRAY_DEFAULT_BIN=/usr/local/bin/intercom-tray
INTERCOM_TRAY_DEFAULT_RELEASE_FILE=/usr/local/share/intercom-tray/release
# the bundle member's name (STRIH_INTERCOM_OPTIONAL_FILES: the hub install never requires it, this lib does)
INTERCOM_TRAY_MEMBER=intercom-tray

intercom_tray_bin() { printf '%s' "${INTERCOM_TRAY_BIN:-$INTERCOM_TRAY_DEFAULT_BIN}"; }
intercom_tray_release_file() { printf '%s' "${INTERCOM_TRAY_RELEASE_FILE:-$INTERCOM_TRAY_DEFAULT_RELEASE_FILE}"; }

# _intercom_tray_record TAG SHA -> the release record `<tag> <sha>` (0644, temp + rename) unless it already
# holds it. rc 1 + a stderr line.
_intercom_tray_record() {
  local file text dir tmp
  file="$(intercom_tray_release_file)"
  text="$1 $2"
  [ -f "$file" ] && [ "$(cat "$file" 2>/dev/null)" = "$text" ] && return 0
  dir="$(dirname "$file")"
  install -d -m 0755 "$dir" || { echo "intercom-tray: could not create ${dir}" >&2; return 1; }
  tmp="$(mktemp "${dir}/.intercom-tray.XXXXXX")" || { echo "intercom-tray: mktemp in ${dir} failed" >&2; return 1; }
  if ! printf '%s\n' "$text" > "$tmp" || ! chmod 0644 "$tmp" || ! mv -f "$tmp" "$file"; then
    rm -f "$tmp"
    echo "intercom-tray: could not write ${file}" >&2
    return 1
  fi
}

# intercom_tray_install ROOT -> the pinned bundle's intercom-tray at intercom_tray_bin + its release record.
#   * the bundle comes from strih_intercom_release_ensure ROOT (a tarball already in the tree on the pin =
#     no network; step 13 has normally fetched it);
#   * an installed, executable tray with the member's sha256 is kept (its mtime unchanged: verify's process
#     check compares against it);
#   * else installed 0755 as `<bin>.new.<pid>` and renamed over the name (a running tray keeps its inode);
#   * any failure: rc 1 + a stderr line, nothing installed (an installed tray is left as it is).
intercom_tray_install() {
  local root="${1:?tree required}" dir tag src want cur bin
  dir="$(strih_intercom_release_ensure "$root")" || { echo "intercom-tray: the pinned intercom bundle is not available (above)" >&2; return 1; }
  tag="$(strih_intercom_pin_tag "$root")" || return 1
  src="${dir}/${INTERCOM_TRAY_MEMBER}"
  [ -f "$src" ] || { echo "intercom-tray: the ${tag} bundle in ${dir} has no ${INTERCOM_TRAY_MEMBER}" >&2; return 1; }
  want="$(_strih_intercom_sha "$src")"
  [ -n "$want" ] || { echo "intercom-tray: cannot read ${src}" >&2; return 1; }
  bin="$(intercom_tray_bin)"
  cur="$(_strih_intercom_sha "$bin" || true)"
  if [ -x "$bin" ] && [ "$cur" = "$want" ]; then
    echo "  intercom-tray ${tag} present (${bin}, the bundle's member) -- kept"
    _intercom_tray_record "$tag" "$want"
    return
  fi
  if ! install -D -m 0755 "$src" "${bin}.new.$$" || ! mv -f "${bin}.new.$$" "$bin"; then
    rm -f "${bin}.new.$$"
    echo "intercom-tray: could not install ${bin}" >&2
    return 1
  fi
  if [ -n "$cur" ]; then
    echo "  replaced intercom-tray with ${tag} -> ${bin} (the installed sha256 ${cur:0:12}... differed)"
  else
    echo "  installed intercom-tray ${tag} -> ${bin}"
  fi
  _intercom_tray_record "$tag" "$want"
}

# intercom_tray_state ROOT -> ONE token, rc 0 iff ok, read with NO network: `absent` (no executable tray),
# `stale:<why>` (no release record, the record names another tag than the pin, the binary is not the
# recorded sha256, or the pinned bundle in ROOT holds another member than the record), `ok`. A tree with no
# extracted bundle is graded by the record alone (verify on a box set up from a staged tree keeps it).
intercom_tray_state() {
  local root="${1:?tree required}" bin rec rt rs tag have dir member
  bin="$(intercom_tray_bin)"
  [ -x "$bin" ] || { printf 'absent'; return 1; }
  tag="$(strih_intercom_pin_tag "$root" 2>/dev/null)" || { printf 'stale:pin-unreadable'; return 1; }
  rec="$(head -n 1 "$(intercom_tray_release_file)" 2>/dev/null || true)"
  [ -n "$rec" ] || { printf 'stale:no-release-record'; return 1; }
  read -r rt rs _ <<<"$rec"
  [ "$rt" = "$tag" ] || { printf 'stale:installed-%s-pin-%s' "$rt" "$tag"; return 1; }
  have="$(_strih_intercom_sha "$bin" || true)"
  [ -n "$have" ] && [ "$have" = "$rs" ] || { printf 'stale:sha256-is-not-the-recorded-%s' "$tag"; return 1; }
  dir="$(strih_intercom_release_dir "$root" 2>/dev/null || true)"
  if [ -n "$dir" ] && [ -f "${dir}/${INTERCOM_TRAY_MEMBER}" ]; then
    member="$(_strih_intercom_sha "${dir}/${INTERCOM_TRAY_MEMBER}" || true)"
    [ "$member" = "$rs" ] || { printf 'stale:not-the-%s-bundle-member' "$tag"; return 1; }
  fi
  printf 'ok'
}
