#!/usr/bin/env bash
#
# verify-device.sh -- POST-REBOOT runtime acceptance gate for a freshly-provisioned camera-box
# appliance (#454). This is the fourth and final phase of the unified provisioning method:
#
#   1. build USB    scripts/create-usb-linux.sh --target-disk /dev/sdX --yes   (#448)
#   2. boot + reach the box over SSH                                          (:22 up)
#   3. provision    sudo ./setup-device.sh NAME                               (#450, name-resolved)
#   4. ACCEPT       scripts/verify-device.sh NAME                             (#454, THIS script)
#
# Distinct from setup-device.sh's own STEP 19 -- that is an INSTALL-TIME (pre-reboot, still inside
# the live setup session) FILE-PRESENCE check ("is the binary/NDI .so on disk"). This script is a
# RUNTIME check: it runs AFTER the box has rebooted, connects fresh over SSH, and re-derives every
# fact from LIVE signals (systemd state, journald, `ls -la`, `avahi-browse`) -- never trusting the
# installer's own claim of success. The honest proof a fresh box is "identically built" to the
# fleet (#454's acceptance criterion).
#
# Composes ALREADY-TESTED signals instead of reinventing them:
#   - scripts/camera-set.sh          camera_resolve() -- NAME -> IP / CAMERA_GENLOCK_FPS (#24/#451)
#   - scripts/lib/ndi-alive.sh       emit_ok_grep_pattern() / fatal_grep_pattern() (#451)
#   - scripts/clock-offset-guard.sh  offset_us_from_journal() / offset_check() /
#                                    ptp_locked_from_journal() (#8)
#
# Usage:
#   scripts/verify-device.sh NAME
#   scripts/verify-device.sh --help
#
# NAME is resolved via scripts/camera-set.sh (cam1-6), case-insensitive (CAM5 / cam5 both work) --
# same convention as setup-device.sh (#450). An unknown name fails loud through camera-set.sh's own
# fail-closed `case` (never silently certifies the wrong box).
#
# Env:
#   SSH_USER          SSH user for the box (default: root)
#   CAM_PW            box root password (default: newlevel -- the fleet's standard dev password,
#                      same fallback used throughout scripts/deploy-fleet.sh, clock-offset-guard.sh,
#                      setup-device.sh; override via env for a box with a different password)
#   SSH_TIMEOUT       SSH connect timeout in seconds (default: 10)
#   CLOCK_GUARD_BOUND_US  dantesync clock-offset bound in microseconds (default: 2000, #8)
#
# Checks (all must pass):
#   (a) camera-box --version is a valid, well-formed fleet version string
#   (b) systemctl is-active camera-box == active
#   (c) NDI sender is streaming (journalctl emit-fps / genlock-report line), no FATAL/panic
#   (d) dantesync running + logging (LIVENESS, #600) THEN PTP servo LOCKED + a FRESH clock offset
#       within bound (#550/#591 -- rejects a stale boot-step "[NTP] offset:" line; a fresh
#       out-of-bound offset is a hard desync FAIL). A died/hung daemon (journal not advancing vs
#       the box clock) hard-FAILs before the stale lock/offset content is ever trusted.
#   (e) genlock.conf drop-in present, CAMERA_BOX_GENLOCK_FPS matches camera-set.sh's per-cam value
#   (f) cpu-affinity.conf drop-in present (CPUAffinity=<isolated core>)
#   (g) /usr/lib/ndi/libndi.so.6 is a root-owned symlink chain to a root-owned regular file
#   (h) avahi mDNS NDI discovery sees this box's NDI source (avahi-browse -tp _ndi._tcp)
#   (i) capture chroma metric reports "colour", not "grayscale" (#299 regression signal)
#   (j) root filesystem mounted read-only (#547 -- ro appliance)
#   (k) exactly ONE installed kernel, equal to the running one (#547; optional KERNEL_PIN match)
#   (l) fwupd purged (#547 -- it holds a write handle that blocks the ro remount)
#   (m) systemd-networkd-wait-online masked (#547 -- avoids the 120s boot stall)
#   (n) core-isolation kernel cmdline: isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)
#   (o) NDI runtime pinned to the fleet version (NDI_VERSION_PIN, default 6.3.2 -- #132/#547)
#   (p) [REMOVED, #528] the per-box config.toml [display] / ExecStart --display acceptance check
#       no longer applies -- the HDMI cameraman preview is UNCONDITIONAL and fleet-wide (baked
#       into the binary's own default), not a per-box config a box could lose or drift from.
#   (q) WARNING only (never fails the gate): stale `.bak` cruft under /usr/lib/ndi or the systemd
#       drop-in dir (#453 -- inert leftovers from a manual NDI upgrade / a stale drop-in edit;
#       setup-device.sh self-heals this on the box's next provisioning pass)
#   (r) dantesync is the SOLE timesync authority -- NO competing timesync daemon (systemd-timesyncd
#       / chrony / ntp / ntpsec / openntpd) is INSTALLED (even masked), ACTIVE, or enabled (#591;
#       cam5/6 ran systemd-timesyncd alongside dantesync -> a real 5.28s clock desync)
#   (s) /var/log tmpfs is bounded against runaway growth -- /etc/logrotate.d/rsyslog has a `size`
#       cap AND a systemd timer drop-in checks far more often than the stock daily cadence (#679;
#       a chatty logger filled the fixed 50MB tmpfs in ~4-5 days and crashed cam2's
#       camera-box.service). SUPERSEDED by #762 once rsyslog is genuinely purged (the (u) check
#       below) -- /etc/logrotate.d/rsyslog is one of rsyslog's OWN conffiles and is removed with
#       it, so this check passes as "N/A, superseded" on a #762-hardened box instead of FAILing
#       on a file that can no longer exist
#   (t) `fuser` (psmisc) is installed -- a fresh cam2 clone had none at all, which false-FAILed
#       rig-mode.sh's #464 KMS-held check AND silently no-op'd recording-e2e.sh's capture-release
#       busy-wait (fuser exits 127 -> the wait's `while fuser ...` condition reads false
#       immediately, same as "already released") -- (#743)
#   (u) rsyslog is PURGED (not merely masked) AND journald has a RuntimeMaxUse=20M drop-in
#       (#762 -- rsyslog is redundant on this appliance, journald already captures everything;
#       a live cam1 incident showed a full /var/log tmpfs put rsyslogd into a write-error
#       feedback loop burning 42.8% CPU and starving the camera-box send path)
#   (v) cam2 ONLY: the PERMANENT devel-mode dual-QR painter (cam2-painter.service) is installed,
#       active, and genuinely painting (presenter-aware journal read, KMS-or-fbdev per #464) --
#       AND camera-box's own display thread is permanently disabled on this box so it can never
#       contest /dev/fb0 with the painter (#863)
#   (w) the installed /etc/udev/rules.d/99-camera-box.rules wires the video4linux "add" event to
#       the guarded helper script (never the fleet's old UNCONDITIONAL restart, #894) AND the LIVE
#       capture grabber's USB power/control currently reads "on" -- a box that silently drifted
#       back to `auto` (the #894 amplifying re-enumeration feedback loop) FAILS this check instead
#       of degrading invisibly. N/A (not a FAIL) when the box has no capture grabber fitted at all
#       (cam4, #828).
#   (x) ffmpeg is installed AND runs (`ffmpeg -version`) -- the #930 lipsync-test-mode runtime
#       dependency (scripts/lipsync-test-mode.sh); any box may take cam2's painter role, so this
#       is checked fleet-wide, never cam2-only.
#   (x2) mpv is installed AND runs (`mpv --version`) -- the #1187 lipsync-test-mode DRM/KMS
#       playback runtime (mpv --vo=drm replaced the legacy raw-fbdev ffmpeg write, issue 1176);
#       checked fleet-wide like (x), never cam2-only.
#   (y) camera-box.service has the ExecStartPre device-free bake-in (drop-in wired to the helper,
#       helper stops the stray E2E burn UNIT + pkills the burn, never the painter) so every start
#       frees /dev/video instead of crash-looping on "Device or resource busy" (#772).
#   (z) publish-30p.conf drop-in present with CAMERA_BOX_PUBLISH_30P=1 (setup-device.sh STEP 7 bakes
#       it) AND the box is ACTUALLY publishing the secondary "CAMn (30p)" 30fps blend stream right
#       now (the issue-792 publisher's own journal output). A re-provisioned box that lost the
#       drop-in, or an old binary predating issue 792, FAILs instead of regressing to 60p-only (#1087).
#   (aa) interkom audio bake-in (#782): /etc/asound.conf is the by-NAME form (CARD=HID, not the old
#       enumeration-time card NUMBER that dangles on re-enumeration), alsa-utils is installed, and
#       the live `amixer -c HID` Mic/PCM percents match this box's per-box table (cam1-4 75%/79%,
#       cam5-7 80%/94%). A re-provisioned box that drifts back to a dangling config or the CSCTEK
#       power-on gain (Mic 91%) FAILs.
#   (ab) RemoteOS MCP control-channel agent (#1066): remoteos-mcp.service (the linux-camN MCP surface
#       on :8092, provisioned by setup-device.sh STEP 17b) is ENABLED (reboot-survival) AND active
#       AND :8092 is listening. A fresh box that never provisioned the agent FAILs instead of coming
#       up with a dead MCP surface.
#   (ac) realtime-isolation drift (issue 899) -- WARNING ONLY (never fails the gate for now): reports
#       whether the kernel is PREEMPT_RT (defect 1 -- the fleet is not yet, informational) and whether
#       the xhci capture IRQ is routed OFF the isolated grab core on a stock kernel (defect 3 -- the
#       fix is in src/affinity.rs and lands on the next fleet redeploy; a pre-899 box WARNs). The flip
#       to a hard FAIL is a follow-up gated on the redeploy (docs/runbooks/899-realtime-isolation.md).
#
# Exit: 0 iff every check passes. Non-zero if ANY check FAILs or is UNREADABLE (test-strictness --
# an unreachable/unreadable check is a FAIL, never a silent pass).

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/cli-log.sh
. "$HERE/lib/cli-log.sh"         # RED/GREEN/YELLOW/BLUE/NC + log()/info()/warn()/err() (#559/#568)
# shellcheck source=scripts/camera-set.sh
. "$HERE/camera-set.sh"          # camera_resolve() -- NAME -> IP / CAMERA_GENLOCK_FPS (#24/#451)
# shellcheck source=scripts/lib/ndi-alive.sh
. "$HERE/lib/ndi-alive.sh"       # emit_ok_grep_pattern() / fatal_grep_pattern() (#451)
# shellcheck source=scripts/lib/timesync-authority.sh
. "$HERE/lib/timesync-authority.sh"  # dpkg_status_installed/timesync_daemon_verdict/
                                      # timesync_authority_verdict (#591, shared with
                                      # drift-guard.sh's --check-imag facet since #596)
# shellcheck source=scripts/lib/log-bound.sh
. "$HERE/lib/log-bound.sh"       # log_bound_verdict/log_bound_gather_remote_snippet (#679)
# shellcheck source=scripts/lib/log-diet.sh
. "$HERE/lib/log-diet.sh"        # log_diet_provision_verdict/log_diet_gather_remote_snippet (#762)
# shellcheck source=scripts/lib/capture-rate-guard.sh
. "$HERE/lib/capture-rate-guard.sh"  # invocation-id-scoped journalctl builder (#694, shared
                                     # with deploy-fleet.sh + upgrade-fleet-ndi.sh)
# shellcheck source=scripts/lib/v4l2-neutral.sh
. "$HERE/lib/v4l2-neutral.sh"    # v4l2_neutral_resolve_node_cmd -- resolves the LIVE capture
                                 # node for the (w) power/control drift read (#894)
# shellcheck source=scripts/lib/udev-camera-box.sh
. "$HERE/lib/udev-camera-box.sh" # udev_camera_box_rule_is_burn_gated/
                                 # udev_camera_box_grabber_power_control_read_cmd/_from_output/
                                 # _power_control_is_on -- the (w) check (#894)
# shellcheck source=scripts/lib/camera-box-free-device.sh
. "$HERE/lib/camera-box-free-device.sh" # camera_box_free_device_dropin_wired/
                                        # camera_box_free_device_script_is_burn_scoped -- the (y)
                                        # ExecStartPre device-free bake-in check (#772)
# shellcheck source=scripts/lib/interkom-audio.sh
. "$HERE/lib/interkom-audio.sh"  # interkom_asound_by_name_count/interkom_amixer_pct/interkom_mic_pct/
                                 # interkom_pcm_pct -- the (aa) interkom-audio bake-in check (#782)
# clock-offset-guard.sh is sourced ONLY for its pure functions; its own
# `[ "${BASH_SOURCE[0]}" != "${0}" ]` guard skips clock-offset-guard.sh's own `main "$@"` flow.
# shellcheck source=scripts/clock-offset-guard.sh
. "$HERE/clock-offset-guard.sh"  # offset_us_from_journal/offset_check/ptp_locked_from_journal/
                                 # _short_iso_epoch/dantesync_offset_verdict/freshest_offset_us (#595)

SSH_USER="${SSH_USER:-root}"
CAM_PW="${CAM_PW:-newlevel}"
SSH_TIMEOUT="${SSH_TIMEOUT:-10}"
DEVICE_CLOCK_BOUND_US="${CLOCK_GUARD_BOUND_US:-2000}"
# #837: max tolerated SPREAD (max-min) of the FRESH offset samples, in us -- the journal twin of
# the #836 HTTP spread check. Same 2000us default + DANTESYNC_STABILITY_US knob as the gate's
# GATE_STABILITY_US. A scattered-but-in-bound-median clock now FAILS instead of passing silently.
DEVICE_CLOCK_STABILITY_US="${DANTESYNC_STABILITY_US:-2000}"
# #550/#591: the freshest dantesync "[NTP] offset:" line must be no older than this many seconds
# behind the newest journal line, or the check treats it as STALE (never grading on an aged
# boot-step line -- the #550 bug). dantesync emits a [PTP] servo line every second and an
# [NTP] offset line on a ~30s cadence, so 300s is generous headroom that never false-fails a
# healthy box yet rejects the ~1h-old boot-step line that started #550.
DANTESYNC_OFFSET_FRESHNESS_S="${DANTESYNC_OFFSET_FRESHNESS_S:-300}"
# #600 (#591 review): dantesync LIVENESS bound. The (d) lock/offset checks grade the journal's
# CONTENT, but a died/hung dantesync leaves BOTH signals computed against a STALE journal and would
# PASS (the clock has been free-running/undisciplined the whole time). The dantesync journal must
# have advanced within this many seconds of the box's OWN wall clock, or the daemon is presumed
# hung / not logging. dantesync emits a [PTP] servo line ~1/s, so >60s without a new line means it
# has stopped. Overridable via env like the other bounds.
DANTESYNC_JOURNAL_MAX_AGE_S="${DANTESYNC_JOURNAL_MAX_AGE_S:-60}"
EXPECT_KERNEL="${KERNEL_PIN:-}"                 # optional: also require running kernel == this exact version
NDI_VERSION_PIN="${NDI_VERSION_PIN:-6.3.2}"     # fleet NDI runtime pin (#132/#547)

# =================================================================================================
# PURE functions (no network, no SSH -- unit-tested from tests/verify_device_pure_functions.rs by
# sourcing this file; the BASH_SOURCE guard below skips the live SSH flow when sourced. Same
# convention as scripts/setup-device.sh / scripts/clock-offset-guard.sh.)
# =================================================================================================

# --- (a) version -------------------------------------------------------------------------------

# version_is_valid_format V -> 0 iff V matches the fleet's vMAJOR.MINOR.PATCH[-dev.N] shape (the
# CI dev-channel form, e.g. "1.7.0-dev.244", or a plain release "1.7.0").
version_is_valid_format() {
  printf '%s' "$1" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-dev\.[0-9]+)?$'
}

# version_matches_expected ACTUAL EXPECTED -> 0 iff both are non-empty and identical.
version_matches_expected() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# --- (b) systemd service state -------------------------------------------------------------------

# active_state_is_active TEXT -> 0 iff TEXT (the trimmed `systemctl is-active` output) is exactly
# "active". Any other state (inactive/failed/activating/empty) is NOT active.
active_state_is_active() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "active" ]
}

# --- (c) NDI emit + FATAL scan (reuses scripts/lib/ndi-alive.sh) -------------------------------

# ndi_emit_ok JOURNAL_TEXT -> 0 iff the camera-box journal shows the NDI capture->emit path alive
# (the shared emit_ok_grep_pattern() from ndi-alive.sh -- genlock decimation report / plain
# "Streaming: X fps" / generic sender-ready line).
ndi_emit_ok() {
  printf '%s\n' "$1" | grep -qE "$(emit_ok_grep_pattern)"
}

# ndi_journal_has_fatal JOURNAL_TEXT -> 0 iff the journal contains a crash signature (the shared
# fatal_grep_pattern() from ndi-alive.sh).
ndi_journal_has_fatal() {
  printf '%s\n' "$1" | grep -qE "$(fatal_grep_pattern)"
}

# --- (i) colour capture chroma metric (#299) ----------------------------------------------------

# chroma_state_from_journal JOURNAL_TEXT -> the LAST "capture chroma: ... -> (colour|grayscale...)"
# line from a camera-box journal dump ("" if none seen -- the #299 chroma sample is logged only
# after the first sample lands, so a very-fresh box may not show one yet).
chroma_state_from_journal() {
  printf '%s\n' "$1" | grep -oE 'capture chroma:.*-> (colour|grayscale[^"]*)' | tail -1 || true
}

# chroma_check STATE -> 0 iff STATE ends in "-> colour" (healthy); 2 if "-> grayscale..." (the
# #299 regression signal -- capture card delivering monochrome frames); 3 if STATE is empty
# (UNKNOWN -- no chroma sample seen yet; never treated as a silent pass).
chroma_check() {
  case "$1" in
    *'-> colour') return 0 ;;
    *'-> grayscale'*) return 2 ;;
    *) return 3 ;;
  esac
}

# --- (d) dantesync PTP-lock + offset (reuses scripts/clock-offset-guard.sh) --------------------

# dantesync_locked_ok JOURNAL_TEXT -> 0 iff the dantesync journal's most recent clock event is a
# PTP servo line (ptp_locked_from_journal() == "LOCKED"), never DEGRADED or UNKNOWN.
dantesync_locked_ok() {
  [ "$(ptp_locked_from_journal "$1")" = "LOCKED" ]
}

# _short_iso_epoch() and dantesync_offset_verdict() were ORIGINALLY defined here for #591/#600.
# They are now DEFINED in the sourced scripts/clock-offset-guard.sh (#595) so every caller of the
# freshness-aware offset check -- this script's own (d) check below, PLUS dantesync-gate.sh's #7
# precondition and clock-offset-painter-gate.sh's #326 sweep comparator, which were still exposed
# to the #550-class staleness bug -- shares ONE implementation instead of drifting copies. Behavior
# here is UNCHANGED: both functions are transitively available via the `. "$HERE/clock-offset-
# guard.sh"` source at the top of this file.

# --- (d) dantesync LIVENESS gate (#600 / #591 review) -----------------------------------------
# dantesync_locked_ok / dantesync_offset_verdict grade the journal's CONTENT, but a died or hung
# dantesync leaves both signals computed against a STALE journal and passes (#600): the clock has
# been free-running/undisciplined the whole time. These two helpers gate the CONTENT reads on the
# daemon actually running and logging -- a SEPARATE liveness concern, added AHEAD of them. They do
# NOT change dantesync_locked_ok / dantesync_offset_verdict (the freshness-vs-newest-line model is
# correct for grading the OFFSET).

# dantesync_service_active STATE -> 0 iff STATE (the trimmed `systemctl is-active dantesync` output)
# is exactly "active". inactive / failed / activating / deactivating / "" -> non-zero. Catches the
# DIED case (a dead dantesync = the clock free-running with no discipline).
dantesync_service_active() {
  [ "$1" = active ]
}

# dantesync_journal_fresh JOURNAL BOX_NOW_EPOCH MAX_AGE_S -> echoes "fresh" | "stale". Extracts the
# epoch of the NEWEST `-o short-iso` timestamp line in JOURNAL (reusing _short_iso_epoch + the same
# iso_re as dantesync_offset_verdict) and compares it against BOX_NOW_EPOCH (the box's OWN wall
# clock, from `date +%s` on the box -- so the verifier host's clock never enters the comparison):
#   stale -- the newest line is absent/unparseable, OR BOX_NOW is empty/unparseable (fail-closed),
#            OR (BOX_NOW - newest_epoch) > MAX_AGE_S (the journal has stopped advancing vs the box's
#            own wall clock -> daemon hung / not logging).
#   fresh -- otherwise (incl. a NEGATIVE age: a box clock stepped BACKWARD is NOT stale here -- (r)
#            and the fresh-offset drift path catch a stepped clock; this helper only catches "not
#            advancing").
# Catches the HUNG-but-still-"active" case. JOURNAL must be gathered with `-o short-iso`.
dantesync_journal_fresh() {
  local journal="$1" box_now="$2" max_age="$3"
  local iso_re='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}[+-][0-9]{2}:[0-9]{2}'
  local now_iso now_e
  # Fail-closed: BOX_NOW must be a clean non-negative integer epoch, or we cannot judge freshness.
  if [ -z "$box_now" ] || ! printf '%s' "$box_now" | grep -qE '^[0-9]+$'; then
    printf 'stale\n'; return 0
  fi
  now_iso="$(printf '%s\n' "$journal" | grep -oE "^$iso_re" | tail -1 || true)"
  now_e="$(_short_iso_epoch "$now_iso")"
  if [ -z "$now_e" ]; then
    printf 'stale\n'; return 0
  fi
  if [ "$((box_now - now_e))" -gt "$max_age" ]; then
    printf 'stale\n'
  else
    printf 'fresh\n'
  fi
}

# --- (r) single timesync authority: dantesync ONLY, no competing daemon (#591) -----------------
# The rig's clock master is dantesync (PTP/NTP). A minimalist cambox/imag appliance must run NO
# other timesync daemon -- cam5/cam6 shipped with systemd-timesyncd active ALONGSIDE dantesync,
# causing a real 5.28-second clock desync ([NTP] offset:-5280959us) invisible to weeks of "passing"
# verification. This gate makes a 2nd authority impossible to ship: a competing daemon that is
# INSTALLED (even masked), ACTIVE, or merely enabled/unmasked is a hard FAIL. dantesync is the ONE
# authority that must be present; it is deliberately NOT in the competing set.
#
# dpkg_status_installed() / timesync_enabled_state_neutral() / timesync_daemon_verdict() /
# timesync_authority_verdict() now live in scripts/lib/timesync-authority.sh (#596) -- extracted so
# scripts/drift-guard.sh's --check-imag facet (imag-nb) can share the EXACT same verdict instead of
# duplicating it (mirrors the #595 precedent of moving the offset-verdict pair into
# scripts/clock-offset-guard.sh for the SAME reason). Sourced above; transitively available here.

# --- (e) genlock.conf drop-in --------------------------------------------------------------------

# genlock_dropin_fps TEXT -> the numeric value of CAMERA_BOX_GENLOCK_FPS in TEXT (the contents of
# the camera-box.service.d/genlock.conf drop-in), "" if absent. `|| true` (per the #458 footgun:
# a `grep`-into-`tail`-into-`cut` pipeline with no match fails under pipefail even though `tail`/
# `cut` both succeed on empty input) -- a bare bareword-assignment caller (`X="$(genlock_dropin_fps
# ...)"`, as the live flow below does) must NEVER abort the whole script on a merely-missing value.
genlock_dropin_fps() {
  printf '%s\n' "$1" | grep -oE 'CAMERA_BOX_GENLOCK_FPS=[0-9]+' | tail -1 | cut -d= -f2 || true
}

# genlock_fps_matches ACTUAL EXPECTED -> 0 iff both non-empty and identical.
genlock_fps_matches() {
  [ -n "$1" ] && [ -n "$2" ] && [ "$1" = "$2" ]
}

# --- (f) cpu-affinity.conf drop-in ----------------------------------------------------------------

# cpu_affinity_dropin_value TEXT -> the numeric value of CPUAffinity in TEXT (the contents of the
# camera-box.service.d/cpu-affinity.conf drop-in), "" if absent (#289). `|| true` -- same #458
# footgun as genlock_dropin_fps above (a bare-assignment caller must never abort on a merely-
# missing value).
cpu_affinity_dropin_value() {
  printf '%s\n' "$1" | grep -oE 'CPUAffinity=[0-9]+' | tail -1 | cut -d= -f2 || true
}

# --- (y) publish-30p.conf drop-in + live "CAMn (30p)" blend stream (issue 792, baked into
# provisioning by #1087) ---------------------------------------------------------------------------

# publish_30p_dropin_value TEXT -> the numeric value of CAMERA_BOX_PUBLISH_30P in TEXT (the contents
# of the camera-box.service.d/publish-30p.conf drop-in), "" if absent. `|| true` -- same #458
# footgun as genlock_dropin_fps above (a bare-assignment caller must never abort on a merely-missing
# value).
publish_30p_dropin_value() {
  printf '%s\n' "$1" | grep -oE 'CAMERA_BOX_PUBLISH_30P=[0-9]+' | tail -1 | cut -d= -f2 || true
}

# publish_30p_stream_live JOURNAL -> the COUNT of lines in JOURNAL showing the issue-792 publish-30p
# publisher actually emitting the secondary "(30p)" blend stream: the one-shot startup
# `publish-30p ACTIVE` line, or the recurring `camera_box::publish_30p:` output line. "0" iff none.
# Proves the "(30p)" NDI source is genuinely being published, not merely that the drop-in enabling
# it is on disk. `grep -c` (NEVER -q: -q's early pipe close can SIGPIPE the upstream printf and,
# under pipefail, return non-zero even on a real match) + `|| true` (grep -c exits 1 with a printed
# "0" on no match; the bare-substitution caller must never abort).
publish_30p_stream_live() {
  printf '%s\n' "$1" | grep -cE 'publish-30p ACTIVE|camera_box::publish_30p:' || true
}

# --- (g) libndi root-owned symlink chain --------------------------------------------------------

# ndi_symlink_target TEXT -> echoes the resolved target filename of the `libndi.so.6` entry in
# TEXT (an `ls -la /usr/lib/ndi` listing) IFF it is a SYMLINK owned root:root; prints nothing
# (empty) otherwise. Matches the EXACT filename field (avoids a substring false-match against
# `libndi.so` or `libndi.so.6.3.2.0`).
ndi_symlink_target() {
  awk '
    $0 ~ /->/ {
      n = split($0, f, " ")
      if (n < 9) next
      if (substr(f[1], 1, 1) != "l") next
      if (f[9] != "libndi.so.6") next
      if (f[3] != "root" || f[4] != "root") next
      idx = index($0, "-> ")
      if (idx == 0) next
      print substr($0, idx + 3)
      exit 0
    }
  ' <<< "$1"
}

# ndi_regular_file_root_owned TEXT NAME -> 0 iff TEXT (an `ls -la` listing) has a line whose exact
# filename field is NAME, is a REGULAR file (mode starts with "-"), and is owned root:root.
ndi_regular_file_root_owned() {
  local text="$1" name="$2"
  awk -v want="$name" '
    {
      n = split($0, f, " ")
      if (n < 9) next
      if (f[9] != want) next
      if (substr(f[1], 1, 1) != "-") next
      if (f[3] == "root" && f[4] == "root") { found = 1 }
    }
    END { exit(found ? 0 : 1) }
  ' <<< "$text"
}

# ndi_symlink_chain_ok TEXT -> 0 iff /usr/lib/ndi/libndi.so.6 is a root-owned SYMLINK pointing at a
# root-owned REGULAR file -- the canonical fleet layout. The #445 cam3-outlier layout (real files,
# user-owned -- its manual NDI upgrade never fit the fleet script) FAILS this by design: the
# acceptance gate certifies the CANONICAL build, not cam3's drift.
ndi_symlink_chain_ok() {
  local text="$1" target
  target="$(ndi_symlink_target "$text")"
  [ -n "$target" ] || return 1
  ndi_regular_file_root_owned "$text" "$target"
}

# --- (h) avahi mDNS NDI discovery -----------------------------------------------------------------

# avahi_ndi_discoverable TEXT [WANT] -> 0 iff TEXT (an `avahi-browse -tp _ndi._tcp` dump) shows at
# least one NDI service record (a `+` found or `=` resolved parseable line referencing
# `_ndi._tcp`); when WANT is given, at least one such record must also contain WANT (e.g. "CAM5").
avahi_ndi_discoverable() {
  local text="$1" want="${2:-}" matches
  matches="$(printf '%s\n' "$text" | grep -E '^[+=]' | grep -F '_ndi._tcp' || true)"
  [ -n "$matches" ] || return 1
  [ -z "$want" ] && return 0
  printf '%s\n' "$matches" | grep -qF "$want"
}

# --- (j)-(o) fleet-uniformity invariants (#547) -------------------------------------------------
# Every cambox must be IDENTICAL: read-only root, exactly ONE pinned kernel, fwupd purged,
# systemd-networkd-wait-online masked, the #289/#303 core-isolation cmdline, and the pinned NDI
# runtime. These pure decision functions are unit-tested; the live flow below feeds them real
# post-reboot signals gathered over SSH.

# root_mount_is_readonly OPTS -> 0 iff the FIRST comma-token of a mount-options string is exactly
# "ro" (the kernel always emits ro/rw first). Substring-safe: a rw mount carrying
# "errors=remount-ro" is correctly NOT read as read-only.
root_mount_is_readonly() {
  case "$1" in
    ro | ro,*) return 0 ;;
    *) return 1 ;;
  esac
}

# kernels_uniform_ok BOOT_LS RUNNING -> 0 iff BOOT_LS (an `ls -1 /boot/vmlinuz-*` dump) lists
# EXACTLY ONE installed kernel AND its version equals RUNNING (`uname -r`). Two kernels (the cam4
# drift) or a running/installed mismatch FAILs.
kernels_uniform_ok() {
  local boot_ls="$1" running="$2" count ver
  [ -n "$running" ] || return 1
  count="$(printf '%s\n' "$boot_ls" | grep -c 'vmlinuz-' || true)"
  [ "$count" = "1" ] || return 1
  ver="$(printf '%s\n' "$boot_ls" | grep -oE 'vmlinuz-.*' | head -1 | sed 's#.*vmlinuz-##')"
  [ "$ver" = "$running" ]
}

# fwupd_absent STATE -> 0 iff STATE (trimmed `systemctl is-enabled fwupd` output, or "not-found"
# when the unit/package is gone) shows fwupd is NOT installed. The fleet PURGES fwupd (it held a
# write handle that blocked the ro remount); a unit still present in ANY state (enabled/static/
# disabled/masked) FAILs -- "masked but installed" is not identical to "purged".
fwupd_absent() {
  case "$(printf '%s' "$1" | tr -d '[:space:]')" in
    '' | not-found) return 0 ;;
    *) return 1 ;;
  esac
}

# fwupd_verdict RC STATE -> echoes "unreadable" | "ok" | "present". Guards the false-green unique to
# (l): fwupd_absent treats an EMPTY state as "purged" (pass), but a transient ssh failure on the (l)
# call ALSO yields empty stdout -- so an rc!=0 (unreadable) MUST be a FAIL (the file's
# "unreachable = FAIL, never a silent pass" contract), NOT read as purged. (l) is the only new check
# whose empty value means pass -- (j)/(k)/(m)/(n)/(o) fail-safe on empty -- so only it needs this.
fwupd_verdict() {
  if [ "$1" -ne 0 ] 2>/dev/null; then
    printf 'unreadable\n'
  elif fwupd_absent "$2"; then
    printf 'ok\n'
  else
    printf 'present\n'
  fi
}

# waitonline_masked STATE -> 0 iff STATE (trimmed `systemctl is-enabled
# systemd-networkd-wait-online`) is exactly "masked". The fleet MASKS it -- unmasked it timed out
# 120s and delayed network-online.target, starting camera-box ~123s late (cam3). Any other state
# FAILs.
waitonline_masked() {
  [ "$(printf '%s' "$1" | tr -d '[:space:]')" = "masked" ]
}

# cmdline_has_isolation CMDLINE -> 0 iff /proc/cmdline carries ALL of the core-isolation flags
# isolcpus=3 (#289) + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#303), each as a whole
# space-delimited token (so nohz_full=3 never matches nohz_full=30).
cmdline_has_isolation() {
  local cmdline="$1" tok
  for tok in 'isolcpus=3' 'nohz_full=3' 'rcu_nocbs=3' 'irqaffinity=0-2'; do
    printf '%s' " $cmdline " | grep -qE "[[:space:]]${tok}[[:space:]]" || return 1
  done
  return 0
}

# cpulist_contains LIST CORE -> 0 iff CORE is a member of the Linux cpulist LIST (the kernel
# "0-2" / "0,1,2" / "3" comma+range format that /proc/irq/<n>/smp_affinity_list renders). issue 899.
cpulist_contains() {
  local list="$1" want="$2" part a b n
  local -a _parts
  [ -n "$want" ] || return 1
  IFS=',' read -ra _parts <<< "$list" || true
  for part in "${_parts[@]}"; do
    part="${part//[[:space:]]/}"
    case "$part" in
      *-*) a="${part%%-*}"; b="${part##*-}"
           case "$a" in ''|*[!0-9]*) continue ;; esac
           case "$b" in ''|*[!0-9]*) continue ;; esac
           for ((n = a; n <= b; n++)); do [ "$n" = "$want" ] && return 0; done ;;
      *)   [ "$part" = "$want" ] && return 0 ;;
    esac
  done
  return 1
}

# cpulist_max LIST -> the highest core index in the Linux cpulist LIST (the isolated capture core;
# the fleet's isolcpus=3 renders "3", a future "2-3" -> 3), "" if LIST carries no number. issue 899.
cpulist_max() {
  local list="$1" part n max=""
  local -a _parts
  IFS=',' read -ra _parts <<< "$list" || true
  for part in "${_parts[@]}"; do
    part="${part//[[:space:]]/}"
    case "$part" in *-*) n="${part##*-}" ;; *) n="$part" ;; esac
    case "$n" in ''|*[!0-9]*) continue ;; esac
    if [ -z "$max" ] || [ "$n" -gt "$max" ]; then max="$n"; fi
  done
  printf '%s' "$max"
}

# rt_irq_placement_verdict RT IRQ_LIST CAPTURE_CORE -> echoes a verdict token (issue 899 defect 3):
#   RT=1  (PREEMPT_RT kernel): the IRQ handler is a schedulable thread below the grab priority, so
#         co-locating it on the isolated core is the #289 intent.
#   RT!=1 (stock kernel): the non-preemptible hardirq handler MUST run OFF the grab core.
# Tokens: "no-irq" (no capture IRQ / core to grade -> WARN), "ok-off-grab"/"ok-on-grab" (correct for
# the kernel), "drift-on-grab" (non-RT but the IRQ shares the grab core -> the defect-3 state),
# "rt-off-grab" (RT but the IRQ is not co-located -> the RT optimization is not being applied).
rt_irq_placement_verdict() {
  local rt="$1" list="$2" core="$3"
  { [ -n "$list" ] && [ -n "$core" ]; } || { printf 'no-irq'; return 0; }
  if cpulist_contains "$list" "$core"; then
    if [ "$rt" = "1" ]; then printf 'ok-on-grab'; else printf 'drift-on-grab'; fi
  else
    if [ "$rt" = "1" ]; then printf 'rt-off-grab'; else printf 'ok-off-grab'; fi
  fi
}

# ndi_symlink_version LS_TEXT -> the version portion of the libndi.so.6 symlink target in an
# `ls -la /usr/lib/ndi` listing (e.g. "6.3.2.0" from "libndi.so.6 -> libndi.so.6.3.2.0"), "" if
# the target is not resolvable. Builds on ndi_symlink_target (the root-owned symlink check).
ndi_symlink_version() {
  local target
  target="$(ndi_symlink_target "$1")"
  [ -n "$target" ] || return 0
  printf '%s\n' "${target#libndi.so.}"
}

# ndi_version_matches ACTUAL PIN -> 0 iff ACTUAL equals PIN or begins "PIN." (so the fleet pin
# "6.3.2" accepts both the 3-part soname "6.3.2" and the 4-part SDK string "6.3.2.0", but never
# "6.2.1" or the deceptive "6.3.20").
ndi_version_matches() {
  [ -n "$1" ] && [ -n "$2" ] || return 1
  case "$1" in
    "$2" | "$2".*) return 0 ;;
    *) return 1 ;;
  esac
}

# --- (q) .bak cruft drift -- WARNING only, never a FAIL (#453) ----------------------------------
# Inert `.bak` backups left behind by a manual NDI upgrade (fleet cam1/cam2/cam4:
# /usr/lib/ndi/libndi.so.6*.bak) or a stale drop-in edit (cam1:
# camera-box.service.d/genlock.conf.bak-30) are DEAD files -- ldconfig/systemd never load them --
# so they are drift to surface, not a functional defect. setup-device.sh's cleanup_bak_cruft
# (#453) makes a fresh/re-provisioned box self-heal; this check makes the drift visible on boxes
# provisioned BEFORE that fix landed, without failing their acceptance gate for something with
# zero functional impact.

# bak_cruft_names LS_TEXT -> newline-separated list of `.bak` / `.bak-*`-suffixed entry names
# found in LS_TEXT (an `ls -la DIR` or `ls -1 DIR` dump). Empty output means no cruft. Handles
# both an `ls -1` dump (one bare name per line) and an `ls -la` dump (permission/owner rows,
# symlinks rendered "name -> target") by taking the LAST whitespace-separated token before any
# " -> " and matching the suffix on that -- so a live symlink's OWN name is checked, never its
# target.
bak_cruft_names() {
  printf '%s\n' "$1" | awk '
    { line = $0; sub(/ -> .*/, "", line); n = split(line, f, /[ \t]+/); name = f[n];
      if (name ~ /\.bak(-.*)?$/) print name }
  '
}

# bak_cruft_report NDI_LS DROPIN_LS -> newline-separated list combining any .bak cruft found in
# the NDI dir listing (NDI_LS) and the systemd drop-in dir listing (DROPIN_LS), each prefixed with
# its real absolute path so the report names WHERE the cruft is. Empty output == clean.
#
# Uses `sed` (not a `while read` loop) to prefix each name -- a `while read` loop's exit status is
# the exit status of its OWN final (EOF-failing) `read`, which would trip `set -e` at the call
# site on the empty-input (clean) case; `sed` on empty input is a trivially-successful no-op, and
# the explicit `return 0` makes this pure formatting helper never itself fail.
bak_cruft_report() {
  local ndi_ls="$1" dropin_ls="$2"
  bak_cruft_names "$ndi_ls" | sed 's#^#/usr/lib/ndi/#'
  bak_cruft_names "$dropin_ls" | sed 's#^#/etc/systemd/system/camera-box.service.d/#'
  return 0
}

# --- source-guard: when sourced (the unit tests), stop here -- never run the live SSH flow below.
if [ "${BASH_SOURCE[0]}" != "${0}" ]; then
  return 0
fi

# =================================================================================================
# LIVE flow (executed only when run directly) -- requires sshpass + network access to the box.
# =================================================================================================

usage() {
  cat <<EOF
verify-device.sh -- POST-REBOOT runtime acceptance gate for a freshly-provisioned camera-box (#454).

Usage:
  scripts/verify-device.sh NAME
  scripts/verify-device.sh --help

NAME is resolved via scripts/camera-set.sh (cam1-6), case-insensitive (CAM5 / cam5 both work).

Checks:
  (a) camera-box --version is a valid, well-formed fleet version string
  (b) systemctl is-active camera-box == active
  (c) NDI sender is streaming (journalctl emit-fps / genlock-report line), no FATAL/panic
  (d) dantesync PTP servo LOCKED + a FRESH clock offset within bound (#550/#591)
  (e) genlock.conf drop-in present, CAMERA_BOX_GENLOCK_FPS matches camera-set.sh's per-cam value
  (f) cpu-affinity.conf drop-in present (CPUAffinity=<isolated core>)
  (g) /usr/lib/ndi/libndi.so.6 is a root-owned symlink chain to a root-owned regular file
  (h) avahi mDNS NDI discovery sees this box's NDI source (avahi-browse -tp _ndi._tcp)
  (i) capture chroma metric reports "colour" (not "grayscale") -- #299 regression signal
  (j) root filesystem mounted read-only (ro appliance)
  (k) exactly ONE installed kernel, equal to the running one (optional KERNEL_PIN exact match)
  (l) fwupd purged (blocks the ro remount)
  (m) systemd-networkd-wait-online masked (no 120s boot stall)
  (n) core-isolation cmdline: isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)
  (o) NDI runtime pinned to the fleet version (NDI_VERSION_PIN, default 6.3.2)
  (q) WARNING only: stale .bak cruft under the NDI dir or the systemd drop-in dir (#453)
  (r) dantesync is the SOLE timesync authority -- no competing daemon installed/active/enabled (#591)
  (s) /var/log tmpfs bounded against runaway growth -- logrotate size cap + frequent rotation (#679)
  (t) fuser (psmisc) is installed (#743)
  (u) rsyslog PURGED (not just masked) + journald RuntimeMaxUse capped (#762)
  (v) cam2 ONLY: permanent devel-mode dual-QR painter installed+active+painting, camera-box
      permanently no-display so it never contests /dev/fb0 (#863)
  (w) udev rule is burn-gated (never the old unconditional restart) AND the live grabber's USB
      power/control currently reads "on" (drift check; N/A when no grabber is fitted, #894)
  (y) camera-box ExecStartPre device-free bake-in present (drop-in + helper) so every start frees
      /dev/video from a killed E2E run's stray capture burn (#772)
  (z) publish-30p.conf drop-in present (CAMERA_BOX_PUBLISH_30P=1) AND the box is actually
      publishing the secondary "CAMn (30p)" 30fps blend stream (issue 792 / #1087)
  (aa) interkom audio bake-in: by-NAME /etc/asound.conf (CARD=HID), alsa-utils installed, and the
      live amixer Mic/PCM gain matches the per-box table (cam1-4 75%/79%, cam5-7 80%/94%) (#782)
  (ab) RemoteOS MCP agent: remoteos-mcp.service enabled (reboot-survival) + active, :8092 listening
      (the linux-camN MCP control surface, provisioned by setup-device.sh STEP 17b) (#1066)
  (ac) realtime-isolation drift (issue 899, WARN-only): kernel PREEMPT_RT status + the xhci capture
      IRQ routed off the isolated grab core on a stock kernel (defects 1+3; hard-FAIL flip staged)

Env: KERNEL_PIN (optional exact running-kernel pin), NDI_VERSION_PIN (default 6.3.2),
     DANTESYNC_OFFSET_FRESHNESS_S (max age of a fresh [NTP] offset line, default 300),
     DANTESYNC_JOURNAL_MAX_AGE_S (max age of the newest dantesync journal line vs the box
     clock before the daemon is treated as hung/free-running, default 60; #600).

Exit: 0 iff every check passes.
EOF
}

DEVICE_ARG="${1:-}"
case "$DEVICE_ARG" in
  -h|--help) usage; exit 0 ;;
esac
if [ -z "$DEVICE_ARG" ]; then
  usage >&2
  exit 1
fi

# Resolve NAME -> IP / CAMERA_GENLOCK_FPS via camera-set.sh (case-insensitive, same normalization
# as setup-device.sh's resolve_device_name -- fail loud on an unknown name via camera_resolve()'s
# own fail-closed `case`).
LC_NAME="$(printf '%s' "$DEVICE_ARG" | tr '[:upper:]' '[:lower:]')"
camera_resolve "$LC_NAME" || exit 1
NAME_UPPER="$(printf '%s' "$CAMERA_NAME" | tr '[:lower:]' '[:upper:]')"
IP="$CAMERA_IP"
EXPECT_FPS="$CAMERA_GENLOCK_FPS"

command -v sshpass >/dev/null 2>&1 || { echo -e "${RED}ERROR: sshpass is required${NC}" >&2; exit 1; }

echo -e "${GREEN}== verify-device (#454): ${NAME_UPPER} @ ${IP} ==${NC}"

FAILS=0
ok()   { printf "  ${GREEN}[OK]${NC}   %s\n" "$1"; }
fail() { printf "  ${RED}[FAIL]${NC} %s\n" "$1"; FAILS=$((FAILS + 1)); }
# warn() intentionally SHADOWS scripts/lib/cli-log.sh's own warn() (sourced above) for the rest of
# this script -- it matches this report's own 2-space-indented "[OK]"/"[FAIL]" column style rather
# than cli-log.sh's "[!] msg" line, and -- unlike fail() -- never increments FAILS: a WARN never
# fails the acceptance gate (#453's ".bak cruft is drift to surface, not a functional defect").
warn() { printf "  ${YELLOW}[WARN]${NC} %s\n" "$1"; }

ssh_box() {
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no -o ConnectTimeout="$SSH_TIMEOUT" \
    "${SSH_USER}@${IP}" "$1"
}

# (a) version -------------------------------------------------------------------------------------
rc=0
VERSION="$(ssh_box "/usr/local/bin/camera-box --version 2>/dev/null | awk '{print \$NF}'")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$VERSION" ]; then
  fail "camera-box --version unreadable (ssh rc=$rc)"
elif ! version_is_valid_format "$VERSION"; then
  fail "camera-box --version '$VERSION' is not a well-formed fleet version string"
else
  ok "camera-box --version = $VERSION"
fi

# (b) service active --------------------------------------------------------------------------
rc=0
SVC_STATE="$(ssh_box "systemctl is-active camera-box 2>/dev/null")" || rc=$?
if active_state_is_active "$SVC_STATE"; then
  ok "camera-box.service active"
else
  fail "camera-box.service not active (state='${SVC_STATE:-<none>}', ssh rc=$rc)"
fi

# (c) + (i) camera-box journal: NDI emit alive, no FATAL, capture-chroma colour -----------------
# #694: same stale-journal-across-restart exposure #693 fixed for recording-e2e.sh's preflight --
# `journalctl -u camera-box` spans ACROSS a service restart, so a line from a PREVIOUS process
# instance could leak into the lookback window. Resolve the CURRENT InvocationID and scope the
# read via the shared capture_rate_journalctl_cmd(); empty on failure falls back to the old
# unscoped read (never silently skip the whole acceptance check).
rc=0
CB_INVOCATION_ID="$(ssh_box "systemctl show -p InvocationID --value camera-box 2>/dev/null" || true)"
CB_JOURNAL="$(ssh_box "$(capture_rate_journalctl_cmd "$CB_INVOCATION_ID" 300)")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$CB_JOURNAL" ]; then
  fail "camera-box journal unreadable (ssh rc=$rc)"
  fail "capture chroma metric unreadable -- camera-box journal unreadable"
else
  if ndi_emit_ok "$CB_JOURNAL"; then
    ok "NDI sender streaming (emit/genlock report seen in the last 300 journal lines)"
  else
    fail "no NDI emit/streaming report seen in the last 300 camera-box journal lines"
  fi
  if ndi_journal_has_fatal "$CB_JOURNAL"; then
    fail "camera-box journal contains a FATAL/panic signature"
  else
    ok "no FATAL/panic in camera-box journal"
  fi

  chroma_state="$(chroma_state_from_journal "$CB_JOURNAL")"
  chroma_rc=0
  chroma_check "$chroma_state" || chroma_rc=$?
  case "$chroma_rc" in
    0) ok "capture chroma: colour ($chroma_state)" ;;
    2) fail "capture chroma reports GRAYSCALE -- capture card delivering monochrome frames (#299): $chroma_state" ;;
    *) fail "capture chroma metric not seen in the journal (box may be too fresh -- no sample yet)" ;;
  esac
fi

# (d) dantesync PTP lock + FRESH clock offset (#8 / #550 / #591) ---------------------------------
# Gathered with `-o short-iso` (ISO timestamps) so dantesync_offset_verdict can reject a STALE
# offset line (#550); -n 400 gives ~7min of history (dantesync logs ~1 line/s) so a fresh offset
# line reliably falls inside the freshness window.
rc=0
DS_JOURNAL="$(ssh_box "journalctl -u dantesync --no-pager -n 400 -o short-iso 2>/dev/null")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$DS_JOURNAL" ]; then
  fail "dantesync journal unreadable (ssh rc=$rc)"
else
  # (d) LIVENESS gate (#600 / #591 review) — a died/hung dantesync leaves the lock + offset reads
  # below graded on a STALE journal and would otherwise PASS while the clock free-runs. Gather the
  # daemon's is-active state + the box's OWN wall clock, and hard-FAIL on either liveness hole
  # BEFORE trusting the content reads. Both signals come from the box, so the verifier host's clock
  # never enters the freshness comparison.
  # is-active legitimately exits non-zero for an inactive service (expected, NOT an ssh error), so
  # its rc is meaningless -- read its STDOUT ('' means unreachable). date +%s only exits non-zero on
  # a real ssh failure, so capture that rc (the script's `rc=$?` gather pattern) to keep the
  # "box clock unreadable" case DISTINCT from the "daemon hung" case, never misattributing a
  # transient ssh blip to a free-running clock.
  DS_ACTIVE="$(ssh_box "systemctl is-active dantesync 2>/dev/null")" || true
  ds_now_rc=0
  BOX_NOW="$(ssh_box "date +%s 2>/dev/null")" || ds_now_rc=$?
  if ! dantesync_service_active "$DS_ACTIVE"; then
    fail "dantesync service NOT active (state='${DS_ACTIVE:-<none>}') -- clock undisciplined/free-running (#591 review)"
  elif [ "$ds_now_rc" -ne 0 ] || [ -z "$BOX_NOW" ]; then
    fail "could not read the box wall clock over SSH (date +%s, ssh rc=$ds_now_rc) -- cannot certify the dantesync journal is advancing (#591 review)"
  elif [ "$(dantesync_journal_fresh "$DS_JOURNAL" "$BOX_NOW" "$DANTESYNC_JOURNAL_MAX_AGE_S")" = stale ]; then
    fail "dantesync journal has not advanced within ${DANTESYNC_JOURNAL_MAX_AGE_S}s of the box clock -- daemon hung, clock free-running (#591 review)"
  else
    ds_locked=no
    if dantesync_locked_ok "$DS_JOURNAL"; then
      ok "dantesync PTP servo LOCKED"
      ds_locked=yes
    else
      fail "dantesync PTP servo not LOCKED (degraded or unknown)"
    fi
    case "$(dantesync_offset_verdict "$DS_JOURNAL" "$DANTESYNC_OFFSET_FRESHNESS_S" "$DEVICE_CLOCK_BOUND_US" "$DEVICE_CLOCK_STABILITY_US")" in
      ok)
        ok "dantesync clock offset within ${DEVICE_CLOCK_BOUND_US}us bound + samples within ${DEVICE_CLOCK_STABILITY_US}us spread (fresh)"
        ;;
      drift)
        # A FRESH out-of-bound offset = a real clock desync happening NOW -- the cam5/6 5.28s case
        # (a 2nd timesync daemon stepping the clock). Always a hard FAIL, regardless of PTP state.
        fail "dantesync clock offset OUTSIDE the ${DEVICE_CLOCK_BOUND_US}us bound -- a REAL clock desync (#591: e.g. a 2nd timesync daemon stepping the clock, cam5/6 -> 5.28s)"
        ;;
      unstable)
        # #837: median in-bound but the FRESH samples scatter past the stability bound -- a
        # scattered/unusable clock. As hard a FAIL as drift (the #836 spread class, journal twin).
        fail "dantesync clock offset median within the ${DEVICE_CLOCK_BOUND_US}us bound but the FRESH samples scatter past the ${DEVICE_CLOCK_STABILITY_US}us stability bound -- scattered/unusable clock (#837)"
        ;;
      drift_unstable)
        fail "dantesync clock offset OUTSIDE the ${DEVICE_CLOCK_BOUND_US}us bound AND the FRESH samples scatter past the ${DEVICE_CLOCK_STABILITY_US}us stability bound -- a real desync with scattered samples (#837)"
        ;;
      stale | absent)
        # No FRESH [NTP] offset reading. dantesync emits the [NTP] offset line only intermittently
        # (a boot step + a ~30s adaptive cadence), so a settled box may momentarily have only a stale
        # one. If the PTP servo is LOCKED (the µs-grade real-time signal) AND the (r) sole-authority
        # gate below confirms nothing else is stepping the clock, the offset is disciplined near-zero;
        # reading the aged boot-step line and failing on its value was the #550 false-fail. If PTP is
        # NOT locked, we have no trustworthy clock signal at all -> FAIL (never a silent pass).
        if [ "$ds_locked" = yes ]; then
          ok "dantesync offset: no FRESH [NTP] line, but PTP servo LOCKED -- clock disciplined near-zero; a stale boot-step line is deliberately NOT read (#550). Sole-authority gate (r) below guarantees no competing daemon."
        else
          fail "dantesync clock offset has no FRESH reading AND PTP servo is not LOCKED -- clock status UNKNOWN, never a silent pass (#550/#591)"
        fi
        ;;
    esac
  fi
fi

# (r) single timesync authority: dantesync ONLY -- no competing daemon installed/active/enabled (#591)
# cam5/cam6 ran systemd-timesyncd ALONGSIDE dantesync -> a real 5.28s desync. A minimalist appliance
# runs ONLY dantesync. One SSH call gathers, per competing daemon, its dpkg install state +
# systemctl is-active + is-enabled into a `NAME|DPKG|ACTIVE|ENABLED` block; timesync_authority_verdict
# hard-fails on any that is installed (even masked) / active / enabled. The gathering command itself
# is shared via timesync_gather_remote_snippet() (scripts/lib/timesync-authority.sh, #596 review
# finding) so drift-guard.sh's --check-imag facet can never drift from this EXACT daemon list.
rc=0
TS_STATES="$(ssh_box "$(timesync_gather_remote_snippet)")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$TS_STATES" ]; then
  fail "could not read timesync-daemon state over SSH (rc=$rc) -- cannot certify dantesync is the sole clock authority (#591)"
else
  TS_VERDICT="$(timesync_authority_verdict "$TS_STATES")"
  if [ "$TS_VERDICT" = "ok" ]; then
    ok "dantesync is the SOLE timesync authority -- no competing daemon installed/active/enabled (#591)"
  else
    while IFS= read -r _reason; do
      [ -n "$_reason" ] && fail "timesync authority: ${_reason#FAIL: }"
    done <<< "$TS_VERDICT"
  fi
fi

# (s) /var/log tmpfs bounded against runaway growth (#679, SUPERSEDED by #762 once rsyslog is
# purged) -------------------------------------------------------------------------------------
# Every box's /var/log is a fixed 50MB tmpfs; the stock logrotate config rotates ONLY on a weekly
# calendar with no `size` cap, so a chatty logger (dantesync's per-second [PTP] Drift line was the
# fleet's dominant volume driver) filled it in ~4-5 days and crashed cam2's camera-box.service
# (2026-07-11). log_bound_verdict (scripts/lib/log-bound.sh) requires a `size` cap on
# /etc/logrotate.d/rsyslog AND a systemd timer drop-in that checks far more often than daily.
#
# #762: once rsyslog is genuinely PURGED (the (u) check below), /etc/logrotate.d/rsyslog is
# REMOVED WITH IT (a package conffile) -- the #679 size-cap check then becomes structurally
# impossible to satisfy on a box that is otherwise CORRECTLY hardened, which would be a false
# FAIL. Gather the #762 rsyslog/journald state FIRST (shared with the (u) check below -- one ssh
# round trip covers both) and skip the #679 logrotate check entirely when rsyslog is confirmed
# purged; only fall back to the full log_bound_verdict when rsyslog is still present (a box not
# yet re-provisioned onto the #762 fix).
rc=0
LOG_DIET_STATE="$(ssh_box "$(log_diet_gather_remote_snippet)")" || rc=$?
if [ "$rc" -ne 0 ] || [ -z "$LOG_DIET_STATE" ]; then
  fail "could not read rsyslog/journald state over SSH (rc=$rc) -- cannot certify /var/log is bounded (#679/#762)"
elif log_diet_rsyslog_purged "$LOG_DIET_STATE"; then
  ok "/var/log tmpfs bound: rsyslog is purged -- #679's logrotate size-cap is superseded by the #762 journald RuntimeMaxUse cap (checked at (u) below)"
else
  rc=0
  LOG_BOUND_STATE="$(ssh_box "$(log_bound_gather_remote_snippet)")" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$LOG_BOUND_STATE" ]; then
    fail "could not read logrotate/timer state over SSH (rc=$rc) -- cannot certify /var/log is bounded against runaway growth (#679)"
  else
    LOG_BOUND_VERDICT="$(log_bound_verdict "$LOG_BOUND_STATE")"
    if [ "$LOG_BOUND_VERDICT" = "ok" ]; then
      ok "/var/log tmpfs is bounded -- logrotate size cap + frequent (#679) rotation check both present"
    else
      fail "log bound: ${LOG_BOUND_VERDICT#FAIL: }"
    fi
  fi
fi

# (t) fuser (psmisc) installed (#743) -------------------------------------------------------------
# A fresh cam2 clone had NO fuser at all: rig-mode.sh's #464 KMS-held check false-FAILed (fuser
# exits 127, which the check's own `if fuser -s ...` reads the SAME as "not held" even though the
# painter was genuinely alive), and recording-e2e.sh's capture-release busy-wait
# (`while fuser -s $NODE ...`) silently became a no-op the same way. `command -v` alone proves the
# binary is present and on PATH -- exactly what both harness call sites need.
rc=0
FUSER_PATH="$(ssh_box "command -v fuser 2>/dev/null")" || rc=$?
if [ "$rc" -eq 0 ] && [ -n "$FUSER_PATH" ]; then
  ok "fuser present ($FUSER_PATH) -- psmisc installed (#743)"
else
  fail "fuser not found on PATH (ssh rc=$rc) -- psmisc missing; rig-mode.sh's #464 KMS-held check \
and recording-e2e.sh's capture-release wait both silently degrade without it (#743)"
fi

# (u) rsyslog PURGED + journald RuntimeMaxUse capped (#762) ---------------------------------------
# rsyslog is redundant on this appliance -- journald already captures everything, and nothing
# reads /var/log/syslog on a read-only appliance with no operator logging in. A live cam1
# incident (2026-07-15) showed a full /var/log tmpfs put rsyslogd into a write-error feedback
# loop (~400 lines/s, 42.8% CPU), starving the camera-box send path badly enough to measurably
# drift NDI delivery timing. log_diet_provision_verdict (scripts/lib/log-diet.sh) fails LOUD if
# rsyslog is still installed/active/enabled (masking alone is not enough) OR the journald
# RuntimeMaxUse=20M drop-in is missing/wrong. Reuses $LOG_DIET_STATE already gathered at (s)
# above -- ONE ssh round trip covers both checks.
if [ -z "${LOG_DIET_STATE:-}" ]; then
  fail "could not read rsyslog/journald state over SSH -- cannot certify the #762 logging diet is applied"
else
  LOG_DIET_VERDICT="$(log_diet_provision_verdict "$LOG_DIET_STATE")"
  if [ "$LOG_DIET_VERDICT" = "ok" ]; then
    ok "rsyslog purged + journald RuntimeMaxUse=${LOG_DIET_JOURNALD_RUNTIME_MAX} drop-in present (#762)"
  else
    while IFS= read -r _reason; do
      [ -n "$_reason" ] && fail "log diet: ${_reason#FAIL: }"
    done <<< "$LOG_DIET_VERDICT"
  fi
fi

# (e) genlock.conf drop-in --------------------------------------------------------------------
rc=0
GL_CONF="$(ssh_box "cat /etc/systemd/system/camera-box.service.d/genlock.conf 2>/dev/null")" || rc=$?
ACTUAL_FPS="$(genlock_dropin_fps "$GL_CONF")"
if [ -z "$ACTUAL_FPS" ]; then
  fail "genlock.conf drop-in missing or CAMERA_BOX_GENLOCK_FPS not set (ssh rc=$rc)"
elif genlock_fps_matches "$ACTUAL_FPS" "$EXPECT_FPS"; then
  ok "genlock.conf CAMERA_BOX_GENLOCK_FPS=$ACTUAL_FPS (matches camera-set.sh: $EXPECT_FPS)"
else
  fail "genlock.conf CAMERA_BOX_GENLOCK_FPS=$ACTUAL_FPS but camera-set.sh expects $EXPECT_FPS"
fi

# (f) cpu-affinity.conf drop-in -----------------------------------------------------------------
rc=0
CA_CONF="$(ssh_box "cat /etc/systemd/system/camera-box.service.d/cpu-affinity.conf 2>/dev/null")" || rc=$?
CA_VAL="$(cpu_affinity_dropin_value "$CA_CONF")"
if [ -n "$CA_VAL" ]; then
  ok "cpu-affinity.conf present (CPUAffinity=$CA_VAL)"
else
  fail "cpu-affinity.conf drop-in missing or CPUAffinity not set (ssh rc=$rc)"
fi

# (g) libndi root-owned symlink chain ------------------------------------------------------------
rc=0
NDI_LS="$(ssh_box "ls -la /usr/lib/ndi 2>/dev/null")" || rc=$?
if ndi_symlink_chain_ok "$NDI_LS"; then
  ok "libndi.so.6 is a root-owned symlink chain to a root-owned regular file"
else
  fail "libndi.so.6 is not a root-owned symlink chain (ssh rc=$rc) -- see \`ls -la /usr/lib/ndi\`"
fi

# (h) avahi mDNS NDI discovery -----------------------------------------------------------------
rc=0
AVAHI_OUT="$(ssh_box "avahi-browse -tp _ndi._tcp 2>/dev/null")" || rc=$?
if avahi_ndi_discoverable "$AVAHI_OUT" "$NAME_UPPER"; then
  ok "avahi mDNS sees this box's NDI source ($NAME_UPPER)"
else
  fail "avahi-browse did not see an NDI source for $NAME_UPPER (ssh rc=$rc)"
fi

# (j) root filesystem mounted read-only (#547 -- ro appliance) -----------------------------------
rc=0
MOUNT_OPTS="$(ssh_box "findmnt -no OPTIONS / 2>/dev/null || awk '\$2==\"/\"{print \$4; exit}' /proc/mounts")" || rc=$?
if root_mount_is_readonly "$MOUNT_OPTS"; then
  ok "root filesystem mounted read-only (ro appliance)"
else
  fail "root filesystem is NOT read-only (opts='${MOUNT_OPTS:-<none>}', ssh rc=$rc)"
fi

# (k) exactly one installed kernel, equal to the running one (#547 -- one kernel) ----------------
rc=0
BOOT_LS="$(ssh_box "ls -1 /boot/vmlinuz-* 2>/dev/null")" || rc=$?
RUNNING_KERNEL="$(ssh_box "uname -r 2>/dev/null")" || rc=$?
if kernels_uniform_ok "$BOOT_LS" "$RUNNING_KERNEL"; then
  ok "single installed kernel == running ($RUNNING_KERNEL)"
else
  fail "kernel not uniform -- want exactly one installed kernel equal to the running one (running='${RUNNING_KERNEL:-?}', installed='$(printf '%s' "$BOOT_LS" | tr '\n' ' ')', ssh rc=$rc)"
fi
if [ -n "$EXPECT_KERNEL" ]; then
  if [ "$RUNNING_KERNEL" = "$EXPECT_KERNEL" ]; then
    ok "running kernel matches the fleet pin ($EXPECT_KERNEL)"
  else
    fail "running kernel '${RUNNING_KERNEL:-?}' != fleet pin '$EXPECT_KERNEL' (KERNEL_PIN)"
  fi
fi

# (l) fwupd purged (#547 -- it holds a write handle blocking the ro remount) ---------------------
rc=0
# NB: `systemctl is-enabled` prints the state (e.g. "masked"/"disabled") to STDOUT *and* exits
# non-zero for those states -- so `|| true` (never `|| echo <sentinel>`, which would APPEND a
# second word to the captured state and break the exact-match checks below); a purged unit prints
# nothing -> empty state, which fwupd_absent accepts.
FWUPD_STATE="$(ssh_box "systemctl is-enabled fwupd 2>/dev/null || true")" || rc=$?
case "$(fwupd_verdict "$rc" "$FWUPD_STATE")" in
  ok) ok "fwupd is not installed (purged)" ;;
  unreadable) fail "fwupd state unreadable (ssh rc=$rc) -- unreachable check is a FAIL, never a silent pass" ;;
  *) fail "fwupd still present (state='${FWUPD_STATE}') -- purge it; it blocks the ro remount" ;;
esac

# (m) systemd-networkd-wait-online masked (#547 -- avoids the 120s boot stall) -------------------
rc=0
WAITONLINE_STATE="$(ssh_box "systemctl is-enabled systemd-networkd-wait-online 2>/dev/null || true")" || rc=$?
if waitonline_masked "$WAITONLINE_STATE"; then
  ok "systemd-networkd-wait-online masked (no 120s boot stall)"
else
  fail "systemd-networkd-wait-online not masked (state='${WAITONLINE_STATE}') -- unmasked it delays network-online.target ~120s (ssh rc=$rc)"
fi

# (n) core-isolation kernel cmdline (#289 isolcpus + #303 nohz_full/rcu_nocbs/irqaffinity) -------
rc=0
CMDLINE="$(ssh_box "cat /proc/cmdline 2>/dev/null")" || rc=$?
if cmdline_has_isolation "$CMDLINE"; then
  ok "kernel cmdline carries isolcpus=3 + nohz_full=3 + rcu_nocbs=3 + irqaffinity=0-2 (#289/#303)"
else
  fail "kernel cmdline missing a core-isolation flag (#289/#303): '${CMDLINE:-<none>}' (ssh rc=$rc)"
fi

# (o) NDI runtime pinned to the fleet version (#132/#547) ----------------------------------------
# Reuses NDI_LS gathered in (g); ndi_symlink_version extracts the version from the symlink target.
NDI_VER="$(ndi_symlink_version "$NDI_LS")"
if ndi_version_matches "$NDI_VER" "$NDI_VERSION_PIN"; then
  ok "NDI runtime pinned to $NDI_VERSION_PIN (active: ${NDI_VER})"
else
  fail "NDI runtime '${NDI_VER:-?}' != fleet pin '$NDI_VERSION_PIN' (NDI_VERSION_PIN)"
fi

# (p) [REMOVED, #528] -----------------------------------------------------------------------------
# The per-box config.toml [display] / ExecStart --display acceptance check (#528/#557/#558/#562)
# no longer applies: the HDMI cameraman preview is UNCONDITIONAL and fleet-wide, baked into the
# binary's own DEFAULT_DISPLAY_SOURCE default -- there is no per-box config left to drift from or
# lose. See src/main.rs::resolve_display_config (unit tested) for the current contract.

# (v) cam2-only: the PERMANENT devel-mode dual-QR painter is installed, active, and genuinely
# painting -- and camera-box on cam2 must never contest /dev/fb0 (#863) --------------------------
# #863: cam2-painter.service was referenced everywhere (rig-mode.sh's #440 stop/start guards,
# recording-e2e.sh's cleanup()) but was never actually installed by setup-device.sh -- this check
# is the acceptance gate that catches that gap ever recurring on a re-provisioned/replaced box.
if [ "$NAME_UPPER" = "CAM2" ]; then
  rc=0
  PAINTER_UNIT_FILES="$(ssh_box "systemctl list-unit-files cam2-painter.service 2>/dev/null")" || rc=$?
  if [ "$rc" -ne 0 ] || [ -z "$PAINTER_UNIT_FILES" ]; then
    fail "cam2-painter.service not installed on cam2 (#863 -- the permanent devel-mode QR painter; ssh rc=$rc)"
  else
    rc=0
    PAINTER_ACTIVE="$(ssh_box "systemctl is-active cam2-painter.service 2>/dev/null")" || rc=$?
    if ! active_state_is_active "$PAINTER_ACTIVE"; then
      fail "cam2-painter.service not active (state='${PAINTER_ACTIVE:-<none>}', ssh rc=$rc)"
    else
      ok "cam2-painter.service active (#863 permanent devel-mode QR painter)"
      rc=0
      PAINTER_JOURNAL="$(ssh_box "journalctl -u cam2-painter.service -n 200 --no-pager 2>/dev/null")" || rc=$?
      if [ "$rc" -ne 0 ]; then
        fail "could not read cam2-painter.service journal to confirm it is genuinely painting (ssh rc=$rc)"
      elif printf '%s' "$PAINTER_JOURNAL" | grep -q 'presenter: using DRM/KMS page-flip'; then
        if printf '%s' "$PAINTER_JOURNAL" | grep -q 'vblank-locked'; then
          ok "cam2-painter.service genuinely painting (KMS page-flip, vblank-locked)"
        else
          fail "cam2-painter.service selected KMS but never confirmed vblank-locked (see journalctl -u cam2-painter.service)"
        fi
      elif printf '%s' "$PAINTER_JOURNAL" | grep -qi 'falling back to fbdev'; then
        ok "cam2-painter.service genuinely painting (fbdev fallback presenter)"
      else
        fail "cam2-painter.service active but no presenter-selection log line found -- cannot confirm it is genuinely painting (see journalctl -u cam2-painter.service)"
      fi
    fi
  fi
  # camera-box must NEVER contest /dev/fb0 on cam2 -- the permanent no-display drop-in must be
  # baked in so a reboot can't silently regress this (see cam2_painter_no_display_dropin_content
  # in setup-device.sh).
  rc=0
  NODISPLAY_ENV="$(ssh_box "systemctl show -p Environment --value camera-box 2>/dev/null")" || rc=$?
  if [ "$rc" -eq 0 ] && printf '%s' "$NODISPLAY_ENV" | grep -q 'CAMERA_BOX_NO_DISPLAY=1'; then
    ok "camera-box permanently no-display on cam2 (#863 -- cam2-painter.service owns /dev/fb0)"
  else
    fail "camera-box on cam2 is NOT permanently no-display (Environment='${NODISPLAY_ENV:-<none>}', ssh rc=$rc) -- it will contest /dev/fb0 with cam2-painter.service"
  fi
fi

# (w) udev hotplug rule is burn-gated + live USB autosuspend has not drifted back to auto (#894) -
# The fleet's OLD rule unconditionally restarted production camera-box.service on every
# video4linux "add" event, stealing the device back from an in-flight E2E camera-box-burn-*.service
# (77/NOPERM, misreported as frozen_leg on the camera). The fix (scripts/lib/udev-camera-box.sh) is
# a guarded rule + helper script installed by setup-device.sh/create-usb-linux.sh; this check
# proves the box actually has it, and that the LIVE grabber's USB power/control has not silently
# drifted back to `auto` (the same #894 comment's measured amplifying re-enumeration feedback loop).
wrc=0
UDEV_RULE_TEXT="$(ssh_box "cat /etc/udev/rules.d/99-camera-box.rules 2>/dev/null")" || wrc=$?
if [ "$wrc" -ne 0 ]; then
  fail "could not read /etc/udev/rules.d/99-camera-box.rules (ssh rc=$wrc)"
elif [ -z "$UDEV_RULE_TEXT" ]; then
  fail "/etc/udev/rules.d/99-camera-box.rules is missing -- production is NOT protected from an E2E burn-unit device-steal (#894)"
elif ! udev_camera_box_rule_is_burn_gated "$UDEV_RULE_TEXT"; then
  fail "/etc/udev/rules.d/99-camera-box.rules is NOT burn-gated (still the fleet's old unconditional restart, or something else entirely, #894): $UDEV_RULE_TEXT"
else
  ok "udev hotplug rule wired to the burn-gated helper (#894)"
fi

pcrc=0
POWER_CONTROL_OUT="$(ssh_box "$(v4l2_neutral_resolve_node_cmd)
if [ -e \"\$V4L2_NEUTRAL_NODE\" ]; then echo CAMERA_BOX_VIDEO_NODE_EXISTS=1; else echo CAMERA_BOX_VIDEO_NODE_EXISTS=0; fi
$(udev_camera_box_grabber_power_control_read_cmd)")" || pcrc=$?
if [ "$pcrc" -ne 0 ]; then
  fail "could not read the live capture grabber's USB power/control (ssh rc=$pcrc)"
elif ! printf '%s' "$POWER_CONTROL_OUT" | grep -q 'CAMERA_BOX_VIDEO_NODE_EXISTS=1'; then
  ok "no capture grabber fitted -- USB power/control drift check N/A (#828)"
else
  GRABBER_POWER_CONTROL="$(udev_camera_box_grabber_power_control_from_output "$POWER_CONTROL_OUT")"
  if udev_camera_box_power_control_is_on "$GRABBER_POWER_CONTROL"; then
    ok "capture grabber USB power/control=on (autosuspend off, #894)"
  else
    fail "capture grabber USB power/control='${GRABBER_POWER_CONTROL:-<unreadable>}' -- drifted away from 'on' (#894's amplifying re-enumeration feedback loop; the udev rule should have re-applied this on the last hotplug)"
  fi
fi

# (x) ffmpeg installed + runnable (#930 lipsync-test-mode runtime) -------------------------------
# setup-device.sh installs ffmpeg (STEP 16) so ANY box can take cam2's lipsync-test-mode painter
# role (scripts/lipsync-test-mode.sh); confirm it's actually present AND runnable, not just that
# the apt-get step didn't error -- the same "trust but verify" gate the fuser check (t) above
# already applies to psmisc. Inserted BEFORE (q) -- see .claude/rules/provisioning-scripts.md:
# (q) is the intentionally-LAST check (tests/verify_device_pure_functions.rs asserts its block
# runs to end-of-file), so any NEW check goes above it, never after.
rc=0
FFMPEG_VERSION_LINE="$(ssh_box "ffmpeg -version 2>/dev/null | head -1")" || rc=$?
if [ "$rc" -eq 0 ] && [ -n "$FFMPEG_VERSION_LINE" ]; then
  ok "ffmpeg present and runnable ($FFMPEG_VERSION_LINE) -- #930 lipsync-test-mode runtime"
else
  fail "ffmpeg not found/runnable on PATH (ssh rc=$rc) -- scripts/lipsync-test-mode.sh needs it \
for the lipsync cross-validation TEST-mode variant (#930)"
fi

# (x2) mpv installed + runnable (#1187 lipsync-test-mode DRM/KMS playback runtime) ---------------
# setup-device.sh installs mpv (STEP 16) so ANY box can take cam2's lipsync-test-mode painter role
# via the DRM/KMS playback path (scripts/lipsync-test-mode.sh, issue 1187 -- `mpv --vo=drm` replaced
# the legacy raw-fbdev ffmpeg write that leaked a stale frame, issue 1176). Confirm mpv is actually
# present AND runnable, not just that the apt-get step didn't error -- the same "trust but verify"
# gate as the ffmpeg (x) check above. Inserted BEFORE (q) -- see
# .claude/rules/provisioning-scripts.md: (q) is the intentionally-LAST check.
rc=0
MPV_VERSION_LINE="$(ssh_box "mpv --version 2>/dev/null | head -1")" || rc=$?
if [ "$rc" -eq 0 ] && [ -n "$MPV_VERSION_LINE" ]; then
  ok "mpv present and runnable ($MPV_VERSION_LINE) -- #1187 lipsync-test-mode DRM/KMS playback runtime"
else
  fail "mpv not found/runnable on PATH (ssh rc=$rc) -- scripts/lipsync-test-mode.sh needs it \
for the DRM/KMS lipsync playback path (#1187)"
fi

# (y) camera-box ExecStartPre device-free bake-in (#772) -----------------------------------------
# setup-device.sh installs a helper + a camera-box.service.d/free-capture-device.conf drop-in whose
# ExecStartPre frees /dev/video before every camera-box start, so a killed E2E run's stray
# camera-box-burn-*.service can never crash-loop production on "Device or resource busy". Prove the
# box actually has BOTH, and that the helper is burn-scoped (stops the burn UNIT, never the painter)
# -- single-sourced in scripts/lib/camera-box-free-device.sh. Inserted BEFORE (q) -- see
# .claude/rules/provisioning-scripts.md: (q) must remain the intentionally-LAST check.
# A remote `[ -f ] && cat || echo <sentinel>` keeps the ssh command's own exit 0 on a MISSING file,
# so a non-zero ssh rc means genuine UNREACHABILITY (transport), distinct from "file absent" (the
# sentinel) -- otherwise a bare `cat <missing> 2>/dev/null` exit-1 conflates the two and the
# dedicated "is missing" message is unreachable dead code.
yrc=0
FREE_DEV_DROPIN="$(ssh_box "if [ -f /etc/systemd/system/camera-box.service.d/free-capture-device.conf ]; then cat /etc/systemd/system/camera-box.service.d/free-capture-device.conf; else echo __FREE_DEV_DROPIN_ABSENT__; fi")" || yrc=$?
FREE_DEV_HELPER="$(ssh_box "cat /usr/local/bin/camera-box-free-capture-device.sh 2>/dev/null")" || true
if [ "$yrc" -ne 0 ]; then
  fail "could not reach the box to read camera-box.service.d/free-capture-device.conf (ssh rc=$yrc)"
elif [ -z "$FREE_DEV_DROPIN" ] || [ "$FREE_DEV_DROPIN" = "__FREE_DEV_DROPIN_ABSENT__" ]; then
  fail "camera-box.service.d/free-capture-device.conf is missing -- a killed E2E run's stray capture burn will crash-loop camera-box on 'Device or resource busy' (#772)"
elif ! camera_box_free_device_dropin_wired "$FREE_DEV_DROPIN"; then
  fail "camera-box.service.d/free-capture-device.conf is not wired to the ExecStartPre device-free helper (#772): $FREE_DEV_DROPIN"
elif ! camera_box_free_device_script_is_burn_scoped "$FREE_DEV_HELPER"; then
  fail "/usr/local/bin/camera-box-free-capture-device.sh missing or not burn-scoped -- it must stop the stray burn UNIT + pkill the burn and never touch the painter (#772)"
else
  ok "camera-box ExecStartPre frees /dev/video on every start (#772)"
fi

# (z) publish-30p.conf drop-in + live "CAMn (30p)" blend stream (issue 792 feature, baked into
# provisioning by #1087) ------------------------------------------------------------------------
# TWO facets: (1) the camera-box.service.d/publish-30p.conf drop-in is present with
# CAMERA_BOX_PUBLISH_30P=1 -- setup-device.sh STEP 7 bakes it, so a re-provisioned box keeps the
# secondary 30fps blend stream instead of silently regressing to 60p-only -- AND (2) the box is
# ACTUALLY publishing the "(30p)" NDI source right now (the issue-792 publisher's own journal
# output), reusing CB_JOURNAL already gathered in (c). A drop-in on disk without the live stream
# (e.g. an old binary predating issue 792) still FAILs. Inserted BEFORE (q) -- see
# .claude/rules/provisioning-scripts.md: (q) is the intentionally-LAST check.
rc=0
P30_CONF="$(ssh_box "cat /etc/systemd/system/camera-box.service.d/publish-30p.conf 2>/dev/null")" || rc=$?
P30_VAL="$(publish_30p_dropin_value "$P30_CONF")"
if [ "$P30_VAL" != "1" ]; then
  fail "publish-30p.conf drop-in missing or CAMERA_BOX_PUBLISH_30P!=1 (got '${P30_VAL:-<none>}', ssh rc=$rc) -- the secondary 30fps '(30p)' blend stream will not come up (issue 792 / #1087)"
elif [ -z "$CB_JOURNAL" ]; then
  fail "publish-30p.conf enabled but the camera-box journal was unreadable -- cannot confirm the '(30p)' stream is actually being published (issue 792 / #1087)"
elif [ "$(publish_30p_stream_live "$CB_JOURNAL")" != "0" ]; then
  ok "publish-30p.conf CAMERA_BOX_PUBLISH_30P=1 and the '(30p)' blend stream is live (issue 792 / #1087)"
else
  fail "publish-30p.conf enabled but NO '(30p)' publisher activity in the last 300 journal lines -- the secondary blend stream is NOT being published (old binary predating issue 792? issue 792 / #1087)"
fi

# (aa) interkom audio bake-in: by-NAME asound.conf + per-box Mic/PCM mixer gains + alsa-utils
# installed (#782) ------------------------------------------------------------------------------
# Provisioning must reproduce the hand-unified fleet audio state so a re-provisioned box does not
# drift back to the enumeration-time card-NUMBER asound.conf (dangling on re-enumeration, #728) or
# the CSCTEK headset's power-on default gain (Mic 91%). THREE facets, all FAIL on any miss: (1)
# /etc/asound.conf is the by-NAME form (contains CARD=HID), (2) alsa-utils is installed (else the
# gain is neither readable nor persisted across boot -- the cam1/cam3 drift), (3) the LIVE
# `amixer -c HID` Mic/PCM percents match this box's per-box table (interkom_mic_pct/pcm_pct).
# Every ssh_box read is `|| rc=$?`-guarded (the (e)/(z) shape) so an unreachable box FALLS to the
# first fail branch, never aborts the gate. Inserted BEFORE (q) -- (q) stays the LAST check.
asrc=0
ASOUND_CONF="$(ssh_box "cat /etc/asound.conf 2>/dev/null")" || asrc=$?
autrc=0
ALSA_UTILS_N="$(ssh_box "dpkg -l alsa-utils 2>/dev/null | grep -c '^ii' || true")" || autrc=$?
micrc=0
MIC_AMIXER="$(ssh_box "amixer -c HID sget Mic 2>/dev/null")" || micrc=$?
pcmrc=0
PCM_AMIXER="$(ssh_box "amixer -c HID sget PCM 2>/dev/null")" || pcmrc=$?
MIC_ACTUAL="$(interkom_amixer_pct "$MIC_AMIXER")"
PCM_ACTUAL="$(interkom_amixer_pct "$PCM_AMIXER")"
MIC_EXPECT="$(interkom_mic_pct "$NAME_UPPER")"
PCM_EXPECT="$(interkom_pcm_pct "$NAME_UPPER")"
if [ "$(interkom_asound_by_name_count "$ASOUND_CONF")" = "0" ]; then
  fail "/etc/asound.conf is not the by-NAME form (no CARD=HID -- old card-number/dangling config, ssh rc=$asrc) (#782)"
elif [ "${ALSA_UTILS_N:-0}" = "0" ]; then
  fail "alsa-utils not installed (ssh rc=$autrc) -- interkom Mic/PCM gain is neither readable nor persisted across boot (#782)"
elif [ -z "$MIC_ACTUAL" ] || [ -z "$PCM_ACTUAL" ]; then
  fail "could not read interkom Mic/PCM gain via 'amixer -c HID' (Mic='${MIC_ACTUAL:-<none>}' PCM='${PCM_ACTUAL:-<none>}', ssh rc=$micrc/$pcmrc) (#782)"
elif [ "$MIC_ACTUAL" != "$MIC_EXPECT" ] || [ "$PCM_ACTUAL" != "$PCM_EXPECT" ]; then
  fail "interkom mixer gain drift on $NAME_UPPER: Mic ${MIC_ACTUAL}% (expect ${MIC_EXPECT}%) / PCM ${PCM_ACTUAL}% (expect ${PCM_EXPECT}%) (#782)"
else
  ok "interkom audio: by-NAME asound.conf + Mic ${MIC_ACTUAL}%/PCM ${PCM_ACTUAL}% (per-box #782) + alsa-utils installed"
fi

# (ab) RemoteOS MCP control-channel agent (#1066) ----------------------------------------------
# The linux-camN MCP surface (:8092) is served by the SEPARATE zbynekdrlik/remoteos-mcp agent
# (remoteos-mcp.service), provisioned by setup-device.sh STEP 17b. This POST-REBOOT check proves
# the LIVE surface (where setup-device.sh's enable-only gate deliberately stops): the unit is
# ENABLED (reboot-survival) AND active AND :8092 is listening. Every ssh_box read is `|| rc=$?`-
# guarded (the (aa)/(e)/(z) shape) so an unreachable box FALLS to the first fail branch, never
# aborts the gate. Inserted BEFORE (q) -- (q) stays the LAST check.
mcpenr=0
# tr runs on the REMOTE (inside ssh_box's command) so the local `|| mcpenr=$?` captures ssh's OWN
# rc (255 on an unreachable box), not the always-0 exit of a local `| tr` pipe -- an accurate
# `ssh rc=` in the fail message (review 🔵). The verdict itself is fail-closed either way (empty
# state never equals "enabled").
MCP_ENABLED="$(ssh_box "systemctl is-enabled remoteos-mcp 2>/dev/null | tr -d '[:space:]'")" || mcpenr=$?
mcpacr=0
MCP_ACTIVE="$(ssh_box "systemctl is-active remoteos-mcp 2>/dev/null | tr -d '[:space:]'")" || mcpacr=$?
mcplr=0
MCP_LISTEN="$(ssh_box "ss -ltn 2>/dev/null | grep -cE ':8092([^0-9]|\$)' || true")" || mcplr=$?
if [ "${MCP_ENABLED:-}" != "enabled" ]; then
  fail "remoteos-mcp.service is not enabled (is-enabled='${MCP_ENABLED:-<none>}', ssh rc=$mcpenr) -- the linux-camN MCP surface would be dead after a reboot (#1066)"
elif [ "${MCP_ACTIVE:-}" != "active" ]; then
  fail "remoteos-mcp.service is not active (is-active='${MCP_ACTIVE:-<none>}', ssh rc=$mcpacr) -- the linux-camN MCP :8092 surface is down (#1066)"
elif [ "${MCP_LISTEN:-0}" = "0" ]; then
  fail "remoteos-mcp is not listening on :8092 (ss rc=$mcplr) -- the linux-camN MCP surface is unreachable (#1066)"
else
  ok "remoteos-mcp agent enabled + active + listening on :8092 (linux-camN MCP surface, #1066)"
fi

# (ac) realtime-isolation drift (issue 899) -- WARNING only for now ------------------------------
# Surfaces the issue-899 realtime state at the acceptance gate. Two facts, read in ONE ssh call:
#   * whether the running kernel is PREEMPT_RT (defect 1 -- the fleet is NOT yet, so this is
#     informational, never a FAIL);
#   * whether the xhci capture IRQ is routed OFF the isolated grab core on a stock kernel (defect 3
#     -- the fix lives in src/affinity.rs setup_irq_affinity and lands on the next fleet redeploy).
# Scope (deliberate, WARN-only surfacing): it grades the REPRESENTATIVE first capture IRQ (head -1)
# -- the fleet has one xhci host-controller IRQ, and setup_irq_affinity writes the same mask to every
# matching IRQ, so the first is representative of the routing decision. It also derives the grab core
# as cpulist_max(/sys/.../isolated), i.e. the AUTO-derived isolated core -- the same core the binary
# pins unless the CAMERA_BOX_CAPTURE_CORE ops override is set (not used on the fleet).
# WARN-only, inserted BEFORE (q) per .claude/rules/provisioning-scripts.md (the (q)-last invariant).
# A box still running the pre-899 binary WARNs here (capture IRQ on the grab core); the flip to a
# hard FAIL is a documented follow-up gated on the fleet redeploy (docs/runbooks/899-realtime-isolation.md).
rtrc=0
RT_STATE="$(ssh_box '
  rt=0; { grep -q PREEMPT_RT /proc/version || [ "$(cat /sys/kernel/realtime 2>/dev/null)" = "1" ]; } && rt=1
  iso="$(cat /sys/devices/system/cpu/isolated 2>/dev/null)"
  irqn="$(grep -iE "xhci|ehci|ohci|uvcvideo" /proc/interrupts 2>/dev/null | head -1 | sed "s/:.*//" | tr -d " ")"
  irql=""; [ -n "$irqn" ] && irql="$(cat /proc/irq/$irqn/smp_affinity_list 2>/dev/null)"
  printf "%s\n%s\n%s\n%s\n" "$rt" "$iso" "$irqn" "$irql"
')" || rtrc=$?
if [ "$rtrc" -ne 0 ]; then
  warn "could not read realtime-isolation state (ssh rc=$rtrc) -- issue-899 check (ac) incomplete"
else
  RT_FLAG="$(printf '%s' "$RT_STATE" | sed -n 1p)"
  RT_ISO="$(printf '%s' "$RT_STATE" | sed -n 2p)"
  RT_IRQN="$(printf '%s' "$RT_STATE" | sed -n 3p)"
  RT_IRQL="$(printf '%s' "$RT_STATE" | sed -n 4p)"
  RT_CORE="$(cpulist_max "$RT_ISO")"
  RT_VERDICT="$(rt_irq_placement_verdict "$RT_FLAG" "$RT_IRQL" "$RT_CORE")"
  # Defect 1: report the kernel preemption model (never a FAIL -- the fleet is not RT yet).
  if [ "$RT_FLAG" = "1" ]; then
    ok "kernel is PREEMPT_RT -- the SCHED_FIFO priorities + IRQ placement are fully honoured (issue 899 defect 1)"
  else
    warn "kernel is NOT PREEMPT_RT (stock voluntary-preempt); a hardirq can preempt even the prio-90 grab -- see docs/runbooks/899-realtime-isolation.md (issue 899 defect 1)"
  fi
  # Defect 3: capture-IRQ placement relative to the isolated grab core.
  case "$RT_VERDICT" in
    ok-off-grab)
      ok "capture IRQ ${RT_IRQN:-?} routed OFF the isolated grab core ${RT_CORE:-?} (list=${RT_IRQL:-?}) on a stock kernel (issue 899 defect 3)" ;;
    ok-on-grab)
      ok "capture IRQ ${RT_IRQN:-?} co-located on the isolated core ${RT_CORE:-?} on a PREEMPT_RT kernel (issue 899 defect 3)" ;;
    drift-on-grab)
      warn "capture IRQ ${RT_IRQN:-?} is on the isolated grab core ${RT_CORE:-?} (list=${RT_IRQL:-?}) on a stock kernel -- redeploy the issue-899 binary to route it off (docs/runbooks/899-realtime-isolation.md, issue 899 defect 3)" ;;
    rt-off-grab)
      warn "PREEMPT_RT kernel but capture IRQ ${RT_IRQN:-?} is NOT co-located on the isolated core ${RT_CORE:-?} (list=${RT_IRQL:-?}) -- the RT co-location optimization is not applied (issue 899)" ;;
    *)
      warn "could not grade capture-IRQ placement (irq='${RT_IRQN:-}', list='${RT_IRQL:-}', core='${RT_CORE:-}') -- issue-899 check (ac) incomplete" ;;
  esac
fi

# (q) .bak cruft drift -- WARNING only, never a FAIL (#453) -------------------------------------
# Reuses NDI_LS gathered in (g)/(o); a second ssh call lists the systemd drop-in dir. Inert
# cruft (ldconfig/systemd never load a .bak file) is surfaced so it's visible in verify-fleet.sh's
# rollup, but it must NEVER fail this box's acceptance gate -- setup-device.sh's cleanup_bak_cruft
# self-heals it on the box's next provisioning pass.
drc=0
DROPIN_LS="$(ssh_box "ls -1 /etc/systemd/system/camera-box.service.d 2>/dev/null")" || drc=$?
BAK_CRUFT="$(bak_cruft_report "$NDI_LS" "$DROPIN_LS")"
if [ "$drc" -ne 0 ]; then
  # A transient ssh failure (or a missing drop-in dir) must NOT be silently reported as "clean" --
  # surface it as a warning so it isn't mistaken for a verified-empty result. Still never a FAIL.
  warn "could not list the systemd drop-in dir (ssh/ls rc=$drc) -- .bak cruft check (q) is incomplete for that dir"
elif [ -n "$BAK_CRUFT" ]; then
  warn "stale .bak cruft present (inert -- setup-device.sh cleans this on the box's next provisioning pass, #453): $(printf '%s' "$BAK_CRUFT" | tr '\n' ' ')"
else
  ok "no stale .bak cruft under the NDI dir or the systemd drop-in dir"
fi

echo ""
if [ "$FAILS" -eq 0 ]; then
  echo -e "${GREEN}ALL CLEAR${NC} -- $NAME_UPPER passes every acceptance check (#454)."
  exit 0
fi
echo -e "${RED}VERIFY FAILED${NC} -- $FAILS check(s) failed on $NAME_UPPER. See [FAIL] lines above." >&2
exit 1
