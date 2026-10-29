#!/usr/bin/env bash
# scripts/lib/strih-follow.sh -- which strih a cambox follows (issue 1431), verify-device (ax).
# airuleset:script-ok source-only lib -- set -euo pipefail would leak into the sourcing shell (ci-testing-gotchas)
#
# Issue 1431 (joint event): `strih.lan` is the ONE switch. A cambox's intercom target, its dantesync NTP server,
# its bkshading relay registration and its cameraman preview all follow whatever `strih.lan` resolves to ON THE
# BOX. This lib resolves it there and grades the answer against the strih-role fact files
# (scripts/strih-boxes/*.env, PARSED through scripts/lib/strih-box-facts.sh, never sourced): an address that is
# a strih box's STRIH_IP names that box; anything else names the address. Never a guessed default.
# Source-only: function definitions + the lazy fact-loader source. STRIH_BOXES_DIR moves the fact dir.

_STRIH_FOLLOW_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# The DNS name every cambox service follows.
strih_follow_dns_name() { printf '%s\n' strih.lan; }

# strih_follow_gather_remote_snippet -> REMOTE sh text (read-only): the first IPv4 address strih.lan resolves
# to on the box through its own resolver (getent = nsswitch, what dantesync / the relay / camera-box use), or
# nothing. Never fails: an unresolvable name prints nothing and exits 0, so the verdict names it.
strih_follow_gather_remote_snippet() {
  printf '%s\n' "getent ahostsv4 $(strih_follow_dns_name) 2>/dev/null | awk 'NR==1 {print \$1}' || true"
}

# strih_follow_strih_rows -> one "<box><TAB><STRIH_IP>" line per fact file whose STRIH_ROLE is `strih`.
# rc 1 + one stderr line naming the file when a fact file does not parse (never a partial list).
strih_follow_strih_rows() {
  (
    set +f
    shopt -u nullglob failglob nocaseglob nocasematch 2>/dev/null || true
    if ! declare -F strih_box_parse_file >/dev/null; then
      # shellcheck source=scripts/lib/strih-box-facts.sh
      . "${_STRIH_FOLLOW_HERE}/strih-box-facts.sh" || exit 1
    fi
    dir="$(strih_box_dir)"
    for f in "$dir"/*.env; do
      [ -f "$f" ] || continue
      parsed="$(strih_box_parse_file "$f" 2>/dev/null)" \
        || { echo "strih-follow: ${f##*/} does not parse -- the strih list is unreadable (issue 1431)" >&2; exit 1; }
      role="" ip=""
      while IFS= read -r line; do
        case "$line" in
          STRIH_ROLE=*) role="${line#*=}" ;;
          STRIH_IP=*) ip="${line#*=}" ;;
        esac
      done <<<"$parsed"
      [ "$role" = strih ] || continue
      box="${f##*/}"
      printf '%s\t%s\n' "${box%.env}" "$ip"
    done
  )
}

# strih_follow_verdict RESOLVED_IP -> "ok<TAB><box><TAB><ip>" when RESOLVED_IP is a strih box's STRIH_IP, else
# one "FAIL: <reason>" line naming the address and the known strih addresses. Always rc 0 (the caller grades
# the text, so a bare-statement call under set -e never aborts).
strih_follow_verdict() {
  local ip="${1:-}" rows known="" box addr name
  name="$(strih_follow_dns_name)"
  if ! rows="$(strih_follow_strih_rows 2>&1)"; then
    printf 'FAIL: %s\n' "${rows##*$'\n'}"
    return 0
  fi
  while IFS=$'\t' read -r box addr; do
    [ -n "$box" ] || continue
    known="${known:+${known}, }${box} ${addr}"
  done <<<"$rows"
  if [ -z "$ip" ]; then
    printf 'FAIL: %s does not resolve on this box -- it follows no strih (intercom, shading, preview and clock have no target; known strih boxes: %s)\n' \
      "$name" "${known:-none}"
    return 0
  fi
  while IFS=$'\t' read -r box addr; do
    if [ -n "$box" ] && [ "$addr" = "$ip" ]; then
      printf 'ok\t%s\t%s\n' "$box" "$ip"
      return 0
    fi
  done <<<"$rows"
  printf 'FAIL: %s resolves to %s on this box, which is no strih box (known strih boxes: %s) -- fix the router record\n' \
    "$name" "$ip" "${known:-none}"
  return 0
}
