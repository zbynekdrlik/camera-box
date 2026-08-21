#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function library (no side effects at source time) — mirrors
# scripts/lib/imag-display-path.sh / scripts/lib/imag-power-envelope.sh; a sourced lib must NOT
# impose `set -euo pipefail` on its caller.
# scripts/lib/imag-cmdline-isolation.sh — shared imag-nb kernel-cmdline ISOLATION drift core (#784).
#
# Root cause (#784, recurred as #842): the večerný kolaps + its replacement-notebook recurrence were
# both caused by kernel CPU-ISOLATION tokens on /proc/cmdline — `isolcpus=`/`nohz_full=`, written by a
# since-deleted `/etc/default/grub.d/98-imag-isolation.cfg`. `isolcpus=` removes the listed CPUs from
# the kernel scheduler's load-balancing DOMAINS (it exists for explicit PER-THREAD pinning, never for
# handing a whole range mask to a many-threaded process); OBS's ~119-thread pool then piled 114
# threads onto ONE core → NDI receive 60→~53 fps, 7–10 underruns/s. setup-imag.sh (#842) is now
# AFFINITY-ONLY (a `taskset` mask via /etc/imag-isolated-cpus.conf — restricting WHICH cores OBS may
# run on WITHOUT removing them from load balancing), and verify-imag.sh checks (d)+(s) guard it at
# PROVISIONING time. This lib is the CONTINUOUS / drift-check counterpart: it lets
# `drift-guard --check-imag` FAIL LOUDLY on the isolation footgun re-appearing AFTER provisioning (a
# hand-edit, a stray grub.d drop-in, a future kernel-config package) — the #784 remaining item.
#
# THE ONE LEGITIMATE TOKEN (live-verified 10.77.9.182, 2026-08-17): a healthy imag cmdline carries
# `rcu_nocbs=all`, written by the issue-482 low-latency-kernel config (`99-lowlatency.cfg`,
# `preempt=full`) — NOT by any isolation drop-in, and NOT scoped to specific cores. So `rcu_nocbs=all`
# is OK and must NEVER be flagged; only a SCOPED `rcu_nocbs=<cpu-list>` is the isolation-family
# footgun and reads DRIFT. (This is why a blanket "ban any rcu_nocbs" would false-fail the whole
# fleet; verify-imag.sh's own (d) check deliberately covers only isolcpus/nohz_full for the same
# reason — this lib ADDS the scoped-rcu_nocbs detection the drift-guard facet was asked for.)
#
# Contract vs `imag_cpu_isolation_plan`: post-#842 the plan's output feeds ONLY the taskset affinity
# mask, NEVER the cmdline, so the cmdline's prescribed kernel-isolation set is EMPTY by design. This
# facet encodes exactly that CURRENT contract (no isolcpus/nohz_full; rcu_nocbs=all only). If kernel
# isolation ever legitimately returns "LEN s explicitným per-thread pinningom" (issue 784's own bar),
# that is its OWN new, explicit, tested design that would update this facet's contract — never a
# leftover flag surviving unnoticed.
#
# This lib holds the PURE verdict + the remote gather snippet, SHARED by scripts/drift-guard.sh's
# `--check-imag` facet (check #11) — the SAME extraction discipline imag-display-path.sh (#780) /
# imag-power-envelope.sh (#1040) / timesync-authority.sh (#596) already apply, so the gather and the
# OK/DRIFT/UNKNOWN verdict never exist as two driftable copies.
#
# Source-only: defines pure functions; no side effects at source time.

# _ci_field GATHER KEY -> echoes the value after "KEY|" of the FIRST matching line, "" if the key is
# absent OR present with an empty value. Here-string fed (never a pipe) so there is no SIGPIPE under a
# caller's `pipefail`, and the assignment lands in the current shell. `read -r k v` puts EVERYTHING
# after the first `|` into v (so a cmdline's own spaces + `=` are preserved intact).
_ci_field() {
  local k v val="" want="$2"
  while IFS='|' read -r k v; do
    if [ "$k" = "$want" ]; then val="$v"; break; fi
  done <<< "$1"
  printf '%s' "$val"
}

# _ci_has GATHER KEY -> exit 0 iff a "KEY|..." line is present (distinguishes "key present, empty
# value" from "key absent" — the UNKNOWN-vs-real two-tier the verdict relies on).
_ci_has() {
  local k rest want="$2"
  while IFS='|' read -r k rest; do
    [ "$k" = "$want" ] && return 0
  done <<< "$1"
  return 1
}

# _ci_token_value CMDLINE TOKEN -> echoes the value of a whole-token `TOKEN=<value>` on the space-
# padded cmdline (first occurrence), "" if absent. TOKEN is a fixed keyword (isolcpus/nohz_full/
# rcu_nocbs) with no regex metacharacters, so interpolating it into the grep pattern is safe. The
# trailing `| head -1 || true` keeps a no-match (or a SIGPIPE from head closing early) from aborting a
# caller running `set -euo pipefail`.
_ci_token_value() {
  printf '%s' " $1 " | grep -oE "[[:space:]]$2=[^[:space:]]*" 2>/dev/null \
    | sed -E "s/^[[:space:]]*$2=//" | head -1 || true
}

# imag_cmdline_isolation_verdict GATHER -> echoes ONE `cmdline_isolation|<STATUS>|<detail>` line
# (STATUS in OK / DRIFT / UNKNOWN). The caller (drift-guard's check #11, and a future E2E preflight)
# maps it to its own report style + exit-code contract. Two-tier: an ungathered / empty cmdline (SSH
# hiccup, /proc/cmdline unreadable) is UNKNOWN — never a false OK/DRIFT. A gathered cmdline is DRIFT
# iff it carries `isolcpus=`, `nohz_full=`, or a SCOPED `rcu_nocbs=<value≠all>`; OK otherwise
# (`rcu_nocbs=all` is the legitimate #482 low-latency token). The detail never contains a `|` so the
# caller's `IFS='|' read -r facet status detail` split stays clean.
imag_cmdline_isolation_verdict() {
  local g="$1"
  if ! _ci_has "$g" CMDLINE; then
    printf 'cmdline_isolation|UNKNOWN|/proc/cmdline not gathered\n'
    return 0
  fi
  local cmdline
  cmdline="$(_ci_field "$g" CMDLINE)"
  if [ -z "$cmdline" ]; then
    printf 'cmdline_isolation|UNKNOWN|/proc/cmdline read back empty — cannot verify (never read as OK)\n'
    return 0
  fi

  local offenders=""
  # isolcpus= / nohz_full= — the #784/#842 footgun family; the current affinity-only design writes
  # NEITHER to the cmdline, so ANY occurrence of either is drift.
  if grep -qE '[[:space:]]isolcpus=' <<<" $cmdline "; then
    offenders="${offenders:+$offenders, }isolcpus=$(_ci_token_value "$cmdline" isolcpus)"
  fi
  if grep -qE '[[:space:]]nohz_full=' <<<" $cmdline "; then
    offenders="${offenders:+$offenders, }nohz_full=$(_ci_token_value "$cmdline" nohz_full)"
  fi
  # rcu_nocbs: `all` is the legitimate #482 low-latency (preempt=full) token; a SCOPED per-core list
  # (rcu_nocbs=2-11 etc.) is the isolation family and is drift. grep -vxF 'all' keeps only a value
  # that is NOT exactly `all`; the `|| true` neutralizes the no-match exit under a caller's pipefail.
  local rcu_scoped=""
  rcu_scoped="$(printf '%s' " $cmdline " | grep -oE '[[:space:]]rcu_nocbs=[^[:space:]]+' 2>/dev/null \
    | sed -E 's/^[[:space:]]*rcu_nocbs=//' | grep -vxF 'all' | head -1 || true)"
  if [ -n "$rcu_scoped" ]; then
    offenders="${offenders:+$offenders, }rcu_nocbs=$rcu_scoped"
  fi

  if [ -n "$offenders" ]; then
    printf 'cmdline_isolation|DRIFT|kernel CPU-isolation on /proc/cmdline: %s — removes CPUs from the scheduler load-balancing domain, piles OBS threads onto one core (#784/#842); the current design is affinity-only (taskset), NO kernel isolation\n' "$offenders"
  else
    printf 'cmdline_isolation|OK|no kernel isolcpus/nohz_full/scoped-rcu_nocbs on the cmdline (rcu_nocbs=all is the legitimate #482 low-latency preempt=full token)\n'
  fi
}

# imag_cmdline_isolation_gather_remote_snippet -> the REMOTE shell command (a string) the caller runs
# over its own transport to collect /proc/cmdline into the `|`-delimited `CMDLINE|...` block
# imag_cmdline_isolation_verdict parses. Uses only `printf`/`cat` (ubiquitous). An unreadable
# /proc/cmdline yields `CMDLINE|` (empty value) → the verdict's two-tier reads it UNKNOWN, never OK.
imag_cmdline_isolation_gather_remote_snippet() {
  cat <<'REMOTE'
printf 'CMDLINE|%s\n' "$(cat /proc/cmdline 2>/dev/null)"
REMOTE
}

# imag_cmdline_isolation_preflight_assert HOST [USER] -> the E2E `[0/8]` fail-fast (issue 1105 — the
# issue-784 lib's SECOND consumer, mirroring imag_display_path_preflight_assert). Gathers
# /proc/cmdline over ssh, runs the shared verdict, and returns 1 (printing the offending token to
# stderr) iff the cmdline_isolation facet DRIFTs — so a ~40-min recording run refuses to start on a
# known kernel CPU-isolation footgun (isolcpus=/nohz_full=/scoped-rcu_nocbs) that would strip CPUs
# from the scheduler load-balancing domain and pile OBS's ~119-thread pool onto one core
# (issue 784/842). An UNKNOWN facet (an SSH hiccup; the [0/8] reachability preflight already gates
# genuine unreachability) is warned but does NOT fail the run. Thin ssh glue (NOT unit-tested for the
# ssh transport — the JUDGMENT is the pure imag_cmdline_isolation_verdict above; same convention as
# imag_display_path_preflight_assert / optical_chain_preflight_assert).
imag_cmdline_isolation_preflight_assert() {
  local host="${1:?imag_cmdline_isolation_preflight_assert: HOST required}" user="${2:-newlevel}"
  local target="${user}@${host}"
  local ssh_cmd=(timeout 15 ssh -o ConnectTimeout=10 -o BatchMode=yes -- "$target")
  local gather verdict facet status detail fails="" unknowns="" nl
  nl=$'\n'
  gather="$("${ssh_cmd[@]}" "$(imag_cmdline_isolation_gather_remote_snippet)" 2>/dev/null || true)"
  verdict="$(imag_cmdline_isolation_verdict "$gather")"
  while IFS='|' read -r facet status detail; do
    [ -n "$facet" ] || continue
    case "$status" in
      DRIFT)   fails="${fails:+$fails$nl}  - ${facet}: ${detail}" ;;
      UNKNOWN) unknowns="${unknowns:+$unknowns, }${facet}" ;;
    esac
  done <<< "$verdict"
  if [ -n "$fails" ]; then
    printf 'ERROR: imag kernel-cmdline isolation DRIFT on %s — refusing to start the run (would strip CPUs from the scheduler load-balancing domain and pile OBS threads onto one core, issue 784/842):\n%s\n' \
      "$host" "$fails" >&2
    return 1
  fi
  if [ -n "$unknowns" ]; then
    printf 'WARN: imag cmdline-isolation facet UNKNOWN on %s (not read; not a proven drift): %s\n' "$host" "$unknowns" >&2
  fi
  printf 'imag kernel-cmdline isolation preflight OK on %s (no isolcpus/nohz_full/scoped-rcu_nocbs)\n' "$host"
  return 0
}
