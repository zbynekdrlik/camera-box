#!/bin/bash
# airuleset:script-ok source-only lib (defines pure functions + _cmds builders, no top-level
# statements) -- matches the scripts/lib/*.sh convention of deliberately NOT setting
# `set -euo pipefail` here: sourcing this file executes it in the CALLER's shell, so imposing
# strict mode here would leak into whichever caller sources it. scripts/recording-e2e.sh (the
# only caller today) already sets -euo pipefail itself.
#
# #887 -- imag's zero-loss proof stopped at OBS's own compositor (self-reported renderSkip/
# activeFps, known to diverge from reality -- run 30538757420 showed renderSkip=1.72% on imag
# while every recording-side term was clean). Nothing verified what actually left imag-nb's
# HDMI-1 connector. User's settled decision (2026-07-31, option 3, software-only, no hardware):
# compare the compositor's OWN produced-frame count (scripts/imag_produced_frame_check.py, a
# GetStats snapshot) against an INDEPENDENT observer -- the i915 kernel's per-CRTC CRC debugfs
# counter for the connector actually driving HDMI-A-1, sampled over the SAME bounded window.
#
# Every frame the kernel scans out to the connector produces ONE line in `crc/data` (a
# monotonic per-vblank hex frame counter + a CRC of the scanned content). Two CONSECUTIVE lines
# sharing the SAME crc0 value mean the SAME content was presented twice in a row -- a real
# display-level repeat the kernel observed independently of anything OBS itself reports (OBS
# could think it rendered fine while the compositor simply never handed the kernel a new buffer
# for that vblank).
#
# SCOPING (mandatory, part of the deliverable per the ticket): every field/name here says
# "presented on HDMI-1" / "produced by the compositor" -- and stops there, deliberately never
# claiming visibility past the connector. A CRC-observed frame proves it left the GPU on the
# connector; it proves NOTHING about the physical projection surface, or anything else
# downstream of the cable. Hardware options 1 (capture card) and
# 2 (film the projection) remain open, deferred questions -- this covers option 3 only. This is
# also NOT a whole-recording-window measurement -- it is a bounded SAMPLE taken once during the
# run (see `imag_presented_frame_sample_cmds`'s own doc below for why), scoped honestly as such
# in every field name (`sample_window_s`, never "recording window").
#
# Card numbering is NOT stable ABI (.claude/rules/presenter-drm-selection.md, #854) -- the card
# number is resolved dynamically from /sys/class/drm/card*-HDMI-A-1 every call, never hardcoded.
# The debugfs tree is root-only; every privileged read/write goes through `sudo -S` fed the
# box's own password (same plaintext-password posture this whole harness already uses for
# CAM_PW/IMAG_PW -- never a credential that isn't already a shared rig-dev password). Every
# privileged call passes its remote-side dynamic values as POSITIONAL ARGUMENTS to
# `sudo bash -c 'SCRIPT' bash "$@"` rather than interpolating them into the single-quoted SCRIPT
# text -- this sidesteps nested-quoting entirely (the SCRIPT text is a fixed literal; only
# "$1"/"$2"/... vary).
#
# WHY A SYNCHRONOUS BOUNDED SAMPLE, NOT A BACKGROUND CAPTURE SPANNING THE WHOLE RECORDING WINDOW
# (live-verified 2026-08-01, both failure modes reproduced against the real imag-nb box):
#   1. A backgrounded `timeout N cat .../data > file &` cannot be reliably STOPPED from a later,
#      separate ssh call: the PID captured via `$!` is the wrapping pipeline/subshell, not the
#      actual `cat` grandchild spawned inside `sudo bash -c` -- killing it does not reach the
#      real reader.
#   2. The obvious fix -- `pkill -f` on the debugfs path -- is a SELF-MATCH: that same literal
#      path string is, unavoidably, also part of THIS remote script's own source text (the
#      `pkill` invocation itself), so it matches the wrapping `bash -c` this whole script runs
#      under too and kills the SSH session before any result ever prints (the exact `pkill -f`
#      self-match class documented in the top-level CLAUDE.md GOTCHA for camera-box-burn units).
#   3. Consequently the reader is usually STILL open when a later call tries to restore
#      `crc/control` -- writing to `control` while `data` has an active reader returns
#      `Device or resource busy` (confirmed live), silently leaving the CRC source forced to
#      "auto" instead of whatever it originally was.
# A single synchronous `timeout SAMPLE_S cat .../data` blocks for exactly SAMPLE_S seconds, then
# returns on its own -- no PID to track, no kill to attempt, no self-match risk, and the control
# restore right afterward always finds the reader already gone. The cost is exactly what the
# name says: a bounded SAMPLE near the start of the recording window, not full-window coverage --
# honest for a report-only diagnostic (the weakest of the three options the ticket names) and
# far more robust than the alternative.

# imag_presented_frame_pipe_for_connector CONNECTOR -> reads `i915_display_info` text on stdin,
# prints the pipe letter (A/B/C/D) of the FIRST CRTC block that lists CONNECTOR among its
# connectors. A later, unrelated "Connector info" summary section repeats every connector name
# with NO CRTC context -- this returns ONLY the first CRTC-scoped match (never overwritten by
# that later summary), which is the actual bug this function's own test guards against. Pure
# text parsing, no ssh/sudo -- testable with a captured fixture, no rig required.
imag_presented_frame_pipe_for_connector() {
  local conn="$1" line pipe="" found=""
  while IFS= read -r line; do
    case "$line" in
    '[CRTC:'*'pipe '*']:'*)
      pipe="${line#*pipe }"
      pipe="${pipe%%]*}"
      ;;
    *"[CONNECTOR:"*":${conn}]"*)
      if [ -n "$pipe" ] && [ -z "$found" ]; then
        found="$pipe"
      fi
      ;;
    esac
  done
  printf '%s' "$found"
}

# imag_presented_frame_resolve_cmd SUDO_PW -> a REMOTE bash snippet (embed via $(...) into an
# ssh command run as the unprivileged deploy user) that resolves imag-nb's OWN currently
# connected HDMI-A-1 output to its debugfs crtc-N directory. Prints exactly two lines:
#   IMAG_PRESENTED_CRTC_DIR=<crtc-N or empty>
#   IMAG_PRESENTED_CARD_NUM=<N>            (only when the dir line is non-empty)
imag_presented_frame_resolve_cmd() {
  local pw="$1"
  cat <<REMOTE
$(declare -f imag_presented_frame_pipe_for_connector)
_ipf_card=""
for _ipf_d in /sys/class/drm/card*-HDMI-A-1; do
  [ -e "\$_ipf_d/status" ] || continue
  [ "\$(cat "\$_ipf_d/status" 2>/dev/null)" = "connected" ] || continue
  _ipf_card="\$(basename "\$_ipf_d")"
  _ipf_card="\${_ipf_card%-HDMI-A-1}"
  break
done
if [ -z "\$_ipf_card" ]; then
  echo "IMAG_PRESENTED_CRTC_DIR="
else
  _ipf_num="\${_ipf_card#card}"
  _ipf_root="/sys/kernel/debug/dri/\$_ipf_num"
  _ipf_di="\$(printf '%s\n' '$pw' | sudo -S -p '' bash -c 'cat "\$1/i915_display_info" 2>/dev/null' bash "\$_ipf_root")"
  _ipf_pipe="\$(printf '%s\n' "\$_ipf_di" | imag_presented_frame_pipe_for_connector HDMI-A-1)"
  _ipf_dir=""
  if [ -n "\$_ipf_pipe" ]; then
    _ipf_listing="\$(printf '%s\n' '$pw' | sudo -S -p '' bash -c 'for c in "\$1"/crtc-*; do echo "\$c \$(cat "\$c/i915_pipe" 2>/dev/null)"; done' bash "\$_ipf_root")"
    while IFS=' ' read -r _ipf_cdir _ipf_cpipe; do
      if [ "\$_ipf_cpipe" = "\$_ipf_pipe" ]; then
        _ipf_dir="\$(basename "\$_ipf_cdir")"
        break
      fi
    done <<< "\$_ipf_listing"
  fi
  echo "IMAG_PRESENTED_CRTC_DIR=\$_ipf_dir"
  echo "IMAG_PRESENTED_CARD_NUM=\$_ipf_num"
fi
REMOTE
}

# imag_presented_frame_sample_cmds SUDO_PW CARD_NUM CRTC_DIR SAMPLE_S -> a REMOTE bash snippet,
# entirely SYNCHRONOUS (blocks the caller for ~SAMPLE_S seconds, no background job, no PID to
# track -- see the file header for why): saves the CRC source currently selected, enables it
# only if not already active, blocks for SAMPLE_S seconds reading `crc/data` into a temp file,
# restores the CRC source (always safe here -- the reader has already exited by construction),
# parses the sample (total lines = presented_frame_count; consecutive same-crc0 lines =
# repeated_frame_count), cleans up, and prints exactly one line:
#   IMAG_PRESENTED_RESULT presented_frame_count=<N> repeated_frame_count=<M> sample_window_s=<S>
# Always best-effort (every privileged step guarded, never a bare failing command) -- this is a
# REPORT-ONLY diagnostic and must never itself abort or fail the run it's called from.
imag_presented_frame_sample_cmds() {
  local pw="$1" card_num="$2" crtc_dir="$3" sample_s="$4"
  cat <<REMOTE
_ipf_crc="/sys/kernel/debug/dri/${card_num}/${crtc_dir}/crc"
_ipf_log="/tmp/imag-presented-sample-\$\$.log"
_ipf_prev="\$(printf '%s\n' '$pw' | sudo -S -p '' bash -c 'grep -m1 "\\*\$" "\$1/control" 2>/dev/null | sed "s/\\*\$//"' bash "\$_ipf_crc")"
_ipf_prev="\${_ipf_prev:-none}"
if [ "\$_ipf_prev" != "auto" ]; then
  printf '%s\n' '$pw' | sudo -S -p '' bash -c 'echo auto > "\$1/control"' bash "\$_ipf_crc" 2>/dev/null || true
fi
printf '%s\n' '$pw' | sudo -S -p '' bash -c 'timeout "\$2" cat "\$1/data" > "\$3" 2>/dev/null' bash "\$_ipf_crc" "$sample_s" "\$_ipf_log" || true
if [ "\$_ipf_prev" != "auto" ]; then
  printf '%s\n' '$pw' | sudo -S -p '' bash -c 'echo "\$2" > "\$1/control"' bash "\$_ipf_crc" "\$_ipf_prev" 2>/dev/null || true
fi
_ipf_total=0
_ipf_repeat=0
_ipf_prevcrc=""
_ipf_content="\$(printf '%s\n' '$pw' | sudo -S -p '' cat "\$_ipf_log" 2>/dev/null)"
while IFS=' ' read -r _ipf_frame _ipf_crc0 _ipf_rest; do
  [ -n "\$_ipf_frame" ] || continue
  _ipf_total=\$((_ipf_total + 1))
  if [ -n "\$_ipf_prevcrc" ] && [ "\$_ipf_crc0" = "\$_ipf_prevcrc" ]; then
    _ipf_repeat=\$((_ipf_repeat + 1))
  fi
  _ipf_prevcrc="\$_ipf_crc0"
done <<< "\$_ipf_content"
printf '%s\n' '$pw' | sudo -S -p '' rm -f "\$_ipf_log" 2>/dev/null || true
echo "IMAG_PRESENTED_RESULT presented_frame_count=\$_ipf_total repeated_frame_count=\$_ipf_repeat sample_window_s=$sample_s"
REMOTE
}
