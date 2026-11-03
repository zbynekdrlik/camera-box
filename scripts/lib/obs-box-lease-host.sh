#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines the lease-host constants, the pure planner, the renderers,
# the setup-strih step 16l install and the verify-strih grade; no top-level statements besides constants)
# -- the sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell, so strict mode would leak into it. The RENDERED refresh script sets its own.
#
# scripts/lib/obs-box-lease-host.sh -- issue 1474 (finding 24.10.2026, re-found live 3.11.2026): the
# restreamer rig-lease client on the Linux stream box dials http://dev1:8890/rig-lease.json. The box has
# no tailscale, so `dev1` resolved nowhere and the client failed OPEN (E2E jobs ran with no lease). A hand
# /etc/hosts literal was lost, and dev1's rig-LAN address moves by DHCP, so the box keeps the line itself:
#
#   * /usr/local/sbin/camera-box-lease-host-refresh (root, run by camera-box-lease-host.service, a
#     oneshot started by camera-box-lease-host.timer 30 s after boot and every 5 min) resolves dev1.local
#     over mDNS (getent ahostsv4, nss-mdns), keeps only an IPv4 inside the box's own rig network
#     (STRIH_IP/OBS_BOX_LEASE_HOST_PREFIX: dev1 also announces its tailscale and docker addresses), and
#     keeps EXACTLY one managed line `<ip>\tdev1\t# camera-box issue 1474: ...` in /etc/hosts:
#       - rewritten only on a change, through a temp file next to /etc/hosts + rename (mode + owner kept);
#       - only lines carrying OBS_BOX_LEASE_HOST_MARKER are ever replaced or dropped, every other line
#         stays byte for byte;
#       - a failed resolution leaves the file untouched, logs the reason and exits 1 (the unit shows
#         failed, the next timer pass retries);
#       - an UNMANAGED line naming dev1 (an old hand fix) is never edited, only named on stderr: getent
#         returns the first match, so verify then FAILs (dev1 != dev1.local).
#     The script embeds the pure functions below by `declare -f`, so setup, the timer and verify run the
#     same code.
#   * Which boxes: `obs_box_step_applies 16l-lease` (scripts/lib/obs-box-role.sh) = the stream role (the
#     restreamer host, step 16j) at the snv venue (the lease is the SNV rig's lease on dev1, and mDNS
#     does not leave the SNV rig LAN). Everything else SKIPs / NOTEs.
#   * verify-strih: obs_box_lease_host_grade -> (lease-host-unit), (lease-host), (lease-http).
#
# Seams (tests): OBS_BOX_LEASE_SBIN_DIR, OBS_BOX_LEASE_UNIT_DIR, OBS_BOX_LEASE_OWNER (the owner the
# installed files carry), the tools getent / systemctl / curl / dpkg-query / apt-get on PATH. The rendered
# script takes the hosts file as its optional first argument (default /etc/hosts).

OBS_BOX_LEASE_HOST_NAME="dev1"
OBS_BOX_LEASE_HOST_MDNS="dev1.local"
OBS_BOX_LEASE_HOST_PREFIX=23 # the rig LAN is a /23 (setup-strih step 1 assigns STRIH_IP/23)
OBS_BOX_LEASE_HOST_MARKER="# camera-box issue 1474: the rig lease host"
OBS_BOX_LEASE_HOST_URL="http://dev1:8890/rig-lease.json"
OBS_BOX_LEASE_HOST_UNIT="camera-box-lease-host"

# obs_box_lease_host_ip4_int A.B.C.D -> the address as an integer; rc 1 on anything else.
obs_box_lease_host_ip4_int() {
  local ip="${1-}" a b c d
  [[ "$ip" =~ ^([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})\.([0-9]{1,3})$ ]] || return 1
  a=$((10#${BASH_REMATCH[1]})) b=$((10#${BASH_REMATCH[2]})) c=$((10#${BASH_REMATCH[3]})) d=$((10#${BASH_REMATCH[4]}))
  [ "$a" -le 255 ] && [ "$b" -le 255 ] && [ "$c" -le 255 ] && [ "$d" -le 255 ] || return 1
  printf '%s\n' $(((a << 24) | (b << 16) | (c << 8) | d))
}

# obs_box_lease_host_pick BOX_IP PREFIX [CURRENT] < getent-output -> the address to write, as a normalised
# dotted quad: CURRENT when it is among the answers inside BOX_IP/PREFIX (no flapping when dev1 announces
# two rig addresses in a varying order), else the first such answer; rc 1 when none (or BOX_IP / PREFIX
# unreadable). The box's own address is never picked.
obs_box_lease_host_pick() {
  local box="${1-}" prefix="${2-}" current="${3-}" bi mask cand ci cur="" first=""
  bi="$(obs_box_lease_host_ip4_int "$box")" || return 1
  [[ "$prefix" =~ ^[0123456789]+$ ]] && [ "$prefix" -ge 1 ] && [ "$prefix" -le 32 ] || return 1
  mask=$(((0xFFFFFFFF << (32 - prefix)) & 0xFFFFFFFF))
  [ -z "$current" ] || cur="$(obs_box_lease_host_ip4_int "$current")" || cur=""
  while read -r cand _; do
    ci="$(obs_box_lease_host_ip4_int "$cand")" || continue
    [ $((ci & mask)) -eq $((bi & mask)) ] && [ "$ci" -ne "$bi" ] || continue
    [ -n "$first" ] || first="$ci"
    if [ -n "$cur" ] && [ "$ci" -eq "$cur" ]; then first="$ci"; break; fi
  done
  [ -n "$first" ] || return 1
  printf '%s.%s.%s.%s\n' $((first >> 24 & 255)) $((first >> 16 & 255)) $((first >> 8 & 255)) $((first & 255))
}

# obs_box_lease_host_line IP -> the ONE managed /etc/hosts line (no newline).
obs_box_lease_host_line() {
  printf '%s\t%s\t%s, refreshed from %s' "${1:?IP required}" "$OBS_BOX_LEASE_HOST_NAME" \
    "$OBS_BOX_LEASE_HOST_MARKER" "$OBS_BOX_LEASE_HOST_MDNS"
}

# obs_box_lease_host_render IP < hosts-text -> the new hosts text: the first managed line replaced in
# place, every further managed line dropped, every other line byte for byte; with no managed line the
# line is appended (a missing final newline is added first).
obs_box_lease_host_render() {
  local line
  line="$(obs_box_lease_host_line "${1:?IP required}")"
  LC_ALL=C awk -v marker="$OBS_BOX_LEASE_HOST_MARKER" -v line="$line" '
    index($0, marker) { if (!done) { print line; done = 1 } next }
    { print }
    END { if (!done) print line }'
}

# obs_box_lease_host_foreign < hosts-text -> every UNMANAGED, uncommented line that names the lease host
# as a host name (an old hand fix); empty when none.
obs_box_lease_host_foreign() {
  LC_ALL=C awk -v marker="$OBS_BOX_LEASE_HOST_MARKER" -v host="$OBS_BOX_LEASE_HOST_NAME" '
    index($0, marker) { next }
    { s = $0; sub(/\r$/, "", s); sub(/#.*/, "", s); n = split(s, f, /[ \t]+/); for (i = 2; i <= n; i++) if (f[i] == host) { print; next } }'
}

# obs_box_lease_host_refresh HOSTS BOX_IP PREFIX -> the timer pass (see the header). rc 0 = the line is
# right (written or unchanged), rc 1 = nothing written (a resolution or write failure, logged).
obs_box_lease_host_refresh() {
  local hosts="${1:?hosts file required}" box="${2:?box ip required}" prefix="${3:?prefix required}"
  local answer ip have want tmp foreign current have_rest want_rest
  case "$hosts" in
    /*) ;;
    *) echo "camera-box-lease-host: the hosts path '${hosts}' is not absolute -- nothing written" >&2; return 1 ;;
  esac
  if [ -L "$hosts" ]; then
    echo "camera-box-lease-host: ${hosts} is a symlink -- refused, nothing written (a rename would replace the link)" >&2
    return 1
  fi
  # A temp file a killed earlier pass left (systemd runs this oneshot one pass at a time).
  rm -f -- "${hosts%/*}/.${hosts##*/}.lease-host."*
  # `&& printf x` (never `;`): the x only joins a SUCCESSFUL read / render, so a failure reaches the `||`.
  have="$(cat -- "$hosts" && printf x)" || { echo "camera-box-lease-host: cannot read ${hosts} -- nothing written" >&2; return 1; }
  have="${have%x}"
  current="$(printf '%s' "$have" | grep -F -- "$OBS_BOX_LEASE_HOST_MARKER" | { read -r a _ || true; printf '%s' "${a-}"; })" || current=""
  answer="$(timeout 10 getent ahostsv4 "$OBS_BOX_LEASE_HOST_MDNS" 2>/dev/null || true)"
  if ! ip="$(printf '%s\n' "$answer" | obs_box_lease_host_pick "$box" "$prefix" "$current")"; then
    echo "camera-box-lease-host: ${OBS_BOX_LEASE_HOST_MDNS} gave no address in ${box}/${prefix} (answer: $(printf '%s' "$answer" | awk '{print $1}' | sort -u | tr '\n' ' ')) -- ${hosts} left as it is" >&2
    return 1
  fi
  want="$(printf '%s' "$have" | obs_box_lease_host_render "$ip" && printf x)" \
    || { echo "camera-box-lease-host: rendering ${hosts} failed -- nothing written" >&2; return 1; }
  want="${want%x}"
  # Refuse any result that is not "the unmanaged lines unchanged + the one managed line".
  have_rest="$(printf '%s' "$have" | grep -vF -- "$OBS_BOX_LEASE_HOST_MARKER" || true)"
  want_rest="$(printf '%s' "$want" | grep -vF -- "$OBS_BOX_LEASE_HOST_MARKER" || true)"
  if [ "$have_rest" != "$want_rest" ] \
      || [ "$(printf '%s' "$want" | grep -cxF -- "$(obs_box_lease_host_line "$ip")" || true)" != 1 ]; then
    echo "camera-box-lease-host: the rendered ${hosts} would change unmanaged lines or lack the one managed line -- refused, nothing written" >&2
    return 1
  fi
  foreign="$(printf '%s' "$have" | obs_box_lease_host_foreign)" || foreign=""
  [ -z "$foreign" ] || echo "camera-box-lease-host: WARNING an unmanaged line names ${OBS_BOX_LEASE_HOST_NAME} and wins the lookup, remove it by hand: ${foreign}" >&2
  if [ "$have" = "$want" ]; then
    echo "camera-box-lease-host: ${OBS_BOX_LEASE_HOST_NAME} -> ${ip} (${OBS_BOX_LEASE_HOST_MDNS}), ${hosts} unchanged"
    return 0
  fi
  tmp="$(mktemp "${hosts%/*}/.${hosts##*/}.lease-host.XXXXXX")" || { echo "camera-box-lease-host: cannot create a temp file next to ${hosts}" >&2; return 1; }
  if ! { printf '%s' "$want" > "$tmp" && chmod --reference="$hosts" "$tmp" && chown --reference="$hosts" "$tmp" && mv -f "$tmp" "$hosts"; }; then
    rm -f "$tmp"
    echo "camera-box-lease-host: cannot write ${hosts}" >&2
    return 1
  fi
  echo "camera-box-lease-host: ${OBS_BOX_LEASE_HOST_NAME} -> ${ip} (${OBS_BOX_LEASE_HOST_MDNS}), ${hosts} written"
}

# obs_box_lease_host_script_text -> the installed refresh script for the LOADED box (its STRIH_IP baked).
obs_box_lease_host_script_text() {
  local box
  box="$(strih_lx_ip)" || return 1
  printf '%s\n' '#!/usr/bin/env bash' \
    "# camera-box issue 1474: keep the ${OBS_BOX_LEASE_HOST_NAME} rig-lease host line in /etc/hosts, from ${OBS_BOX_LEASE_HOST_MDNS}." \
    '# Rendered by scripts/lib/obs-box-lease-host.sh (setup-strih step 16l) -- do not edit here.' \
    'set -euo pipefail'
  declare -p OBS_BOX_LEASE_HOST_NAME OBS_BOX_LEASE_HOST_MDNS OBS_BOX_LEASE_HOST_MARKER
  declare -f obs_box_lease_host_ip4_int obs_box_lease_host_pick obs_box_lease_host_line \
    obs_box_lease_host_render obs_box_lease_host_foreign obs_box_lease_host_refresh
  printf 'obs_box_lease_host_refresh "${1:-/etc/hosts}" %q %q\n' "$box" "$OBS_BOX_LEASE_HOST_PREFIX"
}

# obs_box_lease_host_service_text / obs_box_lease_host_timer_text -> the two system units.
obs_box_lease_host_service_text() {
  printf '%s\n' '[Unit]' \
    "Description=camera-box issue 1474: the ${OBS_BOX_LEASE_HOST_NAME} rig-lease host line in /etc/hosts, from ${OBS_BOX_LEASE_HOST_MDNS}" \
    'After=network-online.target avahi-daemon.service' 'Wants=network-online.target' '' \
    '[Service]' 'Type=oneshot' "ExecStart=/usr/local/sbin/${OBS_BOX_LEASE_HOST_UNIT}-refresh" \
    'ProtectSystem=full' 'ReadWritePaths=/etc' 'ProtectHome=yes' 'PrivateTmp=yes' 'NoNewPrivileges=yes'
}
obs_box_lease_host_timer_text() {
  printf '%s\n' '[Unit]' \
    "Description=camera-box issue 1474: refresh the ${OBS_BOX_LEASE_HOST_NAME} rig-lease host line every 5 min" '' \
    '[Timer]' 'OnBootSec=30s' 'OnUnitActiveSec=5min' 'AccuracySec=10s' '' \
    '[Install]' 'WantedBy=timers.target'
}

_obs_box_lease_host_paths() {
  LH_SCRIPT="${OBS_BOX_LEASE_SBIN_DIR:-/usr/local/sbin}/${OBS_BOX_LEASE_HOST_UNIT}-refresh"
  LH_SERVICE="${OBS_BOX_LEASE_UNIT_DIR:-/etc/systemd/system}/${OBS_BOX_LEASE_HOST_UNIT}.service"
  LH_TIMER="${OBS_BOX_LEASE_UNIT_DIR:-/etc/systemd/system}/${OBS_BOX_LEASE_HOST_UNIT}.timer"
}

# obs_box_lease_host_install -> setup-strih step 16l. SKIP (rc 0) where the gate says so; else nss-mdns,
# the three files (write-if-changed), daemon-reload on a unit change, the timer enabled + started, and one
# refresh now (a failed refresh is a WARNING: verify grades the result). rc 1 on a provisioning failure.
obs_box_lease_host_install() {
  local why owner before after script
  if ! why="$(obs_box_step_applies 16l-lease)"; then echo "  SKIP: ${why}"; return 0; fi
  owner="${OBS_BOX_LEASE_OWNER:-root:root}"
  _obs_box_lease_host_paths
  if [ "$(dpkg-query -W -f='${Status}' libnss-mdns 2>/dev/null || true)" != "install ok installed" ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y libnss-mdns \
      || { echo "obs-box-lease-host: apt-get install libnss-mdns failed" >&2; return 1; }
  fi
  script="$(obs_box_lease_host_script_text)" || { echo "obs-box-lease-host: cannot render the refresh script (no box IP)" >&2; return 1; }
  before="$({ cat "$LH_SERVICE" "$LH_TIMER" 2>/dev/null || true; } | sha256sum)"
  printf '%s\n' "$script" | obs_box_write_if_changed "$LH_SCRIPT" 0755 "$owner" "lease host refresh"
  obs_box_lease_host_service_text | obs_box_write_if_changed "$LH_SERVICE" 0644 "$owner" "lease host unit"
  obs_box_lease_host_timer_text | obs_box_write_if_changed "$LH_TIMER" 0644 "$owner" "lease host timer"
  after="$({ cat "$LH_SERVICE" "$LH_TIMER" 2>/dev/null || true; } | sha256sum)"
  if [ "$before" != "$after" ]; then systemctl daemon-reload || { echo "obs-box-lease-host: daemon-reload failed" >&2; return 1; }; fi
  systemctl enable --now "${OBS_BOX_LEASE_HOST_UNIT}.timer" \
    || { echo "obs-box-lease-host: cannot enable --now ${OBS_BOX_LEASE_HOST_UNIT}.timer" >&2; return 1; }
  if systemctl start "${OBS_BOX_LEASE_HOST_UNIT}.service"; then
    echo "  ${OBS_BOX_LEASE_HOST_NAME} lease host: refreshed from ${OBS_BOX_LEASE_HOST_MDNS} (timer every 5 min)"
  else
    echo "  WARNING: the first ${OBS_BOX_LEASE_HOST_UNIT} refresh failed (journalctl -u ${OBS_BOX_LEASE_HOST_UNIT}); the timer retries, verify grades it" >&2
  fi
}

# obs_box_lease_host_grade -> verify-strih's three rows through the caller's ok/bad/note; always rc 0.
obs_box_lease_host_grade() {
  local why miss="" f want state box mdns have code
  if ! why="$(obs_box_step_applies 16l-lease)"; then note "(lease-host) not graded: ${why}"; return 0; fi
  _obs_box_lease_host_paths
  for f in script service timer; do
    case "$f" in
      script) want="$(obs_box_lease_host_script_text)" || want="" ; cmp -s "$LH_SCRIPT" <(printf '%s\n' "$want") && [ -x "$LH_SCRIPT" ] || miss="${miss} ${LH_SCRIPT}" ;;
      service) cmp -s "$LH_SERVICE" <(obs_box_lease_host_service_text) || miss="${miss} ${LH_SERVICE}" ;;
      timer) cmp -s "$LH_TIMER" <(obs_box_lease_host_timer_text) || miss="${miss} ${LH_TIMER}" ;;
    esac
  done
  state="$(systemctl is-enabled "${OBS_BOX_LEASE_HOST_UNIT}.timer" 2>/dev/null || true)/$(systemctl is-active "${OBS_BOX_LEASE_HOST_UNIT}.timer" 2>/dev/null || true)"
  if [ -z "$miss" ] && [ "$state" = "enabled/active" ]; then
    ok "(lease-host-unit) ${OBS_BOX_LEASE_HOST_UNIT}.timer enabled/active, refresh script + units as provisioned"
  else
    bad "(lease-host-unit) differs or missing:${miss:- none}; timer ${state} -- re-run setup-strih.sh step 16l"
  fi
  box="$(strih_lx_ip 2>/dev/null || true)"
  have="$(timeout 10 getent hosts "$OBS_BOX_LEASE_HOST_NAME" 2>/dev/null | awk 'NR == 1 {print $1}' || true)"
  # The same pick the refresh makes, preferring the address dev1 resolves to now.
  mdns="$(timeout 10 getent ahostsv4 "$OBS_BOX_LEASE_HOST_MDNS" 2>/dev/null | obs_box_lease_host_pick "$box" "$OBS_BOX_LEASE_HOST_PREFIX" "$have" || true)"
  if [ -n "$have" ] && [ "$have" = "$mdns" ]; then
    ok "(lease-host) ${OBS_BOX_LEASE_HOST_NAME} -> ${have} = ${OBS_BOX_LEASE_HOST_MDNS}"
  else
    bad "(lease-host) ${OBS_BOX_LEASE_HOST_NAME} -> '${have}', ${OBS_BOX_LEASE_HOST_MDNS} -> '${mdns}' (in ${box}/${OBS_BOX_LEASE_HOST_PREFIX}) -- the rig-lease clients fail open; journalctl -u ${OBS_BOX_LEASE_HOST_UNIT}, an unmanaged ${OBS_BOX_LEASE_HOST_NAME} line in /etc/hosts wins the lookup"
  fi
  code="$(curl -s -o /dev/null -m 5 -w '%{http_code}' "$OBS_BOX_LEASE_HOST_URL" 2>/dev/null || true)"
  if [ "$code" = "200" ]; then
    ok "(lease-http) ${OBS_BOX_LEASE_HOST_URL} -> 200"
  else
    bad "(lease-http) ${OBS_BOX_LEASE_HOST_URL} -> '${code:-no answer}' (want 200) -- the rig-lease clients fail open"
  fi
  return 0
}
