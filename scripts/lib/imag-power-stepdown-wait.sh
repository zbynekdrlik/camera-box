#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements beyond
# sourcing its sibling shared lib) -- matches the sibling scripts/lib/*.sh convention
# (genlock-settle.sh, cadence-health.sh) of deliberately NOT setting `set -euo pipefail` here:
# sourcing this file executes it in the CALLER's shell, so strict mode here would leak into whichever
# caller sources it. The caller (scripts/recording-e2e.sh) already runs its own `set -euo pipefail`.
#
# scripts/lib/imag-power-stepdown-wait.sh -- issue 1268 (branch A, the engineering mitigation): a
# bounded pre-gate WAIT that runs BEFORE the imag render-budget family (the [4d1/8] MV-fps floor
# preflight and the [4d/8] render-budget gate) so those STRICT reads never land inside a 25 W thermal
# step-down episode and falsely abort a ~40-min run.
#
# WHY (issue 1268): the #1162 imag-nb holds 45 W only intermittently (TCPU 85-93 C); the
# imag-power-envelope-guard (#1040) steps PL1 down to 25 W ~18x/day (~20% duty, median ~12 min). At
# 25 W the iGPU is pinned to ~400 MHz, so a burns-ON render read is activeFps~57.7 / 15.6 ms -- under
# the 58 fps floor / 16.67 ms budget -> a falošný abort. The gate thresholds are CORRECT; the defect
# is WHEN imag is read. So this WAITS on the MEASURED clamp signal to clear, it is NOT a threshold
# relaxation and NOT a blind sleep -- the same precondition-wait shape as scripts/lib/genlock-settle.sh
# (issue 1221) and the DanteSync settle. The gate is read exactly as today AFTER the wait.
#
# THE SIGNAL + the load-bearing decision (revised per the #1268 review, live-verified 2026-09-02):
#   - PRIMARY = the MMIO RAPL PL1 `package-0` `long_term` power_limit_uw -- the guard's OWN actuator.
#     A 25 W thermal step-down writes 25000000; a RESTORE writes 45000000 (the pinned IMAG_PL1_W).
#     It is DETERMINISTIC (the guard sets it, not a flapping instantaneous bit), world-readable to
#     the non-root E2E ssh (mode 644, verified live -- unlike the root-owned /run guard state file),
#     and it is parsed identity-selected by the SHARED imag_power_zone_select from
#     scripts/lib/imag-power-envelope.sh (REUSED, never a second driftable copy; never a hardcoded
#     intel-rapl-mmio index). `clamped` = long_term != the pinned full envelope (a step-down or a
#     foreign clamp); `clear` = long_term == pinned. This is EXACTLY "wait for the guard RESTORE",
#     and it does NOT conflate the #880 chronic silent punit under-floor clamp (which happens AT the
#     full 45 W envelope, throttle_reason_pl1=1, with NO step-down and NO RESTORE to wait for) --
#     that reads `clear` here and proceeds, so we never wait 20 min for a clamp that never restores.
#   - SUPPLEMENT = the guard's `STEPPED=` state (parsed by the SHARED imag_power_guard_stepped_from_state,
#     REUSED). It ORs into `clamped` and is the fallback when the RAPL read is unreadable. The
#     deployed guard predates the #1188 chmod so its /run state file is root-600 (verified live), so
#     this supplement is currently mostly inert in production -- the RAPL primary is what actually
#     drives the decision. Redeploying the guard (setup-imag.sh) restores the supplement.
#   - CONTEXT ONLY (logged, never in the decision) = throttle_reason_pl1 (identity-globbed across
#     card*), so the operator log shows whether the iGPU is ALSO being punit-throttled.
#   An unreadable RAPL read AND an unknown guard = `unknown` -> the caller FAILS OPEN on the WAIT
#   (proceed to the gate immediately; the gate itself still decides; never a false abort, never a hang).
#
# TOPOLOGY: a PURE decision core (remote-read snippet builder / pl1 parse / state verdict) split from
# a THIN ssh runner, so the pure half is Tier-0-testable with zero ssh and zero real waiting -- the
# crate-root-pure-seam pattern applied to bash (#557 bans local cargo, so the observable local
# red->green is a bash replica sourcing this lib; CI runs the same via the
# tests/harness_imag_power_stepdown_wait_1268.rs run_sourced harness).
#
# Source-only: pure functions, no side effects at source time beyond sourcing the sibling shared lib.

# Source the shared imag-power-envelope lib (from this file's OWN dir) to REUSE the RAPL zone
# identity-selector + the pinned-envelope helpers + the guard state-file path constant + the pure
# STEPPED parser -- never a second driftable copy (the imag-power-envelope rule's shared-lib
# discipline). It only DEFINES functions + sets `:=` default vars at source time (no I/O), and the
# functions it in turn calls from timesync-authority.sh are used only inside imag_power_envelope_verdict,
# which this lib never invokes -- so sourcing it alone is safe.
_ipsw_libdir="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)" || _ipsw_libdir=""
if [ -n "$_ipsw_libdir" ] && [ -f "$_ipsw_libdir/imag-power-envelope.sh" ]; then
  # shellcheck source=scripts/lib/imag-power-envelope.sh
  . "$_ipsw_libdir/imag-power-envelope.sh"
fi

# imag_power_stepdown_remote_snippet -> the REMOTE shell command (a string) the runner sends over ssh
# to collect the clamp signals into the block the pure verdict parses. It REUSES the shared
# imag_power_envelope_gather_remote_snippet verbatim (so the RAPL `CONSTRAINT|package-0|<idx>|long_term|<uw>`
# lines imag_power_zone_select parses are produced by ONE source of truth, never a second driftable
# copy), then APPENDS two supplements: the instantaneous throttle_reason_pl1 (identity-globbed across
# card* -- never a hardcoded cardN; CONTEXT-only, logged not decided) and a `cat` of the guard state
# file (for the STEPPED= supplement). The shared gather is an INSTANTANEOUS snapshot (not the heavy
# 6 s throttle burst), so it is cheap to poll. The `IMAGPWR_PL1|` marker uses `|` (never `=`) so
# imag_power_guard_stepped_from_state (which keys on `STEPPED=`) ignores it, and it is not a
# `CONSTRAINT|` line so imag_power_zone_select ignores it too -- the three parsers read one block
# cleanly. The state file path comes from the SHARED IMAG_POWER_GUARD_STATE_FILE constant.
imag_power_stepdown_remote_snippet() {
  local statefile="${IMAG_POWER_GUARD_STATE_FILE:-/run/imag-power-envelope-guard.state}"
  # The shared instantaneous gather (emits ZONE/CONSTRAINT/ENABLED/SLPC/THERMALD/UNIT/TCPU/ACTFREQ).
  imag_power_envelope_gather_remote_snippet
  # Supplement 1: throttle_reason_pl1 (context only), identity-globbed across card*.
  cat <<'REMOTE'
_pl1=""
for _f in /sys/class/drm/card*/gt/gt*/throttle_reason_pl1; do
  [ -e "$_f" ] && { _pl1="$(cat "$_f" 2>/dev/null || true)"; break; }
done
printf 'IMAGPWR_PL1|%s\n' "$_pl1"
REMOTE
  # Supplement 2: the guard state file body (for the STEPPED= supplement; root-600 on a pre-#1188 guard).
  printf 'cat %s 2>/dev/null || true\n' "$statefile"
}

# imag_power_stepdown_pl1_from_block <block> -> echoes the throttle_reason_pl1 value (digits only)
# from the FIRST `IMAGPWR_PL1|<v>` line in the block, or empty. CONTEXT-only (the operator poll log);
# NOT part of the clamp decision. Pure; text in, no I/O; ALWAYS returns 0.
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

# imag_power_stepdown_state <guard_stepped> <long_term_uw> <pinned_uw> -> echoes clamped | clear | unknown.
#   guard_stepped: one of stepped | not-stepped | unknown (the shared imag_power_guard_stepped_from_state output)
#   long_term_uw:  the MMIO RAPL package-0 long_term power_limit_uw (digits), or empty when unreadable
#   pinned_uw:     the pinned full-envelope in micro-watts (imag_pl1_watts_to_uw IMAG_PL1_W, e.g. 45000000)
#   - clamped: guard CONFIRMED stepped, OR the RAPL long_term is readable AND != the pinned full
#     envelope (a 25 W thermal step-down, or any foreign below-envelope clamp -> wait for RESTORE).
#   - clear:   NOT that -- the RAPL long_term reads the pinned full envelope (the guard has restored),
#     OR (RAPL unreadable but the guard's own state CONFIRMS not-stepped).
#   - unknown: neither signal is readable (RAPL unreadable AND guard unknown) -> fail-open (proceed).
# Pure; ALWAYS returns 0.
imag_power_stepdown_state() {
  local g="${1:-}" lt="${2:-}" pinned="${3:-}"
  lt="$(printf '%s' "$lt" | tr -cd '0-9')"
  pinned="$(printf '%s' "$pinned" | tr -cd '0-9')"
  # Guard CONFIRMED stepped is sufficient clamp evidence on its own.
  if [ "$g" = "stepped" ]; then printf 'clamped\n'; return 0; fi
  # PRIMARY: the deterministic RAPL long_term value (when readable, and a valid pinned to compare to).
  if [ -n "$lt" ] && [ -n "$pinned" ]; then
    if [ "$lt" = "$pinned" ]; then printf 'clear\n'; else printf 'clamped\n'; fi
    return 0
  fi
  # FALLBACK when the RAPL read is unreadable: trust the guard state if it CONFIRMS not-stepped.
  if [ "$g" = "not-stepped" ]; then printf 'clear\n'; return 0; fi
  printf 'unknown\n'
}

# imag_power_stepdown_verdict_from_block <block> -> echoes clamped | clear | unknown for one remote
# snapshot block, fusing the RAPL long_term (PRIMARY) with the shared guard STEPPED parser (SUPPLEMENT).
# Pure; ALWAYS returns 0.
imag_power_stepdown_verdict_from_block() {
  local block="${1:-}" lt="" g="unknown" pinned=""
  # PRIMARY: the MMIO RAPL package-0 long_term uW, identity-selected by the SHARED parser.
  if type imag_power_zone_select >/dev/null 2>&1; then
    lt="$(imag_power_zone_select "$block" 2>/dev/null || true)"
  fi
  # SUPPLEMENT: the guard STEPPED state (shared parser). Fall back to `unknown` if the shared lib was
  # not sourced (defensive -- the source at top of THIS file should always have provided it).
  if type imag_power_guard_stepped_from_state >/dev/null 2>&1; then
    g="$(imag_power_guard_stepped_from_state "$block")"
  fi
  # The pinned full envelope in uW (IMAG_PL1_W default 45 from the shared lib), for the compare.
  if type imag_pl1_watts_to_uw >/dev/null 2>&1; then
    pinned="$(imag_pl1_watts_to_uw "${IMAG_PL1_W:-45}" 2>/dev/null || true)"
  fi
  imag_power_stepdown_state "$g" "$lt" "$pinned"
}

# imag_power_stepdown_write_report <report_file> <waited_s> <state> -> best-effort report-only
# sidecar. Writes `imag_power_stepdown_wait_s=<n>` + `imag_power_stepdown_guard_state_at_gate=<state>`
# so the CI log / a future #711 report can surface how long the run waited on the clamp (report-only,
# never gates). Empty report_file / any write failure -> silent no-op. ALWAYS returns 0.
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
# uses -- sshpass -p "$pw" ssh newlevel@$IMAG_IP). `timeout` execvp()s sshpass directly (a real
# binary, unlike win_ssh_run), so no re-source dance is needed. Best-effort: any read failure yields
# an empty snapshot (that pass measures nothing -> the verdict reads `unknown` -> fail-open).
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
#   (verdict `clamped`), WAITS (poll every poll_s, default 30) up to budget_s (default 720 = 12 min,
#   the issue-1268 12-min median, env-overridable) for the clamp to clear (RAPL long_term back to the
#   pinned full envelope), logging each poll loudly, then RETURNS 0 (the caller reads the gate exactly
#   as today). Default budget capped at 12 min (NOT the issue's 1.7x/20-min figure) because both imag
#   render gates read BEFORE the ~40-min recording under a 75-min job timeout, and this runner is
#   called TWICE (before [4d1/8] and before [4d/8]); 2x720 + recording + settle stays clear of the
#   runner kill, while an abort is EARLY and cheap. RETURN CODES:
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
  local budget="${4:-720}" poll="${5:-30}" report_file="${6:-}"

  # SANITIZE every numeric input to a valid non-negative integer (#1133 class: budget/poll flow into
  # `[ -ge ]`/`[ -lt ]` and the report file `printf` -- a malformed env override must never abort the
  # run nor make the loop unbounded).
  case "$budget" in '' | *[!0-9]*) budget=720 ;; esac
  case "$poll" in '' | *[!0-9]*) poll=30 ;; esac
  local max_passes="${IMAG_POWER_STEPDOWN_MAX_PASSES:-2000}"
  case "$max_passes" in '' | *[!0-9]*) max_passes=2000 ;; esac

  local start block verdict pl1
  start="$(_imag_power_stepdown_now)"
  block="$(_imag_power_stepdown_read_snapshot "$user" "$pw" "$host")"
  verdict="$(imag_power_stepdown_verdict_from_block "$block")"

  if [ "$verdict" = "clear" ]; then
    printf '[4d0/8] imag power step-down: no 25W clamp episode in progress (state=clear, PL1 at the full envelope) — proceeding to the imag render gates (waited 0s)\n'
    imag_power_stepdown_write_report "$report_file" 0 clear
    return 0
  fi
  if [ "$verdict" != "clamped" ]; then
    # unknown: the RAPL + guard signals were both unreadable (ssh/sysfs) -> fail-open, proceed.
    printf '[4d0/8] imag power step-down: clamp signal UNREADABLE (ssh/sysfs, state=unknown) — fail-open, proceeding to the imag render gates; the gate itself decides (waited 0s)\n'
    imag_power_stepdown_write_report "$report_file" 0 unknown
    return 0
  fi

  printf '[4d0/8] imag IS in a 25W thermal step-down episode (state=clamped: RAPL PL1 below the full envelope and/or guard STEPPED) — waiting up to %ds for the guard RESTORE (PL1 back to the full envelope), then reading the render gates as today (issue 1268)\n' "$budget"

  local pass=0 now elapsed est
  while :; do
    now="$(_imag_power_stepdown_now)"
    elapsed=$((now - start))
    est=$((pass * poll))
    if [ "$elapsed" -ge "$budget" ] || [ "$est" -ge "$budget" ] || [ "$pass" -ge "$max_passes" ]; then
      # Still clamped at the budget -> ABORT (never a silent pass). Name the clamp duration (use the
      # larger of the real elapsed and the pass*poll estimate, so a wedged clock still names a
      # non-zero duration rather than "~0s").
      local named="$elapsed"; [ "$est" -gt "$named" ] && named="$est"
      printf 'ERROR: [4d0/8] imag STILL in the 25W thermal step-down clamp after ~%ds (budget %ds, %d poll(s)) — aborting BEFORE the imag render-budget gate (issue 1268).\n' "$named" "$budget" "$pass" >&2
      printf '       At 25W the iGPU is pinned ~400MHz (activeFps~57.7 / 15.6ms, misses the 58fps floor / 16.67ms budget); a gate read now is a FALSE render-regression abort. This is the clamp, not a code regression.\n' >&2
      printf '       The physical fix (cooling) is issue 1268 branch B (owner decision); if this recurs at gate time, the box is under sustained thermal pressure.\n' >&2
      imag_power_stepdown_write_report "$report_file" "$named" "clamped-timeout"
      return 1
    fi
    "${IMAG_POWER_STEPDOWN_SLEEP_CMD:-sleep}" "$poll"
    pass=$((pass + 1))
    block="$(_imag_power_stepdown_read_snapshot "$user" "$pw" "$host")"
    verdict="$(imag_power_stepdown_verdict_from_block "$block")"
    pl1="$(imag_power_stepdown_pl1_from_block "$block")"
    now="$(_imag_power_stepdown_now)"
    elapsed=$((now - start))
    printf '[4d0/8] imag power-clamp poll %d: state=%s (throttle_reason_pl1=%s) (waited ~%ds / %ds budget)\n' "$pass" "$verdict" "${pl1:-?}" "$elapsed" "$budget"
    if [ "$verdict" != "clamped" ]; then
      # RESTORE reached (clear), or the read went unreadable (unknown -> fail-open): proceed now.
      printf '[4d0/8] imag 25W clamp no longer detected (state=%s) after ~%ds — proceeding to the imag render gates as today (issue 1268)\n' "$verdict" "$elapsed"
      imag_power_stepdown_write_report "$report_file" "$elapsed" "$verdict"
      return 0
    fi
  done
}
