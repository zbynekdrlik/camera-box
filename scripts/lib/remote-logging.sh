#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library, sourced by setup-device.sh /
# verify-device.sh / create-usb-linux.sh + the Rust harness (tests/harness_remote_logging_1311.rs)
# — mirrors every sibling in scripts/lib/ (dscp-nft.sh, mgmt-liveness.sh, log-diet.sh,
# udev-camera-box.sh), none of which set -euo pipefail either: sourcing a `set -e`-carrying file
# would silently change the CALLER's shell options too.
#
# scripts/lib/remote-logging.sh — #1311 (Finding 1 step 2): get kernel + journal messages OFF the
# cambox in REAL TIME, before the root fs is needed.
#
# WHY: on 2026-09-13/14 two cam boxes went half-dead (#1309 / Finding 1) — the root fs USB stick
# dropped off the bus while RAM-resident daemons kept running. `/var/log` is a 50 MB tmpfs and
# `rsyslog` is PURGED (#762), so the ONLY durable log is the #1309 on-STICK persistent-journal
# partition — which lives on the very stick that drops, so every incident's kernel/journal evidence
# is lost at exactly the moment it matters. The next death must be diagnosable: messages have to
# leave the box in real time.
#
# TWO complementary transports, each covering what the other cannot:
#   1. netconsole — the in-tree kernel module ships kernel `printk` straight over UDP from kernel
#      memory + the NIC driver. It touches no filesystem and needs no userspace fork, so it keeps
#      emitting through the exact half-dead state (fs gone, exec dead) that kills everything else —
#      the ONLY transport that survives the death instant. Configured via the DYNAMIC configfs
#      target by a boot oneshot that resolves dev1's next-hop MAC at boot (ping -> `ip neigh`), so a
#      dev1 NIC/MAC change is re-resolved every boot (robust vs a hard-coded `netconsole=` cmdline).
#      Carries only kernel printk (no service logs) and is fire-and-forget UDP — acceptable because
#      transport 2 covers the rich run-up.
#   2. systemd-journal-upload — part of systemd (one apt package, systemd-journal-remote), buffers
#      in memory and uploads the FULL structured journal (kernel + every service unit) incrementally
#      to a dev1 systemd-journal-remote HTTP sink, so the last successfully-uploaded entries survive
#      the death and give the minutes-before-the-drop context netconsole cannot. It never has to
#      survive the death instant (that is netconsole's job). Works from a ro root by redirecting its
#      cursor `--save-state` to a /run (tmpfs) path (a persistent cursor on ro root would fail; a
#      runtime cursor is fine — the box dies anyway). NO rsyslog reinstall — respects #762.
#
# The DECISION (verdict) + the dev1-MAC parser are PURE bash here (exhaustively unit-testable at
# Tier-0, #557 kills local cargo). The generated on-box netconsole script EMBEDS the pure MAC parser
# via `declare -f`, so there is ONE source of truth — no drifting inlined copy, and no repo-lib
# dependency on the appliance (the same self-contained-generated-script pattern as mgmt-liveness.sh).
#
# Single source of truth: setup-device.sh (STEP 16 pkg + a [remote-logging] install sub-step),
# create-usb-linux.sh (base-image mirror), and verify-device.sh's (ak) acceptance check all consume
# THESE functions so they can never drift — the SAME discipline dscp-nft.sh / mgmt-liveness.sh apply.
#
# Source-only: defines pure functions + shared constants, no side effects on its own.

# --- shared constants (single source of truth, consumed cross-file) ---------------------------
# dev1 (10.77.9.200) is the fleet log sink (machine-identities.md — the primary workstation +
# deploy target on the venue LAN, reachable at L2 from every cam box so netconsole's UDP-to-next-hop
# reaches it directly).
REMOTE_LOG_DEV1_IP="${REMOTE_LOG_DEV1_IP:-10.77.9.200}"
# netconsole UDP target port on dev1 (rsyslog imudp), and the journal-upload HTTP sink port
# (systemd-journal-remote --listen-http). Both dev1-side receivers are installed by the SUPERVISOR
# via scripts/dev1-remote-log-install.sh — this cambox side only sends.
REMOTE_LOG_NETCONSOLE_PORT="${REMOTE_LOG_NETCONSOLE_PORT:-514}"
REMOTE_LOG_JOURNAL_PORT="${REMOTE_LOG_JOURNAL_PORT:-19532}"
REMOTE_LOG_JOURNAL_URL="http://${REMOTE_LOG_DEV1_IP}:${REMOTE_LOG_JOURNAL_PORT}"

# netconsole (kernel path)
REMOTE_LOG_NC_TARGET="cambox"                                     # configfs dynamic target dir name
REMOTE_LOG_NC_CONFIGFS="/sys/kernel/config/netconsole/${REMOTE_LOG_NC_TARGET}"
REMOTE_LOG_NC_SCRIPT_PATH="/usr/local/sbin/cambox-netconsole-setup.sh"
REMOTE_LOG_NC_SERVICE_NAME="cambox-netconsole"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + create-usb-linux.sh (base image)
REMOTE_LOG_NC_SERVICE_PATH="/etc/systemd/system/cambox-netconsole.service"
# How long the boot oneshot keeps re-resolving dev1's MAC before giving up (ping + neigh read).
REMOTE_LOG_NC_MAC_RETRIES="${REMOTE_LOG_NC_MAC_RETRIES:-30}"
REMOTE_LOG_NC_MAC_RETRY_SLEEP_S="${REMOTE_LOG_NC_MAC_RETRY_SLEEP_S:-2}"

# systemd-journal-upload (rich journal path)
REMOTE_LOG_JU_SERVICE_NAME="systemd-journal-upload"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + create-usb-linux.sh (base image)
REMOTE_LOG_JU_CONF_PATH="/etc/systemd/journal-upload.conf"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + create-usb-linux.sh (base image)
REMOTE_LOG_JU_DROPIN_PATH="/etc/systemd/system/systemd-journal-upload.service.d/10-cambox-rostate.conf"
# ro-root-safe cursor location (tmpfs). The default /var/lib/... is on the read-only root.
REMOTE_LOG_JU_SAVE_STATE="/run/systemd/journal-upload/state"
REMOTE_LOG_JU_BIN="/usr/lib/systemd/systemd-journal-upload"
# The apt package that ships systemd-journal-upload on Ubuntu noble.
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (STEP 16 pkg) + create-usb-linux.sh (chroot apt)
REMOTE_LOG_JU_PKG="systemd-journal-remote"

# remote_log_mac_from_neigh NEIGH_TEXT -> the resolved lladdr MAC (lowercase colon form) iff the
# neighbour entry is genuinely resolved, else EMPTY. `ip neigh show <ip>` yields a line like
# `10.77.9.200 dev enp2s0 lladdr aa:bb:cc:dd:ee:ff REACHABLE`; an UNRESOLVED entry is
# `... FAILED` / `... INCOMPLETE` (no usable lladdr, or a stale one being probed). A pure, exhaustively
# unit-testable parser: netconsole needs a CORRECT next-hop MAC, so a FAILED/INCOMPLETE state must
# read as "not yet resolved" (empty) — the boot oneshot then keeps retrying rather than writing a
# bogus MAC. Returns 0 always (empty stdout == unresolved); never errors under `set -e`.
remote_log_mac_from_neigh() {
  local text="$1" mac
  case " $text " in
    *" FAILED "*|*" INCOMPLETE "*) printf '%s' ""; return 0 ;;
  esac
  mac="$(printf '%s\n' "$text" | grep -oiE 'lladdr[[:space:]]+([0-9a-f]{2}:){5}[0-9a-f]{2}' | head -1 | awk '{print $2}' | tr 'A-Z' 'a-z')"
  printf '%s' "$mac"
}

# remote_log_netconsole_setup_script_content -> the full on-box setup script written to
# ${REMOTE_LOG_NC_SCRIPT_PATH} and run by the boot oneshot. It EMBEDS remote_log_mac_from_neigh via
# `declare -f` (ONE source of truth). `set -uo pipefail` (never `set -e` — the many `|| true` are
# deliberate best-effort steps against a configfs that rejects a write while the target is enabled).
# It: ensures configfs + the netconsole module are loaded; resolves the egress dev + local ip toward
# dev1; resolves dev1's next-hop MAC with a bounded ping/neigh retry; then (idempotently) creates the
# dynamic configfs target and enables it. Fire-and-forget after that — the kernel emits printk over
# UDP with zero fs/userspace dependency, so it survives the #1309 half-dead wedge.
remote_log_netconsole_setup_script_content() {
  cat <<SCRIPT_HEAD
#!/usr/bin/env bash
# cambox-netconsole-setup.sh (#1311) — GENERATED by scripts/lib/remote-logging.sh. Do not hand-edit;
# re-provision (setup-device.sh) to change it. Brings up the netconsole DYNAMIC configfs target so
# the kernel ships printk to dev1 in real time, surviving the #1309 half-dead-stick wedge.
set -uo pipefail

DEV1_IP="${REMOTE_LOG_DEV1_IP}"
PORT="${REMOTE_LOG_NETCONSOLE_PORT}"
CFG="${REMOTE_LOG_NC_CONFIGFS}"
RETRIES="${REMOTE_LOG_NC_MAC_RETRIES}"
RETRY_SLEEP="${REMOTE_LOG_NC_MAC_RETRY_SLEEP_S}"

SCRIPT_HEAD
  declare -f remote_log_mac_from_neigh
  cat <<'SCRIPT_BODY'

# Ensure configfs is mounted (systemd's sys-kernel-config.mount usually does this) and the
# netconsole module is present. All best-effort: a built-in configfs/netconsole makes these no-ops.
modprobe configfs 2>/dev/null || true
mountpoint -q /sys/kernel/config 2>/dev/null || mount -t configfs none /sys/kernel/config 2>/dev/null || true
modprobe netconsole 2>/dev/null || true

# Egress interface + local ip toward dev1 (resolved live — no hard-coded NIC name).
DEV="$(ip -o route get "$DEV1_IP" 2>/dev/null | grep -oE 'dev [^ ]+' | awk '{print $2}' | head -1)"
LOCAL_IP="$(ip -o route get "$DEV1_IP" 2>/dev/null | grep -oE 'src [^ ]+' | awk '{print $2}' | head -1)"
if [ -z "$DEV" ]; then
  echo "cambox-netconsole: no egress route to dev1 ($DEV1_IP) — netconsole not armed" >&2
  exit 1
fi

# Resolve dev1's next-hop MAC (netconsole needs the L2 dest). Ping to populate the neigh cache, then
# read it; retry until resolved or the bound is hit. dev1 is always up on the venue LAN.
MAC=""
i=0
while [ "$i" -lt "$RETRIES" ]; do
  ping -c1 -W1 "$DEV1_IP" >/dev/null 2>&1 || true
  MAC="$(remote_log_mac_from_neigh "$(ip neigh show "$DEV1_IP" 2>/dev/null)")"
  [ -n "$MAC" ] && break
  i=$((i + 1))
  sleep "$RETRY_SLEEP"
done
if [ -z "$MAC" ]; then
  echo "cambox-netconsole: could not resolve dev1 ($DEV1_IP) MAC after ${RETRIES} tries — netconsole not armed" >&2
  exit 1
fi

# (Re)configure the dynamic target idempotently. configfs rejects param writes while a target is
# ENABLED, so disable first (harmless if it did not exist / was already disabled).
[ -d "$CFG" ] && { echo 0 > "$CFG/enabled" 2>/dev/null || true; }
mkdir -p "$CFG"
echo 0 > "$CFG/enabled" 2>/dev/null || true
echo "$DEV" > "$CFG/dev_name"
[ -n "$LOCAL_IP" ] && { echo "$LOCAL_IP" > "$CFG/local_ip" 2>/dev/null || true; }
echo "$DEV1_IP" > "$CFG/remote_ip"
echo "$PORT" > "$CFG/remote_port"
echo "$MAC" > "$CFG/remote_mac"
echo 1 > "$CFG/enabled"
echo "cambox-netconsole: armed -> ${DEV1_IP}:${PORT} via ${DEV} (dev1 mac ${MAC})"
SCRIPT_BODY
}

# remote_log_netconsole_service_unit_content -> the boot oneshot that runs the setup script. After
# network-online (netconsole needs the NIC up + dev1 ARP-resolvable). Type=oneshot + RemainAfterExit
# so `systemctl is-active` reads `active` after a successful arm (the verify (ak) check keys on that).
# Pulled in at boot by multi-user.target (reboot survival via `enable`).
remote_log_netconsole_service_unit_content() {
  cat <<EOF
[Unit]
Description=netconsole: ship kernel printk to dev1 in real time (camera-box #1311)
Documentation=https://github.com/zbynekdrlik/camera-box/issues/1311
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=${REMOTE_LOG_NC_SCRIPT_PATH}

[Install]
WantedBy=multi-user.target
EOF
}

# remote_log_journal_upload_conf_content -> /etc/systemd/journal-upload.conf. Points the uploader at
# the dev1 systemd-journal-remote HTTP sink. Plain HTTP (not HTTPS) — the venue LAN is private and
# TLS on a headless appliance is not worth the cert lifecycle; the receiver runs --listen-http.
remote_log_journal_upload_conf_content() {
  cat <<EOF
[Upload]
URL=${REMOTE_LOG_JOURNAL_URL}
EOF
}

# remote_log_journal_upload_dropin_content -> a systemd-journal-upload.service drop-in that redirects
# the cursor state to a /run (tmpfs) path so the uploader works from a READ-ONLY root (the stock
# --save-state=/var/lib/... is unwritable on the ro appliance). RuntimeDirectory creates the tmpfs
# dir with the service user's ownership. A runtime-only cursor is fine — the box dies anyway, and on
# a clean reboot the uploader simply resumes from the current boot.
remote_log_journal_upload_dropin_content() {
  cat <<EOF
[Service]
# ro-root appliance (#1311): the default --save-state=/var/lib/systemd/journal-upload/state is on the
# read-only root. Point the cursor at a tmpfs /run path so the uploader can persist its cursor.
RuntimeDirectory=systemd/journal-upload
ExecStart=
ExecStart=${REMOTE_LOG_JU_BIN} --save-state=${REMOTE_LOG_JU_SAVE_STATE}
EOF
}

# remote_log_gather_remote_snippet -> the REMOTE bash run over ssh by verify-device.sh's (ak) check,
# emitting one KEY=VALUE line per fact (same convention as dscp-nft.sh / mgmt-liveness.sh). Read-only,
# no side effects. `__NO_TARGET__` distinguishes "configfs target absent" from a present-but-wrong
# one. ssh's own exit stays 0 on any missing unit/file (only a transport failure makes ssh rc
# non-zero — the (ak) check treats that as genuine unreachability).
remote_log_gather_remote_snippet() {
  cat <<'REMOTE'
echo "NC_SVC_ENABLED=$(systemctl is-enabled cambox-netconsole 2>/dev/null)"
echo "NC_SVC_ACTIVE=$(systemctl is-active cambox-netconsole 2>/dev/null)"
echo "NC_SCRIPT_X=$(test -x /usr/local/sbin/cambox-netconsole-setup.sh && echo yes || echo no)"
if [ -d /sys/kernel/config/netconsole/cambox ]; then
  echo "NC_ENABLED=$(cat /sys/kernel/config/netconsole/cambox/enabled 2>/dev/null)"
  echo "NC_REMOTE_IP=$(cat /sys/kernel/config/netconsole/cambox/remote_ip 2>/dev/null)"
  echo "NC_REMOTE_PORT=$(cat /sys/kernel/config/netconsole/cambox/remote_port 2>/dev/null)"
else
  echo "NC_ENABLED=__NO_TARGET__"
fi
echo "JU_SVC_ENABLED=$(systemctl is-enabled systemd-journal-upload 2>/dev/null)"
echo "JU_URL=$(grep -E '^URL=' /etc/systemd/journal-upload.conf 2>/dev/null | head -1 | sed 's/^URL=//')"
echo "JU_STATE_SAVE=$(grep -hoE '[-][-]save-state=[^ ]+' /etc/systemd/system/systemd-journal-upload.service.d/*.conf 2>/dev/null | head -1)"
REMOTE
}

# remote_log_verdict STATE_BLOCK -> "ok" or the newline-joined "FAIL: ..." reasons. STATE_BLOCK is
# the KEY=VALUE text from remote_log_gather_remote_snippet (or a test fixture). Fail-closed: an
# absent/unparseable value is NEVER read as "safely logging" (test-strictness), mirroring
# dscp_nft_verdict / log_diet_provision_verdict.
#
# netconsole facets (the kernel path — does NOT depend on the dev1 receiver, so all are gated):
#   * cambox-netconsole.service enabled (reboot survival) AND active (RemainAfterExit oneshot armed)
#   * the setup script is present + executable
#   * the dynamic configfs target is live: enabled==1, remote_ip==dev1, remote_port==514
# journal-upload facets (the rich path):
#   * systemd-journal-upload.service enabled (reboot survival)
#   * the upload URL points at the dev1 sink
#   * the cursor --save-state is redirected to /run (ro-root safe)
# NOTE: journal-upload ACTIVE is deliberately NOT gated — the uploader's active state depends on the
# dev1 receiver being up, which is a SEPARATE supervisor step; a cambox is correctly provisioned even
# before the dev1 sink exists (netconsole is fire-and-forget and needs no listener either). enabled +
# correct config is the cambox-side acceptance bar.
remote_log_verdict() {
  local block="$1" fails="" nl
  nl=$'\n'
  local nc_enabled_svc nc_active nc_script_x nc_target nc_ip nc_port
  local ju_enabled ju_url ju_state
  nc_enabled_svc="$(printf '%s\n' "$block" | sed -n 's/^NC_SVC_ENABLED=//p' | tr -d '[:space:]')"
  nc_active="$(printf '%s\n' "$block" | sed -n 's/^NC_SVC_ACTIVE=//p' | tr -d '[:space:]')"
  nc_script_x="$(printf '%s\n' "$block" | sed -n 's/^NC_SCRIPT_X=//p' | tr -d '[:space:]')"
  nc_target="$(printf '%s\n' "$block" | sed -n 's/^NC_ENABLED=//p' | tr -d '[:space:]')"
  nc_ip="$(printf '%s\n' "$block" | sed -n 's/^NC_REMOTE_IP=//p' | tr -d '[:space:]')"
  nc_port="$(printf '%s\n' "$block" | sed -n 's/^NC_REMOTE_PORT=//p' | tr -d '[:space:]')"
  ju_enabled="$(printf '%s\n' "$block" | sed -n 's/^JU_SVC_ENABLED=//p' | tr -d '[:space:]')"
  ju_url="$(printf '%s\n' "$block" | sed -n 's/^JU_URL=//p' | tr -d '[:space:]')"
  ju_state="$(printf '%s\n' "$block" | sed -n 's/^JU_STATE_SAVE=//p' | tr -d '[:space:]')"

  [ "$nc_enabled_svc" = "enabled" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service is not enabled (state=${nc_enabled_svc:-<none>}) -- netconsole will not survive a reboot (#1311)"
  [ "$nc_active" = "active" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service is not active (state=${nc_active:-<none>}) -- the netconsole boot oneshot did not arm (#1311)"
  [ "$nc_script_x" = "yes" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SCRIPT_PATH} is missing/not executable -- re-provision with the current setup-device.sh (#1311)"
  if [ "$nc_target" = "__NO_TARGET__" ]; then
    fails="${fails:+$fails$nl}FAIL: no netconsole configfs target at ${REMOTE_LOG_NC_CONFIGFS} -- kernel printk is not leaving the box (#1311)"
  else
    [ "$nc_target" = "1" ] || fails="${fails:+$fails$nl}FAIL: the netconsole target is not enabled (enabled=${nc_target:-<none>}) (#1311)"
    [ "$nc_ip" = "$REMOTE_LOG_DEV1_IP" ] || fails="${fails:+$fails$nl}FAIL: the netconsole target remote_ip=${nc_ip:-<none>} != dev1 ${REMOTE_LOG_DEV1_IP} (#1311)"
    [ "$nc_port" = "$REMOTE_LOG_NETCONSOLE_PORT" ] || fails="${fails:+$fails$nl}FAIL: the netconsole target remote_port=${nc_port:-<none>} != ${REMOTE_LOG_NETCONSOLE_PORT} (#1311)"
  fi

  [ "$ju_enabled" = "enabled" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_JU_SERVICE_NAME}.service is not enabled (state=${ju_enabled:-<none>}) -- the journal uploader will not survive a reboot (#1311)"
  [ "$ju_url" = "$REMOTE_LOG_JOURNAL_URL" ] || fails="${fails:+$fails$nl}FAIL: journal-upload URL=${ju_url:-<none>} != ${REMOTE_LOG_JOURNAL_URL} (#1311)"
  case "$ju_state" in
    *--save-state=/run/*) : ;;
    *) fails="${fails:+$fails$nl}FAIL: journal-upload --save-state is not redirected to /run (got '${ju_state:-<none>}') -- the cursor would be unwritable on the ro root (#1311)" ;;
  esac

  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    printf 'ok\n'
  fi
}
