#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (no side effects at source time) — mirrors
# scripts/lib/imag-power-envelope.sh / scripts/lib/timesync-authority.sh; a sourced lib must NOT
# impose `set -euo pipefail` on its caller.
# scripts/lib/imag-display-path.sh — shared imag-nb DISPLAY-PATH drift gather + verdict core (#780).
#
# Root cause (#780): the whole measurement chain (OBS `GetStats`, the E2E recording verdict decoded
# from a recording branched off BEFORE display, static screenshots) ends PRESENTATIONALLY before the
# real display path — OBS -> compositor -> GPU scanout -> HDMI. A projection lag/tearing that is
# actually a CONFIG state (a compositor state that is wrong for THIS box, an iGPU idling its clock, a
# lost xorg.conf.d option, the wrong output as vsync anchor) lived in a layer with no test. These
# states are DETERMINISTIC, so this lib guards them: a drift FAILs `drift-guard --check-imag` (and
# the E2E `[0/8]` preflight) loudly, naming the drifted facet, in a minute — instead of surviving
# every green run.
#
# HARDWARE REALITY (STEP-0 live validation on 10.77.9.182, read-only): the imag box is Intel-iGPU-
# only (Raptor Lake-P UHD, `modesetting`+glamor, NO discrete NVIDIA) — so the ticket's NVIDIA-era
# facets translate as noted below (see the #780 + issue 1146 validation comments and #816/#841 which
# established the Intel-only reality in setup-imag.sh).
#
# COMPOSITOR DOCTRINE REVERSAL (issue 1146, live-validated 2026-08-20 — SUPERSEDES the #841 "picom
# OFF" facet). #841 concluded "on `modesetting`, the ABSENCE of a compositor gives the tear-free
# direct Present+PageFlip full-screen scanout" and this lib DRIFTed when picom ran. That holds for a
# SINGLE output, but imag drives TWO (eDP panel + HDMI projector), each on its own 60 Hz crystal.
# GL/scanout presentation can vsync to only ONE CRTC (the primary); with a compositor-free direct
# scanout the projector's CRTC is NOT guaranteed to be the sync target, so the two clocks BEAT — a
# clean image when the phases align, a walking tear line when they drift apart ("raz dobre, raz zle"
# = a non-deterministic sync target, not a broken box). The live fix: a picom v10 vsync compositor
# (glx, `unredir-if-possible=false` so the fullscreen Program projector stays composited/vsynced,
# zero eye-candy) ANCHORED on the projector by making HDMI the xrandr PRIMARY. So the facet polarity
# is INVERTED vs #841:
#   * picom RUNNING with vsync      -> now OK (the deterministic tear-free present of the projectors).
#                                      picom NOT running -> DRIFT (the dual-output beat returns).
#   * picom.service ENABLED (user   -> OK: the persistence half — picom launches every graphical
#     systemd unit)                    session (setup-imag.sh step 27). Not enabled -> DRIFT.
#
# REVERTED SAME DAY (issue 1146 revert, live-measured 2026-08-20): the compositor tear fix above
# cost 21.57% OBS render skips on the 25W power envelope (imag render-health preflight, window 2/5
# with MV open) — real dropped output frames chain-wide, strictly worse than the display-only
# tearing it cured; stopping picom returned the same session to 0.00% skips over a 20 s GetStats
# window. So the #841 "picom off" polarity STANDS (facets below expect picom NOT running / unit NOT
# enabled), the picom package+config+unit stay installed DORMANT, and the tear-free direction is
# the OBS projector's own vsync (or a single-display mode) — tracked on issue 1146 / issue 1147.
# The dual-output beat analysis above remains VALID physics; only the compositor CURE is rejected
# for its render cost on this box.
#   * HDMI is xrandr PRIMARY         -> OK: the projector is the vsync anchor. A non-HDMI primary
#                                      (the panel) -> DRIFT (the panel becomes the anchor and the
#                                      projector tears). This REVERSES the #522/#488 "panel primary"
#                                      autostart doctrine — see setup-imag.sh step 16 + the issue
#                                      1146 design comment; projector placement is by connector type
#                                      (imag_scenes.py), NOT by the primary flag, so the flip is safe.
#   * layout: EXTENDED not MIRROR    -> OK: the eDP panel + HDMI projector run at DISTINCT xrandr
#     (issue 1146)                      origins. MIRROR (both outputs at the SAME origin, e.g. +0+0)
#                                      is two independent 60Hz CRTCs at one position: present-vsync
#                                      locks to only ONE, the other free-runs -> a walking tear line
#                                      -> DRIFT. This is the facet that CATCHES the real 2026-08-27
#                                      drift: while the box ran mirrored, hdmi_primary stayed OK
#                                      (HDMI genuinely WAS primary in the mirror), so the gate stayed
#                                      green for days while the projector tore. Position-agnostic
#                                      (origins must be DISTINCT, never a hardcoded position). The
#                                      committed `~/.config/openbox/autostart` sets the extended
#                                      layout at boot; a mirror is a LIVE DRIFT from that intent.
#   * GPUPowerMizerMode=1 (NVIDIA)  -> the genuine Intel counterpart is `imag-igpu-maxperf.service`
#                                      (#841): it pins the iGPU `gt_min_freq` FLOOR to the hardware's
#                                      own `gt_RP0` ceiling so the GPU never idles down and ramp-
#                                      hitches under load. This is NOT the #1040 power-envelope facet
#                                      (that guards PL1/slpc/thermald — the thermal CEILING), so the
#                                      two are complementary, not duplicate.
#   * ForceFullCompositionPipeline  -> NVIDIA-only; the `Option "TearFree"` port was a dead option on
#                                      `modesetting` (#841 live-verified). The picom vsync compositor
#                                      above is the real mechanism now; the inert `20-tearfree.conf`
#                                      left on the live box is deliberately NOT provisioned (it would
#                                      fight the #841 "no dead display xorg.conf.d knob" guard).
#   * touchpad tap conf (#779)      -> still applies (GPU-independent).
#   * drm_output (issue 1152 M4)    -> the DEFAULT-OFF in-OBS DRM-lease HDMI output
#                                      (~/.camera-box/drm-output.json, obs-drm-output.md). Dormant
#                                      = OK (the fleet default — never a false abort); ENABLED
#                                      demands the current OBS log's `program scanout LIVE` proof
#                                      = OK, anything else = DRIFT fail-loud. And when ENABLED the
#                                      hdmi_primary facet flips lease-aware: HDMI is leased OUT of
#                                      the X layout BY DESIGN, so a panel primary is then OK
#                                      (pre-M4 it false-DRIFTed and aborted the [0/8] preflight).
#
# This lib holds the REMOTE gather snippet + the PURE verdict, SHARED by scripts/drift-guard.sh's
# `--check-imag` facet, the E2E `[0/8]` preflight (scripts/recording-e2e.sh), and verify-imag.sh's
# acceptance check — so the gather and the OK/DRIFT/UNKNOWN verdict never exist as driftable copies
# (the SAME extraction discipline #596 (timesync-authority.sh) and #1040 (imag-power-envelope.sh)
# already apply).
#
# Source-only: defines pure functions + one thin ssh-glue preflight; no side effects at source time.

# _dp_field GATHER KEY -> echoes the value after "KEY|" of the FIRST matching line, "" if the key is
# absent OR present with an empty value. Here-string fed (never a pipe) so there is no SIGPIPE under
# a caller's `pipefail`, and the `break`/assignment land in the current shell.
_dp_field() {
  local k v val="" want="$2"
  while IFS='|' read -r k v; do
    if [ "$k" = "$want" ]; then val="$v"; break; fi
  done <<< "$1"
  printf '%s' "$val"
}

# _dp_has GATHER KEY -> exit 0 iff a "KEY|..." line is present (distinguishes "key present, empty
# value" from "key absent" — the UNKNOWN-vs-real two-tier every facet below relies on).
_dp_has() {
  local k rest want="$2"
  while IFS='|' read -r k rest; do
    [ "$k" = "$want" ] && return 0
  done <<< "$1"
  return 1
}

# imag_display_path_verdict GATHER -> echoes one `<facet>|<STATUS>|<detail>` line per facet
# (facets: picom_process, picom_service, hdmi_primary, layout, igpu_maxperf, tap_conf, drm_output;
# STATUS in OK / DRIFT / UNKNOWN). Both callers iterate the lines and map each to their own report style +
# exit-code contract. An EMPTY gather (SSH hiccup), an unread facet, or a missing tool is UNKNOWN —
# never a false OK/DRIFT.
imag_display_path_verdict() {
  local g="$1"

  # --- picom_process (issue 1146 REVERT, live-measured 2026-08-20): picom must NOT be running.
  #     The vsync-compositor tear fix cost 21.57% OBS render skips on the 25W envelope (imag
  #     render-health preflight w2/5); stopping picom returned render to 0.00% skips in the same
  #     session. Render integrity (real output frames) outranks the display-only tearing, so the
  #     #841 "picom off" doctrine stands; the tear-free present must come from the OBS projector's
  #     own vsync (or single-display), never a compositor. #833: a MISSING pgrep must fail loud BY
  #     NAME, never read as a measured verdict.
  if ! _dp_has "$g" PICOM_PGREP; then
    printf 'picom_process|UNKNOWN|picom-process state not gathered\n'
  elif [ "$(_dp_field "$g" PICOM_PGREP)" = "missing" ]; then
    printf 'picom_process|UNKNOWN|pgrep missing on the box — cannot tell if picom runs (install procps); never read as a verdict (#833)\n'
  else
    local _proc
    _proc="$(_dp_field "$g" PICOM_PROC)"
    if [ -n "$_proc" ]; then
      printf 'picom_process|DRIFT|picom running (pid %s) — the compositor starves the OBS render (21.57%% skips measured on the 25W envelope, issue 1146 revert); stop+disable it\n' "$_proc"
    else
      printf 'picom_process|OK|picom not running — full render budget for OBS (issue 1146 revert; tear-free present is the OBS projector vsync direction, not a compositor)\n'
    fi
  fi

  # --- picom_service (issue 1146 REVERT): the persistence half — picom.service (user systemd unit)
  #     must NOT be enabled, or the render-starving compositor comes back at every login (see
  #     picom_process above). The unit + config stay INSTALLED (dormant) for a future A/B. Read
  #     bus-free from the on-disk *.target.wants symlink so a non-login ssh gather is reliable.
  if ! _dp_has "$g" PICOM_SERVICE; then
    printf 'picom_service|UNKNOWN|picom-service state not gathered\n'
  elif [ "$(_dp_field "$g" PICOM_SERVICE)" = "enabled" ]; then
    printf 'picom_service|DRIFT|picom.service enabled (user systemd) — the render-starving compositor relaunches every login (issue 1146 revert); systemctl --user disable picom.service\n'
  else
    printf 'picom_service|OK|picom.service not enabled — the compositor stays dormant (issue 1146 revert)\n'
  fi

  # --- hdmi_primary (issue 1146): HDMI (the projector) must be the xrandr PRIMARY so picom/GL vsync
  #     anchors on the projector CRTC (the dual-output beat: presentation syncs only the primary
  #     CRTC). xrandr presence is probed (#833) — a missing xrandr degrades ONLY this facet to
  #     UNKNOWN; an empty read (X unreachable over ssh, or no primary set) is UNKNOWN, never a false
  #     DRIFT; only a real NON-HDMI primary (the panel) is a DRIFT.
  if [ "$(_dp_field "$g" DRM_OUTPUT_ENABLED)" = "true" ]; then
    # issue 1152 M4: with the in-OBS DRM-lease output ENABLED the HDMI connector is leased OUT of
    # the X layout BY DESIGN — the panel becomes the (only) X primary and that is CORRECT, never
    # the issue-1146 DRIFT. Whether the Program actually reaches the projector is the drm_output
    # facet's verdict below, not this one's.
    printf 'hdmi_primary|OK|drm-output ENABLED (issue 1152) — the HDMI connector is leased OUT of the X layout, so the panel primary is correct by design; projector sync is the DRM page-flip, not the X primary anchor\n'
  elif ! _dp_has "$g" XRANDR; then
    printf 'hdmi_primary|UNKNOWN|xrandr-primary state not gathered\n'
  elif [ "$(_dp_field "$g" XRANDR)" = "missing" ]; then
    printf 'hdmi_primary|UNKNOWN|xrandr missing on the box — cannot read the primary output; never read as a verdict (#833)\n'
  else
    local _prim
    _prim="$(_dp_field "$g" PRIMARY_OUTPUT)"
    case "$_prim" in
      "")    printf 'hdmi_primary|UNKNOWN|no primary output read (X unreachable over ssh, or no primary set) — not a proven drift\n' ;;
      HDMI*) printf 'hdmi_primary|OK|%s is the xrandr primary — the projector is the vsync anchor (issue 1146)\n' "$_prim" ;;
      *)     printf 'hdmi_primary|DRIFT|primary is %s not HDMI — the panel is the vsync anchor, so the HDMI projector shows the dual-output tearing beat (issue 1146)\n' "$_prim" ;;
    esac
  fi

  # --- layout (issue 1146): the eDP panel + HDMI projector must run EXTENDED (each active output at
  #     a DISTINCT xrandr origin), never MIRROR (two outputs at the SAME origin, e.g. both +0+0). A
  #     mirror is two independent 60 Hz CRTCs at one position: the EGL swapInterval(1) / #1107
  #     present-vsync can lock to only ONE of them, the other scans out free-running -> a walking
  #     tear line on the projector (the exact beat the ticket describes). hdmi_primary alone stayed
  #     OK through the whole mirror drift (HDMI genuinely WAS the primary in a mirror), so THIS facet
  #     is what actually catches it. Position-agnostic: the extended origins legitimately vary by
  #     which output is primary/left, so the invariant is simply "origins must be DISTINCT". Two-tier
  #     (#833): xrandr missing / not gathered / origins not gathered / fewer than 2 active outputs /
  #     unreadable -> UNKNOWN, never a false DRIFT. The `|| true` on the count pipelines keeps a
  #     `grep -c` zero-match (exit 1) from aborting the caller under `set -euo pipefail` (#1133).
  if ! _dp_has "$g" XRANDR; then
    printf 'layout|UNKNOWN|display-layout state not gathered\n'
  elif [ "$(_dp_field "$g" XRANDR)" = "missing" ]; then
    printf 'layout|UNKNOWN|xrandr missing on the box — cannot read the monitor layout; never read as a verdict (#833)\n'
  elif ! _dp_has "$g" MONITOR_ORIGINS; then
    printf 'layout|UNKNOWN|monitor origins not gathered (truncated gather) — not a proven drift\n'
  else
    local _origins _ntotal _ndistinct
    _origins="$(_dp_field "$g" MONITOR_ORIGINS)"
    _ntotal="$(printf '%s\n' $_origins | grep -c . || true)"
    _ndistinct="$(printf '%s\n' $_origins | sort -u | grep -c . || true)"
    if [ "${_ntotal:-0}" -lt 2 ]; then
      printf 'layout|UNKNOWN|only %s active output origin(s) read (X unreachable over ssh, or a single-monitor box) — not a proven drift (issue 1146)\n' "${_ntotal:-0}"
    elif [ "$_ntotal" = "$_ndistinct" ]; then
      printf 'layout|OK|extended — %s active outputs at distinct origins (%s); each CRTC has its own position so present-vsync anchors per output (issue 1146)\n' "$_ntotal" "$_origins"
    else
      printf 'layout|DRIFT|MIRROR — %s active outputs share an origin (only %s distinct: %s) -> two unsynchronized 60Hz CRTCs at one position -> projector tears; set the extended layout (issue 1146)\n' "$_ntotal" "$_ndistinct" "$_origins"
    fi
  fi

  # --- igpu_maxperf: the #841 Intel counterpart to GPUPowerMizerMode=1. OK iff the service is
  #     enabled+active AND gt_min_freq is pinned to the hardware ceiling gt_RP0. Hardware-agnostic
  #     (#816): a box with no i915 gt sysfs (MAXPERF_APPLICABLE|0) reads UNKNOWN, never false DRIFT.
  if ! _dp_has "$g" MAXPERF_APPLICABLE; then
    printf 'igpu_maxperf|UNKNOWN|iGPU maxperf state not gathered\n'
  elif [ "$(_dp_field "$g" MAXPERF_APPLICABLE)" != "1" ]; then
    printf 'igpu_maxperf|UNKNOWN|no i915 gt_min_freq sysfs — not an Intel-iGPU box (the #841 pin is Intel-only)\n'
  else
    local _min _rp0 _en _ac
    _min="$(_dp_field "$g" MAXPERF_MIN)"
    _rp0="$(_dp_field "$g" MAXPERF_RP0)"
    _en="$(_dp_field "$g" MAXPERF_ENABLED | tr -d '[:space:]')"
    _ac="$(_dp_field "$g" MAXPERF_ACTIVE | tr -d '[:space:]')"
    if [ "$_en" = "enabled" ] && [ "$_ac" = "active" ] && [ -n "$_min" ] && [ "$_min" = "$_rp0" ]; then
      printf 'igpu_maxperf|OK|imag-igpu-maxperf.service up, gt_min_freq=%s pinned to gt_RP0=%s (Intel GPUPowerMizerMode=1 analog, #841)\n' "$_min" "$_rp0"
    else
      printf 'igpu_maxperf|DRIFT|service enabled=%s active=%s, gt_min_freq=%s vs gt_RP0=%s — iGPU can idle down -> DVFS ramp stutter (#841)\n' \
        "${_en:-<none>}" "${_ac:-<none>}" "${_min:-<none>}" "${_rp0:-<none>}"
    fi
  fi

  # --- tap_conf (#779): /etc/X11/xorg.conf.d/30-touchpad-tap.conf present with Option "Tapping" on.
  #     A gathered-but-absent conf is a genuine DRIFT (provisioning lost); only "not gathered" (SSH
  #     hiccup — no TAPCONF line at all) is UNKNOWN.
  if ! _dp_has "$g" TAPCONF; then
    printf 'tap_conf|UNKNOWN|tap-conf state not gathered\n'
  elif [ "$(_dp_field "$g" TAPCONF)" = "absent" ]; then
    printf 'tap_conf|DRIFT|/etc/X11/xorg.conf.d/30-touchpad-tap.conf is GONE (#779 tap-to-click config removed)\n'
  else
    local _tap
    _tap="$(_dp_field "$g" TAPCONF_TAPPING | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
    if [ "$_tap" = "on" ]; then
      printf 'tap_conf|OK|30-touchpad-tap.conf present with Tapping on (#779)\n'
    else
      printf 'tap_conf|DRIFT|30-touchpad-tap.conf present but Tapping=%s (expected on, #779)\n' "${_tap:-<none>}"
    fi
  fi

  # --- drm_output (issue 1152 M4): the DEFAULT-OFF in-OBS DRM-lease HDMI output. Dormant (config
  #     absent, or present with "enabled" not true) is the fleet default -> OK, so drift-guard /
  #     the E2E [0/8] preflight / verify (z) never false-abort on a box that simply does not run
  #     it. ENABLED demands PROOF the Program actually reaches the projector: the current OBS
  #     session's `program scanout LIVE` marker -> OK; anything else (bind FAILED, solid-only,
  #     no marker, no readable log, or the M1 "program": false solid DIAGNOSTIC mode — cam2's
  #     grabber taps this HDMI, a grey pattern would wreck a data run) -> DRIFT, fail loud.
  #     Two-tier (#833): a partial gather (config read but the log block unread) is UNKNOWN,
  #     never a false DRIFT; a gathered-but-empty log dir IS a proven "no scanout proof" -> DRIFT.
  #     NB (review): the ENABLED verdict is pure LOG forensics on the NEWEST OBS log — it grades
  #     the last OBS session's scanout, NOT whether OBS is alive right now (a stopped OBS whose
  #     final session went LIVE reads OK; liveness is owned by the sibling gates — obs-liveness,
  #     the reachability watchdogs, verify (h)/(i)). A consumer racing a fresh OBS start reads
  #     DRIFT until `program scanout LIVE` lands — correct fail-loud for a preflight (a run must
  #     not start mid-flip).
  if ! _dp_has "$g" DRM_OUTPUT_CONFIG; then
    printf 'drm_output|UNKNOWN|drm-output state not gathered\n'
  elif [ "$(_dp_field "$g" DRM_OUTPUT_CONFIG)" = "absent" ]; then
    printf 'drm_output|OK|dormant — ~/.camera-box/drm-output.json absent (the DEFAULT-OFF fleet state); Program on the X projector path (issue 1152)\n'
  elif ! _dp_has "$g" DRM_OUTPUT_ENABLED; then
    printf 'drm_output|UNKNOWN|drm-output config present but the enabled state was not gathered (truncated gather) — never read as dormant (issue 1152)\n'
  elif [ "$(_dp_field "$g" DRM_OUTPUT_ENABLED)" != "true" ]; then
    printf 'drm_output|OK|dormant — drm-output.json present but not enabled; Program on the X projector path (issue 1152)\n'
  elif [ "$(_dp_field "$g" DRM_OUTPUT_PROGRAM)" = "false" ]; then
    printf 'drm_output|DRIFT|drm-output ENABLED in the M1 solid diagnostic mode ("program": false) — the projector carries a grey test pattern, not the Program; not a production state (issue 1152)\n'
  elif ! _dp_has "$g" DRM_OUTPUT_LOG; then
    printf 'drm_output|UNKNOWN|drm-output ENABLED but the OBS-log scanout state was not gathered — cannot prove nor disprove the Program scanout (issue 1152)\n'
  elif [ "$(_dp_field "$g" DRM_OUTPUT_LOG)" != "present" ]; then
    printf 'drm_output|DRIFT|drm-output ENABLED but no OBS log is readable on the box — no proof the Program reaches the projector (issue 1152)\n'
  elif ! _dp_has "$g" DRM_OUTPUT_SCANOUT; then
    printf 'drm_output|UNKNOWN|drm-output ENABLED, OBS log present, but the scanout marker read was not gathered (truncated gather) — not a proven drift (issue 1152)\n'
  else
    case "$(_dp_field "$g" DRM_OUTPUT_SCANOUT)" in
      live)        printf 'drm_output|OK|ENABLED and program scanout LIVE — the Program is page-flipping on the DRM-leased HDMI connector (issue 1152)\n' ;;
      bind-failed) printf 'drm_output|DRIFT|ENABLED but the program bind FAILED (current OBS log) — the projector shows the solid fallback, not the Program (issue 1152)\n' ;;
      solid-only)  printf 'drm_output|DRIFT|ENABLED but only the solid pattern is active — no program scanout LIVE marker in the current OBS log (issue 1152)\n' ;;
      *)           printf 'drm_output|DRIFT|ENABLED but the current OBS log carries NO drm-output scanout marker — the lease/output did not come up (issue 1152)\n' ;;
    esac
  fi
}

# imag_display_path_gather_remote_snippet -> the REMOTE shell command (a string) both callers run
# over their own transport to collect the observed display-path state into the `|`-delimited block
# imag_display_path_verdict parses. Uses only ubiquitous tools (cat/systemctl/sed/grep/awk); the two
# not-strictly-guaranteed tools are `pgrep` (procps) and `xrandr`, so each presence is probed and
# emitted (#833) — a missing tool must never let a facet read as a false OK/DRIFT. These use INLINE
# PICOM_PGREP / XRANDR markers rather than the shared imag_require_remote_tool_cmd (scripts/lib/imag-
# require-remote-tool.sh) ON PURPOSE: that helper is for a SEPARATE fail-fast preflight probe that
# HARD-ABORTS the whole run on any absent tool; here the desired semantics are per-facet — a missing
# tool degrades ONLY its own facet to UNKNOWN while the others verdict normally, which the inline
# marker (read by the verdict's own two-tier) gives.
imag_display_path_gather_remote_snippet() {
  cat <<'REMOTE'
# --- picom: pgrep presence (#833) then the picom process itself (issue 1146: running = OK) ---
if command -v pgrep >/dev/null 2>&1; then
  printf 'PICOM_PGREP|ok\n'
  printf 'PICOM_PROC|%s\n' "$(pgrep -x picom 2>/dev/null | head -1 || true)"
else
  printf 'PICOM_PGREP|missing\n'
fi
# --- picom user systemd service enabled (issue 1146): the *.target.wants/picom.service enable
# symlink `systemctl --user enable` creates. A bus-free on-disk check (robust over a non-login ssh
# gather); glob any *.target.wants/picom.service so the exact WantedBy dir is never hardcoded. ---
if ls "$HOME"/.config/systemd/user/*.target.wants/picom.service >/dev/null 2>&1; then
  printf 'PICOM_SERVICE|enabled\n'
else
  printf 'PICOM_SERVICE|disabled\n'
fi
# --- HDMI primary (issue 1146): the projector must be the xrandr PRIMARY output so picom/GL vsync
# anchors on it. xrandr presence probed (#833); DISPLAY=:0 reads the running session's layout. ---
if command -v xrandr >/dev/null 2>&1; then
  printf 'XRANDR|ok\n'
  # ONE xrandr read feeds BOTH the primary-output facet AND the issue-1146 layout facet (no extra
  # SSH round-trip). Captured to a var so the two awk passes below never re-invoke xrandr.
  _dp_xq="$(DISPLAY=:0 xrandr --query 2>/dev/null || true)"
  printf 'PRIMARY_OUTPUT|%s\n' "$(printf '%s\n' "$_dp_xq" | awk '/ connected primary/{print $1; exit}')"
  # issue 1146 (MIRROR facet): the +X+Y origin of every ACTIVE connected output. A connected-but-
  # DISABLED output has no `WIDTHxHEIGHT+X+Y` geometry token and is skipped. Two active outputs
  # sharing one origin (both +0+0) = MIRROR = two unsynchronized 60Hz CRTCs -> the projector tears
  # (the layout verdict below reads this: all-distinct = extended = OK, a duplicate = mirror = DRIFT).
  printf 'MONITOR_ORIGINS|%s\n' "$(printf '%s\n' "$_dp_xq" | awk '/ connected/ && match($0, /[0-9]+x[0-9]+[-+][0-9]+[-+][0-9]+/){g=substr($0,RSTART,RLENGTH); sub(/^[0-9]+x[0-9]+/,"",g); printf "%s ", g}')"
else
  printf 'XRANDR|missing\n'
fi
# --- iGPU max-freq pin (#841 imag-igpu-maxperf.service — the Intel GPUPowerMizerMode=1 analog) ---
# Identity-based: glob card* (never a hardcoded cardN — the presenter-drm renumbering hazard).
_dp_min=""; _dp_rp0=""
for _c in /sys/class/drm/card[0-9]; do
  [ -e "$_c/gt_min_freq_mhz" ] || continue
  _dp_min="$(cat "$_c/gt_min_freq_mhz" 2>/dev/null || true)"
  _dp_rp0="$(cat "$_c/gt_RP0_freq_mhz" 2>/dev/null || true)"
  break
done
if [ -n "$_dp_min" ]; then
  printf 'MAXPERF_APPLICABLE|1\n'
  printf 'MAXPERF_MIN|%s\n' "$_dp_min"
  printf 'MAXPERF_RP0|%s\n' "$_dp_rp0"
else
  printf 'MAXPERF_APPLICABLE|0\n'
fi
printf 'MAXPERF_ENABLED|%s\n' "$(systemctl is-enabled imag-igpu-maxperf.service 2>/dev/null || true)"
printf 'MAXPERF_ACTIVE|%s\n' "$(systemctl is-active imag-igpu-maxperf.service 2>/dev/null || true)"
# --- touchpad tap conf (#779): extract the SECOND quoted value of the exact `Option "Tapping"` line
# (never the sibling `Option "TappingDrag"` — the closing quote after Tapping anchors both stages).
# The grep AND the sed are BOTH case-insensitive (grep -i; sed's GNU `I` flag) so they agree — a
# hand-edited `Option "TAPPING" "on"` is captured, not silently dropped to a false DRIFT. ---
_dp_tc="/etc/X11/xorg.conf.d/30-touchpad-tap.conf"
if [ -e "$_dp_tc" ]; then
  printf 'TAPCONF|present\n'
  printf 'TAPCONF_TAPPING|%s\n' "$(grep -iE 'Option[[:space:]]+"Tapping"' "$_dp_tc" 2>/dev/null | sed -E 's/.*"tapping"[[:space:]]+"([^"]*)".*/\1/I' | head -1 || true)"
else
  printf 'TAPCONF|absent\n'
fi
# --- drm-output (issue 1152 M4): the DEFAULT-OFF in-OBS DRM-lease output config + the CURRENT
# OBS session's scanout marker. Config: the same machine-written one-line JSON the vendored C
# module reads (obs-drm-output.md); a grep extract is deliberate (no python dependency in this
# gather). OBS-log greps are LC_ALL=C grep -a — OBS logs carry raw invalid-UTF-8 bytes and a
# UTF-8-locale grep can MISS a present ASCII marker (the #1183/#1184 mojibake net). The marker
# priority order matters: a healthy Program session contains BOTH the solid-phase `ACTIVE` line
# AND the later `program scanout LIVE` line, so LIVE is checked FIRST. ---
# The config greps run over a NEWLINE-STRIPPED copy (tr -d) so a pretty-printed multi-line JSON
# reads the same as the runbook's canonical machine-written one-liner. This grep grammar
# deliberately OVER-detects "enabled" on a malformed config (the C module + the python classifier
# parse full JSON and stay dormant): the verdict then DRIFTs loudly (enabled with no LIVE marker,
# naming the broken config) — the fail-LOUD direction — while the live path stays safely dormant.
_dp_cfg="$HOME/.camera-box/drm-output.json"
if [ -e "$_dp_cfg" ]; then
  printf 'DRM_OUTPUT_CONFIG|present\n'
  if tr -d '\n' <"$_dp_cfg" 2>/dev/null | LC_ALL=C grep -aqE '"enabled"[[:space:]]*:[[:space:]]*true'; then
    printf 'DRM_OUTPUT_ENABLED|true\n'
  else
    printf 'DRM_OUTPUT_ENABLED|false\n'
  fi
  if tr -d '\n' <"$_dp_cfg" 2>/dev/null | LC_ALL=C grep -aqE '"program"[[:space:]]*:[[:space:]]*false'; then
    printf 'DRM_OUTPUT_PROGRAM|false\n'
  else
    printf 'DRM_OUTPUT_PROGRAM|true\n'
  fi
else
  printf 'DRM_OUTPUT_CONFIG|absent\n'
fi
_dp_obslog="$(ls -t "$HOME"/.config/obs-studio/logs/*.txt 2>/dev/null | head -1 || true)"
if [ -n "$_dp_obslog" ]; then
  printf 'DRM_OUTPUT_LOG|present\n'
  if LC_ALL=C grep -aq 'drm-output: program scanout LIVE' "$_dp_obslog" 2>/dev/null; then
    printf 'DRM_OUTPUT_SCANOUT|live\n'
  elif LC_ALL=C grep -aq 'drm-output: program bind FAILED' "$_dp_obslog" 2>/dev/null; then
    printf 'DRM_OUTPUT_SCANOUT|bind-failed\n'
  elif LC_ALL=C grep -aq 'drm-output: ACTIVE' "$_dp_obslog" 2>/dev/null; then
    printf 'DRM_OUTPUT_SCANOUT|solid-only\n'
  else
    printf 'DRM_OUTPUT_SCANOUT|none\n'
  fi
else
  printf 'DRM_OUTPUT_LOG|none\n'
fi
REMOTE
}

# imag_display_path_preflight_assert HOST [USER] -> the E2E `[0/8]` fail-fast (#780 item 6). Gathers
# the display-path state over ssh, runs the shared verdict, and returns 1 (printing the drifted
# facets to stderr) iff any facet DRIFTs — so a 40-min run refuses to start on a known display-path
# config drift. UNKNOWN facets (an SSH hiccup; the [0/8] reachability preflight already gates
# genuine unreachability) are warned but do NOT fail the run. Thin ssh glue (NOT unit-tested — same
# convention as gather_and_check_imag / optical_chain_preflight_assert; the JUDGMENT is the pure
# verdict above).
imag_display_path_preflight_assert() {
  local host="${1:?imag_display_path_preflight_assert: HOST required}" user="${2:-newlevel}"
  local target="${user}@${host}"
  local ssh_cmd=(timeout 15 ssh -o ConnectTimeout=10 -o BatchMode=yes -- "$target")
  local gather verdict facet status detail fails="" unknowns="" nl
  nl=$'\n'
  gather="$("${ssh_cmd[@]}" "$(imag_display_path_gather_remote_snippet)" 2>/dev/null || true)"
  verdict="$(imag_display_path_verdict "$gather")"
  while IFS='|' read -r facet status detail; do
    [ -n "$facet" ] || continue
    case "$status" in
      DRIFT)   fails="${fails:+$fails$nl}  - ${facet}: ${detail}" ;;
      UNKNOWN) unknowns="${unknowns:+$unknowns, }${facet}" ;;
    esac
  done <<< "$verdict"
  if [ -n "$fails" ]; then
    printf 'ERROR: imag display-path DRIFT on %s — refusing to start the run (projection lag/tearing config):\n%s\n' \
      "$host" "$fails" >&2
    return 1
  fi
  if [ -n "$unknowns" ]; then
    printf 'WARN: imag display-path facets UNKNOWN on %s (not read; not a proven drift): %s\n' "$host" "$unknowns" >&2
  fi
  printf 'imag display-path preflight OK on %s (picom off, HDMI primary / drm-output coherent, extended layout, iGPU pinned, tap conf)\n' "$host"
  return 0
}
