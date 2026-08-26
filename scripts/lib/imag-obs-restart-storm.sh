#!/usr/bin/env bash
# scripts/lib/imag-obs-restart-storm.sh -- #1156 dev1-side RESTART-STORM detector for imag-obs.service.
# One-line summary; the full header is below the `set` line (kept early per pre-write-script-check).
set -euo pipefail
#
# WHY (#1156): the #1143 record-encoder lane added `import imag_record_encoder` to imag_scenes.py but
# setup-imag.sh never added the sibling to its on-box install list, so a deploy pushed the importer
# WITHOUT the imported module -> every imag-obs-start.sh seed died on ModuleNotFoundError -> the
# imag-obs.service Restart=on-failure relaunched the cgroup -> 1737 restarts over 8.5h, and NOTHING
# read imag-obs.service's NRestarts counter so it paged nobody. This lib is the PURE core of the
# restart-STORM detector folded into the existing #882 scripts/imag-obs-alert-watchdog.sh (never a
# second prober): a remote-snippet builder + a time-windowed "N restarts per window_s" classifier.
#
# Two PURE functions (no I/O, no ssh, no time of their own -- the caller passes `now`), sourced by
# the dev1-side watchdog. Same command-builder + pure-classifier shape as imag-obs-reachability.sh.
#
# Source-only: defines the functions; runs nothing.

# imag_obs_restart_counter_probe_cmd -> prints a REMOTE bash snippet (embedded via $(...) as the
# LAST arg of an ssh, same always-exit-0 builder shape as imag_obs_reachability_probe_cmd). Reads
# imag-obs.service's systemd NRestarts counter over the USER bus and prints exactly ONE line:
#   NRestarts=<n>            -- the current restart counter
#   NRESTARTS_QUERY=FAILED   -- user bus unreachable / unit unknown / non-numeric (an explicit
#                               sentinel, never an empty line the classifier could misparse).
# A non-login ssh session needs XDG_RUNTIME_DIR to reach the user bus (issue 998). Quoted heredoc:
# every `$` here is literal (it runs on the REMOTE box), and the snippet sits at the END of the ssh
# string so the $(...) trailing-newline strip (#744) is harmless (nothing is concatenated after it).
imag_obs_restart_counter_probe_cmd() {
  cat <<'EOF'
export XDG_RUNTIME_DIR="/run/user/$(id -u)" >/dev/null 2>&1 || true
__n=$(systemctl --user show imag-obs.service -p NRestarts --value 2>/dev/null)
case "$__n" in
  ''|*[!0-9]*) echo "NRESTARTS_QUERY=FAILED" ;;
  *) echo "NRestarts=$__n" ;;
esac
EOF
}

# imag_obs_restart_storm_classify <prev_baseline> <prev_ts> <cur_probe> <now> [threshold=10] [window_s=600]
#   -> stdout: storm=0|1  baseline=<n>  baseline_ts=<epoch>  reason=<short>
#
#   A pure, time-windowed "N restarts within window_s" rule, fail-safe = NEVER false-page:
#     - unreadable current counter (probe != NRestarts=<n>) -> storm=0, PRESERVE the prior anchor
#       (so the next readable pass computes the delta correctly). The down-alert path owns a
#       genuinely-dead unit; a transient unreadable counter is never a storm.
#     - no/ corrupt prior anchor (first pass ever) -> storm=0, baseline=cur, baseline_ts=now.
#     - cur < prev (systemd reset the counter: reboot / `reset-failed` / unit reinstall) -> storm=0,
#       re-baseline to cur/now.
#     - delta = cur-prev >= threshold AND elapsed = now-prev_ts <= window_s -> storm=1, re-anchor to
#       cur/now (so a continuing storm doesn't recount the same restarts).
#     - delta >= threshold but elapsed > window_s (SLOW accumulation, rate below N/window) -> storm=0,
#       slide the window (re-anchor to cur/now).
#     - below threshold, still inside the window -> storm=0, KEEP the ORIGINAL anchor so restarts
#       keep accumulating across sub-window passes.
#   Pure + -e-safe (no grep/pipe; only arithmetic, case, [ ] in if-conditions), so it is safe both
#   called bare and under a sourced caller's `set -euo pipefail`.
imag_obs_restart_storm_classify() {
  local prev="${1:-}" prev_ts="${2:-}" cur_out="${3:-}" now="${4:-}" threshold="${5:-10}" window_s="${6:-600}"
  case "$threshold" in ''|*[!0-9]*) threshold=10 ;; esac
  case "$window_s" in ''|*[!0-9]*) window_s=600 ;; esac
  case "$now" in ''|*[!0-9]*) now=0 ;; esac
  now=$(( 10#$now ))   # force base-10: a leading-zero value is octal-hazardous under $(( )) (#1156 review)

  # parse the current NRestarts from the probe line.
  local cur=""
  case "$cur_out" in
    *NRestarts=*)
      cur="${cur_out##*NRestarts=}"
      cur="${cur%%[!0-9]*}"
      ;;
  esac
  if [ -z "$cur" ]; then
    printf 'storm=0\nbaseline=%s\nbaseline_ts=%s\nreason=unreadable-counter\n' "$prev" "$prev_ts"
    return 0
  fi
  cur=$(( 10#$cur ))   # base-10 (see #1156 review): cur is all-digits here

  # validate the prior anchor; either half missing/corrupt = a first pass -> establish baseline.
  case "$prev" in ''|*[!0-9]*) prev="" ;; esac
  case "$prev_ts" in ''|*[!0-9]*) prev_ts="" ;; esac
  if [ -z "$prev" ] || [ -z "$prev_ts" ]; then
    printf 'storm=0\nbaseline=%s\nbaseline_ts=%s\nreason=first-pass\n' "$cur" "$now"
    return 0
  fi
  prev=$(( 10#$prev )); prev_ts=$(( 10#$prev_ts ))   # base-10 (see #1156 review): both all-digits here

  if [ "$cur" -lt "$prev" ]; then
    printf 'storm=0\nbaseline=%s\nbaseline_ts=%s\nreason=counter-reset (%s<%s: reboot/reset-failed/reinstall)\n' \
      "$cur" "$now" "$cur" "$prev"
    return 0
  fi

  local delta=$(( cur - prev ))
  local elapsed=$(( now - prev_ts ))
  if [ "$elapsed" -lt 0 ]; then
    elapsed=0
  fi

  if [ "$delta" -ge "$threshold" ] && [ "$elapsed" -le "$window_s" ]; then
    printf 'storm=1\nbaseline=%s\nbaseline_ts=%s\nreason=delta=%s>=%s in %ss (imag-obs restart storm)\n' \
      "$cur" "$now" "$delta" "$threshold" "$elapsed"
    return 0
  fi

  if [ "$elapsed" -gt "$window_s" ]; then
    printf 'storm=0\nbaseline=%s\nbaseline_ts=%s\nreason=window-expired (delta=%s in %ss, rate below %s/%ss)\n' \
      "$cur" "$now" "$delta" "$elapsed" "$threshold" "$window_s"
    return 0
  fi

  printf 'storm=0\nbaseline=%s\nbaseline_ts=%s\nreason=accumulating (delta=%s in %ss, threshold %s)\n' \
    "$prev" "$prev_ts" "$delta" "$elapsed" "$threshold"
}
