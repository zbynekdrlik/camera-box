#!/usr/bin/env bash
# airuleset:script-ok source-only pure-function file (no side effects at source time), mirrors
# the sibling scripts/lib/v4l2-neutral.sh convention (also `set -euo pipefail`-free for the same
# reason -- sourcing this file must never mutate the CALLING script's own shell options).
#
# scripts/lib/cpu-affinity-burn.sh -- SINGLE SOURCE OF TRUTH for giving a transient burn-mode
# `systemd-run` unit the SAME CPU affinity mask production's camera-box.service carries (#707,
# a root-cause follow-up to the all_cambox_continuity gate investigation).
#
# WHY: scripts/setup-device.sh writes /etc/systemd/system/camera-box.service.d/cpu-affinity.conf
# (`CPUAffinity=<isolated core(s)>`, issue 289) so the WHOLE process inherits that mask by
# default -- every thread the binary itself doesn't explicitly sched_setaffinity() (tokio
# workers, NDI SDK internals, intercom) sits on the isolated core alongside the pinned
# capture+emit thread. A systemd drop-in only ever applies to a unit literally named
# `camera-box.service` -- the burn-mode `systemd-run --unit=<transient>` deploy in
# scripts/recording-e2e.sh ([2/8] cam1 / [2b/8] ALL_CAMBOX loop) gets NO mask at all, so those
# same unpinned auxiliary threads default to the GENERAL cores instead.
#
# Measured live on cam2 (#707): production keeps 28/32 threads on the isolated core; the SAME
# binary under burn-mode systemd-run keeps only 1/45 threads there -- 44 auxiliary threads land
# on the general cores, which on cam2 (also the dual-QR painter + qpsk-marker box) collide with
# the painter/marker threads and produced a 53.6% off-nominal emit-cadence rate, vs 0.6% on cam4
# (no painter). The gate was measuring a CPU environment the operator never runs.
#
# APPROACH: derive the mask from the BOX'S OWN `/sys/devices/system/cpu/isolated` at deploy time
# -- the exact same source src/affinity.rs's `read_isolated_cores()` reads -- never a hardcoded
# core number, so burn mode and production can never drift apart again. An un-isolated box
# (empty file) gets NO `--property=CPUAffinity=` at all, rather than inventing a bogus core.
#
# `CPUAffinity=` accepts the SAME comma/range cpulist syntax the kernel's own `isolated` file
# uses (systemd.exec(5): "Takes a list of CPU indices or ranges separated by either whitespace
# or commas") -- so the /sys content passes straight through with no reformatting.
#
# Keep `cpu_affinity_burn_resolve_cmd`'s remote decision text in sync with
# `cpu_affinity_burn_property_for_isolated` below -- they implement the SAME two-line decision
# (strip whitespace; empty -> no property, else `--property=CPUAffinity=<mask>`) in two different
# execution contexts (in-process here for the unit tests, embedded remote text for the real
# deploy) and must never diverge.
#
# Source-only: this file defines pure functions (a decision + a remote-command builder) and
# performs no side effects on its own (no ssh, no device I/O) -- safe to source from
# recording-e2e.sh and from the unit tests (which feed the decision function fixture text).

# cpu_affinity_burn_property_for_isolated ISOLATED_TEXT -> "--property=CPUAffinity=<mask>" (the
# verbatim, whitespace-stripped contents of ISOLATED_TEXT) if non-empty, else the empty string.
# ISOLATED_TEXT is exactly what `cat /sys/devices/system/cpu/isolated` returns -- "" / "3" /
# "1,3" / "2-3" etc. Never invents a core number: an un-isolated box (empty file, no isolcpus=
# on the kernel cmdline) gets NO property at all, matching "no mask" rather than a fabricated
# default.
cpu_affinity_burn_property_for_isolated() {
  local isolated
  isolated="$(printf '%s' "$1" | tr -d '[:space:]')"
  if [ -n "$isolated" ]; then
    printf -- '--property=CPUAffinity=%s' "$isolated"
  else
    printf ''
  fi
}

# cpu_affinity_burn_resolve_cmd -> the REMOTE bash TEXT (embed via `$(...)` inside an ssh command
# string, BEFORE the systemd-run invocation) that resolves the box's OWN isolated-core mask into
# the remote variable CPU_AFFINITY_BURN_PROPERTY: either `--property=CPUAffinity=<mask>` or the
# empty string if the box has no isolated core. The systemd-run call site then references
# `\$CPU_AFFINITY_BURN_PROPERTY` (escaped so the LOCAL orchestrator shell defers it to the
# REMOTE shell, same convention as `\$V4L2_NEUTRAL_NODE`) among its other --property= flags --
# safe unquoted because the value never contains whitespace (the cpulist format is comma/range
# only), so a non-empty value expands to exactly one argument and an empty value expands to none.
#
# CRITICAL (mirrors scripts/lib/v4l2-neutral.sh's own #746 comment -- the SAME command-
# substitution trap): the LAST statement ends with an explicit `;` so it can never be glued to
# whatever literal text follows it at the embedding site ($(...) unconditionally strips trailing
# newlines from captured output).
cpu_affinity_burn_resolve_cmd() {
  printf '%s\n' \
    '_cpuaffburn_isolated="$(cat /sys/devices/system/cpu/isolated 2>/dev/null | tr -d "[:space:]")"' \
    'if [ -n "$_cpuaffburn_isolated" ]; then CPU_AFFINITY_BURN_PROPERTY="--property=CPUAffinity=$_cpuaffburn_isolated"; else CPU_AFFINITY_BURN_PROPERTY=""; fi;'
}
