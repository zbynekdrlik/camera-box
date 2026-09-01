#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (optical-chain-preflight.sh, mv-fps-health.sh) of
# deliberately NOT setting `set -euo pipefail` here: sourcing this file executes it in the CALLER's
# shell, so strict mode here would leak into whichever caller sources it. recording-e2e.sh (the only
# caller) already sets -euo pipefail itself -- and every gate call below is written -e-safe so a
# non-zero gate exit (BELOW/UNKNOWN) can never trip the caller's `set -e`.
#
# scripts/lib/mv-fps-preflight.sh -- issue 1091 (issue 771 point 3): read the LATEST `multiview-audit:`
# from each OBS box (strih + imag) BEFORE the E2E run and fail loud when a Multiview projector's
# render cadence is below its floor -- so the gate never wastes a ~40-min run on a box whose Multiview
# has already collapsed. The #675 sourced-lib pattern: recording-e2e.sh gains ONE source line + ONE
# call line, no anchored static-string line edited.
#
# WHY (issue 771 / issue 1083): vendored libobs render_display() emits
# `multiview-audit: monitor=N divisor=D rendered_fps=X target=Z floor=F cx=.. cy=..` ~every 5 s per
# throttleable Multiview projector; issue 1083 shipped the LIVE always-on dev1 watchdog over it, but
# the E2E gate never read it -- a box whose Multiview render already collapsed (measured live: imag
# monitor-3 ~12fps for 5 min, strih 4K MV 9-11fps under contention) still ran. This is the SYNCHRONOUS
# gate-time consumer of the SAME `mv-fps-gate` binary + `mv_audit::gate_log` the watchdog uses; it
# reuses `mv_fps_verdict` (exit -> PASS/BELOW/UNKNOWN) from mv-fps-health.sh. The floor
# (imag 28 / strih 28 = target - tolerance; both boxes now render 30fps MV cells, #776) is EMITTED in each line's `floor=F` and applied by the
# gate binary -- this preflight calibrates nothing.
#
# NEVER FALSE-ABORTS A CI GATE (the user's hardest constraint): only a CONFIRMED below-floor collapse
# (a grace re-read that STAYS below floor) aborts. UNKNOWN (unreadable log / no audit line / a box not
# yet on the issue-771 genlock build / a missing gate binary) is a report-only NOTE -- it must NEVER
# block the whole fleet, exactly the mv-fps-health/watchdog fail-safe (the live issue-1083 watchdog
# owns a sustained collapse either way).
#
# Reading an OBS LOG FILE over ssh is a session-agnostic FILE read, allowed for the headless dev1 E2E
# gate (win-ssh-vs-mcp Context B) -- never a GUI atom over ssh.
#
# Source-only: pure functions, no side effects at source time.

_MVFPS_PREFLIGHT_LIB_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/mv-fps-health.sh
. "$_MVFPS_PREFLIGHT_LIB_DIR/mv-fps-health.sh"
# shellcheck source=scripts/lib/ps-encoded.sh
. "$_MVFPS_PREFLIGHT_LIB_DIR/ps-encoded.sh"

# mv_fps_preflight_read_cmd <os> <log_tail> -> stdout: a REMOTE command string that prints the newest
#   OBS log's tail (the caller greps `multiview-audit:` out of it). linux: a bash one-liner tailing the
#   newest ~/.config/obs-studio/logs/*.txt; win: a single NON-NESTED `powershell -Command` tailing the
#   newest %APPDATA%\obs-studio\logs\*.txt. Mirrors mv-fps-alert-watchdog.sh's probe_mv_log read shape
#   (without its MVFPS_LOGID identity line -- the synchronous preflight tracks no autostart reset).
#   Unknown os -> return 1 (the caller then treats the box as unreadable / UNKNOWN).
mv_fps_preflight_read_cmd() {
  local os="$1" tail_n="${2:-2000}"
  case "$os" in
    linux)
      printf '%s' 'F=$(ls -t ~/.config/obs-studio/logs/*.txt 2>/dev/null | head -1); [ -n "$F" ] && tail -n '"$tail_n"' "$F"'
      ;;
    win)
      # #1259: -EncodedCommand (base64 UTF-16LE), NEVER the naive -Command "$f=(…| sort …); if(…){…}".
      # Win32-OpenSSH's default cmd.exe shell leaks the unescaped `|`/`;`/`{}` -> a mangled/blind read
      # (the issue-1258 root cause). ps_encoded_command (ps-encoded.sh) encodes the whole program to a
      # pure-ASCII blob cmd.exe cannot touch; an empty encode -> empty read -> the caller treats the box
      # as UNKNOWN (report-only), never an abort. Every `$` powershell must see is `\$`-escaped so dev1
      # bash keeps it literal; $tail_n is numeric-clamped so it can never inject shell/PS metachars into
      # the encoded payload (the #1258 guard).
      local _tn="$tail_n"
      case "$_tn" in '' | *[!0-9]*) _tn=2000 ;; esac
      local _enc
      _enc="$(ps_encoded_command "\$f=(gci \$env:APPDATA\\obs-studio\\logs\\*.txt | sort LastWriteTime | select -last 1); if(\$f){ gc \$f.FullName -Tail $_tn }")"
      printf '%s' "powershell -NoProfile -NonInteractive -EncodedCommand $_enc"
      ;;
    *)
      return 1
      ;;
  esac
}

# mv_fps_preflight_probe <ip> <os> <user> <pw> <log_tail> -> stdout: the box's `multiview-audit:` lines
#   (empty on read failure / no audit line -> the caller treats it as UNKNOWN, never a page). The whole
#   read is overridable via MV_FPS_PREFLIGHT_PROBE_CMD (invoked as `$cmd <ip> <os>`) so tests drive the
#   decision with no ssh. All greps end `|| true` so a no-match never trips the caller's `set -e`.
mv_fps_preflight_probe() {
  local ip="$1" os="$2" user="$3" pw="$4" tail_n="$5" raw rcmd
  if [ -n "${MV_FPS_PREFLIGHT_PROBE_CMD:-}" ]; then
    # shellcheck disable=SC2086
    raw="$($MV_FPS_PREFLIGHT_PROBE_CMD "$ip" "$os" 2>/dev/null || true)"
  else
    rcmd="$(mv_fps_preflight_read_cmd "$os" "$tail_n")" || return 0
    raw="$(timeout "${MV_FPS_PREFLIGHT_SSH_TIMEOUT:-20}" sshpass -p "$pw" \
      ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${user}@${ip}" "$rcmd" 2>/dev/null || true)"
  fi
  printf '%s\n' "$raw" | tr -d '\r' | grep -F 'multiview-audit:' 2>/dev/null || true
}

# mv_fps_preflight_assert <gate_bin> <box>...   (box = "name|ip|os|user|pw")
#   For each box: probe the newest OBS log's multiview-audit lines, run <gate_bin> over them, map exit
#   via mv_fps_verdict. PASS -> ok. UNKNOWN -> report-only NOTE (never abort). BELOW -> a grace re-read
#   (one MV_FPS_PREFLIGHT_REPROBE_SLEEP wait) -> if STILL BELOW, record a CONFIRMED collapse. After all
#   boxes, if any confirmed collapse -> print a loud ERROR naming each box+monitor and `exit 1`.
#   Call it as a PLAIN statement (never in a pipeline/$()) so its `exit 1` propagates to the harness.
mv_fps_preflight_assert() {
  local gate_bin="$1"; shift
  local tail_n="${MV_FPS_PREFLIGHT_LOG_TAIL:-2000}"
  local reprobe_sleep="${MV_FPS_PREFLIGHT_REPROBE_SLEEP:-6}"
  local spec name ip os user pw lines out verdict gate_ec detail collapsed=""

  for spec in "$@"; do
    IFS='|' read -r name ip os user pw <<<"$spec" || true
    [ -n "$name" ] && [ -n "$ip" ] && [ -n "$os" ] || continue
    user="${user:-newlevel}"; pw="${pw:-newlevel}"

    lines="$(mv_fps_preflight_probe "$ip" "$os" "$user" "$pw" "$tail_n")"
    if [ -z "$lines" ]; then
      echo "    NOTE: [4d1/8] MV-fps preflight — $name: no multiview-audit line read (box down / a pre-issue-771 OBS build / ssh read failed) — nothing to decide, proceeding (the live issue-1083 watchdog owns a sustained collapse)" >&2
      continue
    fi

    gate_ec=0
    out="$(printf '%s\n' "$lines" | "$gate_bin" 2>/dev/null)" || gate_ec=$?
    verdict="$(mv_fps_verdict "$gate_ec")"
    case "$verdict" in
      PASS)
        echo "    ok: [4d1/8] MV-fps preflight — $name Multiview render cadence at/above floor" ;;
      UNKNOWN)
        echo "    NOTE: [4d1/8] MV-fps preflight — $name: mv-fps-gate could not classify the audit lines (a missing/broken gate binary at '$gate_bin'?) — nothing to decide, proceeding" >&2 ;;
      BELOW)
        # Grace re-read before aborting: never false-abort a CI gate on ONE transient below-floor line
        # (a momentary GPU/CPU contention spike). A sustained collapse stays below floor across a fresh
        # ~5 s audit period; a transient recovers. Mirrors optical_chain_preflight_assert's grace
        # re-probe + the watchdog's 2-pass confirm, adapted to a synchronous one-shot preflight.
        echo "    [4d1/8] MV-fps preflight — $name below floor on first read; grace re-read after ${reprobe_sleep}s before deciding (never false-abort a CI gate)" >&2
        case "$reprobe_sleep" in ''|*[!0-9]*) reprobe_sleep=0 ;; esac
        [ "$reprobe_sleep" -gt 0 ] && sleep "$reprobe_sleep"
        lines="$(mv_fps_preflight_probe "$ip" "$os" "$user" "$pw" "$tail_n")"
        if [ -z "$lines" ]; then
          echo "    NOTE: [4d1/8] MV-fps preflight — $name: below on first read but grace re-read unreadable — nothing to decide, proceeding" >&2
          continue
        fi
        gate_ec=0
        out="$(printf '%s\n' "$lines" | "$gate_bin" 2>/dev/null)" || gate_ec=$?
        verdict="$(mv_fps_verdict "$gate_ec")"
        if [ "$verdict" = "BELOW" ]; then
          # Reuse the health lib's FAIL-line formatter (mv_fps_alert_detail) rather than re-deriving
          # the extraction here (structural reuse); `|| detail=…` keeps it `-e`-safe even if the gate
          # ever exited 1 without a FAIL line (a contract violation the real gate never commits).
          detail="$(mv_fps_alert_detail "$name" "$out")" || detail="$name MV render collapsed below floor"
          collapsed="${collapsed}${detail}
"
        else
          echo "    ok: [4d1/8] MV-fps preflight — $name recovered on grace re-read (transient), proceeding" >&2
        fi
        ;;
    esac
  done

  if [ -n "$collapsed" ]; then
    echo "ERROR: [4d1/8] MV-fps preflight — a Multiview projector's render cadence is CONFIRMED below its floor (target − tolerance) on:" >&2
    printf '%s' "$collapsed" | sed 's/^/         /' >&2
    echo "       A recording made now would capture a juddering Multiview; refusing to start the E2E run (issue 771/1091)." >&2
    echo "       Restart OBS on the named box, or find the process stealing its GPU/CPU render budget, then re-run. NEVER reboot the host." >&2
    exit 1
  fi
  return 0
}
