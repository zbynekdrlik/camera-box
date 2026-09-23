#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines functions only, no top-level statements that act) --
# deliberately NOT `set -euo pipefail`: sourcing runs in the CALLER's shell, and the callers range
# from recording-e2e.sh (`set -euo pipefail`) to the dev1 watchdogs (`set -uo pipefail`, no -e) and
# the Tier-0 harnesses; strict mode here would leak into all of them (the same reason
# ps-encoded.sh / strih-platform.sh carry none). Every public function ALWAYS returns 0.
#
# scripts/lib/strih-log-read.sh -- issue 1360: the ONE platform-resolved reader of the strih OBS
# log, shared by every helper that reads it: qr-align.sh (the floor-aware arrival audit),
# genlock-settle.sh ([4j/8settle]), mv-reverify-escalate.sh (the received= tap, also read by
# frozen-cam-received.sh's [4c/8] gate) and ndi-cadence-heal.sh (the cleanup cadence verify);
# the MV-fps pair (mv-fps-preflight.sh + mv-fps-alert-watchdog.sh) own a two-platform reader keyed
# on an `os` token and take ONLY the platform decision from here (strih_log_os).
#
# WHY (issue 1360): each helper grew its own Windows-only reader (PowerShell Get-Content of the
# newest %APPDATA%\obs-studio\logs file) before the M4 cut-over moved the strih role to the Linux
# notebook strih-lx; on strih-lx every read came back empty and each consumer fell back fail-open
# (READ_FAIL, "ssh flake/timeout", settle never quiet, the align without its measured arrival floor).
#
# PLATFORM: resolved once per read via strih-platform.sh `strih_platform HOST` (the STRIH_PLATFORM
# env override, else the strih-lx address -> linux, else windows).
#   linux   -> plain ssh running tail / wc / tail -n +N on the NEWEST ~/.config/obs-studio/logs/*.txt
#              (newest by mtime via `ls -t`; the filename carries SPACES, e.g.
#              `2026-09-23 09-23-36.txt`, so it is always used as the quoted "$F").
#   windows -> the EXISTING PowerShell command strings, moved here verbatim, sent cmd.exe-proof as
#              `powershell -NoProfile -NonInteractive -EncodedCommand <base64 UTF-16LE>`
#              (ps-encoded.sh; the issue-1258/1259 root cause is a naive -Command whose pipes leak
#              to Win32-OpenSSH's cmd.exe).
#
# TRANSPORT: ONE flat `sshpass -p PW timeout T ssh ...` -- `timeout` sits INSIDE sshpass so a test's
# PATH- or function-stubbed sshpass stays the outermost command (the ci-testing-gotchas stub-bypass
# rule), and the ssh itself is bounded. UserKnownHostsFile=/dev/null: 10.77.9.202 was the Windows
# STRIH-SNV address before M4, so a stale known_hosts key would otherwise make OpenSSH refuse
# password auth to strih-lx. Any failure (unreachable, auth, no log, timeout) -> EMPTY output,
# return 0 -- every consumer already treats empty as "unread", never as a measurement.
#
# Public API (all: <host> <user> <pw> ... [timeout_s], default STRIH_LOG_READ_TIMEOUT or 20):
#   strih_log_os <host>                                -> linux | win (the mv-fps os-token vocabulary)
#   strih_log_line_count <host> <user> <pw> [t]        -> the newest log's line count
#   strih_log_since_line <host> <user> <pw> <start> [t] -> every line AFTER the first <start> lines
#   strih_log_tail <host> <user> <pw> <n> [t]          -> the newest log's last <n> lines
# Pure builder (no network, unit-testable): strih_log_remote_cmd <linux|windows> <count|since|tail> [arg]

_STRIH_LOG_READ_DIR="${BASH_SOURCE[0]%/*}"
command -v strih_platform >/dev/null 2>&1 || . "$_STRIH_LOG_READ_DIR/strih-platform.sh"
command -v ps_encoded_command >/dev/null 2>&1 || . "$_STRIH_LOG_READ_DIR/ps-encoded.sh"

# strih_log_os <host> -> "linux" | "win": strih_platform mapped onto the `os` token the MV-fps
# readers (mv-fps-preflight.sh / mv-fps-alert-watchdog.sh) already switch on.
strih_log_os() {
  case "$(strih_platform "${1:-}")" in
    linux) printf 'linux' ;;
    *) printf 'win' ;;
  esac
}

# strih_log_remote_cmd <platform> <op> [arg] -> stdout: the REMOTE command string for one read, or
# nothing (return 0) for an unknown platform/op or a non-numeric `since` mark. `tail` clamps a
# non-numeric count to 400 (never spliced into a remote shell/PS payload unvalidated).
strih_log_remote_cmd() {
  local platform="${1:-}" op="${2:-}" arg="${3:-}" n ps
  case "$op" in
    tail) n="$(ps_clamp_numeric "$arg" 400)" ;;
    since)
      case "$arg" in '' | *[!0-9]*) return 0 ;; esac
      n="$arg"
      ;;
    count) n="" ;;
    *) return 0 ;;
  esac
  case "$platform" in
    linux)
      local newest='F=$(ls -t ~/.config/obs-studio/logs/*.txt 2>/dev/null | head -1); [ -n "$F" ] && '
      case "$op" in
        tail) printf '%s' "${newest}tail -n ${n} \"\$F\"" ;;
        since) printf '%s' "${newest}tail -n +$((n + 1)) \"\$F\"" ;;
        count) printf '%s' "${newest}wc -l < \"\$F\"" ;;
      esac
      ;;
    windows)
      # Verbatim from the consumers that owned them: count/since = qr-align.sh's Get-ChildItem form;
      # tail = the gc/gci form mv-reverify-escalate.sh + ndi-cadence-heal.sh used.
      local newest='Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1'
      case "$op" in
        tail) ps="gc (gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1).FullName -Tail $n" ;;
        since) ps="Get-Content ($newest) | Select-Object -Skip $n" ;;
        count) ps="(Get-Content ($newest)).Count" ;;
      esac
      local enc
      enc="$(ps_encoded_command "$ps")"
      # An empty encode (iconv/base64 absent) -> no command -> an empty read, never an abort.
      [ -n "$enc" ] || return 0
      printf '%s' "powershell -NoProfile -NonInteractive -EncodedCommand $enc"
      ;;
  esac
  return 0
}

# _strih_log_run <host> <user> <pw> <timeout_s> <remote-cmd> -> the remote command's stdout, or
# nothing. Always returns 0.
_strih_log_run() {
  local host="$1" user="$2" pw="$3" tmo="$4" rcmd="$5"
  [ -n "$host" ] && [ -n "$rcmd" ] || return 0
  case "$tmo" in '' | *[!0-9]*) tmo=20 ;; esac
  sshpass -p "$pw" timeout "$tmo" ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR -o ConnectTimeout=8 "${user}@${host}" "$rcmd" 2>/dev/null || true
  return 0
}

# _strih_log_read <host> <user> <pw> <op> <arg> <timeout_s> -> resolve the platform, build, run.
_strih_log_read() {
  local host="$1" user="$2" pw="$3" op="$4" arg="$5" tmo="$6" rcmd
  rcmd="$(strih_log_remote_cmd "$(strih_platform "$host")" "$op" "$arg")"
  _strih_log_run "$host" "$user" "$pw" "$tmo" "$rcmd"
}

strih_log_line_count() {
  _strih_log_read "${1:-}" "${2:-}" "${3:-}" count "" "${4:-${STRIH_LOG_READ_TIMEOUT:-20}}"
}

strih_log_since_line() {
  _strih_log_read "${1:-}" "${2:-}" "${3:-}" since "${4:-}" "${5:-${STRIH_LOG_READ_TIMEOUT:-20}}"
}

strih_log_tail() {
  _strih_log_read "${1:-}" "${2:-}" "${3:-}" tail "${4:-}" "${5:-${STRIH_LOG_READ_TIMEOUT:-20}}"
}
