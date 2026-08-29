#!/usr/bin/env bash
# airuleset:script-ok source-only lib (defines pure functions only, no top-level statements) --
# matches the sibling scripts/lib/*.sh convention (cadence-health.sh, frozen-input-health.sh,
# network-reach-health.sh) of deliberately NOT setting `set -euo pipefail` here: sourcing this file
# executes it in the CALLER's shell, so strict mode here would leak into whichever caller sources it.
# The caller (scripts/recording-e2e.sh) already runs its own `set -euo pipefail`.
#
# scripts/lib/genlock-settle.sh -- issue 1221: the measured genlock-FIFO SETTLE-WAIT that runs
# AFTER the [4i/8align] per-source latency-pin writes and BEFORE [5/8] StartRecord, so the recording
# measures steady-state instead of the FIFO relock era the align itself induces.
#
# WHY (issue 1221, verdict 950927573, 2026-08-29): each per-source latency-pin write in [4i/8align]
# re-parameterises that source's genlock FIFO -> a relock/drain/regain episode (the
# genlock-fifo-limit-cycle class). StartRecord fired straight after, so the first ~60-90s of the
# recording measured a rocking chain: per-window derived_uniform_fraction 0.644 -> 0.967 monotone
# convergence, strict-contiguity faults concentrated in win0-win2, tail already >= 0.95. The FIFO
# carries the direct steady-state signal itself -- the `genlock-fifo audit '<src>':` line
# (src/jitter_audit.rs) appends ~every 5.017s with cumulative relocks/underruns/dropped_due/
# late_holds counters -- so this WAITS ON A MEASURED settle signal, it is NOT a blind sleep
# (no-timeout-band-aids: a fixed sleep would over-wait a fast run and under-wait a slow one, and
# would measure nothing so a lengthening relock era could silently return).
#
# TOPOLOGY: a PURE decision core (parse latest counters / per-pass quiet verdict / all-settled
# verdict) split from a THIN ssh runner, so the pure half is Tier-0-testable with zero ssh and zero
# real waiting (the crate-root-pure-seam pattern applied to bash; #557 bans local cargo, so the
# observable local red->green is a bash replica sourcing this lib -- CI runs the same via the
# tests/harness_genlock_settle_1221.rs run_sourced harness).
#
# THE LOAD-BEARING DECISION -- the issue-797 phantom-rate avoidance: the quiet verdict compares
# each counter's raw cumulative value between two consecutive audit snapshots (delta == 0 ?), so it
# NEVER divides by a wall-clock / poll interval. There is no rate here at all, only a value-to-value
# delta, so the single-tick-window phantom-50 trap that bit cadence-health cannot apply.
#
# Source-only: pure functions, no side effects at source time. The runner reuses win_ssh_run
# (scripts/lib/win-ssh-exec.sh, sourced by the caller) for the default OBS-log tail read.

# genlock_settle_latest_counters <log_text> <source> -> stdout: the LAST audit line's four counters
#   for <source> as `<relocks> <underruns> <dropped_due> <late_holds>`, or EMPTY when the source has
#   no `genlock-fifo audit '<source>':` line in the text (absent -> not-yet-measurable, never a
#   guessed 0-line). A recognised counter token missing from an otherwise-matching line defaults to
#   0 (the log always carries all four; jitter_audit.rs parses absent as 0 too). Whitespace
#   key=value token scan mirroring parse_audit_line -- decoration tokens (the '%s': name quotes, the
#   (=N ms) / (re-arm@N) fragments) carry no `=` and are skipped.
genlock_settle_latest_counters() {
  local log_text="${1:-}" source="${2:-}"
  local marker="genlock-fifo audit '${source}':"
  printf '%s\n' "$log_text" | LC_ALL=C awk -v marker="$marker" '
    index($0, marker) > 0 {
      r = ""; u = ""; d = ""; l = "";
      for (i = 1; i <= NF; i++) {
        p = index($i, "=");
        if (p == 0) continue;
        k = substr($i, 1, p - 1);
        v = substr($i, p + 1);
        if (k == "relocks") r = v;
        else if (k == "underruns") u = v;
        else if (k == "dropped_due") d = v;
        else if (k == "late_holds") l = v;
      }
      have = 1;
      lr = (r == "" ? "0" : r); lu = (u == "" ? "0" : u);
      ld = (d == "" ? "0" : d); ll = (l == "" ? "0" : l);
    }
    END { if (have) printf "%s %s %s %s\n", lr, lu, ld, ll }
  '
}

# genlock_settle_pass_verdict <prev4> <curr4> -> stdout: quiet | noisy | reset | unmeasurable.
#   Each arg is a 4-field `<relocks> <underruns> <dropped_due> <late_holds>` string (the output of
#   genlock_settle_latest_counters).
#     unmeasurable -- either side empty / not exactly four non-negative integers (a first
#                     observation, or a source that vanished from the log this pass). Reseed
#                     upstream; never counts toward a quiet streak.
#     reset        -- any current counter is BELOW the previous one (the cumulative counter went
#                     backward => OBS restarted / the input was recreated). Reseed, streak resets.
#     quiet        -- all four deltas are exactly zero across this pass (no relock disturbance in
#                     this ~5s window).
#     noisy        -- at least one of relocks/underruns/dropped_due/late_holds advanced.
genlock_settle_pass_verdict() {
  local prev="${1:-}" curr="${2:-}"
  local pr pu pd pl cr cu cd cl extra_p extra_c
  read -r pr pu pd pl extra_p <<<"$prev"
  read -r cr cu cd cl extra_c <<<"$curr"
  # exactly four fields on each side (a 5th field, or a missing field, is malformed -> unmeasurable)
  if [ -n "$extra_p" ] || [ -n "$extra_c" ]; then printf 'unmeasurable\n'; return 0; fi
  local x
  for x in "$pr" "$pu" "$pd" "$pl" "$cr" "$cu" "$cd" "$cl"; do
    case "$x" in '' | *[!0-9]*) printf 'unmeasurable\n'; return 0 ;; esac
  done
  if [ "$cr" -lt "$pr" ] || [ "$cu" -lt "$pu" ] || [ "$cd" -lt "$pd" ] || [ "$cl" -lt "$pl" ]; then
    printf 'reset\n'; return 0
  fi
  if [ "$cr" -eq "$pr" ] && [ "$cu" -eq "$pu" ] && [ "$cd" -eq "$pd" ] && [ "$cl" -eq "$pl" ]; then
    printf 'quiet\n'; return 0
  fi
  printf 'noisy\n'
}

# genlock_settle_all_settled <n_required> [streak ...] -> stdout: SETTLED | CONTINUE.
#   Each trailing arg is one SEEN input's current consecutive-quiet-pass streak. SETTLED iff there
#   is at least one seen input AND every seen input's streak is >= n_required. A non-integer streak
#   (or none) => CONTINUE (never a premature settle). Inputs that never appeared in the log are not
#   passed here at all, so an off-air align-set member never stalls the settle to budget.
genlock_settle_all_settled() {
  local n="${1:-2}"; shift || true
  local count=0 s
  for s in "$@"; do
    case "$s" in '' | *[!0-9]*) printf 'CONTINUE\n'; return 0 ;; esac
    count=$((count + 1))
    if [ "$s" -lt "$n" ]; then printf 'CONTINUE\n'; return 0; fi
  done
  if [ "$count" -ge 1 ]; then printf 'SETTLED\n'; else printf 'CONTINUE\n'; fi
}

# _genlock_settle_now -> stdout: the current time in seconds (a non-negative integer). Overridable
#   via GENLOCK_SETTLE_NOW_CMD (a shell command whose stdout is the "now" value) so a Tier-0 replica
#   can drive a fake clock and exercise budget exhaustion without any real waiting. ALWAYS exits 0
#   and ALWAYS prints a valid integer (a failed/garbage clock read -> 0) so the caller's
#   `now="$(_genlock_settle_now)"` can never fail-abort the run under `set -e` (#1133 class: the
#   runner is called as a bare statement under recording-e2e.sh's `set -euo pipefail`); the pass
#   ceiling below is the independent backstop that still terminates the loop if the clock is wedged.
_genlock_settle_now() {
  local t
  if [ -n "${GENLOCK_SETTLE_NOW_CMD:-}" ]; then
    # shellcheck disable=SC2294  # test seam: run the caller-provided clock command verbatim
    t="$(eval "${GENLOCK_SETTLE_NOW_CMD}" 2>/dev/null)" || t=""
  else
    t="$(date +%s 2>/dev/null)" || t=""
  fi
  case "$t" in '' | *[!0-9]*) t=0 ;; esac
  printf '%s\n' "$t"
}

# _genlock_settle_read_snapshot <user> <pw> <host> -> stdout: the newest strih OBS log tail (the
#   text genlock_settle_latest_counters parses). Overridable via GENLOCK_SETTLE_READER_CMD (a shell
#   command whose stdout is one snapshot) so a Tier-0 replica can feed a scripted snapshot sequence
#   with zero ssh. Default: one flat ssh + a single (non-nested) PowerShell Get-Content tail of the
#   newest OBS log, via win_ssh_run (-EncodedCommand handles the quoting) -- the SAME read the
#   [4g/8] calibration block does (recording-e2e.sh:~3630, which calls win_ssh_run BARE).
#   CRITICAL: `timeout` execvp()s its command directly and CANNOT invoke a shell FUNCTION like
#   win_ssh_run (`timeout: failed to run command 'win_ssh_run'`, rc 127) -- so to bound the read we
#   must re-source win-ssh-exec.sh inside a `timeout bash -c '...'` (bash IS a real binary), exactly
#   the sibling pattern at recording-e2e.sh:755 / :2853. Sourcing genlock-settle.sh alone does NOT
#   put win_ssh_run in the timeout'd subshell, hence the explicit re-source of its own sibling lib
#   (both live in scripts/lib/). Best-effort: any read failure yields an empty snapshot (that pass
#   simply measures nothing -> the budget still bounds the wait, fail-open).
_genlock_settle_read_snapshot() {
  local user="${1:-}" pw="${2:-}" host="${3:-}"
  if [ -n "${GENLOCK_SETTLE_READER_CMD:-}" ]; then
    # shellcheck disable=SC2294  # test seam: run the caller-provided reader command verbatim
    eval "${GENLOCK_SETTLE_READER_CMD}" 2>/dev/null || true
    return 0
  fi
  local tail_n="${GENLOCK_SETTLE_OBS_LOG_TAIL:-400}"
  local ps
  ps='Get-Content (Get-ChildItem "$env:APPDATA\obs-studio\logs\*.txt" | Sort-Object LastWriteTime -Descending | Select-Object -First 1) -Tail '"$tail_n"
  local libdir
  libdir="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)" || libdir=""
  # timeout bash -c re-sources win-ssh-exec.sh (a sibling in scripts/lib/) so win_ssh_run is a real
  # command inside the bounded subshell -- `timeout win_ssh_run` directly would fail rc 127 (below).
  timeout "${GENLOCK_SETTLE_SSH_TIMEOUT:-20}" bash -c '. "$1"; win_ssh_run "$2" "$3" "$4" "$5"' _ \
    "$libdir/win-ssh-exec.sh" "$user" "$pw" "$host" "$ps" 2>/dev/null || true
}

# genlock_settle_wait <user> <pw> <host> <watched_csv> [n_required] [budget_s] [poll_s]
#   The runner. Polls the strih OBS-log genlock-fifo audit counters for the comma-separated
#   <watched_csv> inputs (e.g. "NDI cam1,NDI cam3"), and returns once every input SEEN in the log
#   has reached n_required (default 2) consecutive quiet passes, OR budget_s (default 180) elapses.
#   ALWAYS returns 0 (report-only, fail-open): a settle prints an `[settle] genlock FIFO quiet ...`
#   line; a budget exhaustion prints a loud `WARNING: [settle] ... fail-open ...` line and lets the
#   run proceed (downstream gates judge the recording). It NEVER aborts the run, and NEVER waits
#   unbounded. poll_s (default 7) >= the ~5.017s audit cadence so each poll sees a fresh tick.
genlock_settle_wait() {
  local user="${1:-}" pw="${2:-}" host="${3:-}" watched_csv="${4:-}"
  local n="${5:-2}" budget="${6:-180}" poll="${7:-7}"

  # SANITIZE every numeric input to a valid non-negative integer (#1133 class: n/budget/poll flow
  # into `printf '%d'` and `[ -lt ]`/`[ -ge ]` -- a malformed env value like
  # E2E_GENLOCK_SETTLE_QUIET_PASSES=xyz would make printf fail (rc 1) and, since the runner is called
  # as a BARE statement under the caller's `set -euo pipefail`, ABORT the whole E2E run; a garbage
  # budget/poll likewise. The same guard _genlock_settle_now already applies to the clock -- applied
  # here consistently so EVERY termination-bound input is always a real integer).
  case "$n" in '' | *[!0-9]*) n=2 ;; esac
  case "$budget" in '' | *[!0-9]*) budget=180 ;; esac
  case "$poll" in '' | *[!0-9]*) poll=7 ;; esac

  # Glob-safe comma split: `read -ra` never globs (an unquoted `for raw in $csv` would be
  # glob-subject on a `*`-bearing input name).
  local -a raw_watched=() watched=()
  IFS=',' read -ra raw_watched <<<"$watched_csv" || true
  local raw
  for raw in "${raw_watched[@]}"; do
    raw="${raw#"${raw%%[![:space:]]*}"}"  # ltrim
    raw="${raw%"${raw##*[![:space:]]}"}"  # rtrim
    [ -n "$raw" ] && watched+=("$raw")
  done

  if [ "${#watched[@]}" -eq 0 ]; then
    printf '[settle] genlock settle-wait: no aligned inputs to watch -- skipping\n'
    return 0
  fi

  local -a prev=() streak=() seen=()
  local i
  for i in "${!watched[@]}"; do prev[i]=""; streak[i]=0; seen[i]=0; done

  # THREE INDEPENDENT termination bounds so the loop can never hang AND a wedged clock stays close
  # to the budget: (1) the real wall budget (primary); (2) a pass*poll ESTIMATED elapsed (so a
  # wedged clock -- date +%s stuck, elapsed always 0 -- is still bounded at ~budget instead of
  # ceiling*poll); (3) a hard pass ceiling (the ultimate backstop, immune to any clock or poll=0).
  local max_passes="${GENLOCK_SETTLE_MAX_PASSES:-1000}"
  case "$max_passes" in '' | *[!0-9]*) max_passes=1000 ;; esac
  local start; start="$(_genlock_settle_now)"
  local pass=0
  while :; do
    local snapshot
    snapshot="$(_genlock_settle_read_snapshot "$user" "$pw" "$host")"
    pass=$((pass + 1))
    local -a seen_streaks=()
    for i in "${!watched[@]}"; do
      local src="${watched[i]}"
      local curr4
      curr4="$(genlock_settle_latest_counters "$snapshot" "$src")"
      if [ -n "$curr4" ]; then
        seen[i]=1
        if [ -n "${prev[i]}" ]; then
          local v
          v="$(genlock_settle_pass_verdict "${prev[i]}" "$curr4")"
          if [ "$v" = "quiet" ]; then streak[i]=$((streak[i] + 1)); else streak[i]=0; fi
        fi
        prev[i]="$curr4"
      elif [ "${seen[i]}" = "1" ]; then
        # A previously-seen source that produced NO audit line this pass (input removed, or the tail
        # scrolled past its last tick) is no longer continuously quiet -- reset its streak so a stale
        # streak can never count as settled (#1221 review 🔵-1). It stays in seen_streaks (its stale
        # streak reset to 0) so the run waits for it to reappear and re-settle, or hits the budget.
        streak[i]=0
      fi
      if [ "${seen[i]}" = "1" ]; then seen_streaks+=("${streak[i]}"); fi
    done

    local verdict
    verdict="$(genlock_settle_all_settled "$n" "${seen_streaks[@]}")"
    if [ "$verdict" = "SETTLED" ]; then
      printf '[settle] genlock FIFO quiet: %d aligned input(s) reached %d consecutive quiet pass(es) after %d poll(s) -- steady-state, proceeding to record\n' \
        "${#seen_streaks[@]}" "$n" "$pass"
      return 0
    fi

    local now elapsed est
    now="$(_genlock_settle_now)"
    elapsed=$((now - start))
    est=$((pass * poll))   # estimated elapsed -- bounds a wedged clock at ~budget, not ceiling*poll
    if [ "$elapsed" -ge "$budget" ] || [ "$est" -ge "$budget" ] || [ "$pass" -ge "$max_passes" ]; then
      printf 'WARNING: [settle] genlock FIFO did NOT reach %d quiet pass(es) for every aligned input within %ds budget after %d poll(s) -- proceeding anyway (fail-open, report-only; downstream gates judge the recording)\n' \
        "$n" "$budget" "$pass"
      return 0
    fi
    "${GENLOCK_SETTLE_SLEEP_CMD:-sleep}" "$poll"
  done
}
