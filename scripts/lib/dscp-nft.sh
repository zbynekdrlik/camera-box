#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by verify-device.sh /
# setup-device.sh / create-usb-linux.sh — mirrors every sibling in scripts/lib/ (e.g. log-diet.sh,
# udev-camera-box.sh, log-bound.sh), none of which set -euo pipefail either: sourcing a
# `set -e`-carrying file would silently change the CALLER's shell options too.
#
# scripts/lib/dscp-nft.sh — the "camera-box provisioning half" of dantesync issue 52.
#
# dantesync is the rig's clock authority. Its Linux NTP CLIENT uses rsntp::SntpClient, which
# creates its UDP socket INTERNALLY — the process owns no handle to setsockopt(IP_TOS), so it
# cannot DSCP-mark its own NTP REQUESTS on Linux (see dantesync/.claude/rules/dscp-marking.md's
# coverage table: only the master's NTP-server REPLIES and the Windows pcap client are markable
# in-process). dantesync's code half (src/dscp.rs) EF-marks the master replies; on the cam boxes
# the request half must be marked OUTSIDE the process, at the kernel netfilter layer, by
# provisioning. The venue MikroTik CRS switches honour DSCP in hardware (TRUST-L3), so marking the
# request direction to the SAME class as the replies removes the queue-delay bias at the source.
#
# Mechanism (smallest robust option — justified on dantesync#52): a DEDICATED nftables table
# (`table ip dantesync_dscp`) with one OUTPUT-mangle rule, applied at boot by a tiny systemd
# oneshot (dantesync-dscp.service). It is NOT the distro nftables.service — the cam boxes ship no
# nftables config, and a dedicated table owns nothing but this one rule (never `flush ruleset`), so
# it coexists with any future firewall and is idempotently replaceable. `policy accept` + a
# set-only statement means a wrong/absent rule can never drop a packet or break networking.
#
# Single source of truth: setup-device.sh (STEP 16 pkg + STEP 17c install), create-usb-linux.sh
# (base-image mirror), and verify-device.sh's (ae) acceptance check all consume THESE functions so
# they can never drift — the SAME discipline log-diet.sh / udev-camera-box.sh already apply.
#
# Source-only: this file defines pure functions + shared constants, no side effects on its own.

# The DSCP class to mark NTP-client requests with. EF (Expedited Forwarding, DSCP 46 / 0x2e) —
# matches dantesync src/dscp.rs's own default so request and reply carry the SAME class end to end.
DSCP_NFT_CLASS="ef"
# Dedicated nftables table name (IPv4 family — IPv6 is disabled on the cam boxes; STEP 14 sets
# net.ipv6.conf.all.disable_ipv6=1 — and the NTP master is IPv4, so an `ip` table is the honest
# minimal choice). Never `flush ruleset` — this table coexists with anything else.
DSCP_NFT_TABLE="dantesync_dscp"
# Where the provisioners write the ruleset file and the boot-time oneshot unit.
# /etc/nftables.d/*.nft is the distro's conventional nftables include dir. We apply it via our OWN
# dantesync-dscp oneshot, NOT the distro nftables.service (the boxes ship none), so our table does
# not depend on whether nftables.conf's `include "/etc/nftables.d/*.nft"` line is active. (If a
# future box ever ENABLES nftables.service with that include still COMMENTED -- Ubuntu's shipped
# default -- its boot `flush ruleset` would wipe our table; verify-device.sh's (ae) check would then
# FAIL loud, and the fix is to uncomment the include or keep relying on our oneshot.)
DSCP_NFT_RULESET_PATH="/etc/nftables.d/dantesync-dscp.nft"
DSCP_NFT_SERVICE_NAME="dantesync-dscp"
DSCP_NFT_SERVICE_PATH="/etc/systemd/system/dantesync-dscp.service"

# dscp_nft_ruleset_content -> the full desired content of ${DSCP_NFT_RULESET_PATH}. A dedicated
# table with a single OUTPUT-mangle rule marking udp dport 123 (NTP) with DSCP EF. The leading
# `table`/`delete table` pair is the canonical idempotent single-table replace: `table` ensures it
# exists so `delete` cannot error on a first run, `delete` removes it, then the definition below
# recreates it cleanly — an atomic replace that NEVER touches the rest of the ruleset. `nft -f`
# ignores indentation, so spaces are used here (the kernel renders it back with tabs; the verify
# parser matches on tokens, not whitespace).
dscp_nft_ruleset_content() {
  cat <<'EOF'
#!/usr/sbin/nft -f
# dantesync issue 52 (camera-box provisioning half) — DSCP-mark the Linux NTP CLIENT's outgoing
# requests (udp dport 123) to EF. rsntp creates its socket internally, so dantesync cannot
# setsockopt(IP_TOS) for the request on Linux; this OUTPUT-mangle rule closes that half so the
# venue MikroTik CRS switches (TRUST-L3) prioritise the request direction the same way they
# already prioritise the master's EF-marked replies.
#
# Dedicated table (NEVER `flush ruleset`) so it coexists with any other firewall and is applied
# idempotently: the `table`/`delete table` pair below ensures the table exists then removes it, so
# the definition always recreates it cleanly (atomic single-table replace). `policy accept` + a
# set-only statement — this rule NEVER drops a packet, so a wrong/absent rule cannot break
# networking.
table ip dantesync_dscp
delete table ip dantesync_dscp
table ip dantesync_dscp {
    chain output {
        type filter hook output priority mangle; policy accept;
        udp dport 123 ip dscp set ef
    }
}
EOF
}

# dscp_nft_service_unit_content -> the systemd oneshot unit that applies the ruleset at boot.
# Type=oneshot + RemainAfterExit=yes so `systemctl is-active` reads `active` after a successful
# apply (the verify (ae) check keys on that). Ordered before network-pre.target (the canonical
# firewall-setup slot, mirroring the distro nftables.service) so the rule is in place from the
# first outbound packet, and pulled in at boot by multi-user.target (reboot survival via `enable`).
dscp_nft_service_unit_content() {
  cat <<EOF
[Unit]
Description=DSCP-mark outgoing NTP client packets to EF (dantesync issue 52)
Documentation=https://github.com/zbynekdrlik/dantesync/issues/52
Wants=network-pre.target
Before=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f ${DSCP_NFT_RULESET_PATH}

[Install]
WantedBy=multi-user.target
EOF
}

# dscp_nft_rule_present TEXT -> 0 iff TEXT (a `nft list ruleset` / `nft list table ip
# dantesync_dscp` dump, raw OR `|`-flattened by the gather snippet) shows the dedicated table AND
# its udp-dport-123 -> DSCP-EF mangle rule. The class is matched as the keyword `ef` OR the numeric
# forms (0x2e / 46) so a future nft render variation never false-negatives. A `__NFT_ABSENT__`
# sentinel (nft not installed) is NOT present. grep with no ^/$ anchors, so it is line-boundary
# agnostic (works on the flattened one-line form too).
dscp_nft_rule_present() {
  case "$1" in
    *__NFT_ABSENT__*) return 1 ;;
  esac
  grep -qE 'table (ip|inet) dantesync_dscp' <<<"$1" || return 1
  grep -qE 'udp dport 123 ip dscp set (ef|0x2e|46)' <<<"$1" || return 1
  return 0
}

# dscp_nft_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh's (ae) check,
# emitting one KEY=VALUE line per fact (same convention as log-diet.sh's gather snippet). No side
# effects — read-only. NFT_TABLE is flattened with `tr` so the whole table renders on one KEY line;
# `__NFT_ABSENT__` distinguishes "nftables not installed" from "table missing" (both FAIL, with
# different messages). ssh's own exit stays 0 on a missing table/binary (only a transport failure
# makes the ssh rc non-zero — the (ae) check treats that as genuine unreachability).
dscp_nft_gather_remote_snippet() {
  cat <<'REMOTE'
# Absolute /usr/sbin/nft (matches the oneshot's ExecStart) -- verify runs this over a non-login
# ssh session, whose PATH may omit sbin dirs; a bare `nft` could then false-read __NFT_ABSENT__.
if command -v /usr/sbin/nft >/dev/null 2>&1; then
  echo "NFT_TABLE=$(/usr/sbin/nft list table ip dantesync_dscp 2>/dev/null | tr '\n' '|')"
else
  echo "NFT_TABLE=__NFT_ABSENT__"
fi
echo "DSCP_SVC_ACTIVE=$(systemctl is-active dantesync-dscp 2>/dev/null)"
echo "DSCP_SVC_ENABLED=$(systemctl is-enabled dantesync-dscp 2>/dev/null)"
REMOTE
}

# dscp_nft_verdict STATE_BLOCK -> "ok" or the newline-joined "FAIL: ..." reasons. STATE_BLOCK is
# the KEY=VALUE text produced by dscp_nft_gather_remote_snippet (or a hand-built test fixture).
# Fail-closed on anything unreadable/missing — an absent/unparseable value is never read as "safely
# marked" (test-strictness), mirroring log_diet_provision_verdict's discipline. Three facets:
# nftables installed + the rule live, the oneshot ACTIVE (applied at boot), and ENABLED (survives
# reboot).
dscp_nft_verdict() {
  local block="$1" nft_table active enabled fails="" nl
  nl=$'\n'
  nft_table="$(printf '%s\n' "$block" | sed -n 's/^NFT_TABLE=//p')"
  active="$(printf '%s\n' "$block" | sed -n 's/^DSCP_SVC_ACTIVE=//p')"
  enabled="$(printf '%s\n' "$block" | sed -n 's/^DSCP_SVC_ENABLED=//p')"

  case "$nft_table" in
    *__NFT_ABSENT__*)
      fails="${fails:+$fails$nl}FAIL: nftables (nft) is not installed -- the NTP-client DSCP mangle rule cannot be applied (dantesync issue 52)"
      ;;
    *)
      if ! dscp_nft_rule_present "$nft_table"; then
        fails="${fails:+$fails$nl}FAIL: the ${DSCP_NFT_TABLE} nftables table / udp dport 123 -> dscp ${DSCP_NFT_CLASS} rule is absent (dantesync issue 52)"
      fi
      ;;
  esac

  if [ "$(printf '%s' "$active" | tr -d '[:space:]')" != "active" ]; then
    fails="${fails:+$fails$nl}FAIL: ${DSCP_NFT_SERVICE_NAME}.service is not active (state=${active:-<none>}) -- the DSCP rule oneshot did not apply at boot (dantesync issue 52)"
  fi

  if [ "$(printf '%s' "$enabled" | tr -d '[:space:]')" != "enabled" ]; then
    fails="${fails:+$fails$nl}FAIL: ${DSCP_NFT_SERVICE_NAME}.service (${DSCP_NFT_SERVICE_PATH}) is not enabled (state=${enabled:-<none>}) -- the DSCP rule will not survive a reboot (dantesync issue 52)"
  fi

  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    printf 'ok\n'
  fi
}
