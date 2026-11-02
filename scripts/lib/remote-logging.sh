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
#      target by a boot oneshot that resolves the sink's next-hop MAC at boot (ping -> `ip neigh`), so
#      a sink NIC/MAC change is re-resolved every boot (robust vs a hard-coded `netconsole=` cmdline).
#      Carries only kernel printk (no service logs) and is fire-and-forget UDP — acceptable because
#      transport 2 covers the rich run-up.
#   2. systemd-journal-upload — part of systemd (one apt package, systemd-journal-remote), buffers
#      in memory and uploads the FULL structured journal (kernel + every service unit) incrementally
#      to the venue strih's systemd-journal-remote HTTP sink, so the last successfully-uploaded entries survive
#      the death and give the minutes-before-the-drop context netconsole cannot. It never has to
#      survive the death instant (that is netconsole's job). Works from a ro root by redirecting its
#      cursor `--save-state` to a /run (tmpfs) path (a persistent cursor on ro root would fail; a
#      runtime cursor is fine — the box dies anyway). NO rsyslog reinstall — respects #762.
#
# The DECISION (verdict) + the sink-MAC parser are PURE bash here (exhaustively unit-testable at
# Tier-0, #557 kills local cargo). The generated on-box netconsole script EMBEDS the pure MAC parser
# via `declare -f`, so there is ONE source of truth — no drifting inlined copy, and no repo-lib
# dependency on the appliance (the same self-contained-generated-script pattern as mgmt-liveness.sh).
#
# Single source of truth: setup-device.sh (STEP 16 pkg + a [remote-logging] install sub-step),
# verify-device.sh's (ak) acceptance check, scripts/lib/cambox-ro-units.sh (the live re-point of an
# already-provisioned box) and the strih-side receiver (scripts/lib/strih-cambox-log-sink.sh, the
# ports) all consume THESE functions so they can never drift — the SAME discipline dscp-nft.sh /
# mgmt-liveness.sh apply. create-usb-linux.sh bakes none of it (issue 1394): its image has no box
# identity, so it cannot know the box's venue sink.
#
# Source-only: defines pure functions + shared constants, no side effects on its own.

# --- shared constants (single source of truth, consumed cross-file) ---------------------------
# The SINK (issue 1394, owner ROZHODNUTE 9.10.2026): the cambox's OWN venue strih box -- "strih nb,
# which has the performance and the disk"; no rig service and no rig data on dev1. It is never a
# literal here: remote_log_sink_ip CAMERA derives it from the venue fact files
# (scripts/strih-boxes/*.env, the STRIH_IP of the strih-role box whose STRIH_CAMERAS lists the camera),
# and every generator / the verdict takes it as an argument. A cambox reaches its venue strih at L2
# (the same /23), which netconsole's UDP-to-next-hop needs. The receivers are provisioned on the strih
# box by setup-strih step 16k (scripts/lib/strih-cambox-log-sink.sh) and graded by verify-strih item 46.
# The directory this lib lives in (the fact loader scripts/lib/strih-box-facts.sh sits next to it).
REMOTE_LOG_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# netconsole UDP target port on the sink (rsyslog imudp), and the journal-upload HTTP sink port
# (systemd-journal-remote --listen-http). The receiver lib reads these two, so both sides agree.
REMOTE_LOG_NETCONSOLE_PORT="${REMOTE_LOG_NETCONSOLE_PORT:-514}"
REMOTE_LOG_JOURNAL_PORT="${REMOTE_LOG_JOURNAL_PORT:-19532}"

# netconsole (kernel path)
REMOTE_LOG_NC_TARGET="cambox"                                     # configfs dynamic target dir name
# Overridable so a Tier-0 test can point the arm at a temp dir (production never sets it — #1311).
REMOTE_LOG_NC_CONFIGFS="${REMOTE_LOG_NC_CONFIGFS:-/sys/kernel/config/netconsole/${REMOTE_LOG_NC_TARGET}}"
REMOTE_LOG_NC_SCRIPT_PATH="/usr/local/sbin/cambox-netconsole-setup.sh"
REMOTE_LOG_NC_SERVICE_NAME="cambox-netconsole"
# shellcheck disable=SC2034  # consumed cross-file by setup-device.sh (install) + create-usb-linux.sh (base image)
REMOTE_LOG_NC_SERVICE_PATH="/etc/systemd/system/cambox-netconsole.service"
# How long the boot oneshot keeps re-resolving the sink's MAC before giving up (ping + neigh read).
REMOTE_LOG_NC_MAC_RETRIES="${REMOTE_LOG_NC_MAC_RETRIES:-30}"
REMOTE_LOG_NC_MAC_RETRY_SLEEP_S="${REMOTE_LOG_NC_MAC_RETRY_SLEEP_S:-2}"
# How long the boot oneshot waits for the egress ROUTE to the sink to appear before giving up. A cambox
# MASKS systemd-networkd-wait-online (setup-device.sh STEP 11 / create-usb-linux.sh — the #547
# boot-stall fix), so network-online.target is reached INSTANTLY, before systemd-networkd applies
# the static IP/route. This retry is what makes the oneshot survive a clean boot where the route
# comes up late (#1311). Generous enough for a static-IP netplan apply (~60s at the defaults).
REMOTE_LOG_NC_ROUTE_RETRIES="${REMOTE_LOG_NC_ROUTE_RETRIES:-30}"
REMOTE_LOG_NC_ROUTE_RETRY_SLEEP_S="${REMOTE_LOG_NC_ROUTE_RETRY_SLEEP_S:-2}"

# The kernel console LEVEL (issue 1394, 26.10.2026). console_loglevel is ONE level for every console,
# and Ubuntu's /etc/sysctl.d/10-console-messages.conf pins it at 4, so netconsole shipped only
# emerg..err: the info lines issue 1311 exists for (`usb 4-1: USB disconnect`, link changes, ext4
# remounts) never left the box. Kernel 7.0 has no netconsole- or console-specific level (the configfs
# target has no loglevel attribute, struct console no level field). The other console, tty0, is drawn
# by fbcon on the very framebuffer camera-box writes the cameraman preview to, so a bare level 7 would
# print every info line over the picture. So the setup script first routes the VT console's kernel
# messages to a BACKGROUND VT (TIOCL_SETKMSGREDIRECT: the VT layer draws a VT only while it is in the
# foreground; logind spawns gettys on 1-6 only, so 12 never is; Alt+F12 at the rig shows the kernel
# log), reads the redirect back, and only then raises the level to 7 (emerg..info; debug stays local).
REMOTE_LOG_KMSG_VT=12
REMOTE_LOG_CONSOLE_LOGLEVEL=7
# Overridable so a Tier-0 test can point the level write at a temp file (production never sets it).
REMOTE_LOG_PRINTK_PATH="${REMOTE_LOG_PRINTK_PATH:-/proc/sys/kernel/printk}"

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

# remote_log_ipv4_ok VALUE -> 0 iff VALUE is a dotted-quad IPv4 address (each octet 0-255). The
# number set is spelled out: a bracket RANGE follows the locale in bash (ci-testing-gotchas.md).
remote_log_ipv4_ok() {
  local ip="${1-}" o
  local -a parts
  [[ "$ip" =~ ^[0123456789]{1,3}\.[0123456789]{1,3}\.[0123456789]{1,3}\.[0123456789]{1,3}$ ]] || return 1
  # split on the dots alone, whatever IFS the caller set (review round 1)
  IFS=. read -r -a parts <<<"$ip"
  for o in "${parts[@]}"; do [ "$((10#$o))" -le 255 ] || return 1; done
  return 0
}

# remote_log_journal_url SINK_IP -> the systemd-journal-upload URL of that sink (plain HTTP: the
# receiver runs --listen-http on a private venue LAN). rc 1 + a stderr line for a non-IPv4 sink.
remote_log_journal_url() {
  remote_log_ipv4_ok "${1-}" || { echo "remote-logging: sink '${1-}' is not an IPv4 address (issue 1394)" >&2; return 1; }
  printf 'http://%s:%s\n' "$1" "$REMOTE_LOG_JOURNAL_PORT"
}

# remote_log_sink_ip CAMERA -> the cambox's log sink (issue 1394): the STRIH_IP of the ONE strih fact
# file (scripts/strih-boxes/*.env, PARSED through scripts/lib/strih-box-facts.sh, never sourced) whose
# STRIH_ROLE is `strih` and whose STRIH_CAMERAS lists CAMERA's number -- the cutter of the venue that
# HAS the camera. stdout = the address, rc 0. rc 1 + one `remote-logging:` stderr line, nothing on
# stdout, for: a name that is not camN, a fact file that does not parse, no such file, two of them, or
# a STRIH_IP that is not IPv4. Never a guessed or default address. Runs in a subshell: the caller's
# shell (shell options, the loader's globals) is left as it was. STRIH_BOXES_DIR moves the fact dir
# (the loader's own seam).
remote_log_sink_ip() {
  local row
  row="$(_remote_log_strih_row "${1-}")" || return 1
  printf '%s\n' "${row%%$'\t'*}"
}

# remote_log_camera_venue CAMERA -> the STRIH_VENUE of the same ONE strih fact file that gives the
# camera its log sink (issue 1450: the rig lease resource `<venue>-cams` of a cambox apply). rc 1 +
# the same stderr lines as remote_log_sink_ip, or a named line for a fact file with no venue.
remote_log_camera_venue() {
  local row venue
  row="$(_remote_log_strih_row "${1-}")" || return 1
  venue="${row#*$'\t'}"
  venue="${venue%%$'\t'*}"
  [ -n "$venue" ] || { echo "remote-logging: the strih fact file of ${1-} declares no STRIH_VENUE (issue 1450)" >&2; return 1; }
  printf '%s\n' "$venue"
}

# _remote_log_strih_row CAMERA -> "<STRIH_IP><TAB><STRIH_VENUE><TAB><JXS port>" of the ONE strih fact file
# that lists CAMERA (the lookup + checks remote_log_sink_ip documents above). The third field is the UDP
# port that file's STRIH_JXS_CAMERAS gives the camera, or `none` (issue 1387: the cambox JPEG XS target of
# scripts/lib/cambox-jxs-target.sh is the same venue strih -- ONE camera -> venue-strih lookup).
_remote_log_strih_row() {
  (
    set +f
    shopt -u nullglob failglob nocaseglob nocasematch 2>/dev/null || true
    cam="${1-}"
    case "$cam" in
      cam*) n="${cam#cam}" ;;
      *) echo "remote-logging: '${cam}' is not a camera name (camN) -- no log sink (issue 1394)" >&2; exit 1 ;;
    esac
    [[ "$n" =~ ^[123456789][0123456789]*$ ]] \
      || { echo "remote-logging: '${cam}' is not a camera name (camN) -- no log sink (issue 1394)" >&2; exit 1; }
    if ! declare -F strih_box_parse_file >/dev/null; then
      # shellcheck source=scripts/lib/strih-box-facts.sh
      . "${REMOTE_LOG_LIB_DIR}/strih-box-facts.sh" || exit 1
    fi
    dir="$(strih_box_dir)"
    hits="" sink="" venue="" jxs=""
    for f in "$dir"/*.env; do
      [ -f "$f" ] || continue
      parsed="$(strih_box_parse_file "$f")" \
        || { echo "remote-logging: ${f} does not parse -- no log sink for ${cam} (issue 1394)" >&2; exit 1; }
      role="" cams="" ip="" fvenue="" fjxs=""
      while IFS= read -r line; do
        case "$line" in
          STRIH_ROLE=*) role="${line#*=}" ;;
          STRIH_CAMERAS=*) cams="${line#*=}" ;;
          STRIH_IP=*) ip="${line#*=}" ;;
          STRIH_VENUE=*) fvenue="${line#*=}" ;;
          STRIH_JXS_CAMERAS=*) fjxs="${line#*=}" ;;
        esac
      done <<<"$parsed"
      [ "$role" = strih ] || continue
      case " ${cams} " in *" ${n} "*) ;; *) continue ;; esac
      hits="${hits:+${hits} }${f##*/}"
      sink="$ip"
      venue="$fvenue"
      jxs="$(strih_box_jxs_port_of "$fjxs" "$n")"
    done
    case "$hits" in
      "") echo "remote-logging: no strih fact file in ${dir} lists ${cam} in STRIH_CAMERAS -- no log sink (issue 1394)" >&2; exit 1 ;;
      *" "*) echo "remote-logging: ${cam} is listed by two strih fact files (${hits}) -- a camera is in one venue (issue 1394)" >&2; exit 1 ;;
    esac
    remote_log_ipv4_ok "$sink" \
      || { echo "remote-logging: ${hits} STRIH_IP '${sink}' is not an IPv4 address -- no log sink for ${cam} (issue 1394)" >&2; exit 1; }
    printf '%s\t%s\t%s\n' "$sink" "$venue" "$jxs"
  )
}

# remote_log_mac_from_neigh NEIGH_TEXT -> the resolved lladdr MAC (lowercase colon form) iff the
# neighbour entry is genuinely resolved, else EMPTY. `ip neigh show <ip>` yields a line like
# `10.77.9.202 dev enp2s0 lladdr aa:bb:cc:dd:ee:ff REACHABLE`; an UNRESOLVED entry is
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

# remote_log_console_level_decision READBACK VT -> "raise" iff the kernel message redirect READ BACK
# exactly VT (a positive whole number), else "keep". Pure (issue 1394): the setup script raises
# console_loglevel only when the VT console's kernel messages provably go to the background VT, so a
# failed or unread redirect never prints info lines over the cameraman preview. Embedded into the
# on-box script via `declare -f`. The digit set is spelled out (a bracket RANGE follows the locale).
remote_log_console_level_decision() {
  local readback="${1-}" vt="${2-}"
  if [[ "$vt" =~ ^[123456789][0123456789]*$ ]] && [ "$readback" = "$vt" ]; then
    printf 'raise\n'
  else
    printf 'keep\n'
  fi
}

# remote_log_kmsg_redirect_perl -> the perl program the setup script runs as `perl -e PROG VT`: open
# /dev/ttyVT without taking it as the controlling tty (the oneshot is a session leader; opening the VT
# also allocates it, which TIOCL_SETKMSGREDIRECT needs), set the kernel message redirect to VT
# (TIOCLINUX subcode 11), read it back (subcode 17: the kernel returns 0 and put_user()s the VT into
# byte 0 of the argument, so the VT is read from the buffer, never the return value) and print it. perl-base
# is Essential on Ubuntu, so it is on every cambox; no single quote in the text (it is embedded in one).
remote_log_kmsg_redirect_perl() {
  cat <<'PERL'
use Fcntl;
my $vt = $ARGV[0];
sysopen(my $t, "/dev/tty$vt", O_RDONLY | O_NOCTTY) or die "open /dev/tty$vt: $!\n";
my $s = pack("CC", 11, $vt);
defined(ioctl($t, 0x541C, $s)) or die "TIOCL_SETKMSGREDIRECT: $!\n";
my $g = pack("C", 17);
defined(ioctl($t, 0x541C, $g)) or die "TIOCL_GETKMSGREDIRECT: $!\n";
print unpack("C", $g), "\n";
PERL
}

# remote_log_netconsole_setup_script_content SINK_IP -> the full on-box setup script written to
# ${REMOTE_LOG_NC_SCRIPT_PATH} and run by the boot oneshot, with SINK_IP (the venue strih,
# remote_log_sink_ip) baked in. rc 1 + a stderr line, no output, for a non-IPv4 SINK_IP. It EMBEDS
# remote_log_mac_from_neigh via `declare -f` (ONE source of truth). `set -uo pipefail` (never
# `set -e` — the many `|| true` are deliberate best-effort steps against a configfs that rejects a
# write while the target is enabled). It: ensures configfs + the netconsole module are loaded;
# resolves the egress dev + local ip toward the sink; resolves the sink's next-hop MAC with a bounded
# ping/neigh retry; then (idempotently) creates the dynamic configfs target and enables it.
# Fire-and-forget after that — the kernel emits printk over UDP with zero fs/userspace dependency, so
# it survives the #1309 half-dead wedge.
remote_log_netconsole_setup_script_content() {
  local sink="${1-}"
  remote_log_ipv4_ok "$sink" \
    || { echo "remote-logging: the netconsole setup script needs the sink IPv4 address (got '${sink}', issue 1394)" >&2; return 1; }
  cat <<SCRIPT_HEAD
#!/usr/bin/env bash
# cambox-netconsole-setup.sh (#1311) — GENERATED by scripts/lib/remote-logging.sh. Do not hand-edit;
# re-provision (setup-device.sh / scripts/cambox-ro-units-apply.sh) to change it. Brings up the
# netconsole DYNAMIC configfs target so the kernel ships printk to this box's venue strih (the log
# sink, issue 1394) in real time, surviving the #1309 half-dead-stick wedge.
set -uo pipefail

SINK_IP="${sink}"
PORT="${REMOTE_LOG_NETCONSOLE_PORT}"
CFG="${REMOTE_LOG_NC_CONFIGFS}"
RETRIES="${REMOTE_LOG_NC_MAC_RETRIES}"
RETRY_SLEEP="${REMOTE_LOG_NC_MAC_RETRY_SLEEP_S}"
ROUTE_RETRIES="${REMOTE_LOG_NC_ROUTE_RETRIES}"
ROUTE_RETRY_SLEEP="${REMOTE_LOG_NC_ROUTE_RETRY_SLEEP_S}"
KMSG_VT="${REMOTE_LOG_KMSG_VT}"
CONSOLE_LEVEL="${REMOTE_LOG_CONSOLE_LOGLEVEL}"
PRINTK="${REMOTE_LOG_PRINTK_PATH}"
KMSG_REDIRECT_PERL='$(remote_log_kmsg_redirect_perl)'

SCRIPT_HEAD
  declare -f remote_log_mac_from_neigh remote_log_console_level_decision
  cat <<'SCRIPT_BODY'

# Ensure configfs is mounted (systemd's sys-kernel-config.mount usually does this) and the
# netconsole module is present. All best-effort: a built-in configfs/netconsole makes these no-ops.
modprobe configfs 2>/dev/null || true
mountpoint -q /sys/kernel/config 2>/dev/null || mount -t configfs none /sys/kernel/config 2>/dev/null || true
modprobe netconsole 2>/dev/null || true

# Egress interface + local ip toward the sink (resolved live — no hard-coded NIC name). A cambox MASKS
# systemd-networkd-wait-online (#547 boot-stall fix), so network-online.target is reached BEFORE
# networkd applies the static IP/route — wait for the route to appear in a bounded retry loop rather
# than giving up on the first miss (#1311).
DEV=""
r=0
while [ "$r" -lt "$ROUTE_RETRIES" ]; do
  DEV="$(ip -o route get "$SINK_IP" 2>/dev/null | grep -oE 'dev [^ ]+' | awk '{print $2}' | head -1)"
  [ -n "$DEV" ] && break
  r=$((r + 1))
  sleep "$ROUTE_RETRY_SLEEP"
done
if [ -z "$DEV" ]; then
  echo "cambox-netconsole: no egress route to the log sink ($SINK_IP) after ${ROUTE_RETRIES} tries — netconsole not armed" >&2
  exit 1
fi
LOCAL_IP="$(ip -o route get "$SINK_IP" 2>/dev/null | grep -oE 'src [^ ]+' | awk '{print $2}' | head -1)"

# Resolve the sink's next-hop MAC (netconsole needs the L2 dest). Ping to populate the neigh cache,
# then read it; retry until resolved or the bound is hit.
MAC=""
i=0
while [ "$i" -lt "$RETRIES" ]; do
  ping -c1 -W1 "$SINK_IP" >/dev/null 2>&1 || true
  MAC="$(remote_log_mac_from_neigh "$(ip neigh show "$SINK_IP" 2>/dev/null)")"
  [ -n "$MAC" ] && break
  i=$((i + 1))
  sleep "$RETRY_SLEEP"
done
if [ -z "$MAC" ]; then
  echo "cambox-netconsole: could not resolve the log sink ($SINK_IP) MAC after ${RETRIES} tries — netconsole not armed" >&2
  exit 1
fi

# (Re)configure the dynamic target idempotently. configfs rejects param writes while a target is
# ENABLED, so disable first (harmless if it did not exist / was already disabled).
[ -d "$CFG" ] && { echo 0 > "$CFG/enabled" 2>/dev/null || true; }
mkdir -p "$CFG"
echo 0 > "$CFG/enabled" 2>/dev/null || true
echo "$DEV" > "$CFG/dev_name"
[ -n "$LOCAL_IP" ] && { echo "$LOCAL_IP" > "$CFG/local_ip" 2>/dev/null || true; }
echo "$SINK_IP" > "$CFG/remote_ip"
echo "$PORT" > "$CFG/remote_port"
echo "$MAC" > "$CFG/remote_mac"
echo 1 > "$CFG/enabled"

# Verify the arm actually took (#1311 review F1): under set -uo pipefail (no -e) a failed configfs
# write does NOT abort, so without this the oneshot would exit 0 and `systemctl is-active` would read
# `active` even when the netconsole module/configfs was absent and every write above silently failed.
# netconsole is the death-instant transport, so a truthful is-active matters — fail the unit if the
# target did not end up enabled.
if [ "$(cat "$CFG/enabled" 2>/dev/null)" != "1" ]; then
  echo "cambox-netconsole: arm FAILED (target enabled != 1) -> ${SINK_IP}:${PORT}" >&2
  exit 1
fi
echo "cambox-netconsole: armed -> ${SINK_IP}:${PORT} via ${DEV} (sink mac ${MAC})"

# Issue 1394: ship warning/info too, never on the cameraman preview. console_loglevel is one level for
# every console, and tty0 is drawn on the HDMI framebuffer, so FIRST route the VT console's kernel
# messages to the background VT and read the redirect back, THEN raise the level. A redirect that does
# not read back leaves the level as it is (netconsole keeps err and above) and says so; the arm above
# stays a success either way, and verify-device (ak) grades both facts.
KMSG_READBACK="$(LC_ALL=C perl -e "$KMSG_REDIRECT_PERL" "$KMSG_VT" 2>&1)" || true
if [ "$(remote_log_console_level_decision "$KMSG_READBACK" "$KMSG_VT")" = raise ]; then
  read -r _ PRINTK_DEF PRINTK_MIN PRINTK_BOOT _ < "$PRINTK" || true
  if printf '%s %s %s %s\n' "$CONSOLE_LEVEL" "${PRINTK_DEF:-4}" "${PRINTK_MIN:-1}" "${PRINTK_BOOT:-7}" > "$PRINTK"; then
    echo "cambox-netconsole: kernel messages redirected to VT ${KMSG_VT}; console_loglevel ${CONSOLE_LEVEL} (warning/info now reach the sink, issue 1394)"
  else
    echo "cambox-netconsole: WARNING: could not write console_loglevel ${CONSOLE_LEVEL} to ${PRINTK} -- netconsole ships err and above only (issue 1394)" >&2
  fi
else
  echo "cambox-netconsole: WARNING: the kernel message redirect to VT ${KMSG_VT} did not read back (${KMSG_READBACK:-<no output>}) -- console_loglevel left as is, netconsole ships err and above only (issue 1394)" >&2
fi
SCRIPT_BODY
}

# remote_log_netconsole_service_unit_content -> the boot oneshot that runs the setup script. Ordered
# after systemd-networkd.service (netconsole needs the NIC up + the static route/ARP resolvable) —
# network-online.target is meaningless on this box class (wait-online is MASKED, #547/#1311), so the
# setup script also retries the egress-route lookup. Type=oneshot + RemainAfterExit
# so `systemctl is-active` reads `active` after a successful arm (the verify (ak) check keys on that).
# Issue 1394: an arm that still fails (the sink's MAC not resolvable, a late route) used to stay failed
# until the next boot (cam1 from 4.10 06:24Z, the box `degraded`, no kernel printk off-box).
# Restart=on-failure retries it (30 s backing off to 10 min), with no start-rate limit. systemd 255
# allows on-failure for a oneshot (always/on-success it refuses) and has RestartSteps /
# RestartMaxDelaySec; tests/python/test_cambox_ro_units_1394.py runs systemd-analyze verify on this
# text. Pulled in at boot by multi-user.target (reboot survival via `enable`).
remote_log_netconsole_service_unit_content() {
  cat <<EOF
[Unit]
Description=netconsole: ship kernel printk to the venue strih log sink in real time (camera-box #1311, #1394)
Documentation=https://github.com/zbynekdrlik/camera-box/issues/1311
# #1311: a cambox MASKS systemd-networkd-wait-online (#547 boot-stall fix), so network-online.target
# is reached INSTANTLY — before networkd applies the static IP/route. Order after systemd-networkd
# itself so the arm does not race the static-route apply; the setup script additionally retries the
# egress-route lookup. The network-online lines stay (documented intent + belt-and-suspenders).
After=network-online.target
After=systemd-networkd.service network.target
Wants=network-online.target
Wants=systemd-networkd.service
# Issue 1394: no start-rate limit. A safeguard: at RestartSec=30 the default limit (5 starts in
# 10 s) cannot trip, but the Restart= retries below must never run out.
StartLimitIntervalSec=0

[Service]
Type=oneshot
RemainAfterExit=yes
# Issue 1394: a failed arm retries until the sink answers, instead of staying failed (and the box
# degraded) until the next boot. The delay grows from 30 s to 10 min over 4 steps, so a box whose
# sink is down does not write a failed arm into the on-box journal every 30 s.
Restart=on-failure
RestartSec=30
RestartSteps=4
RestartMaxDelaySec=600
ExecStart=${REMOTE_LOG_NC_SCRIPT_PATH}

[Install]
WantedBy=multi-user.target
EOF
}

# remote_log_journal_upload_conf_content SINK_IP -> /etc/systemd/journal-upload.conf. Points the
# uploader at the venue strih's systemd-journal-remote HTTP sink (remote_log_journal_url). Plain HTTP
# (not HTTPS) — the venue LAN is private and TLS on a headless appliance is not worth the cert
# lifecycle; the receiver runs --listen-http. rc 1 + a stderr line, no output, for a non-IPv4 sink.
remote_log_journal_upload_conf_content() {
  local url
  url="$(remote_log_journal_url "${1-}")" || return 1
  cat <<EOF
[Upload]
URL=${url}
EOF
}

# remote_log_journal_upload_dropin_content -> a systemd-journal-upload.service drop-in that redirects
# the cursor state to a /run (tmpfs) path so the uploader works from a READ-ONLY root (the stock
# --save-state=/var/lib/... is unwritable on the ro appliance). RuntimeDirectory creates the tmpfs
# dir with the service user's ownership. A runtime-only cursor is fine — the box dies anyway, and on
# a clean reboot the uploader simply resumes from the current boot.
remote_log_journal_upload_dropin_content() {
  cat <<EOF
[Unit]
# #1311 boot-order: on a cambox network-online.target is reached BEFORE the sink is reachable (masked
# wait-online, #547), so the stock unit's Restart=on-failure + default StartLimit exhausts after 5
# instant "connection refused" restarts and the unit stays FAILED for the whole boot. Disable the
# start-limit so the uploader keeps retrying until the sink answers (it is idempotent — the /run cursor
# resumes; a down sink is harmless).
StartLimitIntervalSec=0

[Service]
# #1311: keep retrying the upload until the venue strih sink is reachable; RestartSec spaces the retries so
# a genuinely-down sink is not hammered.
Restart=always
RestartSec=5
# ro-root appliance (#1311): the default --save-state=/var/lib/systemd/journal-upload/state is on the
# read-only root. Point the cursor at a tmpfs /run path so the uploader can persist its cursor.
RuntimeDirectory=systemd/journal-upload
# #1311 review F2: the stock noble unit carries StateDirectory=systemd/journal-upload, which systemd
# tries to mkdir under /var/lib at EVERY start -- and /var/lib is on the READ-ONLY root (only
# /var/log|tmp|cache|spool are tmpfs), so the unit would FAIL regardless of the --save-state override.
# Clearing it (empty assignment resets the list) removes that /var/lib dependency; the cursor lives in
# /run via RuntimeDirectory + --save-state below.
StateDirectory=
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
echo "NC_SVC_RESULT=$(systemctl show -p Result --value cambox-netconsole 2>/dev/null)"
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
echo "JU_STATE_SAVE=$(grep -hE '^ExecStart=.*[-][-]save-state=' /etc/systemd/system/systemd-journal-upload.service.d/*.conf 2>/dev/null | grep -oE '[-][-]save-state=[^ ]+' | tail -1)"
echo "CONSOLE_LOGLEVEL=$(awk '{print $1}' /proc/sys/kernel/printk 2>/dev/null)"
echo "KMSG_REDIRECT=$(LC_ALL=C perl -e 'use Fcntl; sysopen(my $t, "/dev/tty0", O_RDONLY | O_NOCTTY) or exit 1; my $g = pack("C", 17); defined(ioctl($t, 0x541C, $g)) or exit 1; print unpack("C", $g);' 2>/dev/null)"
REMOTE
}

# remote_log_verdict STATE_BLOCK SINK_IP -> "ok" or the newline-joined "FAIL: ..." reasons.
# STATE_BLOCK is the KEY=VALUE text from remote_log_gather_remote_snippet (or a test fixture); SINK_IP
# is THIS box's venue strih (remote_log_sink_ip). Fail-closed: an absent/unparseable value, or a
# missing/non-IPv4 SINK_IP, is NEVER read as "safely logging" (test-strictness), mirroring
# dscp_nft_verdict / log_diet_provision_verdict.
#
# netconsole facets (the kernel path — does NOT depend on the sink's receiver, so all are gated):
#   * cambox-netconsole.service enabled (reboot survival) AND active (RemainAfterExit oneshot armed)
#   * the setup script is present + executable
#   * the dynamic configfs target is live: enabled==1, remote_ip==SINK_IP, remote_port==514
# journal-upload facets (the rich path):
#   * systemd-journal-upload.service enabled (reboot survival)
#   * the upload URL points at SINK_IP's journal sink
#   * the cursor --save-state is redirected to /run (ro-root safe)
# NOTE: journal-upload ACTIVE is deliberately NOT gated — the uploader's active state depends on the
# sink's receiver being up (setup-strih step 16k, graded on the strih box by verify-strih item 46); a
# cambox is correctly provisioned even before the sink exists (netconsole is fire-and-forget and
# needs no listener either). enabled + correct config is the cambox-side acceptance bar.
remote_log_verdict() {
  local block="$1" sink="${2-}" url="" fails="" nl
  nl=$'\n'
  local nc_enabled_svc nc_active nc_result nc_script_x nc_target nc_ip nc_port
  local ju_enabled ju_url ju_state cl kr
  if remote_log_ipv4_ok "$sink"; then
    url="$(remote_log_journal_url "$sink")"
  else
    fails="FAIL: no log sink resolved for this box (got '${sink}') -- its venue strih fact file must list it in STRIH_CAMERAS (issue 1394)"
  fi
  nc_enabled_svc="$(printf '%s\n' "$block" | sed -n 's/^NC_SVC_ENABLED=//p' | tr -d '[:space:]')"
  nc_active="$(printf '%s\n' "$block" | sed -n 's/^NC_SVC_ACTIVE=//p' | tr -d '[:space:]')"
  nc_result="$(printf '%s\n' "$block" | sed -n 's/^NC_SVC_RESULT=//p' | tr -d '[:space:]')"
  nc_script_x="$(printf '%s\n' "$block" | sed -n 's/^NC_SCRIPT_X=//p' | tr -d '[:space:]')"
  nc_target="$(printf '%s\n' "$block" | sed -n 's/^NC_ENABLED=//p' | tr -d '[:space:]')"
  nc_ip="$(printf '%s\n' "$block" | sed -n 's/^NC_REMOTE_IP=//p' | tr -d '[:space:]')"
  nc_port="$(printf '%s\n' "$block" | sed -n 's/^NC_REMOTE_PORT=//p' | tr -d '[:space:]')"
  ju_enabled="$(printf '%s\n' "$block" | sed -n 's/^JU_SVC_ENABLED=//p' | tr -d '[:space:]')"
  ju_url="$(printf '%s\n' "$block" | sed -n 's/^JU_URL=//p' | tr -d '[:space:]')"
  ju_state="$(printf '%s\n' "$block" | sed -n 's/^JU_STATE_SAVE=//p' | tr -d '[:space:]')"
  cl="$(printf '%s\n' "$block" | sed -n 's/^CONSOLE_LOGLEVEL=//p' | tr -d '[:space:]')"
  kr="$(printf '%s\n' "$block" | sed -n 's/^KMSG_REDIRECT=//p' | tr -d '[:space:]')"

  [ "$nc_enabled_svc" = "enabled" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service is not enabled (state=${nc_enabled_svc:-<none>}) -- netconsole will not survive a reboot (#1311)"
  # Issue 1394: a `failed` unit is named with its systemd Result (exit-code, timeout, ...), so the
  # report says WHY it never armed; an old gather without the Result line reads `<unread>`.
  case "$nc_active" in
    active) : ;;
    failed) fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service FAILED (Result=${nc_result:-<unread>}) -- the netconsole arm failed and no kernel printk leaves the box; a unit without the issue-1394 Restart=on-failure stays failed until the next boot (#1311)" ;;
    *) fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SERVICE_NAME}.service is not active (state=${nc_active:-<none>}, Result=${nc_result:-<unread>}) -- the netconsole boot oneshot did not arm (#1311)" ;;
  esac
  [ "$nc_script_x" = "yes" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_NC_SCRIPT_PATH} is missing/not executable -- re-provision with the current setup-device.sh (#1311)"
  if [ "$nc_target" = "__NO_TARGET__" ]; then
    fails="${fails:+$fails$nl}FAIL: no netconsole configfs target at ${REMOTE_LOG_NC_CONFIGFS} -- kernel printk is not leaving the box (#1311)"
  else
    [ "$nc_target" = "1" ] || fails="${fails:+$fails$nl}FAIL: the netconsole target is not enabled (enabled=${nc_target:-<none>}) (#1311)"
    { [ -n "$url" ] && [ "$nc_ip" = "$sink" ]; } || fails="${fails:+$fails$nl}FAIL: the netconsole target remote_ip=${nc_ip:-<none>} != the venue strih sink ${sink:-<unresolved>} (#1311, issue 1394)"
    [ "$nc_port" = "$REMOTE_LOG_NETCONSOLE_PORT" ] || fails="${fails:+$fails$nl}FAIL: the netconsole target remote_port=${nc_port:-<none>} != ${REMOTE_LOG_NETCONSOLE_PORT} (#1311)"
  fi

  [ "$ju_enabled" = "enabled" ] || fails="${fails:+$fails$nl}FAIL: ${REMOTE_LOG_JU_SERVICE_NAME}.service is not enabled (state=${ju_enabled:-<none>}) -- the journal uploader will not survive a reboot (#1311)"
  { [ -n "$url" ] && [ "$ju_url" = "$url" ]; } || fails="${fails:+$fails$nl}FAIL: journal-upload URL=${ju_url:-<none>} != ${url:-<the venue strih sink, unresolved>} (#1311, issue 1394)"
  case "$ju_state" in
    *--save-state=/run/*) : ;;
    *) fails="${fails:+$fails$nl}FAIL: journal-upload --save-state is not redirected to /run (got '${ju_state:-<none>}') -- the cursor would be unwritable on the ro root (#1311)" ;;
  esac

  # Issue 1394: the console level netconsole obeys, and the redirect that keeps it off the preview.
  if ! [[ "$cl" =~ ^[0123456789]+$ ]]; then
    fails="${fails:+$fails$nl}FAIL: the kernel console_loglevel is unread (got '${cl:-<none>}') -- cannot tell which levels netconsole ships (issue 1394)"
  elif [ "$((10#$cl))" -lt "$REMOTE_LOG_CONSOLE_LOGLEVEL" ]; then
    fails="${fails:+$fails$nl}FAIL: the kernel console_loglevel=${cl} < ${REMOTE_LOG_CONSOLE_LOGLEVEL} -- netconsole ships only the levels below it, so warning/info (a USB disconnect) never leave the box; re-point with scripts/cambox-ro-units-apply.sh (issue 1394)"
  elif [ "$kr" != "$REMOTE_LOG_KMSG_VT" ]; then
    fails="${fails:+$fails$nl}FAIL: console_loglevel=${cl} while kernel messages are not redirected to VT ${REMOTE_LOG_KMSG_VT} (redirect=${kr:-<unread>}) -- every warning/info line is drawn on the cameraman HDMI preview; re-point with scripts/cambox-ro-units-apply.sh (issue 1394)"
  fi

  if [ -n "$fails" ]; then
    printf '%s\n' "$fails"
  else
    printf 'ok\n'
  fi
}
