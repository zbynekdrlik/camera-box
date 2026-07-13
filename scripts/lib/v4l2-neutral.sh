#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# the sibling scripts/lib/capture-rate-guard.sh / marker-device-resolve.sh convention which are
# also `set -euo pipefail`-free for the same reason (sourcing this file must never mutate the
# CALLING script's own shell options).
#
# scripts/lib/v4l2-neutral.sh — SINGLE SOURCE OF TRUTH for resetting a capture card's picture
# controls to ITS OWN reported neutral (#744).
#
# WHY: scripts/recording-e2e.sh hardcoded `v4l2-ctl --set-ctrl=saturation=50,contrast=50` at three
# sites ([0/8] preflight, [2/8] cam1 deploy, [2b/8] ALL_CAMBOX per-box loop — #338/#312). That
# literal `50` IS the ShadowCast 2's own factory default (its V4L2 range is 0-100) — but on the
# Elgato 4K S cards (0-255 range, default 128) the SAME literal is ~39% of default: a dark,
# chroma-muted picture. Live evidence (#744, 2026-07-13): both Elgato boxes (CAM1 10.77.9.61, CAM6
# 10.77.9.66) sitting at contrast=50/saturation=50 on their 0-255 range — resetting to 128/128
# instantly fixed the dark multiview tiles (mean luma 29/30 -> 101/100). This is EXACTLY the #456
# lesson already fixed in src/capture.rs (`ControlTarget::RangeScaled`: `reference_pct=50` resolves
# to the device's OWN queried `default_value`, never a literal calibrated for a different card,
# see `scale_to_range`) — the bash harness never got the same fix.
#
# APPROACH: read EACH control's own `default=` straight out of `v4l2-ctl --list-ctrls`, apply
# THAT. Never a foreign literal, never hue (#338: hue's neutral is NOT 0 on every card — the
# ShadowCast's hue is `min=0 max=100 default=50`; forcing hue=0 tints the picture pink/magenta —
# the certified COLOUR set has never touched hue and this helper doesn't either). This naturally
# "model-gates" itself the same way #729 model-gates the Rust capture path: a card that exposes NO
# saturation/contrast controls (the NZXT Signal HD60 — "no V4L2 picture controls exposed" per
# targets.md) simply has no matching `default=` line to parse, so nothing is applied — no
# per-model enum needed here, the parse-only-what-exists shape can never force a value onto a
# control the card doesn't have, or a value that isn't the card's own.
#
# Node resolution (#744 item 2): recording-e2e.sh also hardcoded `/dev/video0` at these same sites
# (used both for the v4l2-ctl calls AND an unrelated `fuser` busy-wait). USB grabber nodes RENUMBER
# on re-enumeration (#728: CAM1's Elgato was `/dev/video1` at swap time, `/dev/video0` days later)
# — this helper instead walks `/dev/video*` and picks the one whose OWN
# `/sys/class/video4linux/videoN/index` sysfs attribute is `0` (the kernel's own designation of the
# CAPTURE node on a dual-node UVC card — the sibling node, if any, is a metadata-only node and is
# never what v4l2-ctl's picture controls apply to). Falls back to `/dev/video0` if no indexed node
# is found (degenerate/older driver) — matches the pre-#744 hardcode as the last resort rather than
# silently touching nothing.
#
# Source-only: this file defines pure functions (string builders + a pure parser) and performs no
# side effects on its own (no ssh, no device I/O) — safe to source from recording-e2e.sh and from
# the unit tests (which feed the parser captured --list-ctrls fixtures).

# The pure AWK program mapping a captured `v4l2-ctl --list-ctrls` text to a
# "saturation=<dev-default>,contrast=<dev-default>" v4l2-ctl --set-ctrl argument (either clause
# omitted if that control isn't present in the text; empty output if NEITHER is present). Defined
# ONCE as a variable so the EXACT same program text runs both embedded in the remote command
# (`v4l2_neutral_set_default_cmd`, piped from a LIVE --list-ctrls) and locally against a captured
# fixture (`v4l2_neutral_default_ctrl_arg`, called directly by the unit tests) — they can never
# drift apart. Anchored on the CONTROL NAME as the line's first whitespace-delimited field so it
# can never false-match a related control (e.g. a hypothetical "contrast_automatic" would NOT
# match — after "contrast" comes "_", not whitespace); only ever reads `default=`, never `value=`
# (the live-observed FOREIGN value is exactly what this resets) and never touches `hue` (#338 — the
# program doesn't even look for it).
# shellcheck disable=SC2016  # this is AWK source (its `$i`/`$1` are AWK field refs), not a shell
# variable meant for expansion here.
V4L2_NEUTRAL_AWK_PROGRAM='
/^[[:space:]]*contrast[[:space:]]/   { for (i=1;i<=NF;i++) if ($i ~ /^default=/) { split($i,a,"="); c=a[2] } }
/^[[:space:]]*saturation[[:space:]]/ { for (i=1;i<=NF;i++) if ($i ~ /^default=/) { split($i,a,"="); s=a[2] } }
END {
  out=""
  if (s != "") out = "saturation=" s
  if (c != "") { if (out != "") out = out ","; out = out "contrast=" c }
  if (out != "") print out
}
'

# v4l2_neutral_default_ctrl_arg LIST_CTRLS_TEXT -> "saturation=X,contrast=Y" (either clause
# omitted if the card doesn't expose it; empty if neither) by running V4L2_NEUTRAL_AWK_PROGRAM
# against ALREADY-CAPTURED `v4l2-ctl --list-ctrls` text. Pure function (no I/O) — this is what the
# unit tests call directly against fixture text.
v4l2_neutral_default_ctrl_arg() {
  printf '%s\n' "$1" | awk "$V4L2_NEUTRAL_AWK_PROGRAM"
}

# v4l2_neutral_resolve_node_cmd -> the REMOTE bash TEXT (embed via `$(...)` inside an ssh command
# string) that resolves the capture node into the remote variable `V4L2_NEUTRAL_NODE`: the
# `/dev/videoN` whose OWN `/sys/class/video4linux/videoN/index` is `0` (falls back to
# `/dev/video0` if none is found). Sets the variable but produces no other output/side effect —
# callers needing the fuser-wait/v4l2-ctl calls to target the SAME node just reference
# `$V4L2_NEUTRAL_NODE` in their own remote text right after embedding this.
v4l2_neutral_resolve_node_cmd() {
  printf '%s\n' \
    'V4L2_NEUTRAL_NODE=""' \
    'for _v4l2n in /dev/video*; do' \
    '  [ -e "$_v4l2n" ] || continue' \
    '  _v4l2b="$(basename "$_v4l2n")"' \
    '  if [ "$(cat "/sys/class/video4linux/$_v4l2b/index" 2>/dev/null)" = "0" ]; then' \
    '    V4L2_NEUTRAL_NODE="$_v4l2n"' \
    '    break' \
    '  fi' \
    'done' \
    'V4L2_NEUTRAL_NODE="${V4L2_NEUTRAL_NODE:-/dev/video0}"'
}

# v4l2_neutral_set_default_cmd -> the REMOTE bash TEXT that reads `$V4L2_NEUTRAL_NODE`'s OWN
# `--list-ctrls` defaults, applies them (skips --set-ctrl entirely if the card exposes neither
# control), and reads the result back for the caller's log. ASSUMES `V4L2_NEUTRAL_NODE` is already
# set (by v4l2_neutral_resolve_node_cmd, embedded immediately before this in the same remote
# script/ssh session).
v4l2_neutral_set_default_cmd() {
  printf "_v4l2_ctrlarg=\$(v4l2-ctl -d \"\$V4L2_NEUTRAL_NODE\" --list-ctrls 2>/dev/null | awk '%s')\n" \
    "$V4L2_NEUTRAL_AWK_PROGRAM"
  printf '%s\n' \
    'if [ -n "$_v4l2_ctrlarg" ]; then' \
    '  v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --set-ctrl="$_v4l2_ctrlarg" 2>/dev/null' \
    'fi' \
    'v4l2-ctl -d "$V4L2_NEUTRAL_NODE" --get-ctrl=saturation,contrast 2>/dev/null'
}

# v4l2_neutral_apply_cmds -> the FULL standalone remote command (resolve node -> apply its own
# defaults -> read back), for a call site that owns its whole ssh command (e.g. the [0/8]
# preflight, which previously ran the hardcoded set+get as its entire ssh argument). Just
# v4l2_neutral_resolve_node_cmd followed by v4l2_neutral_set_default_cmd; sites that ALSO need the
# resolved node for something else in between (e.g. [2/8]/[2b/8]'s fuser busy-wait) embed the two
# pieces separately instead of calling this combined form.
v4l2_neutral_apply_cmds() {
  v4l2_neutral_resolve_node_cmd
  v4l2_neutral_set_default_cmd
}
