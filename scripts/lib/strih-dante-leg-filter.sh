#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions + constants only, no top-level statements) --
# the scripts/lib/*.sh convention: sourcing runs in the CALLER's shell, so strict mode here would leak.
#
# scripts/lib/strih-dante-leg-filter.sh -- issue 1519 (dstack #264, the owner's decision relayed 13.11.2026:
# "Dante works on switch-on, one identical filter on every PC"). A box with Dante devices on its leg runs
# dstack's GENERIC Dante-leg filter, a pinned artifact of the dstack repo, never a per-box ruleset of ours:
#   tools/linux/dante-leg.nft          table inet dstack_dante_leg (stateful; every dstack device binds fixed
#                                      slot ports in 29800-31847, so the file names no device; 22 + 8092 in)
#   tools/linux/dante-leg-notrack.nft  table inet dstack_dante_leg_notrack (raw chains: conntrack sees only
#                                      the leg, never the NDI / JPEG XS on the rig NIC; this box has no NAT)
# ONE pin, scripts/dstack-leg-filter.pin: the tag, its commit, both files' sha256. strih_dante_leg_filter_ensure
# fetches both by the EXACT tag into <tree>/dstack-release/<tag>/ (the strih deploy fills it on dev1, so the
# box needs no network), proves them against the pin, no fallback ever.
#
# setup-strih step 13 (strih_dante_leg_filter_apply, from strih-vban-leg.sh's filter step): the two files to
# /etc/dstack/, ONE boot oneshot dstack-dante-leg-filter.service loading them in dstack's order
# (`nft -D dante_leg=<leg> -f dante-leg.nft`, then the notrack file), read back against a render of the same
# files in a throwaway network namespace; THEN our issue-1439/1485 `inet vban_dante70` table, its unit and
# file go (never before: the leg is never unfiltered); avahi gets `deny-interfaces=<leg>` (dstack's ask).
# Setup never restarts avahi (an avahi restart risks the NDI announcement): the deny takes effect at its
# next start, and verify FAILs an avahi that runs from before it. Setup refuses the whole step on a box with
# NAT or another stateful firewall (the notrack file's own condition, strih_dante_leg_nat_problems). The old table's avahi uid drop goes with the table (deny-interfaces is
# its replacement). A box without Dante devices removes a leftover generic filter (strih_dante_leg_filter_remove).
# verify item 9 (strih_dante_leg_filter_grade): `(dante-leg) files|filter|avahi|old table` rows.
#
# Seams: STRIH_DANTE_LEG_ROOT (the checkout), STRIH_DANTE_LEG_PIN_FILE, STRIH_DANTE_LEG_CACHE (the release
# dir, default <root>/dstack-release), STRIH_DANTE_LEG_URL (the raw base; tests file://), STRIH_DANTE_LEG_ETC
# (/etc/dstack), STRIH_DANTE_LEG_UNIT_FILE, STRIH_DANTE_AVAHI_CONF; nft / unshare / systemctl from PATH.

STRIH_DANTE_LEG_UNIT=dstack-dante-leg-filter.service
STRIH_DANTE_LEG_FILES=(dante-leg.nft dante-leg-notrack.nft)
STRIH_DANTE_LEG_TABLES=(dstack_dante_leg dstack_dante_leg_notrack)

_strih_dante_leg_root() { printf '%s' "${STRIH_DANTE_LEG_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"; }
_strih_dante_leg_etc() { printf '%s' "${STRIH_DANTE_LEG_ETC:-/etc/dstack}"; }
_strih_dante_leg_unit_file() { printf '%s' "${STRIH_DANTE_LEG_UNIT_FILE:-/etc/systemd/system/${STRIH_DANTE_LEG_UNIT}}"; }
_strih_dante_avahi_conf() { printf '%s' "${STRIH_DANTE_AVAHI_CONF:-/etc/avahi/avahi-daemon.conf}"; }

# _strih_dante_leg_pin ROOT KEY -> the pin's value of KEY (nothing + rc 1 + a named line when absent).
_strih_dante_leg_pin() {
  local v
  v="$(sed -n "s/^${2}=//p" "${STRIH_DANTE_LEG_PIN_FILE:-${1}/scripts/dstack-leg-filter.pin}" 2>/dev/null | head -n 1)"
  [ -n "$v" ] || { echo "dstack-leg-filter: no ${2} in ${1}/scripts/dstack-leg-filter.pin" >&2; return 1; }
  printf '%s\n' "$v"
}

# strih_dante_leg_filter_tag ROOT -> the pinned dstack tag.
strih_dante_leg_filter_tag() { _strih_dante_leg_pin "${1:?tree required}" DSTACK_LEG_FILTER_TAG; }

# strih_dante_leg_file_sha ROOT FILE -> the pinned sha256 of FILE (dante-leg.nft | dante-leg-notrack.nft).
strih_dante_leg_file_sha() {
  case "$2" in
    dante-leg.nft) _strih_dante_leg_pin "$1" DSTACK_LEG_FILTER_NFT_SHA256 ;;
    dante-leg-notrack.nft) _strih_dante_leg_pin "$1" DSTACK_LEG_FILTER_NOTRACK_SHA256 ;;
    *) echo "dstack-leg-filter: no pinned file ${2}" >&2; return 1 ;;
  esac
}

_strih_dante_leg_sha() { sha256sum "$1" 2>/dev/null | cut -d' ' -f1; }

# strih_dante_leg_filter_ensure ROOT -> prints <cache>/<tag> once both files there carry the pinned sha256;
# a missing or different file is fetched by the exact tag and proved. rc 1 + a named line otherwise, and no
# unproved byte is left under the file's name.
strih_dante_leg_filter_ensure() {
  local root="${1:?tree required}" tag dir f want got url
  tag="$(strih_dante_leg_filter_tag "$root")" || return 1
  dir="${STRIH_DANTE_LEG_CACHE:-${root%/}/dstack-release}/${tag}"
  mkdir -p "$dir" || { echo "dstack-leg-filter: cannot create ${dir}" >&2; return 1; }
  for f in "${STRIH_DANTE_LEG_FILES[@]}"; do
    want="$(strih_dante_leg_file_sha "$root" "$f")" || return 1
    [ "$(_strih_dante_leg_sha "${dir}/${f}")" != "$want" ] || continue
    rm -f "${dir}/${f}"
    url="${STRIH_DANTE_LEG_URL:-https://raw.githubusercontent.com/zbynekdrlik/dstack}/${tag}/tools/linux/${f}"
    # a file:// base (a local mirror, the tests) is copied, never through curl (the deploy tests stub curl)
    if ! case "$url" in file://*) cp -- "${url#file://}" "${dir}/${f}.part" 2>/dev/null ;;
           *) curl -fsSL --max-time 60 -o "${dir}/${f}.part" "$url" ;; esac; then
      rm -f "${dir}/${f}.part"
      echo "dstack-leg-filter: cannot fetch ${url} (dstack ${tag}): never another filter" >&2; return 1
    fi
    got="$(_strih_dante_leg_sha "${dir}/${f}.part")"
    if [ "$got" != "$want" ]; then
      rm -f "${dir}/${f}.part"
      echo "dstack-leg-filter: ${f} of dstack ${tag} has sha256 ${got:-unreadable}, the pin says ${want}" >&2; return 1
    fi
    mv -f "${dir}/${f}.part" "${dir}/${f}" || { echo "dstack-leg-filter: cannot keep ${dir}/${f}" >&2; return 1; }
    # setup-strih runs as root inside the operator's staged tree: the next deploy's rsync must replace it
    chown --reference="$dir" "${dir}/${f}" 2>/dev/null || true
  done
  printf '%s\n' "$dir"
}

# strih_dante_leg_filter_unit_text IF ETC -> the boot oneshot: dstack's two files in dstack's order.
strih_dante_leg_filter_unit_text() {
  cat <<EOF
[Unit]
Description=dstack's generic Dante-leg filter on ${1:?leg required}, conntrack on the leg only (issue 1519)
Wants=network-pre.target
Before=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -D dante_leg=${1} -f ${2:?etc required}/dante-leg.nft
ExecStart=/usr/sbin/nft -D dante_leg=${1} -f ${2}/dante-leg-notrack.nft

[Install]
WantedBy=multi-user.target
EOF
}

# strih_dante_leg_filter_render IF NFT NOTRACK -> both tables as nft lists them after loading the two files
# into a throwaway network namespace, normalized (run as root). rc 1 when it cannot render.
strih_dante_leg_filter_render() {
  # shellcheck disable=SC2016  # the inner script's own positional parameters
  unshare -n sh -c 'nft -D "dante_leg=$1" -f "$2" && nft -D "dante_leg=$1" -f "$3" && nft list table inet "$4" && nft list table inet "$5"' \
    sh "$1" "$2" "$3" "${STRIH_DANTE_LEG_TABLES[@]}" 2>/dev/null | strih_vban_leg_nft_normalize
  [ "${PIPESTATUS[0]}${PIPESTATUS[1]}" = 00 ]
}

# strih_dante_leg_filter_live -> both live tables, normalized (rc 1 when either is missing or unreadable).
strih_dante_leg_filter_live() {
  { nft list table inet "${STRIH_DANTE_LEG_TABLES[0]}" && nft list table inet "${STRIH_DANTE_LEG_TABLES[1]}"; } \
    2>/dev/null | strih_vban_leg_nft_normalize
  [ "${PIPESTATUS[0]}${PIPESTATUS[1]}" = 00 ]
}

# strih_dante_leg_nat_problems -> one line per thing that forbids dstack's notrack file on this box (its own
# header: load it ONLY with no NAT and no other stateful firewall): a masquerade / snat / dnat rule in the
# ruleset outside dstack's tables, an enabled ufw, an active firewalld. rc 0 when there is one, rc 1 when the
# box qualifies, rc 2 when the ruleset is unreadable (never read as "qualifies").
strih_dante_leg_nat_problems() {
  local rs hits ufw out=""
  rs="$(nft list ruleset 2>/dev/null)" || { echo "nft list ruleset is unreadable (run as root)"; return 2; }
  hits="$(printf '%s\n' "$rs" | awk '/^table / { skip = ($3 ~ /^dstack_dante_leg/) } !skip && /(masquerade|snat|dnat)/' | head -n 3)"
  [ -z "$hits" ] || out="NAT in the ruleset ($(printf '%s' "$hits" | tr -s ' \t\n' ' '))"
  ufw="${STRIH_DANTE_UFW_CONF:-/etc/ufw/ufw.conf}"
  if grep -qiE '^[[:space:]]*ENABLED[[:space:]]*=[[:space:]]*yes' "$ufw" 2>/dev/null; then out="${out}${out:+; }ufw is enabled (${ufw})"; fi
  if systemctl is-active -q firewalld 2>/dev/null; then out="${out}${out:+; }firewalld is active"; fi
  [ -n "$out" ] || return 1
  printf '%s\n' "$out"
}

# _strih_dante_leg_copy SRC DEST -> `written` (temp + rename, 0644) or `unchanged`; rc 1 on a write error.
_strih_dante_leg_copy() {
  local tmp
  if cmp -s "$1" "$2" && [ "$(stat -c %a "$2" 2>/dev/null)" = 644 ]; then echo unchanged; return 0; fi
  mkdir -p "${2%/*}" && tmp="$(mktemp "${2%/*}/.${2##*/}.XXXXXX")" || return 1
  if ! { cp "$1" "$tmp" && chmod 0644 "$tmp" && mv -f "$tmp" "$2"; }; then rm -f "$tmp"; return 1; fi
  echo written
}

# strih_dante_avahi_deny_text IF < CONF -> the avahi-daemon.conf text with IF in [server]'s deny-interfaces
# (an active list extended, else one line right after the [server] header, else a [server] section added);
# every other line kept as it is.
strih_dante_avahi_deny_text() {
  awk -v ifc="${1:?leg required}" '
    { line[NR] = $0
      if ($0 ~ /^[[:space:]]*\[/) sec = $0
      if (sec ~ /^[[:space:]]*\[server\]/ && $0 ~ /^[[:space:]]*deny-interfaces[[:space:]]*=/) has = NR
      if ($0 ~ /^[[:space:]]*\[server\][[:space:]]*$/ && !hdr) hdr = NR }
    END {
      for (i = 1; i <= NR; i++) {
        l = line[i]
        if (i == has) {
          v = l; sub(/^[^=]*=[[:space:]]*/, "", v); sub(/[[:space:]]+$/, "", v)
          n = split(v, a, /[[:space:]]*,[[:space:]]*/); found = 0
          for (k = 1; k <= n; k++) if (a[k] == ifc) found = 1
          if (!found) l = "deny-interfaces=" (v == "" ? ifc : v "," ifc)
        }
        print l
        if (i == hdr && !has) print "deny-interfaces=" ifc
      }
      if (!hdr && !has) { print ""; print "[server]"; print "deny-interfaces=" ifc }
    }'
}

# strih_dante_avahi_denies IF < CONF -> rc 0 when [server]'s active deny-interfaces lists IF.
strih_dante_avahi_denies() {
  awk -v ifc="${1:?leg required}" '
    /^[[:space:]]*\[/ { sec = $0 }
    sec ~ /^[[:space:]]*\[server\]/ && /^[[:space:]]*deny-interfaces[[:space:]]*=/ {
      v = $0; sub(/^[^=]*=[[:space:]]*/, "", v); n = split(v, a, /[[:space:]]*,[[:space:]]*/)
      for (k = 1; k <= n; k++) { sub(/[[:space:]]+$/, "", a[k]); if (a[k] == ifc) ok = 1 } }
    END { exit ok ? 0 : 1 }'
}

# _strih_dante_avahi_apply IF -> avahi's deny-interfaces for the leg (never a restart of avahi).
_strih_dante_avahi_apply() {
  local conf w txt
  conf="$(_strih_dante_avahi_conf)"
  if [ ! -f "$conf" ]; then echo "  NOTE: no ${conf} (avahi not installed): nothing to deny on ${1}"; return 0; fi
  txt="$(strih_dante_avahi_deny_text "$1" < "$conf")" && [ -n "$txt" ] \
    || { echo "dstack-leg-filter: could not read or edit ${conf} -- left as it is" >&2; return 1; }
  w="$(_strih_vban_leg_put "$conf" 0644 "$txt")" \
    || { echo "dstack-leg-filter: could not write ${conf}" >&2; return 1; }
  if [ "$w" = written ]; then
    echo "  ${conf}: deny-interfaces lists ${1} (dstack two-networks); avahi takes it at its next start -- restart avahi-daemon with OBS stopped, setup never does (the NDI announcement)"
  else
    echo "  ${conf}: deny-interfaces already lists ${1}"
  fi
}

# _strih_dante_leg_old_table_remove -> our issue-1439/1485 `inet vban_dante70` table, its unit and its file
# go (called only after the generic filter read back live).
_strih_dante_leg_old_table_remove() {
  local nf uf gone=""
  nf="$(_strih_vban_leg_nft_file)"; uf="$(_strih_vban_leg_unit_file)"
  if nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" >/dev/null 2>&1; then
    nft delete table inet "$STRIH_VBAN_LEG_NFT_TABLE" \
      || { echo "dstack-leg-filter: could not delete table inet ${STRIH_VBAN_LEG_NFT_TABLE}" >&2; return 1; }
    gone="the table"
  fi
  if systemctl is-enabled -q "$STRIH_VBAN_LEG_UNIT" 2>/dev/null; then
    systemctl disable --now "$STRIH_VBAN_LEG_UNIT" \
      || { echo "dstack-leg-filter: could not disable ${STRIH_VBAN_LEG_UNIT}" >&2; return 1; }
    gone="${gone}${gone:+, }its unit"
  fi
  if [ -e "$uf" ] || [ -e "$nf" ]; then
    rm -f "$uf" "$nf" || { echo "dstack-leg-filter: could not remove ${uf} / ${nf}" >&2; return 1; }
    systemctl daemon-reload || { echo "dstack-leg-filter: systemctl daemon-reload failed after removing ${uf}" >&2; return 1; }
    gone="${gone}${gone:+, }its files"
  fi
  [ -z "$gone" ] || echo "  inet ${STRIH_VBAN_LEG_NFT_TABLE} removed (${gone}): dstack's generic filter replaces it (issue 1519)"
}

# strih_dante_leg_filter_apply IF -> setup step 13 on a Dante box: the pinned files, the oneshot (enabled),
# the live tables read back, then our old table gone and avahi's deny. rc 1 + a named line on a failure.
strih_dante_leg_filter_apply() {
  local ifc="${1:?leg required}" root dir etc uf f w ww="" wu want live
  root="$(_strih_dante_leg_root)"; etc="$(_strih_dante_leg_etc)"; uf="$(_strih_dante_leg_unit_file)"
  if w="$(strih_dante_leg_nat_problems)" || [ $? = 2 ]; then
    echo "dstack-leg-filter: dstack's notrack file must not load on this box: ${w} -- nothing changed, our old table stays (dstack-leg-filter.sh header)" >&2
    return 1
  fi
  dir="$(strih_dante_leg_filter_ensure "$root")" || return 1
  for f in "${STRIH_DANTE_LEG_FILES[@]}"; do
    w="$(_strih_dante_leg_copy "${dir}/${f}" "${etc}/${f}")" || { echo "dstack-leg-filter: could not write ${etc}/${f}" >&2; return 1; }
    [ "$(_strih_dante_leg_sha "${etc}/${f}")" = "$(strih_dante_leg_file_sha "$root" "$f")" ] \
      || { echo "dstack-leg-filter: ${etc}/${f} does not read back as the pinned bytes" >&2; return 1; }
    [ "$w" = unchanged ] || ww=written
  done
  wu="$(_strih_vban_leg_put "$uf" 0644 "$(strih_dante_leg_filter_unit_text "$ifc" "$etc")")" \
    || { echo "dstack-leg-filter: could not write ${uf}" >&2; return 1; }
  echo "  ${etc}/{dante-leg,dante-leg-notrack}.nft (dstack $(strih_dante_leg_filter_tag "$root")): ${ww:-unchanged}; ${uf}: ${wu}"
  if [ "$wu" = written ]; then
    systemctl daemon-reload || { echo "dstack-leg-filter: systemctl daemon-reload failed after writing ${uf}" >&2; return 1; }
  fi
  if ! systemctl is-enabled -q "$STRIH_DANTE_LEG_UNIT" 2>/dev/null; then
    systemctl enable "$STRIH_DANTE_LEG_UNIT" || { echo "dstack-leg-filter: could not enable ${STRIH_DANTE_LEG_UNIT}" >&2; return 1; }
  fi
  want="$(strih_dante_leg_filter_render "$ifc" "${etc}/dante-leg.nft" "${etc}/dante-leg-notrack.nft")" && [ -n "$want" ] \
    || { echo "dstack-leg-filter: the pinned files do not load in a throwaway network namespace (unshare -n, as root)" >&2; return 1; }
  live="$(strih_dante_leg_filter_live || true)"
  if [ -n "$ww" ] || [ "$wu" = written ] || [ "$live" != "$want" ] \
     || ! systemctl is-active -q "$STRIH_DANTE_LEG_UNIT" 2>/dev/null; then
    systemctl restart "$STRIH_DANTE_LEG_UNIT" \
      || { echo "dstack-leg-filter: ${STRIH_DANTE_LEG_UNIT} failed to load the filter on ${ifc} (journalctl -u ${STRIH_DANTE_LEG_UNIT}) -- our old table stays" >&2; return 1; }
    live="$(strih_dante_leg_filter_live || true)"
    [ "$live" = "$want" ] \
      || { echo "dstack-leg-filter: the live dstack tables do not read back as the pinned files on ${ifc} -- our old table stays" >&2; return 1; }
    echo "  dstack's generic Dante-leg filter (re)loaded on ${ifc}"
  else
    echo "  dstack's generic Dante-leg filter live and unchanged on ${ifc}"
  fi
  _strih_dante_leg_old_table_remove || return 1
  _strih_dante_avahi_apply "$ifc"
}

# strih_dante_leg_filter_remove -> a box without Dante devices: a leftover generic filter goes (its unit, its
# two tables, its files). Silent when there is none.
strih_dante_leg_filter_remove() {
  local uf etc t gone=""
  uf="$(_strih_dante_leg_unit_file)"; etc="$(_strih_dante_leg_etc)"
  if [ -e "$uf" ] || systemctl is-enabled -q "$STRIH_DANTE_LEG_UNIT" 2>/dev/null; then
    systemctl disable --now "$STRIH_DANTE_LEG_UNIT" 2>/dev/null || true
    rm -f "$uf" || { echo "dstack-leg-filter: could not remove ${uf}" >&2; return 1; }
    systemctl daemon-reload || { echo "dstack-leg-filter: systemctl daemon-reload failed after removing ${uf}" >&2; return 1; }
    gone="its unit"
  fi
  for t in "${STRIH_DANTE_LEG_TABLES[@]}"; do
    if nft list table inet "$t" >/dev/null 2>&1; then
      nft delete table inet "$t" || { echo "dstack-leg-filter: could not delete table inet ${t}" >&2; return 1; }
      gone="${gone}${gone:+, }table ${t}"
    fi
  done
  for t in "${STRIH_DANTE_LEG_FILES[@]}"; do
    if [ -e "${etc}/${t}" ]; then rm -f "${etc}/${t}" || return 1; gone="${gone}${gone:+, }${t}"; fi
  done
  [ -z "$gone" ] || echo "  dstack's generic Dante-leg filter removed (${gone}): no Dante devices on this box"
}

# strih_dante_leg_filter_grade IF -> verify item 9's `(dante-leg)` rows (the caller's ok / bad / note).
strih_dante_leg_filter_grade() {
  local ifc="$1" root etc uf f bad_files="" want live conf ts mt left="" w
  root="$(_strih_dante_leg_root)"; etc="$(_strih_dante_leg_etc)"; uf="$(_strih_dante_leg_unit_file)"
  for f in "${STRIH_DANTE_LEG_FILES[@]}"; do
    [ "$(_strih_dante_leg_sha "${etc}/${f}")" = "$(strih_dante_leg_file_sha "$root" "$f" 2>/dev/null)" ] \
      || bad_files="${bad_files}${bad_files:+, }${f}"
  done
  if [ -n "$bad_files" ]; then
    bad "(dante-leg) files: ${etc}/ ${bad_files} missing or not the pinned bytes (scripts/dstack-leg-filter.pin) -- re-run setup-strih.sh step 13"
  else
    ok "(dante-leg) files: ${etc}/dante-leg.nft + dante-leg-notrack.nft = dstack $(strih_dante_leg_filter_tag "$root" 2>/dev/null)"
  fi
  if ! want="$(strih_dante_leg_filter_render "$ifc" "${etc}/dante-leg.nft" "${etc}/dante-leg-notrack.nft")" \
     || [ -z "$want" ]; then
    bad "(dante-leg) filter: the installed files do not load in a throwaway network namespace (run verify-strih as root)"
  elif ! live="$(strih_dante_leg_filter_live)" || [ "$live" != "$want" ] \
       || [ "$(cat "$uf" 2>/dev/null)" != "$(strih_dante_leg_filter_unit_text "$ifc" "$etc")" ] \
       || [ "$(systemctl is-enabled "$STRIH_DANTE_LEG_UNIT" 2>/dev/null || true)" != enabled ] \
       || ! systemctl is-active -q "$STRIH_DANTE_LEG_UNIT" 2>/dev/null; then
    bad "(dante-leg) filter: tables inet ${STRIH_DANTE_LEG_TABLES[*]} are not the pinned files live on ${ifc}, or ${STRIH_DANTE_LEG_UNIT} is not as rendered, enabled + active (a reboot would leave the leg open) -- re-run setup-strih.sh step 13"
  else
    ok "(dante-leg) filter: ${STRIH_DANTE_LEG_UNIT} enabled + active, dstack's two tables live on ${ifc} as pinned"
  fi
  conf="$(_strih_dante_avahi_conf)"
  if [ ! -f "$conf" ]; then
    note "(dante-leg) avahi: no ${conf} (avahi not installed): nothing announces on ${ifc}"
  elif ! strih_dante_avahi_denies "$ifc" < "$conf"; then
    bad "(dante-leg) avahi: ${conf} [server] deny-interfaces does not list ${ifc} -- re-run setup-strih.sh step 13"
  else
    ts="$(systemctl show -p ActiveEnterTimestamp --value --timestamp=unix avahi-daemon.service 2>/dev/null || true)"
    ts="${ts#@}"; mt="$(stat -c %Y "$conf" 2>/dev/null || echo 0)"
    if [[ "$ts" =~ ^[0123456789]+$ ]] && [ "$ts" -lt "$mt" ]; then
      bad "(dante-leg) avahi: ${conf} denies ${ifc}, but avahi-daemon started before it and still announces on the leg: restart avahi-daemon with OBS stopped (or reboot)"
    else
      ok "(dante-leg) avahi: deny-interfaces lists ${ifc}"
    fi
  fi
  if w="$(strih_dante_leg_nat_problems)" || [ $? = 2 ]; then
    bad "(dante-leg) nat: dstack's notrack file needs a box with no NAT and no other stateful firewall: ${w}"
  else
    ok "(dante-leg) nat: no NAT and no other stateful firewall, the notrack file may stay"
  fi
  if nft list table inet "$STRIH_VBAN_LEG_NFT_TABLE" >/dev/null 2>&1; then left="table inet ${STRIH_VBAN_LEG_NFT_TABLE} live"; fi
  if [ "$(systemctl is-enabled "$STRIH_VBAN_LEG_UNIT" 2>/dev/null || true)" = enabled ]; then left="${left}${left:+, }${STRIH_VBAN_LEG_UNIT} enabled"; fi
  for f in "$(_strih_vban_leg_nft_file)" "$(_strih_vban_leg_unit_file)"; do
    if [ -e "$f" ]; then left="${left}${left:+, }${f}"; fi
  done
  if [ -n "$left" ]; then
    bad "(dante-leg) old table: our per-box vban_dante70 filter is left (${left}) -- re-run setup-strih.sh step 13"
  else
    ok "(dante-leg) old table: no vban_dante70 table, unit or file left"
  fi
}
