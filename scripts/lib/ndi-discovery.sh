#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (plus a tiny CLI when executed, which sets its
# own strict mode below), sourced by setup-device.sh / verify-device.sh / setup-strih.sh /
# verify-strih.sh + tests/python/test_ndi_discovery_1342.py -- mirrors every sibling in scripts/lib/
# (remote-logging.sh, dscp-nft.sh, ndi-runtime.sh), none of which set -euo pipefail at file level:
# sourcing a `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/ndi-discovery.sh -- issue 1342: the ONE source of truth for the fleet's RECEIVER-side
# NDI config (`ndi-config.v1.json`), which lists the managed OBS-box NDI senders by IP -- and, since
# issue 1389, NEVER a cambox.
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
# them. The cambox's own list keeps its OUTBOUND connections to the listed OBS boxes; whether those
# can abort it too is not measured yet (the rule's runbook acceptance checks it).
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

# _ndi_discovery_networks_key KEY TEXT -> the string value of ndi.networks.KEY in TEXT, "" if absent.
# With python3 (dev1, strih-lx, the tests) it reads that exact JSON path, so an unrelated `"KEY"`
# elsewhere in the file never stands in for it (issue 1389). Without python3 (a cambox), or when TEXT
# is not JSON, it falls back to the grep reader above. Never errors.
_ndi_discovery_networks_key() {
  local v
  if command -v python3 >/dev/null 2>&1 \
    && v="$(printf '%s' "$2" | NDI_DISCOVERY_KEY="$1" python3 -c '
import json, os, sys
try:
    node = json.load(sys.stdin)
except Exception:
    sys.exit(1)
for part in ("ndi", "networks", os.environ["NDI_DISCOVERY_KEY"]):
    node = node.get(part) if isinstance(node, dict) else None
sys.stdout.write(node if isinstance(node, str) else "")
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
#   JSON      -- TEXT is not valid JSON (checked only when python3 is available; verify-device runs
#                this on dev1, verify-strih on strih-lx -- both have it)
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

# ndi_discovery_dropin_content -> the systemd drop-in that points a root/ProtectHome NDI receiver at
# NDI_DISCOVERY_SYSTEM_DIR.
ndi_discovery_dropin_content() {
  printf '[Service]\n# issue 1342: libndi reads $NDI_CONFIG_DIR/%s (networks.ips = the OBS-fleet NDI senders, never a cambox: issue 1389).\n# ProtectHome hides /root/.ndi, so the config lives in the system dir.\nEnvironment=NDI_CONFIG_DIR=%s\n' \
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

# ndi_discovery_cambox_apply_remote_snippet IPS -> the on-box bash program (fed to `bash -s` as root
# over ssh, see the CLI below) that rewrites ONLY $NDI_DISCOVERY_SYSTEM_DIR/ndi-config.v1.json on a cambox with
# networks.ips = IPS: issue 1389's smallest safe equivalent of re-running setup-device.sh STEP 7 on a
# live box. It embeds the SAME ndi_discovery_write_config STEP 7 calls (declare -f, never a copy).
# It writes nothing and never remounts when the box's camera-box drop-in does not point
# NDI_CONFIG_DIR at the config dir (camera-box never reads the file, so the box is mDNS-only already;
# read like verify-device (an) reads it), or when the file is already right: with python3 on the box,
# a file that parses as JSON whose ndi.networks.ips equals IPS with no networks.discovery (whatever
# other keys it carries); without python3, only the canonical rendering. A file that is not JSON is
# always rewritten. Otherwise, on a read-only root it remounts rw, writes, syncs and remounts ro again
# (retried 3x; a root it cannot put back is a loud non-zero, and the EXIT trap puts it back on any
# failure). The drop-in and every other file stay untouched.
# Non-zero (and no output) on an EMPTY IPS, on an IPS that names a cambox, or when the cambox set is
# unknown.
ndi_discovery_cambox_apply_remote_snippet() {
  local ips="${1:-}" camboxes listed
  if [ -z "$(_ndi_discovery_norm_list "$ips")" ]; then
    echo "ndi-discovery: refusing an apply program with an EMPTY networks.ips" >&2
    return 1
  fi
  camboxes="$(ndi_discovery_cambox_ips)" || return 1
  listed="$(ndi_discovery_list_common "$ips" "$camboxes")"
  if [ -n "$listed" ]; then
    echo "ndi-discovery: refusing an apply program that lists the cambox IP(s) ${listed} (issue 1389)" >&2
    return 1
  fi
  printf 'set -eu\n'
  printf 'NDI_DISCOVERY_CONFIG_NAME=%q\n' "$NDI_DISCOVERY_CONFIG_NAME"
  printf '_ndi_dir=%q\n_ndi_ips=%q\n_ndi_dropin=%q\n' "$NDI_DISCOVERY_SYSTEM_DIR" "$ips" "$NDI_DISCOVERY_CAMBOX_DROPIN"
  declare -f _ndi_discovery_json_string _ndi_discovery_networks_key ndi_discovery_config_ips \
    ndi_discovery_config_servers _ndi_discovery_norm_list ndi_discovery_dropin_config_dir \
    ndi_discovery_config_json _ndi_discovery_merged_json ndi_discovery_write_config
  cat <<'NDI_APPLY'
_ndi_target="$_ndi_dir/$NDI_DISCOVERY_CONFIG_NAME"
if [ "$(ndi_discovery_dropin_config_dir "$(cat "$_ndi_dropin" 2>/dev/null || true)")" != "$_ndi_dir" ]; then
  echo "ndi-discovery: $_ndi_dropin does not point NDI_CONFIG_DIR at $_ndi_dir -- camera-box never reads $_ndi_target, so this box is mDNS-only already (the safe state); nothing written"
  exit 0
fi
_ndi_have="$(cat "$_ndi_target" 2>/dev/null || true)"
_ndi_same=0
if [ -n "$_ndi_have" ]; then
  if command -v python3 >/dev/null 2>&1 \
    && printf '%s' "$_ndi_have" | python3 -c 'import json, sys; json.load(sys.stdin)' >/dev/null 2>&1; then
    if [ "$(_ndi_discovery_norm_list "$(ndi_discovery_config_ips "$_ndi_have")")" = "$(_ndi_discovery_norm_list "$_ndi_ips")" ] \
      && [ -z "$(_ndi_discovery_norm_list "$(ndi_discovery_config_servers "$_ndi_have")")" ]; then
      _ndi_same=1
    fi
  elif [ "$_ndi_have" = "$(ndi_discovery_config_json "$_ndi_ips")" ]; then
    _ndi_same=1
  fi
fi
if [ "$_ndi_same" = 1 ]; then
  echo "ndi-discovery: $_ndi_target unchanged (networks.ips=$_ndi_ips) -- nothing written"
  exit 0
fi
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
ndi_discovery_write_config "$_ndi_dir" "$_ndi_ips"
sync
if ! _ndi_restore_ro; then
  trap - EXIT
  exit 1
fi
trap - EXIT
echo "OK: $_ndi_target rewritten (networks.ips=$_ndi_ips); restart camera-box.service to load it"
NDI_APPLY
}

# --- CLI (executed, not sourced): print the list / config for a box this repo does not provision ---
# (stream, resolume, an owner laptop -- see the rule). `resolve` (default) resolves the traveling
# hostname senders from THIS machine; `pinned` is the deterministic checked-in form.
#   bash scripts/lib/ndi-discovery.sh --ips  [resolve|pinned]
#   bash scripts/lib/ndi-discovery.sh --json [resolve|pinned]
#   bash scripts/lib/ndi-discovery.sh --cambox-ips            (the FORBIDDEN set, issue 1389)
#   bash scripts/lib/ndi-discovery.sh --cambox-apply [resolve|pinned] > apply.sh
#        (the on-box cambox program, then: ssh root@<cambox> bash -s < apply.sh)
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
  set -euo pipefail
  case "${1:-}" in
    --ips)  _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; printf '%s\n' "$_ndi_ips" ;;
    --json) _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"; ndi_discovery_config_json "$_ndi_ips" ;;
    --cambox-ips) _ndi_cams="$(ndi_discovery_cambox_ips)"; printf '%s\n' "$_ndi_cams" ;;
    --cambox-apply)
      _ndi_ips="$(ndi_discovery_sender_ips "${2:-resolve}")"
      _ndi_prog="$(ndi_discovery_cambox_apply_remote_snippet "$_ndi_ips")"
      printf '%s\n' "$_ndi_prog"
      ;;
    *)
      echo "usage: $0 --ips|--json [resolve|pinned] | --cambox-ips | --cambox-apply [resolve|pinned]" >&2
      exit 2
      ;;
  esac
fi
