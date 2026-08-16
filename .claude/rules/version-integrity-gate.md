---
paths:
  - "scripts/version-integrity-gate.sh"
  - "scripts/bundle_state_gather.py"
  - "scripts/bundle-state-server.py"
  - "tests/version_integrity_gate.rs"
  - "tests/python/test_bundle_state_gather.py"
  - "tests/python/test_bundle_state_server_log.py"
  - "tests/python/test_bundle_state_server_port4455.py"
---

# version-integrity-gate.sh — the pre-rig-test Windows-stack drift gate (#123/#119)

`scripts/version-integrity-gate.sh` runs FIRST before every rig E2E, reads each Windows box's
observed state (served by `bundle-state-server.py` on `:8899`, gathered by `bundle_state_gather.py`),
and REFUSES the run on DRIFT (exit 20) or UNKNOWN (exit 11). Exit-code roll-up in `main()`:
`bad>0 → 20`, else `unknown>0 → 11`, else `GATE PASS → 0`. It is invoked with `--win-state
"strih=<file>"` + `--win-state "stream=<file>"` (labels are the box `$name`).

## Two-step facet rollout: opt-in (#756-shape) → ENFORCED (#758-shape)

New machine-check facets land **opt-in** first (engage only when the box reports the key) so an
un-upgraded `bundle-state-server` is silently skipped, never a false UNKNOWN. Once the servers are
redeployed to serve the key fleet-wide, a follow-up flips the facet to **enforced** (runs
unconditionally; an absent box → gate-blocking UNKNOWN). `genlock_build_sha` did this #756 → #758;
the #826 obs-identity facet did it #826 → #829.

## Enforced-facet fixtures MUST carry the enforced keys (the with_* injection pattern)

In `tests/version_integrity_gate.rs`, healthy pinned fixtures are built by wrapping the minimal
`STRIH_PINNED`/`STREAM_PINNED` constants at each call site with injection helpers — `with_sha(base,
sha)` (genlock_build_sha) and `with_obs_identity_ok(base, is_strih)` (the #826 keys). When you
ENFORCE a facet, every GATE-PASS test must start carrying its keys or it flips to UNKNOWN and breaks.
`with_obs_identity` (raw pairs) is for the DRIFT/wrong-value tests — inject ONLY the bad key so the
single intended signal is isolated. `state_json_value` is a first-match regex parser, so NEVER
double-inject the same key (baked-in + wrapped) — the first occurrence wins and a wrong-value test
silently reads the healthy value. DRIFT/unread tests that use bare pinned constants still pass by the
`bad>0→20` / `unknown>0→11` precedence even with extra unconditional UNKNOWNs — leave them unless you
need a single clean signal.

## RESOLVED (#1067) — `port4455_owner_path` gather fixed via WMI ExecutablePath, then ENFORCED

**History (#829):** `bundle-state-server.py::port4455_owner()` resolved the :4455 listener's exe
PATH via `Get-NetTCPConnection | Get-Process | .Path`. From the deployed **non-elevated, hidden**
`BundleStateServer` scheduled-task context that was **access-denied** on the elevated OBS process →
`.Path` null → the key OMITTED (omit-when-empty) on the WHOLE live fleet, even though OBS
legitimately owns :4455 at the pinned path (an ELEVATED `win-strih` MCP `Get-NetTCPConnection
-LocalPort 4455 | Get-Process` returned the path fine; a plain `curl :8899/bundle-state.json` did
NOT show `port4455_owner_path`). So `port4455_identity` stayed opt-in behind its own
`if [ -n "$port4455_owner_path" ]` guard — the last obs-identity facet not yet enforced.

**Fix (#1067):** `port4455_owner()` now resolves the path via `Get-CimInstance Win32_Process -Filter
"ProcessId=$id"`.ExecutablePath — the WMI/CIM provider returns ExecutablePath for an elevated
process from a NON-elevated caller where the OpenProcess-based `Get-Process.Path` is denied — with
`Get-Process.Path` kept as a fallback. Then the opt-in guard was REMOVED from
`version-integrity-gate.sh` main(): `port_identity_verdict` now runs UNCONDITIONALLY like
`obs_installs`/`obs_process_count` (empty owner → gate-blocking UNKNOWN). Same 756→758 two-step. The
elevate-the-task alternative was REJECTED (security-boundary escalation of a LAN-facing HTTP task +
a rig redeploy out of a code PR's scope); it stays the documented fallback if a box is ever found
where even CIM ExecutablePath is denied.

**Deploy/verify caveat (still true):** whether CIM ExecutablePath actually reads the elevated obs64
path in the deployed task context is a LIVE-Windows-box property — no worktree worker can verify it.
After deploying the new `bundle-state-server.py` to strih+stream, `curl :8899/bundle-state.json` on
BOTH and CONFIRM `port4455_owner_path`/`_version` now appear BEFORE trusting the enforced gate on a
rig E2E; if still absent, fall back to running the `BundleStateServer` task elevated.

## GOTCHA — strih obs_installs / startup_chain DRIFT is #826's PHYSICAL cleanup, not a code bug

Once strih serves `obs_installs`, the gate flags its 8 `D:\_APPS\_RETIRED_*_2026-07-27` leftover
installs as DRIFT ("renaming aside is not removing"), and `ahk_dead_config_present=1` makes
`startup_chain` DRIFT. Both are #826's remaining PHYSICAL cleanup (delete the retired install folders;
strip the dead `app1_binarypath`/`app2_*` block from `NL_STARTUP.ahk`) — a rig + destructive action,
not fixable in the gate code. They go live under the opt-in code the moment the server serves the key.

## GOTCHA — Tier-0 for these tests: no `# airuleset:build-ok` bypass, cold builds are contended

The `# airuleset:build-ok` bypass is DISABLED for camera-box, so `cargo test` (RUN) cannot execute
locally at all — only `cargo test --no-run` (compile). In a fresh WORKTREE the cold `--no-run` build
(criterion/proptest/etc.) exceeds the ~10-min Bash foreground cap and is heavily CPU-contended by
sibling fleet-worker builds; treat the SUPERVISOR's integration as the authoritative compile+test.
Local Tier-0 you CAN do here: `cargo fmt --all --check`, `bash -n` + `shellcheck` on the `.sh`, and
the Python tests (`python3 -m pytest tests/python/test_bundle_state_*.py` — these DO run locally and
gave a real RED→GREEN for the `log()` dead-stdout fix). `log()` legitimately swallows the dead-stdout
`OSError` (stdout is the broken resource, cannot log it) — bypass-marked `# airuleset:script-ok`.
