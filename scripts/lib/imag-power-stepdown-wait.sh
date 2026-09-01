#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements beyond
# sourcing its sibling shared lib) -- matches the sibling scripts/lib/*.sh convention
# (genlock-settle.sh, cadence-health.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so strict mode here would leak into whichever
# caller sources it. The caller (scripts/recording-e2e.sh) already runs its own `set -euo pipefail`.
#
# scripts/lib/imag-power-stepdown-wait.sh -- issue 1268 (branch A, the engineering mitigation): a
# bounded, guard-state-aware PRE-GATE WAIT that runs BEFORE the imag render-budget family (the
# `[4d1/8]` MV-fps floor preflight and the `[4d/8]` render-budget gate) so those STRICT reads never
# land inside a 25 W thermal step-down episode and falsely abort a ~40-min run.
#
# WHY (issue 1268): the #1162 imag-nb holds 45 W only intermittently (TCPU 85-93 C); the
# imag-power-envelope-guard (#1040) steps PL1 down to 25 W ~18x/day (~20% duty, median ~12 min). At
# 25 W the iGPU is pinned to ~400 MHz, so a burns-ON render read is activeFps~57.7 / 15.6 ms -- under
# the 58 fps / 16.67 ms budget -> a falošný abort. The gate thresholds are CORRECT; the defect is
# WHEN imag is read. So this WAITS on the MEASURED clamp signal to clear, it is NOT a threshold
# relaxation and NOT a blind sleep -- the same precondition-wait shape as scripts/lib/genlock-settle.sh
# (issue 1221) and the DanteSync settle. The gate is read exactly as today AFTER the wait.
#
# THE SIGNALS + the load-bearing decision:
#   - `throttle_reason_pl1` (a world-readable i915 sysfs, identity-globbed across card* -- never a
#     hardcoded cardN, the presenter-drm renumbering hazard): 1 == the punit is holding the iGPU
#     below the pinned floor at the MMIO RAPL PL1 power budget (the clamp signature, #880/#1040).
#     This is the PRIMARY signal: the guard's /run state file is root-owned and NOT readable to the
#     non-root E2E ssh (verified live 2026-09-02 on 10.77.9.182), so pl1 is the workhorse.
#   - the guard's `STEPPED=` state (parsed by the SHARED imag_power_guard_stepped_from_state from
#     scripts/lib/imag-power-envelope.sh -- REUSED, never a second driftable copy): a SUPPLEMENT that
#     also catches a guard thermal step-down (and covers the moment pl1 momentarily reads 0 while the
#     guard is still stepped -- "wait for RESTORE"). Inert when the state file is unreadable/absent.
#   The verdict treats pl1=1 OR guard STEPPED as `clamped` (either alone is sufficient clamp
#   evidence); `clear` needs guard not-stepped AND pl1=0 (RESTORE + throttle 0); everything else is
#   `unknown`. An `unknown` (unreadable read) FAILS OPEN on the WAIT -> proceed to the gate
#   immediately (the gate itself still decides; never a false abort, never a hang).
#
# TOPOLOGY: a PURE decision core (remote-read snippet builder / pl1 parse / state verdict) split from
# a THIN ssh runner, so the pure half is Tier-0-testable with zero ssh and zero real waiting -- the
# crate-root-pure-seam pattern applied to bash (#557 bans local cargo, so the observable local
# red->green is a bash replica sourcing this lib; CI runs the same via the
# tests/harness_imag_power_stepdown_wait_1268.rs run_sourced harness).
#
# Source-only: pure functions, no side effects at source time beyond sourcing the sibling shared lib.

# Source the shared imag-power-envelope lib (from this file's OWN dir) to REUSE the guard state-file
# path constant + the pure STEPPED parser -- never a second driftable copy (the imag-power-envelope
# rule's shared-lib discipline). It only DEFINES functions + sets `:=` default vars at source time
# (no I/O), and the functions it in turn calls from timesync-authority.sh are used only inside
# imag_power_envelope_verdict, which this lib never invokes -- so sourcing it alone is safe.
_ipsw_libdir="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)" || _ipsw_libdir=""
if [ -n "$_ipsw_libdir" ] && [ -f "$_ipsw_libdir/imag-power-envelope.sh" ]; then
  # shellcheck source=scripts/lib/imag-power-envelope.sh
  . "$_ipsw_libdir/imag-power-envelope.sh"
fi

# imag_power_stepdown_remote_snippet -> the REMOTE shell command (a string) the runner sends over ssh
# to collect the two clamp signals into the block the pure verdict parses. LIGHT + INSTANTANEOUS
# (one identity-globbed throttle_reason_pl1 sample + a `cat` of the guard state file) -- deliberately
# NOT the shared 6 s throttle burst (imag_power_throttle_burst_remote_snippet), which is far too heavy
# for a signal polled every ~30 s. Emits exactly:
#     IMAGPWR_PL1|<value>        (the pl1 sample; empty if the box has no i915 freq surface)
#     <verbatim guard state file body, or nothing if absent/unreadable>
# The `IMAGPWR_PL1|` marker uses `|` (never `=`), so the shared imag_power_guard_stepped_from_state
# (which keys on `STEPPED=` lines) ignores it -- both parsers read the same block cleanly. The state
# file path comes from the SHARED IMAG_POWER_GUARD_STATE_FILE constant (never a second literal).
imag_power_stepdown_remote_snippet() {
  local statefile="${IMAG_POWER_GUARD_STATE_FILE:-/run/imag-power-envelope-guard.state}"
  cat <<'REMOTE'
_pl1=""
for _f in /sys/class/drm/card*/gt/gt*/throttle_reason_pl1; do
  [ -e "$_f" ] && { _pl1="$(cat "$_f" 2>/dev/null || true)"; break; }
done
printf 'IMAGPWR_PL1|%s\n' "$_pl1"
REMOTE
  printf 'cat %s 2>/dev/null || true\n' "$statefile"
}

# imag_power_stepdown_pl1_from_block <block> -> echoes the throttle_reason_pl1 value (digits only)
# from the FIRST `IMAGPWR_PL1|<v>` line in the block, or empty (absent line / empty value / a
# non-numeric value -> empty, never a guessed number). Pure; text in, no I/O; ALWAYS returns 0 (a
# set-e caller invokes it inside `$(...)`).
imag_power_stepdown_pl1_from_block() {
  local block="${1:-}" line val="" seen=0
  while IFS= read -r line; do
    case "$line" in
      "IMAGPWR_PL1|"*) [ "$seen" -eq 0 ] && { val="${line#IMAGPWR_PL1|}"; seen=1; } ;;
    esac
  done <<< "$block"
  # keep only digits (defensive against surrounding whitespace / a stray CR); non-numeric -> empty.
  printf '%s\n' "$(printf '%s' "$val" | tr -cd '0-9')"
}

# imag_power_stepdown_state <guard_stepped> <pl1> -> echoes exactly one of `clamped | clear | unknown`.
#   guard_stepped: one of stepped | not-stepped | unknown (the shared imag_power_guard_stepped_from_state output)
#   pl1:           any string; normalised to digits (empty/non-numeric -> "").
#   - clamped: guard STEPPED, OR pl1 == 1 (either alone is sufficient clamp evidence -- this also
#     catches the #880 silent punit clamp that produces no guard step-down, and keeps waiting while
#     the guard is stepped even if pl1 momentarily reads 0).
#   - clear:   NOT guard-stepped AND pl1 == 0. pl1=0 is a REAL no-clamp reading (the punit is not
#     holding the iGPU below floor), so it reads as clear even when the guard's /run state file is
#     unreadable (guard "unknown") -- the live production case, since that root-owned file is not
#     readable to the non-root E2E ssh. It is downgraded to `clamped` ONLY if the guard is CONFIRMED
#     stepped (a readable state file mid-episode -> keep waiting for RESTORE).
#   - unknown: pl1 itself is unreadable (empty/non-numeric) and the guard is not confirmed stepped --
#     i.e. we genuinely could not read the clamp signal -> the caller FAILS OPEN and proceeds to the
#     gate (never a false abort).
# Pure; ALWAYS returns 0.
imag_power_stepdown_state() {
  local g="${1:-}" p="${2:-}"
  p="$(printf '%s' "$p" | tr -cd '0-9')"
  if [ "$g" = "stepped" ] || [ "$p" = "1" ]; then printf 'clamped\n'; return 0; fi
  if [ "$p" = "0" ]; then printf 'clear\n'; return 0; fi
  printf 'unknown\n'
}

# imag_power_stepdown_verdict_from_block <block> -> echoes clamped | clear | unknown for one remote
# snapshot block, fusing the pl1 sample with the shared guard STEPPED parser. Pure; ALWAYS returns 0.
imag_power_stepdown_verdict_from_block() {
  local block="${1:-}" pl1 g
  pl1="$(imag_power_stepdown_pl1_from_block "$block")"
  # The shared parser (imag-power-envelope.sh). Fall back to `unknown` if that lib was not sourced
  # (defensive -- the source at top of THIS file should always have provided it).
  if type imag_power_guard_stepped_from_state >/dev/null 2>&1; then
    g="$(imag_power_guard_stepped_from_state "$block")"
  else
    g="unknown"
  fi
  imag_power_stepdown_state "$g" "$pl1"
}

# imag_power_stepdown_write_report <report_file> <waited_s> <state> -> best-effort report-only
# sidecar. Writes `imag_power_stepdown_wait_s=<n>` + `imag_power_stepdown_guard_state_at_gate=<state>`
# so the #711 report / CI log can surface how long the run waited on the clamp (report-only, never
# gates). Empty report_file / any write failure -> silent no-op. ALWAYS returns 0.
imag_power_stepdown_write_report() {
  local report_file="${1:-}" waited_s="${2:-0}" state="${3:-unknown}"
  [ -n "$report_file" ] || return 0
  {
    printf 'imag_power_stepdown_wait_s=%s\n' "$waited_s"
    printf 'imag_power_stepdown_guard_state_at_gate=%s\n' "$state"
  } > "$report_file" 2>/dev/null || true
  return 0
}

# _imag_power_stepdown_now -> the current time in seconds (a non-negative integer). Overridable via
# IMAG_POWER_STEPDOWN_NOW_CMD (a shell command whose stdout is the "now" value) so a Tier-0 replica
# can drive a fake clock and exercise budget exhaustion without real waiting. ALWAYS exits 0 and
# ALWAYS prints a valid integer (a failed/garbage clock read -> 0) so the caller's
# `now="$(_imag_power_stepdown_now)"` can never fail-abort under set -e; the pass ceiling is the
# independent backstop that still terminates the loop if the clock is wedged.
_imag_power_stepdown_now() {
  local t
  if [ -n "${IMAG_POWER_STEPDOWN_NOW_CMD:-}" ]; then
    # shellcheck disable=SC2294  # test seam: run the caller-provided clock command verbatim
    t="$(eval "${IMAG_POWER_STEPDOWN_NOW_CMD}" 2>/dev/null)" || t=""
  else
    t="$(date +%s 2>/dev/null)" || t=""
  fi
  case "$t" in '' | *[!0-9]*) t=0 ;; esac
  printf '%s\n' "$t"
}

# _imag_power_stepdown_read_snapshot <user> <pw> <host> -> stdout: one remote snapshot block (the
# text imag_power_stepdown_verdict_from_block parses). Overridable via IMAG_POWER_STEPDOWN_READER_CMD
# (a shell command whose stdout is one snapshot) so a Tier-0 replica can feed a scripted sequence
# with zero ssh. Default: one bounded sshpass ssh (the SAME imag ssh path recording-e2e.sh already
# uses -- sshpass -p "$pw" ssh newlevel@$IMAG_IP, l.3197). `timeout` execvp()s sshpass directly
# (a real binary, unlike win_ssh_run), so no re-source dance is needed. Best-effort: any read failure
# yields an empty snapshot (that pass measures nothing -> the verdict reads `unknown` -> fail-open).
_imag_power_stepdown_read_snapshot() {
  local user="${1:-}" pw="${2:-}" host="${3:-}"
  if [ -n "${IMAG_POWER_STEPDOWN_READER_CMD:-}" ]; then
    # shellcheck disable=SC2294  # test seam: run the caller-provided reader command verbatim
    eval "${IMAG_POWER_STEPDOWN_READER_CMD}" 2>/dev/null || true
    return 0
  fi
  local snippet
  snippet="$(imag_power_stepdown_remote_snippet)"
  timeout "${IMAG_POWER_STEPDOWN_SSH_TIMEOUT:-15}" sshpass -p "$pw" ssh \
    -o StrictHostKeyChecking=no -o ConnectTimeout="${IMAG_POWER_STEPDOWN_SSH_CONNECT_TIMEOUT:-8}" \
    "$user@$host" "$snippet" 2>/dev/null || true
}

# imag_power_stepdown_wait <user> <pw> <host> [budget_s] [poll_s] [report_file]
#   The runner. Reads the imag clamp signals over ssh; if a 25 W step-down episode is IN PROGRESS
#   (verdict `clamped`), WAITS (poll every poll_s, default 30) up to budget_s (default 1200 = 20 min
#   ~ 1.7x median episode) for the clamp to clear (RESTORE + throttle_reason_pl1=0), logging each poll
#   loudly, then RETURNS 0 (the caller reads the gate exactly as today). RETURN CODES:
#     0  -> proceed to the gate. The no-episode case (verdict clear/unknown at first read -> waited 0)
#           AND the clamp-cleared-within-budget case AND every unreadable/fail-open case.
#     1  -> ABORT: a CONFIRMED clamp held for the WHOLE budget (never a silent pass). The caller wraps
#           the call in `if ! ...; then <clamp-specific abort>; exit 1; fi`; the runner also prints an
#           explicit ERROR line naming the clamp duration to stderr.
#   Fail-open on the WAIT: an unreadable read (`unknown`) proceeds immediately -- it NEVER aborts on a
#   mere ssh hiccup (the gate itself still decides) and NEVER hangs. TWO independent termination bounds
#   (wall budget + pass ceiling) so a wedged clock can never hang the loop.
imag_power_stepdown_wait() {
  local user="${1:-}" pw="${2:-}" host="${3:-}"
  local budget="${4:-1200}" poll="${5:-30}" report_file="${6:-}"

  # SANITIZE every numeric input to a valid non-negative integer (#1133 class: budget/poll flow into
  # `[ -ge ]`/`[ -lt ]` and the report file `printf` -- a malformed env override must never abort the
  # run nor make the loop unbounded).
  case "$budget" in '' | *[!0-9]*) budget=1200 ;; esac
  case "$poll" in '' | *[!0-9]*) poll=30 ;; esac
  local max_passes="${IMAG_POWER_STEPDOWN_MAX_PASSES:-2000}"
  case "$max_passes" in '' | *[!0-9]*) max_passes=2000 ;; esac

  local start block verdict
  start="$(_imag_power_stepdown_now)"
  block="$(_imag_power_stepdown_read_snapshot "$user" "$pw" "$host")"
  verdict="$(imag_power_stepdown_verdict_from_block "$block")"

  if [ "$verdict" != "clamped" ]; then
    # No episode in progress (clear), or the read was unreadable (unknown -> fail-open): proceed now.
    printf '[4d0/8] imag power step-down: no 25W clamp episode in progress (state=%s) — proceeding to the imag render gates (waited 0s)\n' "$verdict"
    imag_power_stepdown_write_report "$report_file" 0 "$verdict"
    return 0
  fi

  printf '[4d0/8] imag IS in a 25W thermal step-down episode (state=clamped: throttle_reason_pl1=1 and/or guard STEPPED) — waiting up to %ds for RESTORE + throttle_reason_pl1=0 (poll %ds), then reading the render gates as today (issue 1268)\n' "$budget" "$poll"

  local pass=0 now elapsed est
  while :; do
    now="$(_imag_power_stepdown_now)"
    elapsed=$((now - start))
    est=$((pass * poll))
    if [ "$elapsed" -ge "$budget" ] || [ "$est" -ge "$budget" ] || [ "$pass" -ge "$max_passes" ]; then
      # Still clamped at the budget -> ABORT (never a silent pass). Name the clamp duration.
      printf 'ERROR: [4d0/8] imag STILL in the 25W thermal step-down clamp after ~%ds (budget %ds, %d poll(s)) — aborting BEFORE the imag render-budget gate (issue 1268).\n' "$elapsed" "$budget" "$pass" >&2
      printf '       At 25W the iGPU is pinned ~400MHz (activeFps~57.7 / 15.6ms, misses the 60fps budget); a gate read now is a FALSE render-regression abort. This is the clamp, not a code regression.\n' >&2
      printf '       The physical fix (cooling) is issue 1268 branch B (owner decision); if this recurs at gate time, the box is under sustained thermal pressure.\n' >&2
      imag_power_stepdown_write_report "$report_file" "$elapsed" "clamped-timeout"
      return 1
    fi
    "${IMAG_POWER_STEPDOWN_SLEEP_CMD:-sleep}" "$poll"
    pass=$((pass + 1))
    block="$(_imag_power_stepdown_read_snapshot "$user" "$pw" "$host")"
    verdict="$(imag_power_stepdown_verdict_from_block "$block")"
    now="$(_imag_power_stepdown_now)"
    elapsed=$((now - start))
    printf '[4d0/8] imag power-clamp poll %d: state=%s (waited ~%ds / %ds budget)\n' "$pass" "$verdict" "$elapsed" "$budget"
    if [ "$verdict" != "clamped" ]; then
      # RESTORE reached (clear), or the read went unreadable (unknown -> fail-open): proceed now.
      printf '[4d0/8] imag 25W clamp no longer detected (state=%s) after ~%ds — proceeding to the imag render gates as today (issue 1268)\n' "$verdict" "$elapsed"
      imag_power_stepdown_write_report "$report_file" "$elapsed" "$verdict"
      return 0
    fi
  done
}
