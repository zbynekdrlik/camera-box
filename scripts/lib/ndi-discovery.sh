#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by setup-device.sh /
# verify-device.sh / setup-strih.sh / verify-strih.sh + tests/python/test_ndi_discovery_1342.py --
# mirrors every sibling in scripts/lib/ (remote-logging.sh, dscp-nft.sh, ndi-runtime.sh), none of
# which set -euo pipefail: sourcing a `set -e`-carrying file would silently change the CALLER's
# shell options too. Each caller sets its own strict mode.
#
# scripts/lib/ndi-discovery.sh -- issue 1342: the ONE source of truth for the fleet's NDI
# Discovery Server client config (`ndi-config.v1.json`).
#
# WHY: NDI source discovery on this rig was pure mDNS (`_ndi._tcp` multicast via avahi). On the
# venue MikroTik LAN that is unreliable: the strih OBS missed the RESOLUME-SNV sources, a freshly
# connected laptop did not list every source (owner, 18.9.2026). The NDI SDK's own answer is the
# NDI Discovery Server -- one unicast registry every sender registers with and every finder queries.
# The server runs on dev1 (`systemd/ndi-discovery-server.service`, a --user unit that ships
# DISABLED); every managed box gets the client config below, and a foreign laptop needs the same
# one file (`scripts/ndi-discovery/ndi-config.v1.json`, `scripts/ndi-discovery-laptop.ps1`).
#
# The SDK config key is `ndi.networks.discovery` (a comma-delimited server list, default port
# 5959). `ndi.networks.ips` (a static list of extra machines to query) is written EMPTY on purpose:
# a hand-kept list is exactly what went stale on the Windows strih (dead 10.77.8.5x addresses).
#
# SENDER CAVEAT (NDI docs, quoted in .claude/rules/ndi-discovery.md): a RECEIVER with a discovery
# server configured merges the server's list with mDNS, but a SENDER with one configured STOPS
# announcing over mDNS. Configured senders are then visible only to receivers that are configured
# too, so the supervisor rollout order is receivers first, senders (the camboxes) last.
#
# Config LOCATION: the SDK reads `$HOME/.ndi/ndi-config.v1.json` on Linux, or
# `$NDI_CONFIG_DIR/ndi-config.v1.json` when that env var is set. `camera-box.service` runs as root
# with `ProtectHome=yes` (and no User=, so no guaranteed $HOME), so a /root/.ndi file would be
# invisible to it -- the cambox config therefore lives in the system dir /etc/ndi and a
# camera-box.service.d drop-in points NDI_CONFIG_DIR at it. The same drop-in shape serves any other
# root/ProtectHome NDI service (strih-lx's intercom-hub).
#
# Source-only: defines constants + functions, no side effects on its own.

# --- shared constants (single source of truth, consumed cross-file) ---------------------------
# The discovery server list. dev1's rig-LAN IP (machine-identities: dev1 = 10.77.9.200 on the
# venue /23, the same address scripts/lib/remote-logging.sh uses for the log sink). A comma list
# (NDI's redundancy form, e.g. "10.77.9.200,10.77.9.202") is accepted everywhere below.
NDI_DISCOVERY_SERVERS="${NDI_DISCOVERY_SERVERS:-10.77.9.200}"
# The SDK's own config file name.
NDI_DISCOVERY_CONFIG_NAME="ndi-config.v1.json"
# System config dir for root / ProtectHome services (pointed at by NDI_CONFIG_DIR).
NDI_DISCOVERY_SYSTEM_DIR="${NDI_DISCOVERY_SYSTEM_DIR:-/etc/ndi}"
# The camera-box.service drop-in that points the appliance's libndi at NDI_DISCOVERY_SYSTEM_DIR.
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + verify-device.sh (an)
NDI_DISCOVERY_CAMBOX_DROPIN="/etc/systemd/system/camera-box.service.d/ndi-discovery.conf"

# ndi_discovery_config_json [SERVERS] -> the canonical ndi-config.v1.json text (2-space indent,
# one trailing newline, no BOM). SERVERS defaults to NDI_DISCOVERY_SERVERS. The checked-in laptop
# file scripts/ndi-discovery/ndi-config.v1.json is pinned byte-identical to this output.
# shellcheck disable=SC2120  # SERVERS is optional (a redundant list); in-file callers use the default
ndi_discovery_config_json() {
  local servers="${1:-$NDI_DISCOVERY_SERVERS}"
  printf '{\n  "ndi": {\n    "networks": {\n      "ips": "",\n      "discovery": "%s"\n    }\n  }\n}\n' "$servers"
}

# _ndi_discovery_json_string KEY TEXT -> the string value of the LAST `"KEY": "<value>"` pair in
# TEXT, "" if absent. Pure grep/sed (no python on a cambox); `|| true` so a no-match never aborts a
# caller under `set -euo pipefail` (the #458 footgun -- grep exits 1 on zero matches).
_ndi_discovery_json_string() {
  printf '%s\n' "$2" | grep -oE "\"$1\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" | tail -1 \
    | sed -E 's/.*:[[:space:]]*"([^"]*)"$/\1/' || true
}

# ndi_discovery_config_servers TEXT -> the `networks.discovery` value in TEXT, "" if absent.
ndi_discovery_config_servers() { _ndi_discovery_json_string discovery "$1"; }

# ndi_discovery_config_ips TEXT -> the `networks.ips` value in TEXT, "" if absent.
ndi_discovery_config_ips() { _ndi_discovery_json_string ips "$1"; }

# _ndi_discovery_norm_list LIST -> LIST with every space removed ("a, b" == "a,b").
_ndi_discovery_norm_list() { printf '%s' "$1" | tr -d '[:space:]'; }

# ndi_discovery_config_verdict TEXT [EXPECTED_SERVERS] -> "ok", or one `FAIL: <facet>` line per
# failing facet. Always exits 0 (the caller branches on the printed verdict). Facets:
#   missing   -- TEXT empty (no config file on the box)
#   JSON      -- TEXT is not valid JSON (checked only when python3 is available; verify-device runs
#                this on dev1, verify-strih on strih-lx -- both have it)
#   discovery -- networks.discovery != EXPECTED (spaces ignored)
#   ips       -- networks.ips is non-empty (a hand-kept static source list; they go stale)
ndi_discovery_config_verdict() {
  local text="$1" want="${2:-$NDI_DISCOVERY_SERVERS}" got ips out=""
  if [ -z "$text" ]; then
    printf 'FAIL: config missing (no %s)\n' "$NDI_DISCOVERY_CONFIG_NAME"
    return 0
  fi
  if command -v python3 >/dev/null 2>&1 \
    && ! printf '%s' "$text" | python3 -c 'import json,sys; json.load(sys.stdin)' >/dev/null 2>&1; then
    out="${out}FAIL: not valid JSON (the NDI SDK ignores an unparseable config)"$'\n'
  fi
  got="$(ndi_discovery_config_servers "$text")"
  if [ "$(_ndi_discovery_norm_list "$got")" != "$(_ndi_discovery_norm_list "$want")" ]; then
    out="${out}FAIL: networks.discovery='${got}' (want '${want}')"$'\n'
  fi
  ips="$(ndi_discovery_config_ips "$text")"
  if [ -n "$(_ndi_discovery_norm_list "$ips")" ]; then
    out="${out}FAIL: networks.ips='${ips}' is a static source list (must be empty -- a hand-kept list goes stale)"$'\n'
  fi
  if [ -z "$out" ]; then
    printf 'ok\n'
  else
    printf '%s' "$out"
  fi
}

# ndi_discovery_dropin_content -> the systemd drop-in that points a root/ProtectHome NDI service
# at NDI_DISCOVERY_SYSTEM_DIR.
ndi_discovery_dropin_content() {
  printf '[Service]\n# issue 1342: libndi reads $NDI_CONFIG_DIR/%s (the NDI Discovery Server client config).\n# ProtectHome hides /root/.ndi, so the config lives in the system dir.\nEnvironment=NDI_CONFIG_DIR=%s\n' \
    "$NDI_DISCOVERY_CONFIG_NAME" "$NDI_DISCOVERY_SYSTEM_DIR"
}

# ndi_discovery_dropin_config_dir TEXT -> the NDI_CONFIG_DIR value in a drop-in TEXT, "" if absent.
ndi_discovery_dropin_config_dir() {
  printf '%s\n' "$1" | grep -oE '^Environment=NDI_CONFIG_DIR=[^[:space:]]+' | tail -1 | cut -d= -f3- || true
}

# ndi_discovery_write_config DIR [OWNER] -> write DIR/ndi-config.v1.json (the canonical config),
# mode 0644, via a temp file + atomic rename so a reader never sees a half-written file. With OWNER,
# DIR and the file are chowned to OWNER (a desktop user's ~/.ndi). Idempotent. Returns non-zero
# (with a message on stderr) when DIR cannot be created or written -- callers run it under their
# own `set -e` or wrap it in `|| fail`.
ndi_discovery_write_config() {
  local dir="${1:?ndi_discovery_write_config: DIR required}" owner="${2:-}" tmp
  mkdir -p "$dir" || { echo "ndi-discovery: cannot create $dir" >&2; return 1; }
  tmp="$(mktemp "$dir/.${NDI_DISCOVERY_CONFIG_NAME}.XXXXXX")" \
    || { echo "ndi-discovery: cannot write into $dir" >&2; return 1; }
  if ! ndi_discovery_config_json > "$tmp"; then
    rm -f "$tmp"
    echo "ndi-discovery: cannot write $tmp" >&2
    return 1
  fi
  chmod 0644 "$tmp"
  if [ -n "$owner" ]; then
    chown "$owner":"$owner" "$dir" "$tmp" || { rm -f "$tmp"; echo "ndi-discovery: chown $owner failed" >&2; return 1; }
  fi
  mv -f "$tmp" "$dir/$NDI_DISCOVERY_CONFIG_NAME" || { rm -f "$tmp"; echo "ndi-discovery: rename into $dir failed" >&2; return 1; }
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
