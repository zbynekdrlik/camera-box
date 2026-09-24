#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (plus a tiny CLI when executed, which sets its
# own strict mode below), sourced by setup-device.sh / verify-device.sh / setup-strih.sh /
# verify-strih.sh + tests/python/test_ndi_discovery_1342.py -- mirrors every sibling in scripts/lib/
# (remote-logging.sh, dscp-nft.sh, ndi-runtime.sh), none of which set -euo pipefail at file level:
# sourcing a `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/ndi-discovery.sh -- issue 1342: the ONE source of truth for the fleet's RECEIVER-side
# NDI config (`ndi-config.v1.json`), which lists every managed NDI sender by IP.
#
# WHY: NDI source discovery on this rig was pure mDNS (`_ndi._tcp` multicast via avahi). On the
# venue MikroTik LAN that is unreliable: the strih OBS missed the RESOLUME-SNV sources, and a freshly
# connected laptop did not list every source (owner, 18.9.2026).
#
# MECHANISM -- the NDI SDK's own receiver-side list (vendor/distroav/lib/ndi/Processing.NDI.Find.h,
# `p_extra_ips`): "The list of additional IP addresses that exist that we should query for sources on
# ... those sources will be available locally even though they are not mDNS discoverable ... When
# none is specified the registry is used." The registry is `ndi.networks.ips` in the config file. A
# finder queries every listed IP directly (unicast) AND keeps using mDNS. Senders never consult the
# list, so every sender keeps announcing over mDNS and stock TVs, guest laptops and the avahi-based
# port-map audit are untouched.
#
# The config therefore carries `networks.ips` ONLY. It never carries `networks.discovery`: a SENDER
# with a discovery server configured STOPS announcing over mDNS (NDI docs), which is why the part-1
# Discovery-Server model was dropped (the lane finding + the revised main design on issue 1342). The
# managed-box writer below DELETES a stray `networks.discovery` for the same reason.
#
# The list is GENERATED, never hand-typed (a hand-kept list is what went stale on the old Windows
# strih, dead 10.77.8.5x addresses):
#   * every camera `camera_resolve` knows (scripts/camera-set.sh), walked cam1, cam2, ... to the first
#     unknown name -- NOT CAMERA_ACTIVE_SET: a camera retired from MEASUREMENT is still a powered
#     sender, and walking the resolver means a new `camN)` arm is picked up with no second roster;
#   * every member of the obs-fleet `ndi-sender` facet (scripts/lib/obs-fleet.sh: strih-lx, stream,
#     resolume; `retired` rows excluded by obs_fleet_boxes).
# A fleet host that is a hostname (resolume.lan, a traveling DHCP box) is resolved to IPv4 at WRITE
# time; unresolvable -> skipped with a log line, and its sources stay mDNS-only exactly as before.
# The VERIFIERS require only the PINNED part (every IPv4 entry), so a traveling lease never makes a
# verify flap, while a renumbered camera / strih-lx / stream still FAILS until re-provisioned.
#
# Config LOCATION: the Linux SDK reads `$HOME/.ndi/ndi-config.v1.json`, or
# `$NDI_CONFIG_DIR/ndi-config.v1.json` when that env var is set. `camera-box.service` (the cameraman
# HDMI preview receives `STRIH-LX (interkom)`) runs as root with `ProtectHome=yes` and no User=, so a
# /root/.ndi file would be invisible to it -- the cambox config lives in the system dir /etc/ndi and a
# camera-box.service.d drop-in points NDI_CONFIG_DIR at it. strih-lx's intercom-hub gets the same.
#
# Rule + supervisor steps: .claude/rules/ndi-discovery.md.

# --- the two fleet sources of truth (lazy-sourced; a caller that already sourced them keeps its own) ---
if ! command -v camera_resolve >/dev/null 2>&1; then
  # shellcheck source=scripts/camera-set.sh
  . "${BASH_SOURCE[0]%/*}/../camera-set.sh"
fi
if ! command -v obs_fleet_boxes >/dev/null 2>&1; then
  # shellcheck source=scripts/lib/obs-fleet.sh
  . "${BASH_SOURCE[0]%/*}/obs-fleet.sh"
fi

# --- shared constants (single source of truth, consumed cross-file) ---------------------------
# The SDK's own config file name.
NDI_DISCOVERY_CONFIG_NAME="ndi-config.v1.json"
# System config dir for root / ProtectHome services (pointed at by NDI_CONFIG_DIR). Overridable for tests.
NDI_DISCOVERY_SYSTEM_DIR="${NDI_DISCOVERY_SYSTEM_DIR:-/etc/ndi}"
# The camera-box.service drop-in that points the appliance's libndi at NDI_DISCOVERY_SYSTEM_DIR.
# Overridable for tests.
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + verify-device.sh (an)
NDI_DISCOVERY_CAMBOX_DROPIN="${NDI_DISCOVERY_CAMBOX_DROPIN:-/etc/systemd/system/camera-box.service.d/ndi-discovery.conf}"
# The strih-lx intercom-hub drop-in (ProtectHome hides ~/.ndi from it). Overridable for tests.
# shellcheck disable=SC2034  # consumed cross-file by setup-strih.sh (install) + verify-strih.sh (item 34)
NDI_DISCOVERY_INTERCOM_DROPIN="${NDI_DISCOVERY_INTERCOM_DROPIN:-/etc/systemd/system/intercom-hub.service.d/ndi-discovery.conf}"
# The obs-fleet facet whose members are the managed OBS-box NDI SENDERS.
NDI_DISCOVERY_FLEET_FACET="ndi-sender"
# A bound on the camera_resolve walk (a guard against a runaway loop, not a roster).
NDI_DISCOVERY_CAMERA_MAX=99

# _ndi_discovery_is_ipv4 X -> exit 0 iff X is a dotted-quad IPv4 literal.
_ndi_discovery_is_ipv4() {
  [[ "${1:-}" =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}$ ]]
}

# ndi_discovery_camera_ips -> one IP per line for every camera camera_resolve knows, walked cam1,
# cam2, ... until the first unknown name. Runs in a SUBSHELL so the caller's CAMERA_IP / CAMERA_NAME /
# CAMERA_SOURCE (setup-device.sh's own box) are never clobbered.
ndi_discovery_camera_ips() {
  (
    n=1
    while [ "$n" -le "$NDI_DISCOVERY_CAMERA_MAX" ] && camera_resolve "cam$n" 2>/dev/null; do
      printf '%s\n' "$CAMERA_IP"
      n=$((n + 1))
    done
  )
}

# ndi_discovery_fleet_hosts -> one host per line for every ndi-sender facet member (IP or hostname).
# Non-zero when the facet or a member is missing from OBS_FLEET -- the caller then writes nothing.
ndi_discovery_fleet_hosts() {
  local roster pair
  roster="$(obs_fleet_boxes "$NDI_DISCOVERY_FLEET_FACET")" || return 1
  for pair in $roster; do
    printf '%s\n' "${pair#*|}"
  done
}

# ndi_discovery_resolve_ipv4 HOST -> HOST's first IPv4 address, "" when unresolvable. A thin seam
# over the fleet lib's bounded resolver (obs_fleet_resolve_host_v4); the tests redefine it.
ndi_discovery_resolve_ipv4() {
  obs_fleet_resolve_host_v4 "${1:-}"
}

# ndi_discovery_sender_ips [resolve|pinned] -> the comma-separated networks.ips list, in fleet order,
# each IP once.
#   pinned  -- the cameras + every IPv4 fleet host. Deterministic: what the verifiers REQUIRE and
#              what the checked-in config / the laptop .ps1 default carry.
#   resolve -- (default, the provisioners) pinned + every HOSTNAME fleet host resolved to IPv4; an
#              unresolvable or non-IPv4 answer is skipped and named on stderr.
# Non-zero (and no output) when the fleet lookup fails or the camera walk finds no camera (a renamed
# camera_resolve arm must never leave the writer AND the grader agreeing on a camera-less list).
ndi_discovery_sender_ips() {
  local mode="${1:-resolve}" hosts cams cands c ip out="" seen=" "
  case "$mode" in
    resolve|pinned) ;;
    *) echo "ndi-discovery: unknown mode '${mode}' (expected resolve|pinned)" >&2; return 1 ;;
  esac
  hosts="$(ndi_discovery_fleet_hosts)" || {
    echo "ndi-discovery: obs-fleet facet '${NDI_DISCOVERY_FLEET_FACET}' lookup failed -- no sender list" >&2
    return 1
  }
  cams="$(ndi_discovery_camera_ips)"
  if [ -z "$cams" ]; then
    echo "ndi-discovery: camera_resolve knows no camera (cam1 unresolvable) -- no sender list" >&2
    return 1
  fi
  cands="$cams"$'\n'"$hosts"
  while IFS= read -r c; do
    [ -n "$c" ] || continue
    if _ndi_discovery_is_ipv4 "$c"; then
      ip="$c"
    elif [ "$mode" = resolve ]; then
      ip="$(ndi_discovery_resolve_ipv4 "$c")"
      if ! _ndi_discovery_is_ipv4 "$ip"; then
        echo "ndi-discovery: sender '$c' did not resolve to IPv4 ('${ip}') -- skipped, its sources stay mDNS-only" >&2
        continue
      fi
    else
      continue
    fi
    case "$seen" in *" $ip "*) continue ;; esac
    seen="${seen}${ip} "
    out="${out:+$out,}$ip"
  done <<EOF
$cands
EOF
  printf '%s' "$out"
}

# ndi_discovery_config_json [IPS] -> the canonical ndi-config.v1.json text (2-space indent, one
# trailing newline, no BOM), networks.ips ONLY. IPS defaults to the PINNED list, so the checked-in
# file scripts/ndi-discovery/ndi-config.v1.json is pinned byte-identical to this with no argument.
ndi_discovery_config_json() {
  local ips
  if [ "$#" -ge 1 ]; then
    ips="$1"
  else
    ips="$(ndi_discovery_sender_ips pinned)" || return 1
  fi
  printf '{\n  "ndi": {\n    "networks": {\n      "ips": "%s"\n    }\n  }\n}\n' "$ips"
}

# _ndi_discovery_json_string KEY TEXT -> the string value of the LAST `"KEY": "<value>"` pair in
# TEXT, "" if absent. Pure grep/sed (no python on a cambox); `|| true` so a no-match never aborts a
# caller under `set -euo pipefail` (the #458 footgun -- grep exits 1 on zero matches).
_ndi_discovery_json_string() {
  printf '%s\n' "$2" | grep -oE "\"$1\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | tail -1 \
    | sed -E 's/.*:[[:space:]]*"([^"]*)"$/\1/' || true
}

# ndi_discovery_config_ips TEXT -> the `networks.ips` value in TEXT, "" if absent.
ndi_discovery_config_ips() { _ndi_discovery_json_string ips "$1"; }

# ndi_discovery_config_servers TEXT -> a `networks.discovery` value in TEXT, "" if absent (graded
# only to catch a stray one: a managed box must never carry it).
ndi_discovery_config_servers() { _ndi_discovery_json_string discovery "$1"; }

# _ndi_discovery_norm_list LIST -> LIST with every space removed ("a, b" == "a,b").
_ndi_discovery_norm_list() { printf '%s' "$1" | tr -d '[:space:]'; }

# ndi_discovery_missing_ips TEXT LIST -> the comma list of LIST entries absent from TEXT's
# networks.ips ("" when every entry is present). Spaces in either list are ignored.
ndi_discovery_missing_ips() {
  local have ip missing=""
  have=",$(_ndi_discovery_norm_list "$(ndi_discovery_config_ips "$1")"),"
  for ip in ${2//,/ }; do
    case "$have" in *",$ip,"*) ;; *) missing="${missing:+$missing,}$ip" ;; esac
  done
  printf '%s' "$missing"
}

# ndi_discovery_list_minus A B -> the comma list of A's entries that are not in B, in A's order ("" when
# none). Spaces in either list are ignored. Pure.
ndi_discovery_list_minus() {
  local b ip out=""
  b=",$(_ndi_discovery_norm_list "$2"),"
  for ip in ${1//,/ }; do
    case "$b" in *",$ip,"*) ;; *) out="${out:+$out,}$ip" ;; esac
  done
  printf '%s' "$out"
}

# ndi_discovery_config_verdict TEXT [REQUIRED] -> "ok", or one `FAIL: <facet>` line per failing facet.
# Always exits 0 (the caller branches on the printed verdict). REQUIRED defaults to the PINNED list.
# Facets:
#   missing   -- TEXT empty (no config file on the box)
#   JSON      -- TEXT is not valid JSON (checked only when python3 is available; verify-device runs
#                this on dev1, verify-strih on strih-lx -- both have it)
#   discovery -- a networks.discovery is set (it would silence this box's senders on mDNS)
#   ips       -- networks.ips is empty, or a REQUIRED IP is absent from it (a renumber / new
#                sender: re-provision).
#                Extra entries (a resolved traveling box, a stale lease) are fine: a finder just
#                queries one more address.
ndi_discovery_config_verdict() {
  local text="$1" req="${2-}" missing out="" disc
  if [ "$#" -lt 2 ]; then
    req="$(ndi_discovery_sender_ips pinned)" || req=""
  fi
  if [ -z "$text" ]; then
    printf 'FAIL: config missing (no %s)\n' "$NDI_DISCOVERY_CONFIG_NAME"
    return 0
  fi
  if command -v python3 >/dev/null 2>&1 \
    && ! printf '%s' "$text" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null 2>&1; then
    out="${out}FAIL: not valid JSON (the NDI SDK ignores an unparseable config)"$'\n'
  fi
  disc="$(ndi_discovery_config_servers "$text")"
  if [ -n "$(_ndi_discovery_norm_list "$disc")" ]; then
    out="${out}FAIL: networks.discovery='${disc}' is set (a configured sender stops mDNS; receivers need only networks.ips)"$'\n'
  fi
  if [ -z "$(_ndi_discovery_norm_list "$(ndi_discovery_config_ips "$text")")" ]; then
    out="${out}FAIL: networks.ips is empty (no managed sender listed -- re-provision)"$'\n'
  fi
  missing="$(ndi_discovery_missing_ips "$text" "$(_ndi_discovery_norm_list "$req")")"
  if [ -n "$missing" ]; then
    out="${out}FAIL: networks.ips lacks ${missing} (a renumbered or new sender -- re-provision)"$'\n'
  fi
  if [ -z "$out" ]; then
    printf 'ok\n'
  else
    printf '%s' "$out"
  fi
}

# ndi_discovery_dropin_content -> the systemd drop-in that points a root/ProtectHome NDI receiver at
# NDI_DISCOVERY_SYSTEM_DIR.
ndi_discovery_dropin_content() {
  printf '[Service]\n# issue 1342: libndi reads $NDI_CONFIG_DIR/%s (networks.ips = every managed NDI sender).\n# ProtectHome hides /root/.ndi, so the config lives in the system dir.\nEnvironment=NDI_CONFIG_DIR=%s\n' \
    "$NDI_DISCOVERY_CONFIG_NAME" "$NDI_DISCOVERY_SYSTEM_DIR"
}

# ndi_discovery_dropin_config_dir TEXT -> the NDI_CONFIG_DIR value in a drop-in TEXT, "" if absent.
ndi_discovery_dropin_config_dir() {
  printf '%s\n' "$1" | grep -oE '^Environment=NDI_CONFIG_DIR=[^[:space:]]+' | tail -1 | cut -d= -f3- || true
}

# _ndi_discovery_merged_json FILE IPS -> FILE's JSON with ndi.networks.ips = IPS, any
# ndi.networks.discovery REMOVED, every other key kept (2-space indent, trailing newline -- a canonical
# file comes back byte-identical). Non-zero when python3 is absent or FILE is not a JSON object.
_ndi_discovery_merged_json() {
  command -v python3 >/dev/null 2>&1 || return 1
  NDI_DISCOVERY_MERGE_IPS="$2" python3 -c '
import json, os, sys
with open(sys.argv[1], encoding="utf-8-sig") as fh:
    doc = json.load(fh)
if not isinstance(doc, dict):
    sys.exit(1)
ndi = doc.get("ndi")
if not isinstance(ndi, dict):
    ndi = doc["ndi"] = {}
net = ndi.get("networks")
if not isinstance(net, dict):
    net = ndi["networks"] = {}
net["ips"] = os.environ["NDI_DISCOVERY_MERGE_IPS"]
net.pop("discovery", None)
sys.stdout.write(json.dumps(doc, indent=2) + "\n")
' "$1"
}

# ndi_discovery_write_config DIR IPS [OWNER] -> write DIR/ndi-config.v1.json (networks.ips = IPS),
# mode 0644, via a temp file + atomic rename so a reader never sees a half-written file. No file yet
# -> the canonical config. An existing file is MERGED (networks.ips set, networks.discovery removed,
# every other key kept); when it cannot be merged (not JSON, or no python3 on the box) and differs
# from the canonical config, it is backed up to ndi-config.v1.json.bak-<stamp> before being replaced.
# With OWNER, DIR and the file are chowned to OWNER (a desktop user's ~/.ndi). Idempotent. Returns
# non-zero (with a message on stderr) on an EMPTY IPS (a failed generator must never write an empty
# list) or when DIR cannot be created or written -- the caller aborts (setup-device.sh via its own
# `set -e`, setup-strih.sh via `|| fail`).
ndi_discovery_write_config() {
  local dir="${1:?ndi_discovery_write_config: DIR required}" ips="${2:-}" owner="${3:-}" tmp target
  [ -n "$ips" ] || { echo "ndi-discovery: refusing to write an EMPTY networks.ips into $dir" >&2; return 1; }
  # Root writing into a USER-owned dir (OWNER set): never follow a symlinked dir or config -- a
  # planted ~/.ndi -> /etc link would otherwise have root chown /etc.
  if [ -n "$owner" ] && { [ -L "$dir" ] || [ -L "$dir/$NDI_DISCOVERY_CONFIG_NAME" ]; }; then
    echo "ndi-discovery: refusing a symlinked $dir (or its config) -- remove the link and re-run" >&2
    return 1
  fi
  target="$dir/$NDI_DISCOVERY_CONFIG_NAME"
  mkdir -p "$dir" || { echo "ndi-discovery: cannot create $dir" >&2; return 1; }
  tmp="$(mktemp "$dir/.${NDI_DISCOVERY_CONFIG_NAME}.XXXXXX")" \
    || { echo "ndi-discovery: cannot write into $dir" >&2; return 1; }
  if [ -s "$target" ] && _ndi_discovery_merged_json "$target" "$ips" > "$tmp" 2>/dev/null; then
    : # merged: every existing key kept, networks.ips set, networks.discovery removed
  else
    if ! ndi_discovery_config_json "$ips" > "$tmp"; then
      rm -f "$tmp"
      echo "ndi-discovery: cannot write $tmp" >&2
      return 1
    fi
    if [ -s "$target" ] && ! cmp -s "$target" "$tmp"; then
      cp -p "$target" "$target.bak-$(date +%Y%m%d-%H%M%S)" \
        || { rm -f "$tmp"; echo "ndi-discovery: cannot back up $target" >&2; return 1; }
      echo "ndi-discovery: $target could not be merged -- backed up, replaced with the canonical config" >&2
    fi
  fi
  chmod 0644 "$tmp"
  if [ -n "$owner" ]; then
    chown -h "$owner":"$owner" "$dir" "$tmp" || { rm -f "$tmp"; echo "ndi-discovery: chown $owner failed" >&2; return 1; }
  fi
  mv -f "$tmp" "$target" || { rm -f "$tmp"; echo "ndi-discovery: rename into $dir failed" >&2; return 1; }
}

# ndi_discovery_gather_remote_snippet -> the on-box bash that prints the cambox config + drop-in
# for verify-device's (an) check, between fixed markers (one ssh round trip, read-only).
ndi_discovery_gather_remote_snippet() {
  printf 'echo "__NDI_CONF_BEGIN__"; cat %q 2>/dev/null; echo "__NDI_CONF_END__"; echo "__NDI_DROPIN_BEGIN__"; cat %q 2>/dev/null; echo "__NDI_DROPIN_END__"\n' \
    "$NDI_DISCOVERY_SYSTEM_DIR/$NDI_DISCOVERY_CONFIG_NAME" "$NDI_DISCOVERY_CAMBOX_DROPIN"
}

# ndi_discovery_block_section BLOCK NAME -> the text between __NAME_BEGIN__ and __NAME_END__ in a
# gathered BLOCK ("" if absent). Pure awk, never errors.
ndi_discovery_block_section() {
  printf '%s\n' "$1" | awk -v b="__${2}_BEGIN__" -v e="__${2}_END__" '$0==b{on=1;next} $0==e{on=0} on' || true
}

# --- CLI (executed, not sourced): print the list / config for a box this repo does not provision ---
# (stream, resolume, an owner laptop -- see the rule). `resolve` (default) resolves the traveling
# hostname senders from THIS machine; `pinned` is the deterministic checked-in form.
#   bash scripts/lib/ndi-discovery.sh --ips  [resolve|pinned]
#   bash scripts/lib/ndi-discovery.sh --json [resolve|pinned]
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  case "${1:-}" in
    --ips)  _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; printf '%s\n' "$_ndi_ips" ;;
    --json) _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; ndi_discovery_config_json "$_ndi_ips" ;;
    *)
      echo "usage: $0 --ips|--json [resolve|pinned]" >&2
      exit 2
      ;;
  esac
fi
