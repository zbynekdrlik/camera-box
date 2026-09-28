#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (plus a tiny CLI when executed, which sets its
# own strict mode below), sourced by setup-device.sh / verify-device.sh / setup-strih.sh /
# verify-strih.sh + tests/python/test_ndi_discovery_1342.py -- mirrors every sibling in scripts/lib/
# (remote-logging.sh, dscp-nft.sh, ndi-runtime.sh), none of which set -euo pipefail at file level:
# sourcing a `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/ndi-discovery.sh -- issue 1342: the ONE source of truth for the fleet's RECEIVER-side
# NDI config (`ndi-config.v1.json`), which lists the managed OBS-box NDI senders by IP -- and, since
# issue 1389, NEVER a cambox. A cambox itself carries NO list at all (ROZHODNUTÉ 5879261962): the
# CAMBOX section near the end takes it off.
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
# strih, dead 10.77.8.5x addresses): every member of the obs-fleet `ndi-sender` facet
# (scripts/lib/obs-fleet.sh: strih-lx, stream, resolume; `retired` rows excluded by obs_fleet_boxes).
# A fleet host that is a hostname (resolume.lan, a traveling DHCP box) is resolved to IPv4 at WRITE
# time; unresolvable -> skipped with a log line, and its sources stay mDNS-only exactly as before.
# The VERIFIERS require only the PINNED part (every IPv4 entry), so a traveling lease never makes a
# verify flap, while a renumbered strih-lx / stream still FAILS until re-provisioned.
#
# NEVER A CAMBOX (issue 1389). A finder with extra IPs opens a TCP discovery connection to each
# listed sender's :5960 listener (an mDNS-only finder opens none -- proven live), and camera-box
# serves each such INBOUND connection on a libndi 6.3.2 `disc:recv` thread. When the remote NDI
# process exits or restarts (strih OBS, SongPlayer, an OBS relaunch), that thread's teardown
# intermittently throws an uncaught std::system_error (EINVAL) inside libndi's static C++ runtime and
# ABORTS camera-box -- a ~3 s camera outage and a V4L2 re-open. So the camboxes are found by mDNS
# alone, as before 24.9.2026 (they were never among the missed sources). The camera walk below stays
# only as the FORBIDDEN set: the generator drops a cambox IP by construction (and fails loud when it
# cannot derive the set), the verdict FAILs a config that lists one, and the Windows .ps1 removes
# them. A cambox's OWN config carries no list either (ROZHODNUTÉ 5879261962): with any listed host it
# would hold OUTBOUND discovery connections to the very boxes whose restarts trigger the abort.
#
# Config LOCATION: the Linux SDK reads `$HOME/.ndi/ndi-config.v1.json`, or
# `$NDI_CONFIG_DIR/ndi-config.v1.json` when that env var is set. strih-lx's intercom-hub runs with
# `ProtectHome=true`, so its config lives in the system dir /etc/ndi and an intercom-hub.service.d
# drop-in points NDI_CONFIG_DIR at it. The camboxes used the same pair (/etc/ndi + a camera-box.service.d
# drop-in) until issue 1389; the cambox writer below now removes both, so camera-box (root,
# `ProtectHome=yes`, no readable $HOME/.ndi) runs on libndi's defaults: mDNS only.
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
# The camera-box.service drop-in that pointed the appliance's libndi at NDI_DISCOVERY_SYSTEM_DIR (issue
# 1342); since issue 1389 the cambox writer removes it with the config. Overridable for tests.
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (removal) + verify-device.sh (an)
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
# cam2, ... until the first unknown name -- NOT CAMERA_ACTIVE_SET: a camera retired from MEASUREMENT is
# still a powered NDI sender, and walking the resolver picks up a new `camN)` arm with no second
# roster. Issue 1389: this is the FORBIDDEN set, never a list member. Runs in a SUBSHELL so the
# caller's CAMERA_IP / CAMERA_NAME / CAMERA_SOURCE (setup-device.sh's own box) are never clobbered.
ndi_discovery_camera_ips() {
  (
    n=1
    while [ "$n" -le "$NDI_DISCOVERY_CAMERA_MAX" ] && camera_resolve "cam$n" 2>/dev/null; do
      printf '%s\n' "$CAMERA_IP"
      n=$((n + 1))
    done
  )
}

# ndi_discovery_cambox_ips -> the comma list of every cambox IP (ndi_discovery_camera_ips, in camera
# order). Non-zero (and no output) when camera_resolve knows no camera: without the set nothing can
# prove a list is cambox-free (issue 1389).
ndi_discovery_cambox_ips() {
  local ips
  ips="$(ndi_discovery_camera_ips)"
  if [ -z "$ips" ]; then
    echo "ndi-discovery: camera_resolve knows no camera (cam1 unresolvable) -- the cambox IP set is unknown" >&2
    return 1
  fi
  printf '%s' "${ips//$'\n'/,}"
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

# ndi_discovery_sender_ips [resolve|pinned] -> the comma-separated networks.ips list: the obs-fleet
# ndi-sender hosts, in fleet order, each IP once, NEVER a cambox (issue 1389).
#   pinned  -- every IPv4 fleet host. Deterministic: what the verifiers REQUIRE and what the
#              checked-in config / the laptop .ps1 default carry.
#   resolve -- (default, the provisioners) pinned + every HOSTNAME fleet host resolved to IPv4; an
#              unresolvable or non-IPv4 answer is skipped and named on stderr.
# A host (pinned or resolved) on a cambox IP is skipped and named on stderr. Non-zero (and no output)
# when the fleet lookup fails, when the cambox set cannot be derived (a renamed camera_resolve arm
# must never let a cambox through unnoticed), or when no host is left (an empty list is never written).
ndi_discovery_sender_ips() {
  local mode="${1:-resolve}" hosts camboxes c ip out="" seen=" "
  case "$mode" in
    resolve|pinned) ;;
    *) echo "ndi-discovery: unknown mode '${mode}' (expected resolve|pinned)" >&2; return 1 ;;
  esac
  hosts="$(ndi_discovery_fleet_hosts)" || {
    echo "ndi-discovery: obs-fleet facet '${NDI_DISCOVERY_FLEET_FACET}' lookup failed -- no sender list" >&2
    return 1
  }
  camboxes="$(ndi_discovery_cambox_ips)" || {
    echo "ndi-discovery: cannot exclude the camboxes (issue 1389) -- no sender list" >&2
    return 1
  }
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
    case ",$camboxes," in
      *",$ip,"*)
        echo "ndi-discovery: sender '$c' is on the cambox IP $ip -- never listed (a remote finder's discovery connection aborts camera-box, issue 1389)" >&2
        continue
        ;;
    esac
    case "$seen" in *" $ip "*) continue ;; esac
    seen="${seen}${ip} "
    out="${out:+$out,}$ip"
  done <<EOF
$hosts
EOF
  if [ -z "$out" ]; then
    echo "ndi-discovery: no obs-fleet '${NDI_DISCOVERY_FLEET_FACET}' host left to list -- no sender list" >&2
    return 1
  fi
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

# _ndi_discovery_networks_key KEY TEXT -> the value of ndi.networks.KEY in TEXT: a string as-is, any
# other non-null value (a hand-edited array, a number, true/false) as its compact JSON, "" when absent
# or null.
# With python3 (dev1, strih-lx, the camboxes, the tests) it reads that exact JSON path, so an unrelated
# `"KEY"` elsewhere in the file never stands in for it (issue 1389). Without python3 (a stripped-down
# box), or when TEXT is not JSON, it falls back to the grep reader above. Never errors.
_ndi_discovery_networks_key() {
  local v
  if command -v python3 >/dev/null 2>&1 \
    && v="$(printf '%s' "$2" | NDI_DISCOVERY_KEY="$1" python3 -c '
import json, os, sys
try:
    node = json.loads(sys.stdin.buffer.read().decode("utf-8-sig"))
except Exception:
    sys.exit(1)
for part in ("ndi", "networks", os.environ["NDI_DISCOVERY_KEY"]):
    node = node.get(part) if isinstance(node, dict) else None
# A string is the value; absent / null is ""; any other type (a hand-edited array) is still set, so
# it comes back as its compact JSON -- never "" (issue 1389: a cambox carries no networks.ips at all).
if node is None:
    node = ""
elif not isinstance(node, str):
    node = json.dumps(node, separators=(",", ":"))
sys.stdout.write(node)
' 2>/dev/null)"; then
    # Same output shape as the grep reader: the value plus a newline, nothing when absent.
    [ -z "$v" ] || printf '%s\n' "$v"
    return 0
  fi
  _ndi_discovery_json_string "$1" "$2"
}

# ndi_discovery_config_ips TEXT -> the `ndi.networks.ips` value in TEXT, "" if absent.
ndi_discovery_config_ips() { _ndi_discovery_networks_key ips "$1"; }

# ndi_discovery_config_servers TEXT -> the `ndi.networks.discovery` value in TEXT, "" if absent
# (graded only to catch a stray one: a managed box must never carry it).
ndi_discovery_config_servers() { _ndi_discovery_networks_key discovery "$1"; }

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

# ndi_discovery_list_common A B -> the comma list of A's entries that are also in B, in A's order
# ("" when none). An A entry is also matched by its bare IPv4 address: a leading `::ffff:` (an
# IPv4-mapped address) and a single `:port` are stripped for the comparison, and the entry is named
# as listed. Any other form with a colon (an IPv6 literal) is compared as written. Spaces in either
# list are ignored; A is split with read, never glob-expanded (it may be config text read off a
# box). Pure.
ndi_discovery_list_common() {
  local b ip bare out="" items=()
  b=",$(_ndi_discovery_norm_list "$2"),"
  IFS=', ' read -r -a items <<<"$1" || true
  for ip in "${items[@]}"; do
    [ -n "$ip" ] || continue
    bare="${ip#::ffff:}"
    case "$bare" in *:*:*) ;; *:*) bare="${bare%%:*}" ;; esac
    if [ -n "$bare" ]; then
      case "$b" in *",$bare,"*) out="${out:+$out,}$ip" ;; esac
    fi
  done
  printf '%s' "$out"
}

# ndi_discovery_config_verdict TEXT [REQUIRED [FORBIDDEN]] -> "ok", or one `FAIL: <facet>` line per
# failing facet. Always exits 0 (the caller branches on the printed verdict). REQUIRED defaults to the
# PINNED list, FORBIDDEN to every cambox IP (ndi_discovery_cambox_ips). Facets:
#   missing   -- TEXT empty (no config file on the box)
#   JSON      -- TEXT is not valid JSON (checked only when python3 is available; verify-strih runs
#                this on strih-lx, which has it)
#   discovery -- a networks.discovery is set (it would silence this box's senders on mDNS)
#   ips       -- networks.ips is empty, or a REQUIRED IP is absent from it (a renumber / new
#                sender: re-provision).
#                Extra non-cambox entries (a resolved traveling box, a stale lease) are fine: a finder
#                just queries one more address.
#   cambox    -- networks.ips lists a FORBIDDEN (cambox) IP (issue 1389: the remote finder's discovery
#                connection into that cambox aborts camera-box when it closes), or the default cambox
#                set cannot be derived (then nothing proves the list is cambox-free).
ndi_discovery_config_verdict() {
  local text="$1" req="${2-}" forbid="${3-}" forbid_known=1 missing out="" disc listed
  if [ "$#" -lt 2 ]; then
    req="$(ndi_discovery_sender_ips pinned)" || req=""
  fi
  if [ "$#" -lt 3 ]; then
    forbid="$(ndi_discovery_cambox_ips 2>/dev/null)" || forbid=""
    [ -n "$forbid" ] || forbid_known=0
  fi
  if [ -z "$text" ]; then
    printf 'FAIL: config missing (no %s)\n' "$NDI_DISCOVERY_CONFIG_NAME"
    return 0
  fi
  # Strict JSON (a leading BOM FAILs): this config must reach the SDK, and every writer of it (this lib,
  # the .ps1) writes it without a BOM. The cambox verdict below reads utf-8-sig instead, because there
  # the only question is whether a list is left, and the cambox plan rewrites or removes the file.
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
  if [ "$forbid_known" = 0 ]; then
    out="${out}FAIL: the cambox IP set is unavailable (camera_resolve knows no camera) -- cannot prove networks.ips names no cambox (issue 1389)"$'\n'
  else
    listed="$(ndi_discovery_list_common "$(ndi_discovery_config_ips "$text")" "$forbid")"
    if [ -n "$listed" ]; then
      out="${out}FAIL: networks.ips lists the cambox IP(s) ${listed} (a remote finder's discovery connection into a cambox aborts camera-box when it closes, issue 1389 -- re-provision)"$'\n'
    fi
  fi
  if [ -z "$out" ]; then
    printf 'ok\n'
  else
    printf '%s' "$out"
  fi
}

# ndi_discovery_verdict_oneline VERDICT -> a verdict's `FAIL: <facet>` lines (either verdict above or
# below) as ONE line for a check's FAIL message: the prefix dropped, facets joined by '; ', blank lines
# skipped. verify-device (an) and verify-strih item 34 both print through it. Never errors.
ndi_discovery_verdict_oneline() {
  printf '%s\n' "$1" | awk 'NF { if (n++) printf "; "; sub(/^FAIL: /, ""); printf "%s", $0 }' || true
}

# ndi_discovery_dropin_content -> the systemd drop-in that points a root/ProtectHome NDI receiver at
# NDI_DISCOVERY_SYSTEM_DIR.
ndi_discovery_dropin_content() {
  printf '[Service]\n# issue 1342: libndi reads $NDI_CONFIG_DIR/%s (networks.ips = the OBS-fleet NDI senders, never a cambox: issue 1389).\n# ProtectHome hides /root/.ndi, so the config lives in the system dir.\nEnvironment=NDI_CONFIG_DIR=%s\n' \
    "$NDI_DISCOVERY_CONFIG_NAME" "$NDI_DISCOVERY_SYSTEM_DIR"
}

# ndi_discovery_dropin_config_dir TEXT -> the NDI_CONFIG_DIR value in a drop-in TEXT, "" if absent.
# Reads both forms systemd accepts: `Environment=NDI_CONFIG_DIR=/etc/ndi` and the quoted
# `Environment="NDI_CONFIG_DIR=/etc/ndi"` (a hand-edited drop-in; issue 1389 review round 3).
ndi_discovery_dropin_config_dir() {
  printf '%s\n' "$1" | grep -oE '^Environment="?NDI_CONFIG_DIR=[^[:space:]"]+' | tail -1 \
    | sed -E 's/^Environment="?NDI_CONFIG_DIR=//' || true
}

# _ndi_discovery_dropin_foreign TEXT DIR -> exit 0 and print the dir drop-in TEXT points NDI_CONFIG_DIR
# at ("" when none) when TEXT has content that does not point it at DIR: not this repo's drop-in. Exit 1
# (nothing printed) when TEXT is blank -- an inert file -- or points at DIR. The ONE test the cambox
# plan (refuse) and the cambox verdict (FAIL) share, so they cannot drift apart (issue 1389).
_ndi_discovery_dropin_foreign() {
  local dropdir
  [ -n "$(_ndi_discovery_norm_list "$1")" ] || return 1
  dropdir="$(ndi_discovery_dropin_config_dir "$1")"
  [ "$dropdir" != "$2" ] || return 1
  printf '%s\n' "$dropdir"
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
# for verify-device's (an) check, between fixed markers (one ssh round trip, read-only). Both are
# expected ABSENT on a cambox since issue 1389 (ndi_discovery_cambox_verdict grades them). The `echo`
# after each `cat` keeps a file without a final newline (a hand edit) off the END marker's line; the
# extra blank line is dropped by the caller's $(...).
ndi_discovery_gather_remote_snippet() {
  printf 'echo "__NDI_CONF_BEGIN__"; cat %q 2>/dev/null; echo; echo "__NDI_CONF_END__"; echo "__NDI_DROPIN_BEGIN__"; cat %q 2>/dev/null; echo; echo "__NDI_DROPIN_END__"\n' \
    "$NDI_DISCOVERY_SYSTEM_DIR/$NDI_DISCOVERY_CONFIG_NAME" "$NDI_DISCOVERY_CAMBOX_DROPIN"
}

# ndi_discovery_block_section BLOCK NAME -> the text between __NAME_BEGIN__ and __NAME_END__ in a
# gathered BLOCK ("" if absent). Pure awk, never errors.
ndi_discovery_block_section() {
  printf '%s\n' "$1" | awk -v b="__${2}_BEGIN__" -v e="__${2}_END__" '$0==b{on=1;next} $0==e{on=0} on' || true
}

# --- the CAMBOX config: NO networks.ips at all (issue 1389, ROZHODNUTÉ 5879261962) ---------------
# A cambox is an mDNS-only NDI receiver. camera-box's finder needs only the strih preview source
# (`STRIH-LX (interkom)`), which mDNS finds in milliseconds, and any listed host would give the cambox
# an OUTBOUND discovery connection whose teardown at every strih / stream / resolume restart is the
# same abort risk as the inbound one. So setup-device STEP 7 and `--cambox-apply` take the list OFF a
# cambox (the plan/apply pair below), and verify-device (an) FAILs a cambox that still lists any IP
# (ndi_discovery_cambox_verdict). The receiver list above is for the OBS boxes and laptops only.

# _ndi_discovery_stripped_json FILE -> FILE's JSON object with ndi.networks.ips and
# ndi.networks.discovery removed, and any object that removal left empty dropped (2-space indent,
# trailing newline; `{}` when nothing is left). Exit 1 without python3, exit 2 when FILE is not a JSON
# object.
_ndi_discovery_stripped_json() {
  command -v python3 >/dev/null 2>&1 || return 1
  python3 -c '
import json, sys
try:
    with open(sys.argv[1], encoding="utf-8-sig") as fh:
        doc = json.load(fh)
except Exception:
    sys.exit(2)
if not isinstance(doc, dict):
    sys.exit(2)
ndi = doc.get("ndi")
if isinstance(ndi, dict):
    net = ndi.get("networks")
    if isinstance(net, dict):
        net.pop("ips", None)
        net.pop("discovery", None)
        if not net:
            ndi.pop("networks")
    if not ndi:
        doc.pop("ndi")
sys.stdout.write(json.dumps(doc, indent=2) + "\n")
' "$1"
}

# ndi_discovery_cambox_plan DIR DROPIN -> ONE line: what makes this cambox mDNS-only. DIR is the dir the
# camera-box drop-in points libndi at ($NDI_DISCOVERY_SYSTEM_DIR), DROPIN that drop-in's path. Pure
# read: never writes, always exits 0.
#   none           -- nothing to do: no list, no discovery key, nothing stale to remove
#   remove         -- delete DIR/ndi-config.v1.json (it holds only the list, or nothing) and the drop-in
#   remove-backup  -- the same, the config backed up first: it is not a JSON object, or python3 is
#                     missing and the file lists something but is not this lib's own rendering, so any
#                     other keys it holds are kept in the backup only
#   strip          -- rewrite the config without networks.ips / networks.discovery; its other keys
#                     still matter, so the config and the drop-in stay
#   refuse <why>   -- the drop-in has content that does not point NDI_CONFIG_DIR at DIR: not this
#                     repo's file, left for a human. A BLANK drop-in is inert (verify-device grades it
#                     as none) and is never refused: it goes with the config on remove / remove-backup,
#                     alone when no config is left, and stays (still inert) on none / strip.
ndi_discovery_cambox_plan() {
  local dir="${1:?ndi_discovery_cambox_plan: DIR required}" dropin="${2:?ndi_discovery_cambox_plan: DROPIN required}"
  local f have droptext dropdir ips disc rest rc nonstr
  f="$dir/$NDI_DISCOVERY_CONFIG_NAME"
  droptext=""
  [ ! -e "$dropin" ] || droptext="$(cat "$dropin" 2>/dev/null || true)"
  if dropdir="$(_ndi_discovery_dropin_foreign "$droptext" "$dir")"; then
    printf 'refuse %s points NDI_CONFIG_DIR at %s, not %s -- not this repo%ss drop-in; inspect it by hand\n' \
      "$dropin" "${dropdir:-<nothing>}" "$dir" "'"
    return 0
  fi
  if [ ! -e "$f" ]; then
    if [ -e "$dropin" ]; then echo remove; else echo none; fi
    return 0
  fi
  have="$(cat "$f" 2>/dev/null || true)"
  if [ -z "$(_ndi_discovery_norm_list "$have")" ]; then
    echo remove
    return 0
  fi
  ips="$(_ndi_discovery_norm_list "$(ndi_discovery_config_ips "$have")")"
  disc="$(_ndi_discovery_norm_list "$(ndi_discovery_config_servers "$have")")"
  rc=0
  rest="$(_ndi_discovery_stripped_json "$f" 2>/dev/null)" || rc=$?
  case "$rc" in
    0)
      if [ "$rest" = "{}" ]; then
        echo remove
      elif [ -z "$ips$disc" ]; then
        echo none
      else
        echo strip
      fi
      ;;
    1)
      # No python3: the grep reader finds a string list; a non-string ips / discovery value (an array,
      # a number, true/false -- anything but a string or null) it cannot read is counted here, so the
      # file goes with a backup like any list. A config that sets neither needs no change.
      nonstr="$(printf '%s\n' "$have" | grep -cE '"(ips|discovery)"[[:space:]]*:[[:space:]]*[^"[:space:]n]' || true)"
      if [ -z "$ips$disc" ] && [ "$nonstr" = 0 ]; then
        echo none
      elif [ "$have" = "$(ndi_discovery_config_json "$(ndi_discovery_config_ips "$have")")" ]; then
        echo remove
      else
        echo remove-backup
      fi
      ;;
    *) echo remove-backup ;;
  esac
}

# ndi_discovery_cambox_apply_plan DIR DROPIN ACTION -> carry out ACTION (from ndi_discovery_cambox_plan)
# on a WRITABLE root: strip rewrites the config through a temp file + atomic rename (0644); remove
# deletes the config and the drop-in; remove-backup first copies the config to .bak-<stamp>. The caller
# runs `systemctl daemon-reload` after a removed drop-in. Non-zero with a message on any failure, and on
# none / refuse (nothing to carry out).
ndi_discovery_cambox_apply_plan() {
  local dir="${1:?ndi_discovery_cambox_apply_plan: DIR required}" dropin="${2:?ndi_discovery_cambox_apply_plan: DROPIN required}"
  local action="${3:-}" f tmp
  f="$dir/$NDI_DISCOVERY_CONFIG_NAME"
  case "$action" in
    strip)
      tmp="$(mktemp "$dir/.${NDI_DISCOVERY_CONFIG_NAME}.XXXXXX")" \
        || { echo "ndi-discovery: cannot write into $dir" >&2; return 1; }
      if ! _ndi_discovery_stripped_json "$f" > "$tmp"; then
        rm -f "$tmp"
        echo "ndi-discovery: cannot strip networks.ips from $f" >&2
        return 1
      fi
      chmod 0644 "$tmp"
      mv -f "$tmp" "$f" || { rm -f "$tmp"; echo "ndi-discovery: rename into $dir failed" >&2; return 1; }
      ;;
    remove | remove-backup)
      if [ "$action" = remove-backup ] && [ -e "$f" ]; then
        cp -p "$f" "$f.bak-$(date +%Y%m%d-%H%M%S)" || { echo "ndi-discovery: cannot back up $f" >&2; return 1; }
      fi
      rm -f "$f" "$dropin" || { echo "ndi-discovery: cannot remove $f / $dropin" >&2; return 1; }
      ;;
    *)
      echo "ndi-discovery: nothing to carry out for plan '${action}'" >&2
      return 1
      ;;
  esac
}

# ndi_discovery_cambox_verdict CONF DROPIN -> "ok", or one `FAIL: <facet>` line per failing facet, for a
# CAMBOX. CONF is the text of its $NDI_DISCOVERY_SYSTEM_DIR/ndi-config.v1.json ("" = absent), DROPIN
# the text of its camera-box drop-in ("" = absent). Always exits 0. Facets:
#   dropin    -- a drop-in that does not point NDI_CONFIG_DIR at the system dir (not this repo's)
#   JSON      -- CONF is not valid JSON (checked only when python3 is there; verify-device runs on dev1)
#   ips       -- CONF lists any IP, named; also without the drop-in, which would load it again
#   discovery -- CONF sets a discovery server
# No config, or one holding only other keys, is ok: the box is an mDNS-only receiver.
ndi_discovery_cambox_verdict() {
  local text="$1" dropin="${2-}" dropdir ips disc out=""
  if dropdir="$(_ndi_discovery_dropin_foreign "$dropin" "$NDI_DISCOVERY_SYSTEM_DIR")"; then
    out="${out}FAIL: camera-box.service.d/ndi-discovery.conf points NDI_CONFIG_DIR at '${dropdir:-<nothing>}', not ${NDI_DISCOVERY_SYSTEM_DIR} -- not this repo's drop-in, inspect it"$'\n'
  fi
  if [ -n "$(_ndi_discovery_norm_list "$text")" ]; then
    # utf-8-sig: a leading BOM is valid here exactly as it is for the plan's reader.
    if command -v python3 >/dev/null 2>&1 \
      && ! printf '%s' "$text" \
        | python3 -c 'import json,sys; json.loads(sys.stdin.buffer.read().decode("utf-8-sig"))' >/dev/null 2>&1; then
      out="${out}FAIL: not valid JSON (the NDI SDK ignores it; --cambox-apply removes it)"$'\n'
    fi
    ips="$(ndi_discovery_config_ips "$text")"
    if [ -n "$(_ndi_discovery_norm_list "$ips")" ]; then
      out="${out}FAIL: networks.ips lists ${ips} -- a cambox carries no networks.ips (mDNS only: a listed host's discovery connection can abort camera-box, issue 1389)"$'\n'
    fi
    disc="$(ndi_discovery_config_servers "$text")"
    if [ -n "$(_ndi_discovery_norm_list "$disc")" ]; then
      out="${out}FAIL: networks.discovery='${disc}' is set (a configured sender stops mDNS)"$'\n'
    fi
  fi
  if [ -z "$out" ]; then
    printf 'ok\n'
  else
    printf '%s' "$out"
  fi
}

# ndi_discovery_cambox_apply_remote_snippet -> the on-box bash program (fed to `bash -s` as root over ssh,
# see the CLI below) that makes a live cambox mDNS-only: issue 1389's smallest safe equivalent of
# re-running setup-device.sh STEP 7. It embeds the SAME plan/apply pair STEP 7 calls (declare -f, never
# a copy). A clean box (plan none) writes nothing and never remounts; a refused plan (a foreign drop-in)
# exits non-zero with nothing touched. Otherwise, on a read-only root it remounts rw, carries out the
# plan, syncs and remounts ro again (retried 3x; a root it cannot put back is a loud non-zero, and the
# EXIT trap puts it back on any failure), then runs `systemctl daemon-reload` when it removed the
# drop-in. It needs no fleet list, so the program is the same for every cambox.
ndi_discovery_cambox_apply_remote_snippet() {
  printf 'set -eu\n'
  printf 'NDI_DISCOVERY_CONFIG_NAME=%q\n' "$NDI_DISCOVERY_CONFIG_NAME"
  printf '_ndi_dir=%q\n_ndi_dropin=%q\n' "$NDI_DISCOVERY_SYSTEM_DIR" "$NDI_DISCOVERY_CAMBOX_DROPIN"
  declare -f _ndi_discovery_json_string _ndi_discovery_networks_key ndi_discovery_config_ips \
    ndi_discovery_config_servers _ndi_discovery_norm_list ndi_discovery_dropin_config_dir \
    _ndi_discovery_dropin_foreign ndi_discovery_config_json _ndi_discovery_stripped_json ndi_discovery_cambox_plan \
    ndi_discovery_cambox_apply_plan
  cat <<'NDI_APPLY'
_ndi_plan="$(ndi_discovery_cambox_plan "$_ndi_dir" "$_ndi_dropin")"
case "$_ndi_plan" in
  none)
    echo "ndi-discovery: this cambox carries no networks.ips already (mDNS only) -- nothing written"
    exit 0
    ;;
  refuse*)
    echo "ndi-discovery: REFUSED: ${_ndi_plan#refuse } (issue 1389)" >&2
    exit 1
    ;;
esac
_ndi_had_dropin=0
[ ! -e "$_ndi_dropin" ] || _ndi_had_dropin=1
_ndi_ro=0
_ndi_opts="$(findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null || true)"
case "$_ndi_opts" in ro | ro,*) _ndi_ro=1 ;; esac
_ndi_restore_ro() {
  [ "$_ndi_ro" = 1 ] || return 0
  for _ndi_try in 1 2 3; do
    if mount -o remount,ro /; then
      _ndi_ro=0
      return 0
    fi
    [ "$_ndi_try" = 3 ] || sleep 2
  done
  echo "ERROR: mount -o remount,ro / FAILED 3x -- the root stays read-WRITE; find the holder (lsof +L1; fuser -vm /) and run 'mount -o remount,ro /' by hand" >&2
  return 1
}
trap '_ndi_restore_ro || true' EXIT
if [ "$_ndi_ro" = 1 ]; then
  mount -o remount,rw /
fi
ndi_discovery_cambox_apply_plan "$_ndi_dir" "$_ndi_dropin" "$_ndi_plan"
sync
if ! _ndi_restore_ro; then
  trap - EXIT
  exit 1
fi
trap - EXIT
if [ "$_ndi_had_dropin" = 1 ] && [ ! -e "$_ndi_dropin" ]; then
  systemctl daemon-reload
fi
echo "OK: ${_ndi_plan} done -- this cambox carries no networks.ips now (mDNS only); restart camera-box.service to load it"
NDI_APPLY
}

# --- CLI (executed, not sourced): print the list / config for a box this repo does not provision ---
# (stream, resolume, an owner laptop -- see the rule). `resolve` (default) resolves the traveling
# hostname senders from THIS machine; `pinned` is the deterministic checked-in form.
#   bash scripts/lib/ndi-discovery.sh --ips  [resolve|pinned]
#   bash scripts/lib/ndi-discovery.sh --json [resolve|pinned]
#   bash scripts/lib/ndi-discovery.sh --cambox-ips            (the FORBIDDEN set, issue 1389)
#   bash scripts/lib/ndi-discovery.sh --cambox-apply > apply.sh
#        (the on-box cambox program that strips the list, then: ssh root@<cambox> bash -s < apply.sh)
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  case "${1:-}" in
    --ips)  _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; printf '%s\n' "$_ndi_ips" ;;
    --json) _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; ndi_discovery_config_json "$_ndi_ips" ;;
    --cambox-ips) _ndi_cams="$(ndi_discovery_cambox_ips)"; printf '%s\n' "$_ndi_cams" ;;
    --cambox-apply)
      _ndi_prog="$(ndi_discovery_cambox_apply_remote_snippet)"
      printf '%s\n' "$_ndi_prog"
      ;;
    *)
      echo "usage: $0 --ips|--json [resolve|pinned] | --cambox-ips | --cambox-apply" >&2
      exit 2
      ;;
  esac
fi
