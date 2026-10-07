#!/usr/bin/env bash
# airuleset:script-ok source-only lib (rig-mode.sh sources it; only constants at source time) -- the
# sibling scripts/lib/*.sh convention of NOT setting `set -euo pipefail` here: sourcing runs in the
# CALLER's shell. Every function checks its own return codes and always returns 0 (report-only).
#
# scripts/lib/program-audio-mode.sh -- issue 1404 (ROZHODNUTÉ 6039368611): the stream program-audio
# sampler on strih-lx (the YouTube channel guard of the CI/test streams) runs in TEST mode only.
#   rig-mode.sh test  -> leave the TEST marker, clear a failed state, `systemctl --user start` the unit
#   rig-mode.sh event -> remove the TEST marker, `systemctl --user stop` the unit, clear a failed state
# (`reset-failed`: a crash loop that hit StartLimitBurst refuses the next start for up to 300 s, and
# `stop` leaves a failed unit failed -- verify-strih item 41 would FAIL it in EVENT mode).
# The unit's ExecCondition skips every start without the marker
# (~/.config/camera-box/program-audio-sampler.test-mode, STRIH_PROGRAM_AUDIO_TEST_MARKER): the sampler is
# down by default and stays down across a reboot during a production; a reboot in TEST mode brings it
# back. Both run as the operator over plain ssh (the strih-lx transport: sshpass, `timeout`
# INSIDE it, UserKnownHostsFile=/dev/null). Report-only: a failure is a WARNING line naming the state
# and never changes rig-mode's exit status (a stopped sampler fails closed for its consumers: UNKNOWN).
# A Windows strih has no sampler: one SKIP line.
#
# Public API:
#   program_audio_mode_remote_cmd test|event  -> the remote shell text (pure, unit-tested)
#   program_audio_mode_apply MODE HOST        -> run it on HOST (STRIH_LX_USER / STRIH_LX_PW, default
#                                                newlevel / newlevel, the rig's shared Linux-box creds)

_PROGRAM_AUDIO_MODE_DIR="${BASH_SOURCE[0]%/*}"
if ! declare -F strih_program_audio_prefix >/dev/null; then
  # shellcheck source=scripts/lib/strih-program-audio.sh
  . "$_PROGRAM_AUDIO_MODE_DIR/strih-program-audio.sh"
fi
if ! declare -F strih_platform >/dev/null; then
  # shellcheck source=scripts/lib/strih-platform.sh
  . "$_PROGRAM_AUDIO_MODE_DIR/strih-platform.sh"
fi

PROGRAM_AUDIO_MODE_SSH_OPTS=(-o UserKnownHostsFile=/dev/null -o StrictHostKeyChecking=no -o LogLevel=ERROR -o ConnectTimeout=12)

# program_audio_mode_remote_cmd test|event -> the shell text the operator's login shell runs on strih-lx.
# It prints `program-audio-sampler: <is-active>` and exits 0 only when the sampler reached the mode's
# state (test: still active 2 s after the start -- a Type=simple unit reads active the moment it is
# forked, so a sampler that dies on import would pass an immediate read; event: `inactive` after the
# stop + reset-failed -- an EMPTY answer, the operator's user manager unreachable, is no proof of a stop).
program_audio_mode_remote_cmd() {
  local mode="${1-}" u="$STRIH_PROGRAM_AUDIO_UNIT" m="$STRIH_PROGRAM_AUDIO_TEST_MARKER"
  case "$mode" in
    test)
      printf 'mkdir -p "$HOME/%s" && : > "$HOME/%s"; systemctl --user reset-failed %s 2>/dev/null; systemctl --user start %s; sleep 2; s="$(systemctl --user is-active %s)"; echo "program-audio-sampler: $s"; [ "$s" = active ]' \
        "${m%/*}" "$m" "$u" "$u" "$u"
      ;;
    event)
      printf 'rm -f "$HOME/%s"; systemctl --user stop %s; systemctl --user reset-failed %s 2>/dev/null; s="$(systemctl --user is-active %s)"; echo "program-audio-sampler: $s"; [ "$s" = inactive ]' \
        "$m" "$u" "$u" "$u"
      ;;
    *)
      echo "program_audio_mode_remote_cmd: mode must be test or event, got '${mode}'" >&2
      return 1
      ;;
  esac
}

# program_audio_mode_apply MODE HOST -> start (test) / stop (event) the sampler on HOST; one result
# line, a WARNING on failure. Always rc 0.
program_audio_mode_apply() {
  local mode="${1-}" host="${2-}" user="${STRIH_LX_USER:-newlevel}" pw="${STRIH_LX_PW:-newlevel}" cmd out rc=0
  if ! cmd="$(program_audio_mode_remote_cmd "$mode")"; then
    echo "WARNING: [program-audio] unknown mode '${mode}' -- the sampler left as it is" >&2
    return 0
  fi
  if [ "$(strih_platform "$host")" != linux ]; then
    echo "  [program-audio ${host}] SKIP: not the Linux strih -- no program-audio sampler there"
    return 0
  fi
  out="$(sshpass -p "$pw" timeout "${PROGRAM_AUDIO_MODE_SSH_TIMEOUT:-30}" ssh "${PROGRAM_AUDIO_MODE_SSH_OPTS[@]}" \
    "${user}@${host}" "$cmd" 2>&1)" || rc=$?
  out="${out//$'\n'/ }"
  if [ "$rc" = 0 ]; then
    echo "  [program-audio ${host}] ${mode}: ${out}"
  elif [ "$mode" = test ]; then
    echo "WARNING: [program-audio ${host}] the program-audio sampler did not start (rc=${rc}: ${out:-no answer}) -- the YouTube guard reads UNKNOWN until it runs; is it provisioned (setup-strih.sh step 16e)? journalctl --user -u ${STRIH_PROGRAM_AUDIO_UNIT} on ${host}" >&2
  else
    echo "WARNING: [program-audio ${host}] the program-audio sampler did not stop (rc=${rc}: ${out:-no answer}) -- stop it by hand: ssh ${user}@${host} 'rm -f ~/${STRIH_PROGRAM_AUDIO_TEST_MARKER}; systemctl --user stop ${STRIH_PROGRAM_AUDIO_UNIT}'" >&2
  fi
  return 0
}
