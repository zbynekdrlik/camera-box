#!/bin/bash
# airuleset:script-ok source-only lib -- the sourcing setup script owns strict mode (set -euo pipefail) and fail()
# obs-box-kiosk.sh (issue 1357) -- the KIOSK-SESSION half of the shared OBS-box appliance baseline.
#
# Sourced by scripts/lib/obs-box-baseline.sh (the one entry point both setup-imag.sh and
# setup-strih.sh source); split out only to keep each lib file readable. This half owns what makes the
# box an OBS-only appliance instead of a desktop: never-sleep, the de-jitter masks + the crash-popup
# item, the lightdm-autologin -> openbox-on-plain-Xorg kiosk with the GNOME purge (and its
# panel-brightness keys facet), the touchpad InputClass, and the shared openbox autostart preamble +
# root menu. The system half (network, performance, boot safety, kernel, CPU affinity, GPU, power
# envelope) lives in obs-box-baseline.sh;
# see its header for the box-fact ARGUMENTS convention and the item order. The bodies keep
# setup-imag.sh's column-0 layout so their heredocs stay byte-identical to what imag has always written.
# issue 1415: this half also sets the rig time zone (obs_box_timezone), the operator's clocks.
# issue 1418: it also holds the iGPU panel-sink item (obs_box_igpu_sink_modesetting: remove the amdgpu
# Xorg DDX + an xorg.conf.d OutputClass), which the system half's obs_box_nvidia_prime runs on a
# discrete-NVIDIA box.

# The ONE rig time-zone declaration (scripts/lib/rig-timezone.sh, also used by setup-device.sh,
# create-usb-linux.sh and verify-device.sh). The directory is a pure parameter expansion (no dirname),
# like obs-box-baseline.sh's own source of this file.
# shellcheck source=scripts/lib/rig-timezone.sh
if [ "${BASH_SOURCE[0]%/*}" != "${BASH_SOURCE[0]}" ]; then . "${BASH_SOURCE[0]%/*}/rig-timezone.sh"; else . ./rig-timezone.sh; fi

# obs_box_timezone [ROOT] -- issue 1415: the box runs the rig time zone (Europe/Bratislava), so every
# browser source (Camera info, Interkom, Odpocet) shows local time. Writes the /etc/localtime symlink +
# /etc/timezone through rig_timezone_apply (never timedatectl, whose read-back lies on a read-only root),
# installing tzdata first when the zoneinfo file is missing. ROOT is for the tests; both setup scripts
# call it bare, in step 1. Fails loud via the caller's fail().
obs_box_timezone() {
    local ROOT="${1:-}" before
    if [ ! -f "$(rig_timezone_zoneinfo "$ROOT")" ]; then
        echo "  issue 1415: $(rig_timezone_zoneinfo "$ROOT") is missing -- installing tzdata"
        obs_box_apt_update
        DEBIAN_FRONTEND=noninteractive apt-get install -y tzdata >/dev/null \
            || fail "issue 1415: could not install tzdata -- no zoneinfo file for the rig time zone ${RIG_TIMEZONE}"
    fi
    before="$(rig_timezone_read "$ROOT")"
    rig_timezone_apply "$ROOT" \
        || fail "issue 1415: could not set the rig time zone ${RIG_TIMEZONE} (/etc/localtime) -- the operator's clocks would run on UTC"
    # A running process keeps the zone it loaded: OBS (its browser-source clocks) and the :8899
    # bundle-state server, whose log-age reads compare against OBS's log timestamps, must restart together.
    if [ "$before" != "$RIG_TIMEZONE" ]; then
        echo -e "  ${YELLOW}issue 1415: the zone changed from '${before}' -- reboot the box (or restart the OBS unit, the :8899 bundle-state server and the session apps together) so every running clock reads ${RIG_TIMEZONE}${NC}"
    fi
}

# obs_box_crash_popup_units -> the system units the crash-popup item masks, one per line: apport +
# whoopsie (the imag #485 list) PLUS the Ubuntu 26.04 apport coredump hook TEMPLATE. apport writes the
# /var/crash reports that raise the update-notifier-crash desktop popup (and multi-GB cores right when
# OBS already crashed); whoopsie phones crash reports home. On 26.04 apport ALSO ships a systemd-coredump
# drop-in (`OnSuccess=apport-coredump-hook@%i.service`) that runs `apport --from-systemd-coredump` and
# writes /var/crash regardless of apport.service's mask -- so with systemd-coredump installed the popup
# survives unless the hook TEMPLATE is masked too (masking a template blocks every instance; on 24.04
# the template does not exist and the mask is a harmless /dev/null link).
obs_box_crash_popup_units() {
    printf '%s\n' apport.service apport-coredump-hook@.service whoopsie.service
}

# obs_box_dejitter_user_units -> the desktop user's --user units obs_box_dejitter MASKS (the tracker
# file indexer + the evolution groupware factories). The ONE list both the de-jitter mask and the verify
# gather use (a masked --user unit is a ~/.config/systemd/user/<unit> -> /dev/null link).
obs_box_dejitter_user_units() {
    printf '%s\n' tracker-miner-fs-3.service tracker-miner-fs-control-3.service \
        tracker-writeback-3.service tracker-xdg-portal-3.service \
        evolution-source-registry.service evolution-calendar-factory.service \
        evolution-addressbook-factory.service evolution-user-prompter.service evolution-alarm-notify.service
}

# u_systemctl ARGS... -- `systemctl --user ARGS` as the CALLER's DESKTOP_USER (obs_box_dejitter's local,
# visible here by bash's dynamic scope) on that user's own bus. Best-effort (|| true), like every
# per-user desktop tweak here; the baseline verify grades the masks it leaves.
u_systemctl() {
    local uid
    uid="$(id -u "$DESKTOP_USER")"
    sudo -u "$DESKTOP_USER" \
        XDG_RUNTIME_DIR="/run/user/${uid}" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${uid}/bus" \
        systemctl --user "$@" >/dev/null 2>&1 || true
}

# obs_box_crash_popups_off -- the crash-popup baseline item (issue 1357, moved from setup-strih.sh
# sub-step 11c, which generalised imag's #485 apport/whoopsie mask): per unit, so an absent whoopsie
# never skips apport; a plain unit is also disabled + STOPPED (a unit masked earlier by hand can still
# be active); a template has no instance to stop. systemd-coredump stays the core collector, so a crash
# is diagnosable via coredumpctl instead of a GUI popup. Fails loud on a mask that does not take.
obs_box_crash_popups_off() {
    local CRASH_UNIT
    while IFS= read -r CRASH_UNIT; do
        [ -n "$CRASH_UNIT" ] || continue
        case "$CRASH_UNIT" in
            *@.service) ;;
            *)
                systemctl disable --now "$CRASH_UNIT" >/dev/null 2>&1 || true
                systemctl stop "$CRASH_UNIT" >/dev/null 2>&1 || true
                ;;
        esac
        systemctl mask "$CRASH_UNIT" >/dev/null 2>&1 \
            || fail "could not mask ${CRASH_UNIT} -- the operator crash popup would return (systemctl mask ${CRASH_UNIT} by hand, then re-run)"
    done < <(obs_box_crash_popup_units)
    DEBIAN_FRONTEND=noninteractive apt-get install -y systemd-coredump >/dev/null \
        || fail "systemd-coredump install failed -- needed so a crash lands in coredumpctl once apport is masked"
    echo "  apport + its coredump hook + whoopsie masked (no operator crash popup); systemd-coredump installed (crashes -> coredumpctl)"
}

# obs_box_openbox_autostart_preamble -> the kiosk lines EVERY OBS box's openbox autostart carries,
# verbatim from imag's step-16 autostart (the shared verify grader greps exactly these lines): screen
# blanking/DPMS off (the openbox kiosk has no GNOME idle settings), and the stale OBS crash sentinels
# cleared BEFORE the supervised OBS unit starts, so a hard reboot never hangs the boot headless on the
# "Crash or unclean shutdown detected" modal.
obs_box_openbox_autostart_preamble() {
    cat <<'PREAMBLE_EOF'
xset s off -dpms s noblank 2>/dev/null || true
rm -rf "$HOME/.config/obs-studio/.sentinel"/* 2>/dev/null || true
PREAMBLE_EOF
}

# obs_box_openbox_menu_xml LABEL START_CMD STOP_CMD -> the kiosk openbox root menu (#785/#791) on stdout:
# start OBS through its supervised unit, a GRACEFUL stop (the operator's unsaved UI state is persisted
# on a clean shutdown), btop, a terminal, and clean reboot/poweroff (the hardware power key stays
# HandlePowerKey=ignore, #727). openbox's stock rc.xml binds the desktop right-click to id="root-menu".
# All three args are required (rc 1 otherwise) -- a menu entry pointing at nothing is worse than none.
obs_box_openbox_menu_xml() {
    local label="${1-}" start="${2-}" stop="${3-}"
    if [ -z "$label" ] || [ -z "$start" ] || [ -z "$stop" ]; then
        echo "obs_box_openbox_menu_xml: LABEL START_CMD STOP_CMD required" >&2
        return 1
    fi
    cat <<MENU_EOF
<?xml version="1.0" encoding="UTF-8"?>
<openbox_menu xmlns="http://openbox.org/3.4/menu">
  <menu id="root-menu" label="${label}">
    <item label="Spustiť OBS">
      <action name="Execute">
        <command>${start}</command>
      </action>
    </item>
    <item label="Zastav OBS (korektne)">
      <action name="Execute">
        <command>${stop}</command>
      </action>
    </item>
    <item label="Systémový monitor (CPU+GPU)">
      <action name="Execute">
        <command>x-terminal-emulator -e btop</command>
      </action>
    </item>
    <item label="Terminál">
      <action name="Execute">
        <command>x-terminal-emulator</command>
      </action>
    </item>
    <separator />
    <item label="Reštartovať počítač">
      <action name="Execute">
        <command>systemctl reboot</command>
      </action>
    </item>
    <item label="Vypnúť počítač">
      <action name="Execute">
        <command>systemctl poweroff</command>
      </action>
    </item>
  </menu>
</openbox_menu>
MENU_EOF
}

# user_bus_alive / gs -- moved from setup-imag.sh step 5 (issue 1357), used by the never-sleep and
# de-jitter functions below AND by the callers' later steps (setup-imag.sh 21/27/28). Both read the
# caller's DESKTOP_USER (a function-local one when called from inside a baseline function) and the
# UBUS that obs_box_never_sleep exports.
# #1182: is the desktop user's systemd USER MANAGER bus up? It lives at /run/user/<uid>/bus and
# exists only once that user has a live login session (the kiosk lightdm autologin) or lingering.
# On a from-scratch box provisioned detached, BEFORE the first kiosk boot, it does NOT exist yet,
# so any `sudo -u "$DESKTOP_USER" ... systemctl --user ...` dies "Failed to connect to bus:
# Connection refused". Steps 21/27 gate their `systemctl --user` half on this and DEFER to the
# first kiosk boot when it is absent -- the direct structural analogue of step 17's dead-:0 gate
# ([ -S /tmp/.X11-unix/X0 ] -> defer the OBS launch to the next boot).
user_bus_alive() { [ -S "/run/user/$(id -u "$DESKTOP_USER")/bus" ]; }
gs() { sudo -u "$DESKTOP_USER" DBUS_SESSION_BUS_ADDRESS="$UBUS" gsettings set "$@" 2>/dev/null || true; }

# obs_box_never_sleep DESKTOP_USER BOX -- the imag step 5 never-sleep: lid/power/suspend keys ignored
# (#727), sleep targets masked, GNOME idle/lock off while GNOME is still installed (the openbox
# kiosk's own xset never-blank lives in the openbox autostart). Exports UBUS (global, see below).
obs_box_never_sleep() {
    local DESKTOP_USER="${1:?obs_box_never_sleep: desktop user required}" BOX="${2:?obs_box_never_sleep: BOX required}"
mkdir -p /etc/systemd/logind.conf.d
cat > "/etc/systemd/logind.conf.d/99-${BOX}-no-sleep.conf" <<'EOF'
[Login]
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
HandleLidSwitchDocked=ignore
IdleAction=ignore
EOF
# #727: an OBS box is a PRODUCTION device (imag-nb) — a short accidental power-button press
# suspended/shut it down during the 2026-07-12 live event. Mirrors setup-device.sh's
# STEP 12 fleet convention (HandlePowerKey/HandleSuspendKey/HandleHibernateKey=ignore)
# in a separate drop-in, matching the file already hand-applied live on the box.
cat > /etc/systemd/logind.conf.d/99-production-no-powerkey.conf <<'EOF'
[Login]
HandlePowerKey=ignore
HandleSuspendKey=ignore
HandleHibernateKey=ignore
HandleLidSwitch=ignore
HandleLidSwitchExternalPower=ignore
EOF
systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target >/dev/null 2>&1 || true
systemctl restart systemd-logind
# UBUS is deliberately GLOBAL (no `local`): the caller's later steps reuse it (setup-imag.sh 16/17/21/27).
UBUS="unix:path=/run/user/$(id -u "$DESKTOP_USER")/bus"
gs org.gnome.desktop.session idle-delay 0
gs org.gnome.settings-daemon.plugins.power sleep-inactive-ac-type "'nothing'"
gs org.gnome.settings-daemon.plugins.power sleep-inactive-battery-type "'nothing'"
gs org.gnome.desktop.screensaver lock-enabled false
gs org.gnome.desktop.screensaver idle-activation-enabled false
}

# obs_box_dejitter DESKTOP_USER BOX OBS_CFG -- the imag step 14 (#485) desktop de-jitter: oomd/tracker/
# evolution masked, the crash-popup item (obs_box_crash_popups_off), snapd refresh held, apt-daily-upgrade
# pinned to 04:00 (security updates stay ON), animations off, OBS ProcessPriority=High in OBS_CFG.
obs_box_dejitter() {
    local DESKTOP_USER="${1:?obs_box_dejitter: desktop user required}" BOX="${2:?obs_box_dejitter: BOX required}" OBS_CFG="${3:?obs_box_dejitter: OBS config dir required}"
# imag is a single-app OBS kiosk — no human ever browses, mails, or searches files on it. All
# masks below are low-risk + reversible; security updates stay ON (only their SCHEDULE is
# pinned, Automatic-Reboot is already false by Ubuntu default and is deliberately left untouched).

# systemd-oomd: known to kill WHOLE GNOME sessions (incl. OBS) on transient PSI memory-pressure
# spikes even with GB of RAM free — kernel OOM remains the real backstop.
systemctl disable --now systemd-oomd.service systemd-oomd.socket >/dev/null 2>&1 || true
systemctl mask systemd-oomd.service systemd-oomd.socket >/dev/null 2>&1 || true

# File indexer + groupware factories: no files worth indexing, no mail/calendar account, ever.
local DESKTOP_UID
DESKTOP_UID="$(id -u "$DESKTOP_USER")"
# gs (below) needs the user bus address; obs_box_never_sleep exports it, but never depend on the call order.
: "${UBUS:=unix:path=/run/user/${DESKTOP_UID}/bus}"
# shellcheck disable=SC2046  # word-split on purpose: one unit name per word, one source of truth
u_systemctl mask $(obs_box_dejitter_user_units)
sudo -u "$DESKTOP_USER" tracker3 reset -s >/dev/null 2>&1 || true

# apport/whoopsie (+ the 26.04 apport coredump hook): apport writes multi-GB core dumps right when OBS
# already crashed (worst-time disk spike) and raises the operator crash popup; whoopsie phones crash
# reports home — neither has value on a kiosk appliance. issue 1357: one baseline item for every box.
obs_box_crash_popups_off

# snapd: hold auto-refresh forever (unused firefox/snap-store snaps) — a mid-service "restart to
# update" banner popping over the fullscreen program output is the failure mode this avoids.
snap refresh --hold=forever >/dev/null 2>&1 || true

# apt-daily-upgrade.timer: pin the SCHEDULE to a fixed off-hours time via a drop-in — security
# updates themselves stay fully enabled, never disabled here.
mkdir -p /etc/systemd/system/apt-daily-upgrade.timer.d
cat > "/etc/systemd/system/apt-daily-upgrade.timer.d/${BOX}-offhours.conf" <<'EOF'
[Timer]
OnCalendar=
OnCalendar=*-*-* 04:00
RandomizedDelaySec=30min
EOF
systemctl daemon-reload
systemctl restart apt-daily-upgrade.timer >/dev/null 2>&1 || true

# GNOME animations off — one less compositor cost on the fullscreen program output.
gs org.gnome.desktop.interface enable-animations false

# OBS-native: ProcessPriority=High is OBS's own render-starvation knob (zero cost; ships Normal
# by default). global.ini was just seeded above — flip the value in place if present, else
# append a [General] section (same duplicate-section convention seed_ini already uses for
# LastVersion; Qt's ini backend merges duplicate group headers).
if grep -q '^ProcessPriority=' "$OBS_CFG/global.ini" 2>/dev/null; then
    sed -i 's/^ProcessPriority=.*/ProcessPriority=High/' "$OBS_CFG/global.ini"
else
    printf '\n[General]\nProcessPriority=High\n' >> "$OBS_CFG/global.ini"
fi
chown "$DESKTOP_USER:$DESKTOP_USER" "$OBS_CFG/global.ini"
echo "  de-jitter: oomd/tracker/evolution/apport(+coredump hook)/whoopsie masked, snapd held, apt-daily pinned 04:00, animations off, OBS ProcessPriority=High"
}

# obs_box_kiosk DESKTOP_USER BOX -- the imag step 15 (#504) kiosk: lightdm autologin -> openbox on PLAIN
# Xorg (no GNOME Shell, no Wayland/XWayland), display-manager.service -> lightdm BEFORE any purge, the
# desktop-bloat services disabled, gdm3 disabled for the next boot, the owner's explicit GNOME purge list.
obs_box_kiosk() {
    local DESKTOP_USER="${1:?obs_box_kiosk: desktop user required}" BOX="${2:?obs_box_kiosk: BOX required}"
    # Optional box fact: `keep-bluetooth` leaves bluetooth enabled (strih-lx is operated with a Bluetooth
    # mouse); every other box gets the plain disable list below.
    local KEEP_BT="${3:-}"
# imag-nb is a single-purpose OBS cutting appliance — it must boot straight into a bare,
# non-compositing openbox kiosk (fullscreen OBS projectors on the full panel+HDMI), NOT the full
# GNOME user desktop (owner directive #504, 2026-07-04): GNOME's dock/top-bar steal OBS's screen,
# mutter's "application not responding / force quit?" modal pops over the live output, and the
# desktop bloat/services waste resources on a production box. This step CODIFIES the hand-driven
# live conversion so a from-scratch provision lands in the kiosk, not GNOME.
#
# HARD ORDER (owner incident 2026-07-04): install openbox+lightdm AND switch the display-manager to
# lightdm BEFORE any GNOME purge, so the box ALWAYS has a working DM — purging gdm3 first with no
# lightdm yet left the box with NO display manager on the next boot → black wall + an extra reboot.
# The purge only takes over the SESSION on the NEXT boot; on the live box (already an openbox
# session) it removes dormant packages without touching the running OBS/openbox.

# (a) Install the light WM + display manager. Idempotent (apt-get install on an already-installed
#     package is a no-op). lightdm's default-Recommends greeter (lightdm-gtk-greeter) comes along;
#     the owner's list names no specific greeter, so none is pinned here.
#     #833: wmctrl rides along here too — recording-e2e.sh's [0/8] projector-count preflight (and
#     the #769 windowed-stray heal) shell out to it over SSH; a freshly provisioned box without it
#     made that preflight misread "tool absent" as "0 projectors" (three wasted gate re-runs).
#     #791: btop rides along too — the generated openbox menu (step 16, #785) "Systémový monitor"
#     item runs `x-terminal-emulator -e btop`; the live box has it hand-installed, so a fresh box
#     without it would carry a menu item pointing at a missing binary.
#     issue 1357: xserver-xorg + x11-xserver-utils (xrandr/xset) ride along -- Ubuntu 26.04 (strih-lx)
#     no longer installs the Xorg server with the Wayland-only GNOME desktop, and lightdm+openbox need
#     it; on 24.04 (imag) both are already present, so the install is a no-op there.
#     issue 1361: python3-websocket rides along -- both OBS launchers run a websocket seeder
#     (strih-obs-start.sh refuses to start OBS without it, imag_scenes.py imports it); strih-lx had
#     it only as a hand install. Graded by the `websocket` row of obs-box-baseline-verify.sh.
obs_box_apt_update
DEBIAN_FRONTEND=noninteractive apt-get install -y openbox lightdm feh wmctrl btop xserver-xorg x11-xserver-utils python3-websocket xinput \
    || fail "#504: openbox+lightdm install failed — cannot convert the box to the kiosk WM"
# issue 1418: on a fresh box that install brings xserver-xorg, whose `xserver-xorg-video-all |
# xorg-driver-video` dependency pulls the amdgpu DDX back in AFTER the NVIDIA step removed it. Remove it
# again here, on a discrete-NVIDIA box only (see obs_box_amdgpu_ddx_remove; a no-op when absent).
if lspci -nn 2>/dev/null | obs_box_has_discrete_nvidia; then
    obs_box_amdgpu_ddx_remove
fi

# (b) lightdm autologin → openbox. Idempotent full-file write of a fixed drop-in (always the same
#     content). ${DESKTOP_USER} logs in headless and openbox launches the OBS kiosk (step 16
#     autostart). autologin-user-timeout=0 + user-session=openbox mirror the live-proven config.
mkdir -p /etc/lightdm/lightdm.conf.d
cat > "/etc/lightdm/lightdm.conf.d/50-${BOX}-autologin.conf" <<EOF
[Seat:*]
autologin-user=${DESKTOP_USER}
autologin-user-timeout=0
autologin-session=openbox
user-session=openbox
EOF

# (c) Switch the display-manager to lightdm EXPLICITLY via the symlink — NOT `systemctl enable
#     lightdm`, which fails "Failed to enable unit: Invalid unit name ... instance name specified"
#     and, critically, does NOT (re)create /etc/systemd/system/display-manager.service (owner
#     incident 2026-07-04: the missing symlink brought the box up with no DM → black wall). Guard:
#     only after lightdm's unit file actually exists on disk (it was just installed in (a)).
[ -f /lib/systemd/system/lightdm.service ] \
    || fail "#504: lightdm.service unit missing after install — refuse to switch the DM symlink"
ln -sf /lib/systemd/system/lightdm.service /etc/systemd/system/display-manager.service
echo "  #504: display-manager.service → lightdm (openbox autologin for ${DESKTOP_USER})"

# (d) Disable the desktop-bloat services still running on the appliance. KEEP, NEVER touched:
#     avahi (NDI mDNS — CRITICAL), sshd, dantesync, remoteos-mcp (the MCP agent), NetworkManager,
#     lightdm. `disable --now` also stops each; the per-service `|| true` keeps this idempotent and
#     robust to a static/alias/absent unit (colord is `static`).
local GNOME_PURGE_PKGS GNOME_TO_PURGE p svc
for svc in cups cups-browsed bluetooth ModemManager colord switcheroo-control gnome-remote-desktop; do
    [ "$svc" = bluetooth ] && [ "$KEEP_BT" = keep-bluetooth ] && continue
    systemctl disable --now "$svc" >/dev/null 2>&1 || true
done
# gdm3 is handled SEPARATELY and deliberately WITHOUT `--now` (review finding, 2026-07-05): on a
# genuine from-scratch GNOME box (not this already-openbox live box) gdm3 still OWNS the current
# :0 session that setup-imag.sh step 17 launches OBS into (DISPLAY=:0, $UBUS captured back in step 5) —
# stopping it immediately would kill that X server + D-Bus session mid-provision and fail step 17's
# launch against a now-dead :0. `disable` alone (no `--now`) only stops gdm3 from starting again on
# the NEXT boot, the same "takes effect on the next boot" convention already used by the kernel
# (step 7) / CPU-isolation (step 8) / NVIDIA (step 9) changes above — the actual handover to
# lightdm+openbox happens at the reboot this script deliberately does not perform.
systemctl disable gdm3 >/dev/null 2>&1 || true
if [ "$KEEP_BT" = keep-bluetooth ]; then
    systemctl enable --now bluetooth >/dev/null 2>&1 \
        || fail "keep-bluetooth: could not enable bluetooth -- the operator's Bluetooth mouse would not reconnect"
    echo "  kiosk: bluetooth kept enabled (box fact keep-bluetooth)"
fi
echo "  #504: disabled cups/cups-browsed/bluetooth/ModemManager/colord/switcheroo-control/gnome-remote-desktop now; gdm3 disabled for next boot (avahi/sshd/dantesync/remoteos-mcp/NetworkManager/lightdm kept)"

# (e) Purge the GNOME desktop bloat — the owner's EXPLICIT package list (#504). NEVER a bare
#     `apt-get autoremove`: that would sweep every now-orphaned FORWARD dependency (an unbounded
#     cascade — the exact hazard the owner called out; it could reach ssh/NetworkManager helpers).
#     apt's own purge cascade removes only the REVERSE-deps that DEPEND on these listed packages
#     (ubuntu-session, ubuntu-desktop-minimal, the desktop-icons-ng extension) — bounded and safe
#     (SIMULATED 2026-07-05: 11 pkgs removed, NONE of sshd/NetworkManager/lightdm/avahi/dantesync/
#     remoteos-mcp). Scope the purge to packages ACTUALLY installed so the command is idempotent and
#     never aborts on an absent package (`firefox` may be a snap-only stub, `libreoffice` isn't on
#     this box, a re-run has nothing left) — the explicit owner set stays literal in GNOME_PURGE_PKGS.
GNOME_PURGE_PKGS="gnome-shell gdm3 nautilus firefox gnome-remote-desktop \
    gnome-shell-extension-ubuntu-dock gnome-shell-extension-ubuntu-tiling-assistant \
    gnome-shell-extension-appindicator libreoffice-core"
GNOME_TO_PURGE=""
for p in $GNOME_PURGE_PKGS; do
    # Same install-status idiom as step 9's driver check: a bare `dpkg -s` exit code is NOT enough
    # (it exits 0 for a removed-not-purged package in "deinstall ok config-files" state) — match the
    # Status field content. `>/dev/null` (not `-q`) mirrors that step's convention.
    if dpkg -s "$p" 2>/dev/null | grep '^Status: install ok installed' >/dev/null; then
        GNOME_TO_PURGE="$GNOME_TO_PURGE $p"
    fi
done
if [ -n "$GNOME_TO_PURGE" ]; then
    DEBIAN_FRONTEND=noninteractive apt-get purge -y $GNOME_TO_PURGE \
        || fail "#504: GNOME desktop purge failed —$GNOME_TO_PURGE"
    echo "  #504: purged GNOME desktop packages —$GNOME_TO_PURGE"
else
    echo "  #504: no GNOME desktop packages left to purge (already a clean kiosk)"
fi

# Defense-in-depth re-assert (review finding, 2026-07-05): gdm3's dpkg postrm runs AFTER the DM
# symlink switch in (c) above — re-verify it still points at lightdm rather than trusting the
# earlier switch blindly. A postrm that silently re-pointed display-manager.service back is exactly
# the black-wall failure mode this whole step exists to prevent; refuse to leave the box in an
# uncertain DM state rather than discover it only on the next reboot.
obs_box_same_unit /etc/systemd/system/display-manager.service /lib/systemd/system/lightdm.service \
    || fail "#504: display-manager.service no longer points at lightdm after the GNOME purge — refuse to leave the box with an uncertain display manager"

# (f) issue 1357: the panel-brightness keys (openbox has no brightness handler) -- helper + udev rule +
#     the two keybinds merged into the kiosk rc.xml. After (a), which installs openbox and its stock rc.xml.
obs_box_brightness_keys "$DESKTOP_USER"

echo "  NOTE: the kiosk (lightdm+openbox) takes over the SESSION on the NEXT boot — this script does not reboot the box"
}

# --- issue 1418: the iGPU panel sink of an NVIDIA-primary box runs on the Xorg modesetting DDX ----------
# On a notebook NVIDIA renders the X screen (PRIME nvidia-primary) and the iGPU scans the panel out as a
# reverse-PRIME sink. strih-snv (AMD Radeon 780M), 11/12.10.2026: Xorg loaded xf86-video-amdgpu for that
# sink, the panel's commit_work blocked forever on an NVIDIA fence and the panel froze for 18 h while OBS
# kept rendering. The Intel notebook before it ran the SAME topology with the sink on modesetting and was
# stable. An /etc/X11/xorg.conf.d OutputClass `MatchDriver "amdgpu"` -> `Driver "modesetting"` does NOT
# move it: tried live as 20-, 05- and a same-named 10-amdgpu.conf shadow, Xorg still applied the package's
# own /usr/share/X11/xorg.conf.d/10-amdgpu.conf `Driver "amdgpu"` first. Removing the DDX package (which
# owns that file) does: Xorg then logs `Failed to load module "amdgpu"` (harmless) and runs the sink on
# modesetting with glamor. The OutputClass file is still written, belt and braces, never relied on.

# obs_box_amdgpu_ddx_package -> the package of the amdgpu Xorg DDX (the grader reads the same name).
obs_box_amdgpu_ddx_package() {
    printf '%s' xserver-xorg-video-amdgpu
}

# obs_box_amdgpu_ddx_packages -> the DDX and the meta package that depends on it (its only reverse
# dependency on strih-snv), one per line, in removal order.
obs_box_amdgpu_ddx_packages() {
    printf '%s\n' "$(obs_box_amdgpu_ddx_package)" xserver-xorg-video-all
}

# obs_box_pkg_status PKG -> PKG's dpkg status (`install ok installed`, `deinstall ok config-files` ...),
# or `absent` when dpkg knows no such package; prints nothing and returns 1 when dpkg-query itself
# fails (a broken status file, no dpkg-query), so an unread state is never "not installed". The grader
# embeds it via declare -f.
obs_box_pkg_status() {
    local out rc=0
    out="$(LC_ALL=C dpkg-query -W -f='${Status}' "${1:?obs_box_pkg_status: PKG required}" 2>&1)" || rc=$?
    if [ "$rc" = 0 ]; then
        printf '%s' "$out"
        return 0
    fi
    case "$out" in *"no packages found"*) printf 'absent'; return 0 ;; esac
    return 1
}

# obs_box_pkg_status_absent STATUS -> rc 0 when the obs_box_pkg_status STATUS means "not on the box"
# (absent, not-installed, only config files left); any unpacked state (installed, half-installed,
# unpacked, ...) and an empty (unread) status return 1.
obs_box_pkg_status_absent() {
    case "${1-}" in
        absent | *" not-installed" | *" config-files") return 0 ;;
        *) return 1 ;;
    esac
}

# obs_box_amdgpu_ddx_remove -- remove the installed obs_box_amdgpu_ddx_packages. Called by the NVIDIA
# step AND again by obs_box_kiosk after its Xorg install (which can pull the DDX back in). A dry run first
# (`apt-get -s remove`): a removal that would take ANY other package (xserver-xorg, the session) is
# refused, never done. Idempotent (nothing installed = nothing touched), fails loud via the caller's
# fail(); the dpkg lock wait is the baseline's apt.conf.d timeout. Takes effect at the next X start.
obs_box_amdgpu_ddx_remove() {
    local p st sim r
    local -a ddx_present=() extra=()
    while IFS= read -r p; do
        st="$(obs_box_pkg_status "$p")" \
            || fail "igpu-sink: dpkg-query cannot read ${p} -- cannot tell whether the amdgpu DDX is installed (dpkg -l ${p})"
        if ! obs_box_pkg_status_absent "$st"; then ddx_present+=("$p"); fi
    done < <(obs_box_amdgpu_ddx_packages)
    if [ "${#ddx_present[@]}" = 0 ]; then
        echo "  igpu-sink: $(obs_box_amdgpu_ddx_package) not installed -- unchanged (the iGPU sink runs on modesetting)"
        return 0
    fi
    sim="$(LC_ALL=C apt-get -s remove "${ddx_present[@]}" 2>&1)" \
        || fail "igpu-sink: apt-get -s remove ${ddx_present[*]} failed: ${sim}"
    while IFS= read -r r; do
        case " ${ddx_present[*]} " in *" ${r} "*) ;; *) extra+=("$r") ;; esac
    done < <(awk '$1 == "Remv" { print $2 }' <<<"$sim" || true)
    if [ "${#extra[@]}" != 0 ]; then
        fail "igpu-sink: removing ${ddx_present[*]} would also remove ${extra[*]} -- refused; remove the amdgpu DDX by hand without them (issue 1418)"
    fi
    DEBIAN_FRONTEND=noninteractive apt-get remove -y "${ddx_present[@]}" >/dev/null \
        || fail "igpu-sink: apt-get remove ${ddx_present[*]} failed -- the iGPU panel sink keeps the amdgpu DDX that froze strih-snv"
    echo "  igpu-sink: removed ${ddx_present[*]} -- the iGPU panel sink runs on modesetting from the next X start"
}

# obs_box_igpu_sink_conf_path -> where the belt-and-braces OutputClass file lives.
obs_box_igpu_sink_conf_path() {
    printf '%s' /etc/X11/xorg.conf.d/20-igpu-sink-modesetting.conf
}

# obs_box_igpu_sink_modesetting_conf -> that OutputClass text (an Intel iGPU already gets modesetting;
# the class only matches the amdgpu kernel driver).
obs_box_igpu_sink_modesetting_conf() {
    cat <<'EOF'
# camera-box shared OBS-box baseline (issue 1418): the iGPU panel sink of the NVIDIA-primary
# (reverse-PRIME) session runs on the Xorg modesetting DDX, never xf86-video-amdgpu. strih-snv
# 11/12.10.2026: on the amdgpu DDX the panel's commit_work blocked forever on an NVIDIA fence and the
# panel froze; the Intel notebook before it ran modesetting and was stable. Belt and braces only: this
# class loses to the xserver-xorg-video-amdgpu package's own 10-amdgpu.conf, so the baseline removes
# that package.
Section "OutputClass"
    Identifier "igpu-sink-modesetting"
    MatchDriver "amdgpu"
    Driver "modesetting"
EndSection
EOF
}

# obs_box_igpu_sink_modesetting [DEST [OWNER]] -- the item: obs_box_amdgpu_ddx_remove, then the
# OutputClass file at DEST (default obs_box_igpu_sink_conf_path, OWNER root:root, 0644) through
# obs_box_write_if_changed. Called by obs_box_nvidia_prime (the system half) on a discrete-NVIDIA box
# only; fails loud via the caller's fail().
obs_box_igpu_sink_modesetting() {
    local dest="${1:-$(obs_box_igpu_sink_conf_path)}" owner="${2:-root:root}"
    obs_box_amdgpu_ddx_remove
    mkdir -p "${dest%/*}" || fail "igpu-sink: could not create ${dest%/*}"
    obs_box_igpu_sink_modesetting_conf | obs_box_write_if_changed "$dest" 0644 "$owner" "igpu-sink (OutputClass, belt and braces, issue 1418)"
}

# obs_box_touchpad BOX -- the imag step 25 (#779) touchpad usability: tap-to-click + natural scroll +
# ScrollPixelDistance 50 as an Xorg libinput InputClass (the heredoc carries no `$`/backtick, so it is
# expanded only for the BOX name in its comment).
obs_box_touchpad() {
    local BOX="${1:?obs_box_touchpad: BOX required}"
# imag-nb is a NOTEBOOK; the operator drives its touchpad directly. tap-to-click + natural scrolling
# + a gentler scroll step were set LIVE (2026-07-15) as /etc/X11/xorg.conf.d/30-touchpad-tap.conf but
# NEVER provisioned here -- so a reimage silently dropped them (the same "provisioning gap hidden by a
# hand patch" class issue 840 documented for imag-obs-start.sh). Bake the file in so a reprovision
# reproduces the live-verified libinput InputClass byte-for-byte. The four Option values match what
# is live on the box; ScrollPixelDistance 50 is the user's final tuning (the libinput default 15 is
# far too sensitive). verify-imag.sh check (w) reads this file back and fails loud if it is dropped.
mkdir -p /etc/X11/xorg.conf.d
cat > /etc/X11/xorg.conf.d/30-touchpad-tap.conf <<EOF
# ${BOX} touchpad usability (#779) -- tap-to-click + natural scroll + gentler scroll,
# reprovision-durable (matches the live-verified 30-touchpad-tap.conf on the box).
Section "InputClass"
    Identifier "touchpad tap-to-click"
    MatchIsTouchpad "on"
    Driver "libinput"
    Option "Tapping" "on"
    Option "TappingDrag" "on"
    Option "NaturalScrolling" "on"
    Option "ScrollPixelDistance" "50"
EndSection
EOF
echo "  #779: /etc/X11/xorg.conf.d/30-touchpad-tap.conf provisioned (tap-to-click + natural scroll + ScrollPixelDistance 50)"
}

# --- issue 1357: panel-brightness keys (the kiosk brightness facet) -------------------------------
# Openbox, the kiosk WM, has no brightness handler, so a notebook's Fn brightness keys did nothing (the
# owner hit it on strih-lx during the 24.9.2026 production; imag had the same problem). A hand fix went
# live on strih-lx (24.9.2026 16:58); this facet carries the SAME text to every kiosk box. The helper,
# the udev rule and the keybind lines render the live strih-lx text byte-for-byte (read 25.9.2026).
# obs_box_kiosk runs obs_box_brightness_keys; the shared grader's `brightness` row grades it.

# obs_box_brightness_helper_text -> /usr/local/bin/obs-box-brightness: steps the first sysfs backlight by
# a tenth of max_brightness (at least 1), clamps to max and a 5 % floor, exits 2 on a bad argument and 1
# (logged) when the box has no backlight device.
obs_box_brightness_helper_text() {
    cat <<'HELPER_EOF'
#!/bin/bash
# Laptop panel brightness step for the OBS-box kiosk (openbox has no brightness handler).
# Usage: obs-box-brightness up|down   (bound to XF86MonBrightnessUp/Down in ~/.config/openbox/rc.xml)
set -euo pipefail
dir="$(ls -d /sys/class/backlight/* 2>/dev/null | head -1)"
[ -n "$dir" ] || { logger -t obs-box-brightness "no backlight device"; exit 1; }
max="$(cat "$dir/max_brightness")"; cur="$(cat "$dir/brightness")"
step=$(( max / 10 )); [ "$step" -ge 1 ] || step=1
case "${1:-}" in
  up)   new=$(( cur + step )) ;;
  down) new=$(( cur - step )) ;;
  *)    echo "usage: $0 up|down" >&2; exit 2 ;;
esac
[ "$new" -gt "$max" ] && new="$max"
min=$(( max / 20 )); [ "$new" -lt "$min" ] && new="$min"
echo "$new" > "$dir/brightness"
HELPER_EOF
}

# obs_box_backlight_udev_rule -> /etc/udev/rules.d/90-obs-box-backlight.rules: every backlight node
# becomes group `video` + group-writable when it appears, so the desktop user's key binding can write it
# without root.
obs_box_backlight_udev_rule() {
    cat <<'RULE_EOF'
# OBS-box kiosk: let the desktop user (group video) step the panel backlight from openbox key bindings.
ACTION=="add", SUBSYSTEM=="backlight", RUN+="/bin/chgrp video /sys/class/backlight/%k/brightness", RUN+="/bin/chmod g+w /sys/class/backlight/%k/brightness"
RULE_EOF
}

# obs_box_brightness_keybinds_xml -> the three rc.xml lines (a comment + the Up/Down keybinds, indented as
# openbox's own <keyboard> entries). ONE source for the merge below AND the grader (which embeds this
# function and checks both keybind lines in the effective rc.xml).
obs_box_brightness_keybinds_xml() {
    cat <<'KEYS_EOF'
  <!-- OBS-box kiosk: panel brightness keys (openbox has no brightness handler) -->
  <keybind key="XF86MonBrightnessUp"><action name="Execute"><command>/usr/local/bin/obs-box-brightness up</command></action></keybind>
  <keybind key="XF86MonBrightnessDown"><action name="Execute"><command>/usr/local/bin/obs-box-brightness down</command></action></keybind>
KEYS_EOF
}

# obs_box_openbox_rc_with_brightness_keys (stdin: an openbox rc.xml) -> the same rc.xml with every
# MISSING keybind line (plus the comment, when absent) inserted right before the first </keyboard> line.
# Pure filter, the kiosk's ONE rc.xml writer: an rc.xml that already carries both keybinds comes back
# unchanged (idempotent), operator content is never replaced or reordered (the root-menu binding that
# verify-imag checks stays as it is). No </keyboard> line -> rc 1 and NO output (never a partial file).
obs_box_openbox_rc_with_brightness_keys() {
    local rc line trimmed comment="" block="" need=0
    rc="$(cat; printf x)"
    rc="${rc%x}"
    while IFS= read -r line; do
        trimmed="${line#"${line%%[![:space:]]*}"}"
        case "$rc" in *"$trimmed"*) continue ;; esac
        case "$trimmed" in
            '<!--'*) comment="${line}"$'\n' ;;
            *) block="${block}${line}"$'\n'; need=1 ;;
        esac
    done < <(obs_box_brightness_keybinds_xml)
    if [ "$need" = 0 ]; then
        printf '%s' "$rc"
        return 0
    fi
    printf '%s' "$rc" | awk -v block="${comment}${block}" '
        { lines[NR] = $0 }
        !found && /^[[:space:]]*<\/keyboard>/ { at = NR; found = 1 }
        END {
            if (!found) exit 1
            for (i = 1; i <= NR; i++) { if (i == at) printf "%s", block; print lines[i] }
        }'
}

# obs_box_brightness_keys DESKTOP_USER -- install the facet: the helper (root 0755), the udev rule + a
# backlight trigger so it applies now, the desktop user in group `video`, and the keybinds merged into the
# user's ~/.config/openbox/rc.xml (seeded from the stock /etc/xdg/openbox/rc.xml when the user has none --
# openbox reads the user file first). Every file goes through obs_box_write_if_changed (compared, rewritten
# only on a difference, logged). The running openbox picks the keybinds up at the next login (the kiosk
# takes over at the next boot anyway). Fails loud when a file cannot be written or rc.xml has no
# <keyboard> section.
obs_box_brightness_keys() {
    local DESKTOP_USER="${1:?obs_box_brightness_keys: desktop user required}"
    local home grp rc_user rc_src merged rule_new=1
    grp="$(id -gn "$DESKTOP_USER")" || fail "brightness keys: unknown desktop user ${DESKTOP_USER}"
    home="$(getent passwd "$DESKTOP_USER" | cut -d: -f6 || true)"
    [ -n "$home" ] || home="/home/${DESKTOP_USER}"
    obs_box_brightness_helper_text | obs_box_write_if_changed /usr/local/bin/obs-box-brightness 0755 root:root "brightness helper"
    if cmp -s /etc/udev/rules.d/90-obs-box-backlight.rules <(obs_box_backlight_udev_rule); then rule_new=0; fi
    obs_box_backlight_udev_rule | obs_box_write_if_changed /etc/udev/rules.d/90-obs-box-backlight.rules 0644 root:root "backlight udev rule"
    # Reload + re-trigger only when the rule text changed: a re-run (every strih-lx genlock deploy runs
    # setup-strih) leaves udev alone, and the rule applies at every boot anyway.
    if [ "$rule_new" = 1 ]; then
        if udevadm control --reload-rules && udevadm trigger --subsystem-match=backlight --action=add; then
            echo "  brightness keys: udev reloaded + backlight nodes re-triggered (group video may write them now)"
        else
            echo "  WARNING: brightness keys: udevadm reload/trigger failed -- the backlight rule applies at the next boot"
        fi
    fi
    if case " $(id -nG "$DESKTOP_USER") " in *" video "*) true ;; *) false ;; esac; then
        echo "  brightness keys: ${DESKTOP_USER} already in group video"
    else
        usermod -aG video "$DESKTOP_USER" || fail "brightness keys: could not add ${DESKTOP_USER} to group video"
        echo "  brightness keys: ${DESKTOP_USER} added to group video (takes effect at the next login)"
    fi
    [ -d "${home}/.config" ] || install -d -o "$DESKTOP_USER" -g "$grp" -m 755 "${home}/.config"
    install -d -o "$DESKTOP_USER" -g "$grp" -m 755 "${home}/.config/openbox"
    rc_user="${home}/.config/openbox/rc.xml"
    if [ -f "$rc_user" ]; then rc_src="$rc_user"; else rc_src=/etc/xdg/openbox/rc.xml; fi
    [ -f "$rc_src" ] || fail "brightness keys: no openbox rc.xml to merge into (${rc_src} missing -- is openbox installed?)"
    merged="$(obs_box_openbox_rc_with_brightness_keys < "$rc_src")" \
        || fail "brightness keys: ${rc_src} has no </keyboard> section -- add the two XF86MonBrightness keybinds by hand"
    printf '%s\n' "$merged" | obs_box_write_if_changed "$rc_user" 0644 "${DESKTOP_USER}:${grp}" "openbox rc.xml brightness keys"
}
