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
# PER-BOX TERM (issue 1263): a CONFIRMED collapse is routed per box by
# mv_fps_preflight_term_is_report_only. The STRIH term is REPORT-ONLY while issue 1260 is open (its
# 4K divisor-1 MV floor pre-dates the 7-camera fleet, so a healthy strih idles below it) -- a loud
# `WARNING (issue 1260)` naming the measured line, never an abort. The IMAG term stays STRICT (a
# confirmed imag collapse still aborts). Walk-back tracked on issue 1263: flip strih back to strict
# when issue 1260 lands.
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
# shellcheck source=scripts/lib/strih-log-read.sh
. "$_MVFPS_PREFLIGHT_LIB_DIR/strih-log-read.sh"

# mv_fps_preflight_read_cmd <os> <log_tail> -> stdout: a REMOTE command string that prints the newest
#   OBS log's tail (the caller greps `multiview-audit:` out of it). linux: a bash one-liner tailing the
#   newest ~/.config/obs-studio/logs/*.txt; win: a single `powershell -EncodedCommand` (cmd.exe-proof,
#   issue 1259) tailing the
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
#   os `strih` (issue 1360) = the strih box, whose platform (linux strih-lx | win STRIH-SNV) is
#   resolved by the shared strih_log_os (scripts/lib/strih-log-read.sh) instead of being hard-coded;
#   the resolved token then takes the unchanged linux/win read_cmd below.
mv_fps_preflight_probe() {
  local ip="$1" os="$2" user="$3" pw="$4" tail_n="$5" raw rcmd
  if [ -n "${MV_FPS_PREFLIGHT_PROBE_CMD:-}" ]; then
    # shellcheck disable=SC2086
    raw="$($MV_FPS_PREFLIGHT_PROBE_CMD "$ip" "$os" 2>/dev/null || true)"
  else
    if [ "$os" = strih ]; then
      os="$(strih_log_os "$ip")"
    fi
    rcmd="$(mv_fps_preflight_read_cmd "$os" "$tail_n")" || return 0
    raw="$(timeout "${MV_FPS_PREFLIGHT_SSH_TIMEOUT:-20}" sshpass -p "$pw" \
      ssh -o StrictHostKeyChecking=no -o ConnectTimeout=8 "${user}@${ip}" "$rcmd" 2>/dev/null || true)"
  fi
  # #1262: byte-safe extraction (mv_fps_extract_audit_lines, mv-fps-health.sh, sourced above) --
  # see its own doc comment for the transport-chunk-glue hazard this guards against.
  printf '%s\n' "$raw" | tr -d '\r' | mv_fps_extract_audit_lines
}

# mv_fps_preflight_term_is_report_only <box_name> -> exit 0 if this box's CONFIRMED-collapse term is
#   REPORT-ONLY (a loud WARN, never an abort); exit 1 if it is STRICT (a confirmed collapse aborts).
#   issue 1260 / issue 1263: the STRIH term is REPORT-ONLY while issue 1260 is open -- the strih 4K
#   divisor-1 MV floor (28, the issue-776 canvas/2-tol retarget) pre-dates the 2026-08-28 seven-camera
#   fleet reactivation, so a healthy-core-loop strih now idles the MV below floor and this term would
#   refuse every run (three aborts the day the gate first actually decided, issue 1261). issue 1263 is
#   the walk-back tracker: flip strih back to STRICT in the PR that closes issue 1260 (perf fixed, or
#   the floor honestly recalibrated for the 7-cam era). Every OTHER box -- imag -- stays STRICT (imag
#   holds its floor reliably; its render-health preflight gates it elsewhere too). Fail-safe: an
#   unlisted box defaults to STRICT (a new box is never silently report-only).
mv_fps_preflight_term_is_report_only() {
  case "${1:-}" in
    strih) return 0 ;;
    *) return 1 ;;
  esac
}

# -----------------------------------------------------------------------------------------------
# issue 1040 -- the fresh-sample SETTLE for the STRICT (imag) term after a BELOW first read.
#
# WHY: mv-fps-gate classifies each projector's WINDOW MEDIAN rendered_fps over its most recent ~12
# samples (median_recent_rendered_fps, src/mv_audit.rs). Right after a [4d0/8] 25W PL1 step-down the
# pre-gate just waited out (issue 1268), that recent window STRADDLES the clamp era -- the median
# reads below floor while the LATEST sample is already at target (run 34986461596: median 23.0 while
# latest rendered_fps 30.0 >= floor 28.0). The one-shot 6 s grace re-read re-reads the SAME tail and
# re-runs the SAME median gate, so it cannot move a median dominated by ~45 s of clamp-era samples --
# a false abort of a ~40-min run on a box that has already recovered. So the strict term SETTLES on
# FRESH samples instead: it polls for NEW multiview-audit lines and decides from the individual fresh
# samples (never the backward-looking median). Never false-aborts a CI gate (the user's hardest
# constraint): < N fresh samples within the bounded budget, or an unreadable re-read, is UNKNOWN ->
# report-only NOTE. The strih report-only term (issue 1260) keeps its own grace path, unchanged.
# Same PURE-core-split-from-a-thin-runner shape as scripts/lib/genlock-settle.sh (issue 1221).
# -----------------------------------------------------------------------------------------------

# mv_fps_preflight_latest_sample -- stdin: multiview-audit lines; stdout: "<id>\t<rendered_fps>\t<floor>"
#   for the NEWEST (last) multiview-audit line, where <id> is the whole line VERBATIM (its own
#   identity -- the OBS-log timestamp prefix advances ~every 5 s, so a new emit is a new identity;
#   issue-797-safe: it is an IDENTITY comparison, never a wall-clock rate). Empty output when there is
#   no multiview-audit line, or the newest one lacks rendered_fps= / floor= (the caller then treats it
#   as "no fresh sample this pass"). Always exits 0 (a no-match never trips a strict-mode caller).
mv_fps_preflight_latest_sample() {
  LC_ALL=C awk '
    index($0, "multiview-audit:") > 0 { line = $0 }
    END {
      if (line == "") exit 0
      fps = ""; floor = ""
      n = split(line, a, /[ \t]+/)
      for (i = 1; i <= n; i++) {
        p = index(a[i], "="); if (p == 0) continue
        k = substr(a[i], 1, p - 1); v = substr(a[i], p + 1)
        if (k == "rendered_fps") fps = v
        else if (k == "floor") floor = v
      }
      if (fps == "" || floor == "") exit 0
      printf "%s\t%s\t%s\n", line, fps, floor
    }' 2>/dev/null || true
}

# mv_fps_preflight_sample_verdict <rendered_fps> <floor> -> stdout: ok | below | bad
#   ok    -- rendered_fps >= floor (a healthy fresh sample). below -- rendered_fps < floor (a fresh
#   below-floor sample -> confirms the collapse). bad -- either value missing / non-numeric (a
#   malformed line; the caller neither counts it toward the recovery streak nor confirms a collapse
#   on it). Float-safe compare via awk (rendered_fps=30.00, floor=28.0). Always exits 0.
mv_fps_preflight_sample_verdict() {
  local fps="${1:-}" floor="${2:-}"
  case "$fps" in '' | *[!0-9.]*) printf 'bad\n'; return 0 ;; esac
  case "$floor" in '' | *[!0-9.]*) printf 'bad\n'; return 0 ;; esac
  if LC_ALL=C awk -v a="$fps" -v b="$floor" 'BEGIN { exit !(a + 0 >= b + 0) }' 2>/dev/null; then
    printf 'ok\n'
  else
    printf 'below\n'
  fi
}

# _mv_fps_preflight_settle_now -> stdout: the current time in seconds (a non-negative integer).
#   Overridable via MV_FPS_PREFLIGHT_SETTLE_NOW_CMD (a shell command whose stdout is "now") so a
#   Tier-0 replica can drive a fake clock and exercise budget exhaustion with no real waiting. ALWAYS
#   exits 0 and ALWAYS prints a valid integer (a failed/garbage read -> 0), so the caller's
#   `now="$(_mv_fps_preflight_settle_now)"` can never fail-abort the settle under `set -e`; the pass
#   ceiling in the runner is the independent backstop for a wedged clock. Mirrors genlock-settle.sh.
_mv_fps_preflight_settle_now() {
  local t
  if [ -n "${MV_FPS_PREFLIGHT_SETTLE_NOW_CMD:-}" ]; then
    # shellcheck disable=SC2294  # test seam: run the caller-provided clock command verbatim
    t="$(eval "${MV_FPS_PREFLIGHT_SETTLE_NOW_CMD}" 2>/dev/null)" || t=""
  else
    t="$(date +%s 2>/dev/null)" || t=""
  fi
  case "$t" in '' | *[!0-9]*) t=0 ;; esac
  printf '%s\n' "$t"
}

# mv_fps_preflight_settle_strict <name> <ip> <os> <user> <pw> <tail_n> <baseline_lines>
#   The STRICT-box fresh-sample settle. <baseline_lines> is the first BELOW read's tail -- its newest
#   multiview-audit line is the BASELINE identity (samples matching it are STALE straddling data, not
#   counted). Polls (sleep seam) for NEW multiview-audit lines and decides:
#     - a fresh sample < floor              -> prints the collapse DETAIL to stdout (the caller adds
#                                              it to $collapsed -> the loud ERROR + exit 1)
#     - N (default 3) consecutive fresh >= floor -> prints an `ok:` line to stderr, no stdout detail
#     - < N fresh within the budget / unreadable -> prints a report-only NOTE to stderr, no detail
#   Prints the collapse DETAIL on stdout ONLY when confirmed (empty otherwise), so the caller
#   integrates it exactly like the report-only grace path. Emits progress/ok/NOTE to stderr. ALWAYS
#   returns 0 (never aborts itself; the caller owns the exit). Three termination bounds (wall budget,
#   est = pass*poll for a wedged clock, hard pass ceiling), mirroring genlock_settle_wait.
mv_fps_preflight_settle_strict() {
  local name="$1" ip="$2" os="$3" user="$4" pw="$5" tail_n="$6" baseline="$7"
  local n="${MV_FPS_PREFLIGHT_SETTLE_N:-3}"
  local below_n="${MV_FPS_PREFLIGHT_SETTLE_BELOW_N:-2}"
  local budget="${MV_FPS_PREFLIGHT_SETTLE_S:-120}"
  local poll="${MV_FPS_PREFLIGHT_SETTLE_POLL:-6}"
  local max_passes="${MV_FPS_PREFLIGHT_SETTLE_MAX_PASSES:-1000}"
  # SANITIZE every termination-bound input to a valid non-negative integer (#1133 class: a malformed
  # env value flowing into `[ -ge ]`/`$(( ))` under the caller's `set -euo pipefail` would abort the
  # whole E2E run; the same guard genlock_settle_wait applies).
  case "$n" in '' | *[!0-9]*) n=3 ;; esac
  # below_n must be >= 1 (0 would confirm a collapse on ZERO fresh below samples -> a spurious abort);
  # clamp a garbage / 0 value to the default 2.
  case "$below_n" in '' | *[!0-9]* | 0) below_n=2 ;; esac
  case "$budget" in '' | *[!0-9]*) budget=120 ;; esac
  case "$poll" in '' | *[!0-9]*) poll=6 ;; esac
  case "$max_passes" in '' | *[!0-9]*) max_passes=1000 ;; esac

  local baseline_samp baseline_id last_id
  baseline_samp="$(printf '%s\n' "$baseline" | mv_fps_preflight_latest_sample)"
  baseline_id="${baseline_samp%%$'\t'*}"
  last_id="$baseline_id"
  local ok_streak=0 below_streak=0 fresh_seen=0 pass=0 start
  start="$(_mv_fps_preflight_settle_now)"

  echo "    [4d1/8] MV-fps preflight — $name below floor on first read; settling on FRESH multiview-audit samples (need ${n} consecutive ≥floor to recover, ${below_n} consecutive <floor to confirm, within ${budget}s) — the window median can straddle a just-recovered clamp (issue 1040), never false-abort a CI gate" >&2

  while :; do
    "${MV_FPS_PREFLIGHT_SETTLE_SLEEP_CMD:-sleep}" "$poll"
    pass=$((pass + 1))
    local lines samp id fps floor v rest
    lines="$(mv_fps_preflight_probe "$ip" "$os" "$user" "$pw" "$tail_n")"
    if [ -n "$lines" ]; then
      samp="$(printf '%s\n' "$lines" | mv_fps_preflight_latest_sample)"
      id="${samp%%$'\t'*}"
      if [ -n "$id" ] && [ "$id" != "$last_id" ]; then
        last_id="$id"
        fresh_seen=$((fresh_seen + 1))
        rest="${samp#*$'\t'}"; fps="${rest%%$'\t'*}"; floor="${rest##*$'\t'}"
        v="$(mv_fps_preflight_sample_verdict "$fps" "$floor")"
        if [ "$v" = "below" ]; then
          # A collapse is CONFIRMED only after below_n CONSECUTIVE fresh <floor samples — symmetric
          # with the n-consecutive-≥floor recovery. A SINGLE fresh <floor emit right after the
          # [4d0/8] clamp clears can be the render still catching up (the individual-sample analogue
          # of the straddling median this fix targets); requiring a streak stops that boundary emit
          # from false-aborting a ~40-min run. ok_streak resets (the ≥floor run is broken).
          ok_streak=0
          below_streak=$((below_streak + 1))
          if [ "$below_streak" -ge "$below_n" ]; then
            echo "    [4d1/8] MV-fps preflight — $name: ${below_streak} consecutive fresh sample(s) below floor (latest rendered_fps=$fps < floor=$floor) after ${pass} poll(s) — collapse CONFIRMED on fresh data (not a straddling median)" >&2
            printf '%s\n' "$name MV render collapsed — ${below_streak} consecutive fresh samples < floor (latest rendered_fps=$fps < floor=$floor; confirmed on fresh data, not a straddling median; issue 1040)"
            return 0
          fi
        elif [ "$v" = "ok" ]; then
          below_streak=0
          ok_streak=$((ok_streak + 1))
          if [ "$ok_streak" -ge "$n" ]; then
            local now elapsed
            now="$(_mv_fps_preflight_settle_now)"; elapsed=$((now - start))
            echo "    ok: [4d1/8] MV-fps preflight — $name recovered on fresh samples (${ok_streak}/${n} ≥ floor) after ${elapsed}s, proceeding (issue 1040)" >&2
            return 0
          fi
        else
          # a malformed fresh line breaks BOTH consecutive streaks (never counts, never confirms)
          ok_streak=0; below_streak=0
        fi
      fi
    fi
    local now elapsed est
    now="$(_mv_fps_preflight_settle_now)"; elapsed=$((now - start)); est=$((pass * poll))
    if [ "$elapsed" -ge "$budget" ] || [ "$est" -ge "$budget" ] || [ "$pass" -ge "$max_passes" ]; then
      echo "    NOTE: [4d1/8] MV-fps preflight — $name: only ${fresh_seen} fresh sample(s), ${ok_streak}/${n} ≥floor within the ${budget}s settle budget after ${pass} poll(s) — inconclusive, proceeding report-only (never false-abort a CI gate; the live issue-1083 watchdog owns a sustained collapse)" >&2
      return 0
    fi
  done
}

# mv_fps_preflight_assert <gate_bin> <box>...   (box = "name|ip|os|user|pw")
#   For each box: probe the newest OBS log's multiview-audit lines, run <gate_bin> over them, map exit
#   via mv_fps_verdict. PASS -> ok. UNKNOWN -> report-only NOTE (never abort). BELOW -> a grace re-read
#   (one MV_FPS_PREFLIGHT_REPROBE_SLEEP wait) -> if STILL BELOW, a CONFIRMED collapse. The confirmed
#   collapse is then routed per box by mv_fps_preflight_term_is_report_only: a REPORT-ONLY box (strih,
#   while issue 1260 is open) prints a loud `WARNING (issue 1260)` and does NOT abort; a STRICT box
#   (imag / any other) is recorded in $collapsed. After all boxes, if any STRICT confirmed collapse ->
#   print a loud ERROR naming each box+monitor and `exit 1`.
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
        if mv_fps_preflight_term_is_report_only "$name"; then
          # REPORT-ONLY box (strih, issue 1260): keep the one-shot grace re-read -> WARNING path,
          # UNCHANGED. issue 1040 changes ONLY the STRICT-box term; the strih 4K-floor term stays a
          # loud WARN (never an abort) while issue 1260 is open (walk-back tracked on issue 1263).
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
            echo "    WARNING (issue 1260): $name MV render below floor -- REPORT-ONLY while issue 1260 is open: $detail" >&2
          else
            echo "    ok: [4d1/8] MV-fps preflight — $name recovered on grace re-read (transient), proceeding" >&2
          fi
        else
          # STRICT box (imag) — issue 1040: SETTLE on FRESH samples instead of a one-shot grace
          # re-read of a window whose median can straddle a [4d0/8]-cleared clamp episode. $lines is
          # the first BELOW read (its newest multiview-audit line is the settle BASELINE). A confirmed
          # fresh below-floor sample -> $collapsed (the loud ERROR + exit 1 below); recovered on N
          # consecutive fresh >=floor, or inconclusive within the budget -> the settle emits its own
          # ok:/NOTE: line and returns no detail (never false-abort a CI gate).
          local settle_detail
          settle_detail="$(mv_fps_preflight_settle_strict "$name" "$ip" "$os" "$user" "$pw" "$tail_n" "$lines")"
          if [ -n "$settle_detail" ]; then
            collapsed="${collapsed}${settle_detail}
"
          fi
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
