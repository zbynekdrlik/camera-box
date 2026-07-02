#!/usr/bin/env bash
# scripts/lib/audio-marker-check.sh — SINGLE SOURCE OF TRUTH for the #420 QPSK audio-marker
# AUDIBLE self-check: given the `hw:CARD=<id>,DEV=<n>` ALSA device the marker targets, derive its
# /proc/asound status path and emit the REMOTE bash that polls for `state: RUNNING`, failing loud
# (exit 1, running an optional teardown command first) if the marker never comes up. A started
# emitter is not a PROVEN emitter (#420 root cause: frame-probe was up, painting video, with
# silently no audio marker at all — no flags were even passed) — this is the REAL kernel-reported
# signal that catches exactly that failure class, never a stub that always passes.
#
# Sourced by:
#   - scripts/rig-mode.sh        (#420 — TEST-mode painter launch, the original fix)
#   - scripts/recording-e2e.sh   (#421 — the AV_RESTART_GATE before/after painter launch, same
#                                  risk class: a dropped/mistyped --audio-marker flag or a busy
#                                  ALSA device must abort the gate run, never silently record an
#                                  unmeasured before/after pair)
#
# Source-only: pure string builders, no ssh, no side effects at source time — safe to source from
# any orchestration script and from unit tests. Mirrors scripts/lib/rig-test-dropin.sh's shape
# (#309): a constant/derivation helper + a pure remote-command builder, single-sourced so the two
# call sites can never drift on what "audible" means.

# audio_marker_alsa_status_path AUDIO_DEV -> stdout: "/proc/asound/<card>/pcm<dev>p/sub0/status"
# for a "hw:CARD=<id>,DEV=<n>" ALSA device string. Pure bash parameter expansion (evaluated
# LOCALLY, never on the remote box) so callers can bake a literal path into a remote heredoc/ssh
# command with no remote-side parsing needed.
audio_marker_alsa_status_path() {
  local audio_dev="$1"
  local audio_card="${audio_dev#*CARD=}"; audio_card="${audio_card%%,*}"
  local audio_devnum="${audio_dev#*DEV=}"; audio_devnum="${audio_devnum%%,*}"
  echo "/proc/asound/$audio_card/pcm${audio_devnum}p/sub0/status"
}

# audio_marker_check_cmds AUDIO_DEV [ON_FAIL_CMD] [EXTRA_CONTEXT] -> the REMOTE bash snippet
# (embed via `$(audio_marker_check_cmds ...)` inside a heredoc or a double-quoted ssh command
# string) that polls the ALSA PCM status for up to ~10s and, on a silent marker: prints a
# #420-style FAIL LOUD diagnostic to stderr, runs ON_FAIL_CMD (default: a no-op — the caller has
# nothing extra to tear down) as cleanup for the just-started, now-proven-unmeasured painter, then
# `exit 1` so the enclosing remote command/session returns non-zero (the caller's `set -e` /
# non-guarded `ssh` call then aborts the whole run — fail loud, never a silent unmeasured record).
# On success it prints a PASS line and falls through. EXTRA_CONTEXT (e.g. "cadence=180 ticks,
# log=/run/...") is appended to both the PASS and FAIL lines for comprehensive logging — never
# required for the check itself. Loop var `\$i` is $-escaped so it evaluates on the REMOTE shell,
# not when this heredoc is constructed locally (mirrors the fb0-wait loops in rig-mode.sh /
# recording-e2e.sh, which use the identical `\$i` convention for the same reason).
audio_marker_check_cmds() {
  local audio_dev="$1"
  local on_fail_cmd="${2:-true}"
  local extra="${3:-}"
  local audio_status
  audio_status="$(audio_marker_alsa_status_path "$audio_dev")"
  cat <<CHECK
i=0; while [ \$i -lt 20 ]; do
  if [ -f "$audio_status" ] && grep -q '^state: RUNNING' "$audio_status" 2>/dev/null; then break; fi
  sleep 0.5; i=\$((i+1))
done
if [ ! -f "$audio_status" ] || ! grep -q '^state: RUNNING' "$audio_status" 2>/dev/null; then
  echo "FAIL: #420 QPSK audio marker ($audio_dev -> $audio_status) is NOT RUNNING — no marker audio is being emitted (silent, unmeasured run).${extra:+ [$extra]}" >&2
  echo "      status file: \$(cat "$audio_status" 2>/dev/null || echo '<missing>')" >&2
  $on_fail_cmd
  exit 1
fi
echo "PASS: #420 QPSK audio marker RUNNING on $audio_dev ($audio_status state=RUNNING${extra:+, $extra})"
CHECK
}
