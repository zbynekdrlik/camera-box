---
paths:
  - "scripts/recording-e2e.sh"
  - "scripts/lib/imag-require-remote-tool.sh"
  - "scripts/lib/imag-projector-heal.sh"
  - "tests/harness_imag_require_remote_tool_833.rs"
  - "tests/harness_projector_count_756.rs"
---

# A missing tool on a REMOTE box (over SSH) must never read as a MEASURED zero — #833

`.claude/rules/imag-nb-provisioning.md` already documents #822: a missing `readelf`/`nm` made a
LOCAL provisioning-time verification (`setup-imag.sh`, running ON the box) report a false failure
instead of naming the absent tool. `recording-e2e.sh` hits the SAME bug class in a DIFFERENT
execution context: it runs on dev1 and shells a helper on imag-nb **over SSH**, at E2E-gate time,
long after provisioning — `imag_require_tools()` (local `command -v`) is the wrong tool for this
case; it never runs on the box that might be missing the dependency.

## The bug shape

A remote command like `wmctrl -l | grep -c 'Projector - Multiview'` or
`nm -D -u /usr/bin/obs | grep -c obs_display_set_render_divisor`, run over `ssh ... "$(...)"
2>/dev/null`, emits **nothing** when the tool itself is absent (`command not found` goes to the
remote stderr, swallowed by the inline `2>/dev/null`, and the outer `ssh ... 2>/dev/null || true`
swallows it again). `grep -c` on empty input reads `0`. The caller then reads that `0` as a
genuine measurement — "Multiview=0 Program=0, stray windows accumulating" or "MV divisor
capability MISSING (#756 regression)" — chasing a config/regression that was never real. Live
incident (#833, 2026-07-27/28): three wasted hardware-gate re-runs before `wmctrl` being absent
(freshly reprovisioned box, #791) was suspected.

## The fix — `scripts/lib/imag-require-remote-tool.sh`

- `imag_require_remote_tool_cmd TOOL...` prints a REMOTE bash snippet (embed via `$(...)` into
  the ssh command string, same pattern as `imag_projector_heal_cmds`) that does `command -v` on
  each named tool and prints one `TOOL_MISSING:<name>` line per absent tool. Always exits 0 on
  the remote side — the CALLER decides what a non-empty probe means, so an outer `|| true` can
  never mask this check's own verdict.
- `imag_remote_tool_probe_missing PROBE_OUTPUT` is the pure LOCAL parser (no ssh) — extracts the
  space-joined list of missing tool names from the probe's captured stdout. Empty input -> empty
  output (also the shape an ssh/connectivity failure upstream produces; the caller's own `-n
  "$missing"` check treats that the same as "nothing reported missing", which is correct here —
  a genuinely dead SSH link is caught by the earlier `[0/8]` fleet-reachability preflight, not by
  this check).

**Wire a new remote-shelling preflight this way:** run the probe FIRST (before the real check
that shells the same tool), fail loud naming the tool + its `apt-get install` line if missing,
THEN proceed to the real measurement. Insert as NEW lines before the existing anchored check —
never edit the anchored line itself (the established #675 pattern; `tests/harness_projector_
count_756.rs` and `tests/setup_imag_guards.rs` pin ~100+ literal strings/adjacencies in these two
files, see the project CLAUDE.md GOTCHA).

## Testing without a rig — fake the remote, not the ssh

`tests/harness_imag_require_remote_tool_833.rs`'s `run_probe` sources the real lib on dev1
(generating the snippet text), then re-execs that text via a NESTED `bash -c` with **its own
PATH restricted to a per-test fake-bin tempdir** — this simulates "what the ssh target would
run" without any network/ssh call. Restrict PATH only on the INNER (simulated-remote) bash, not
the outer one: the outer shell still needs the real `bash`/`cat`/`printf` on PATH to source the
lib and generate the snippet text, and restricting it too risks accidentally proving nothing
because this dev box genuinely has both `wmctrl` and `nm` installed already. Use the tool's
absolute path (`/usr/bin/bash`) for the nested invocation rather than relying on PATH lookup to
find `bash` itself.

## When adding the NEXT remote-shelling preflight

Grep the target script for `grep -c`/`wc -l` fed by an `ssh ... 2>/dev/null || true` capture that
feeds a numeric pass/fail compare — that shape is the tell. #833's sweep found exactly one
sibling (the `nm`-based MV divisor-capability check) sharing it with the `wmctrl` count; both are
now guarded. Reuse `imag_require_remote_tool_cmd`/`imag_remote_tool_probe_missing` rather than
writing a third ad-hoc "is the tool there" check.
