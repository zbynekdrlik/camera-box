#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (no side effects at source time) — mirrors
# scripts/lib/imag-power-envelope.sh / scripts/lib/timesync-authority.sh; a sourced lib must NOT
# impose `set -euo pipefail` on its caller.
# scripts/lib/imag-display-path.sh — shared imag-nb DISPLAY-PATH drift gather + verdict core (#780).
#
# Root cause (#780): the whole measurement chain (OBS `GetStats`, the E2E recording verdict decoded
# from a recording branched off BEFORE display, static screenshots) ends PRESENTATIONALLY before the
# real display path — OBS -> compositor -> GPU scanout -> HDMI. A projection lag/tearing that is
# actually a CONFIG state (a compositor that shouldn't be running, an iGPU idling its clock, a lost
# xorg.conf.d option) lived in a layer with no test. These states are DETERMINISTIC, so this lib
# guards them: a drift FAILs `drift-guard --check-imag` (and the E2E `[0/8]` preflight) loudly,
# naming the drifted facet, in a minute — instead of surviving every green run.
#
# HARDWARE REALITY (STEP-0 live validation on 10.77.9.182, read-only, 2026-08-17): the imag box is
# now Intel-iGPU-only (Raptor Lake-P UHD, `modesetting`+glamor, NO discrete NVIDIA) — so the
# ticket's NVIDIA-era facets translate as follows (see the #780 validation comment for the full
# evidence, and #816/#841 which established this in setup-imag.sh):
#   * picom OFF                    -> still applies (GPU-independent). On `modesetting`, the ABSENCE
#                                     of a compositor is precisely what gives the tear-free direct
#                                     Present+PageFlip full-screen scanout (#841) — a compositor
#                                     re-introduces a frame + tearing risk.
#   * GPUPowerMizerMode=1 (NVIDIA) -> the genuine Intel counterpart is `imag-igpu-maxperf.service`
#                                     (#841): it pins the iGPU `gt_min_freq` FLOOR to the hardware's
#                                     own `gt_RP0` ceiling so the GPU never idles down and ramp-
#                                     hitches under load. This is NOT the #1040 power-envelope facet
#                                     (that guards PL1/slpc/thermald — the thermal CEILING), so the
#                                     two are complementary, not duplicate.
#   * ForceFullCompositionPipeline -> NVIDIA-only; has NO counterpart on `modesetting` (TearFree is
#                                     a dead option on this driver, #841 live-verified). #790's
#                                     +1-frame concern is inherently moot on Intel (no FFCP -> no
#                                     extra frame), so NO facet is emitted for it — nothing to
#                                     hardcode that #790 would flip.
#   * touchpad tap conf (#779)     -> still applies (GPU-independent).
#
# This lib holds the REMOTE gather snippet + the PURE verdict, SHARED by scripts/drift-guard.sh's
# `--check-imag` facet and the E2E `[0/8]` preflight (scripts/recording-e2e.sh) so the gather and
# the OK/DRIFT/UNKNOWN verdict never exist as two driftable copies — the SAME extraction discipline
# #596 (timesync-authority.sh) and #1040 (imag-power-envelope.sh) already apply.
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
# (facets: picom_process, picom_autostart, igpu_maxperf, tap_conf; STATUS in OK / DRIFT / UNKNOWN).
# Both callers iterate the lines and map each to their own report style + exit-code contract. An
# EMPTY gather (SSH hiccup), an unread facet, or a missing tool is UNKNOWN — never a false OK/DRIFT.
imag_display_path_verdict() {
  local g="$1"

  # --- picom_process: pgrep -x picom must be empty. #833: a MISSING pgrep must fail loud BY NAME,
  #     never read as a measured "not running = OK".
  if ! _dp_has "$g" PICOM_PGREP; then
    printf 'picom_process|UNKNOWN|picom-process state not gathered\n'
  elif [ "$(_dp_field "$g" PICOM_PGREP)" = "missing" ]; then
    printf 'picom_process|UNKNOWN|pgrep missing on the box — cannot tell if picom runs (install procps); never read as OK (#833)\n'
  else
    local _proc
    _proc="$(_dp_field "$g" PICOM_PROC)"
    if [ -n "$_proc" ]; then
      printf 'picom_process|DRIFT|picom IS running (pid %s) — a compositor breaks the tear-free direct Present+PageFlip scanout (#841)\n' "$_proc"
    else
      printf 'picom_process|OK|picom not running (compositor-free direct scanout)\n'
    fi
  fi

  # --- picom_autostart: ~/.config/autostart/picom.desktop absent, or present with Hidden=true.
  if ! _dp_has "$g" PICOM_AUTOSTART; then
    printf 'picom_autostart|UNKNOWN|picom-autostart state not gathered\n'
  elif [ "$(_dp_field "$g" PICOM_AUTOSTART)" = "absent" ]; then
    printf 'picom_autostart|OK|no picom.desktop autostart entry — picom cannot launch at login\n'
  else
    local _hidden
    _hidden="$(_dp_field "$g" PICOM_AUTOSTART_HIDDEN | tr '[:upper:]' '[:lower:]' | tr -d '[:space:]')"
    if [ "$_hidden" = "true" ]; then
      printf 'picom_autostart|OK|picom.desktop autostart masked (Hidden=true)\n'
    else
      printf 'picom_autostart|DRIFT|picom.desktop autostart present and NOT masked (Hidden=%s) — picom would launch at login\n' "${_hidden:-<none>}"
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
}

# imag_display_path_gather_remote_snippet -> the REMOTE shell command (a string) both callers run
# over their own transport to collect the observed display-path state into the `|`-delimited block
# imag_display_path_verdict parses. Uses only ubiquitous tools (cat/systemctl/sed/grep); the ONE
# not-strictly-guaranteed tool is `pgrep` (procps), so its presence is probed and emitted (#833) —
# a missing pgrep must never let "picom not running" read as a false OK.
imag_display_path_gather_remote_snippet() {
  cat <<'REMOTE'
# --- picom: pgrep presence (#833) then the picom process itself ---
if command -v pgrep >/dev/null 2>&1; then
  printf 'PICOM_PGREP|ok\n'
  printf 'PICOM_PROC|%s\n' "$(pgrep -x picom 2>/dev/null | head -1 || true)"
else
  printf 'PICOM_PGREP|missing\n'
fi
# --- picom autostart entry (~/.config/autostart/picom.desktop absent OR Hidden=true) ---
_dp_as="$HOME/.config/autostart/picom.desktop"
if [ -e "$_dp_as" ]; then
  printf 'PICOM_AUTOSTART|present\n'
  printf 'PICOM_AUTOSTART_HIDDEN|%s\n' "$(sed -n 's/^[[:space:]]*Hidden[[:space:]]*=[[:space:]]*//p' "$_dp_as" 2>/dev/null | head -1 || true)"
else
  printf 'PICOM_AUTOSTART|absent\n'
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
# (never the sibling `Option "TappingDrag"` — the grep anchors on the closing quote after Tapping) ---
_dp_tc="/etc/X11/xorg.conf.d/30-touchpad-tap.conf"
if [ -e "$_dp_tc" ]; then
  printf 'TAPCONF|present\n'
  printf 'TAPCONF_TAPPING|%s\n' "$(grep -iE 'Option[[:space:]]+"Tapping"' "$_dp_tc" 2>/dev/null | sed -E 's/.*"[Tt]apping"[[:space:]]+"([^"]*)".*/\1/' | head -1 || true)"
else
  printf 'TAPCONF|absent\n'
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
  printf 'imag display-path preflight OK on %s (picom off, iGPU pinned, tap conf)\n' "$host"
  return 0
}
