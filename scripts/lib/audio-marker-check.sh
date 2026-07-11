#!/usr/bin/env bash
# scripts/lib/audio-marker-check.sh — SINGLE SOURCE OF TRUTH for the #420 QPSK audio-marker
# AUDIBLE self-check: given the `hw:CARD=<id>,DEV=<n>` ALSA device the marker targets, derive its
# /proc/asound status path and emit the REMOTE bash that polls for `state: RUNNING`, failing loud
# (exit 1, running an optional teardown command first) if the marker never comes up. A started
# emitter is not a PROVEN emitter (#420 root cause: frame-probe was up, painting video, with
# silently no audio marker at all — no flags were even passed) — this is the REAL kernel-reported
# signal that catches exactly that failure class, never a stub that always passes.
#
# #431 HARDENING: `state: RUNNING` alone is NOT proof of audible marker emission. The QPSK emitter
# (src/probe/qpsk_emit.rs) is a CONTINUOUS-FEED design — it ALWAYS writes to the ALSA ring, silence
# between markers, marker samples when one is due, precisely so the PCM never underruns. So
# `state: RUNNING` is satisfied by the silence carrier ALONE, even if the painter's refresh tick
# stalls and ZERO discrete markers ever fire. `audio_marker_emission_check_cmds` (below) closes
# this gap: it polls the marker-log CSV that the emitter now appends to PER EMITTED MARKER (not
# just on shutdown — same #431) and fails loud if the row count never grows.
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

# audio_marker_check_cmds AUDIO_DEV [ON_FAIL_CMD] [EXTRA_CONTEXT] [MARKER_LOG_PATH] -> the REMOTE
# bash snippet (embed via `$(audio_marker_check_cmds ...)` inside a heredoc or a double-quoted ssh
# command string) that polls the ALSA PCM status for up to ~10s and, on a silent marker: prints a
# #420-style FAIL LOUD diagnostic to stderr, runs ON_FAIL_CMD (default: a no-op — the caller has
# nothing extra to tear down) as cleanup for the just-started, now-proven-unmeasured painter, then
# `exit 1` so the enclosing remote command/session returns non-zero (the caller's `set -e` /
# non-guarded `ssh` call then aborts the whole run — fail loud, never a silent unmeasured record).
# On success it prints a PASS line and falls through. EXTRA_CONTEXT (e.g. "cadence=180 ticks,
# log=/run/...") is appended to both the PASS and FAIL lines for comprehensive logging — never
# required for the check itself. Loop var `\$i` is $-escaped so it evaluates on the REMOTE shell,
# not when this heredoc is constructed locally (mirrors the fb0-wait loops in rig-mode.sh /
# recording-e2e.sh, which use the identical `\$i` convention for the same reason).
#
# #431: when MARKER_LOG_PATH is given (non-empty), the RUNNING check above is NOT sufficient on
# its own (see the #431 header note) — this APPENDS `audio_marker_emission_check_cmds`'s output,
# so a caller that passes its marker-log path gets the real emission assert for free, in the same
# combined snippet. Omitting MARKER_LOG_PATH (existing 3-arg callers) skips it entirely — byte-
# identical output to before #431, so no caller is silently changed without opting in.
audio_marker_check_cmds() {
  local audio_dev="$1"
  local on_fail_cmd="${2:-true}"
  local extra="${3:-}"
  local marker_log="${4:-}"
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
  if [ -n "$marker_log" ]; then
    audio_marker_emission_check_cmds "$marker_log" "$on_fail_cmd" "$extra"
  fi
}

# audio_marker_emission_check_cmds MARKER_LOG_PATH [ON_FAIL_CMD] [EXTRA_CONTEXT] -> the REMOTE bash
# snippet (#431) that asserts actual QPSK marker EMISSION, not merely that the ALSA PCM reports
# `state: RUNNING` (that check alone is satisfied by the continuous-feed silence carrier even when
# zero discrete markers ever fire — see the file header). The emitter (src/probe/qpsk_emit.rs)
# appends one CSV data row per emitted marker to MARKER_LOG_PATH AS IT FIRES, so this polls that
# file's data-row count across POLL_ATTEMPTS reads POLL_INTERVAL_SECS apart — both overridable via
# the REMOTE environment (AUDIO_MARKER_EMIT_POLL_INTERVAL_SECS / AUDIO_MARKER_EMIT_POLL_ATTEMPTS;
# default 3s x 3 attempts ≈ the default 180-tick/60fps=3s painter cadence, so ~2-3 cadences of
# margin before declaring a stall) — and FAILS LOUD, running ON_FAIL_CMD first, the moment the
# whole window passes with the count flat. A single row-count read is not enough (the emitter may
# simply not have reached its first cadence tick yet); requiring GROWTH across the window is what
# proves the feed is not just alive but actually producing markers.
#
# #667 FIX: `grep -c PATTERN FILE` returns a NON-ZERO exit status whenever the match count is zero
# (or FILE doesn't exist yet) — even though it correctly prints "0"/nothing to stdout. The caller
# (painter_launch_remote in rig-mode.sh) wraps its WHOLE heredoc in `set -e`, so a bare
# `c0=$(grep -c ...)` assignment SILENTLY ABORTED THE REMOTE SCRIPT the instant it sampled the
# marker log before the very first QPSK marker had fired (~3s cadence race right after painter
# launch) — with ZERO output, before this check ever printed its own PASS/FAIL. Both `grep -c`
# calls below now use the `|| true` guard already established elsewhere in this codebase for the
# same footgun (scripts/verify-device.sh, scripts/genlock-manifest.sh, scripts/drift-guard.sh).
audio_marker_emission_check_cmds() {
  local marker_log="$1"
  local on_fail_cmd="${2:-true}"
  local extra="${3:-}"
  cat <<CHECK
c0=\$(grep -c '^[0-9]' "$marker_log" 2>/dev/null || true); c0=\${c0:-0}
grown=0
interval="\${AUDIO_MARKER_EMIT_POLL_INTERVAL_SECS:-3}"
attempts="\${AUDIO_MARKER_EMIT_POLL_ATTEMPTS:-3}"
i=0; while [ \$i -lt "\$attempts" ]; do
  sleep "\$interval"
  c1=\$(grep -c '^[0-9]' "$marker_log" 2>/dev/null || true); c1=\${c1:-0}
  if [ "\$c1" -gt "\$c0" ]; then grown=1; break; fi
  c0=\$c1
  i=\$((i+1))
done
if [ "\$grown" -eq 0 ]; then
  echo "FAIL: #431 QPSK audio marker log ($marker_log) has NOT GROWN across \$attempts poll(s) \${interval}s apart — the ALSA feed can stay RUNNING on the silence carrier ALONE (continuous-feed design) while ZERO discrete markers actually fire (stalled painter tick or broken frame_id/refresh wiring). RUNNING-but-silent is NOT audible.${extra:+ [$extra]}" >&2
  echo "      marker log row count stuck at \$c0 ($marker_log)" >&2
  $on_fail_cmd
  exit 1
fi
echo "PASS: #431 QPSK marker log growing on $marker_log (rows -> \$c1${extra:+, $extra})"
CHECK
}
