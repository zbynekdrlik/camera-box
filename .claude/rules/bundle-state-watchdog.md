---
paths:
  - "scripts/bundle-state-alert-watchdog.sh"
  - "scripts/lib/bundle-state-health.sh"
  - "systemd/bundle-state-alert-watchdog.*"
  - "tests/harness_bundle_state_*.rs"
---

# dev1-side `:8899` BundleStateServer health-check + auto-restart watchdog (#732)

Closes the "the strih/stream `:8899` version-integrity input died and stayed dead for days" gap
(four recurrences through 2026-08-13). `SCHED_S_TASK_TERMINATED` (`0x40010004`) is an
informational/SUCCESS result, so Windows Task Scheduler's restart-on-failure (`RestartCount=999`)
never engages; a cold-start-after-reboot can also simply never fire. A passive Task-Scheduler policy
cannot cover a non-failure termination — only an ACTIVE external prober can.

## This is the THIRD dev1 alert-watchdog sibling — reuse the framework, do not reinvent it

Same shape as `network-reach-alert-watchdog` (#1001) and `obs-liveness-watchdog` (#391): a
`set -uo pipefail` (NOT `-e`) systemd `--user` oneshot + timer (5-min cadence), a SOURCED pure
decision core, `airuleset.py notify` from dev1. It reuses `scripts/lib/obs-watchdog-decision.sh`
(`obs_watchdog_confirm` 2-pass confirm + `obs_watchdog_alert_throttle` ~1h re-alert) and
`scripts/lib/network-reach-health.sh` (`net_reach_any_reachable` dev1-side-outage anchor +
`net_reach_recovery_decision`) VERBATIM; the only NEW pure logic is `scripts/lib/bundle-state-health.sh`.

## The one thing that makes it DIFFERENT from obs-liveness: it AUTO-RESTARTS (obs-liveness is alert-only)

The discriminator for "may a dev1 headless timer auto-recover this?" is **whether recovery is a
session-agnostic op**:

- OBS recovery needs a **GUI relaunch** → alert-only from dev1 (a headless timer cannot drive it,
  and relaunching OBS could fight a deliberate operator quit — #788).
- BundleStateServer recovery is **`schtasks /run /tn "BundleStateServer"`** — a HIDDEN, headless
  supervisor task (verified live: the task action is `powershell … -WindowStyle Hidden -File
  run-bundle-state-server.ps1`, `Logon Mode: Interactive only`). Starting a hidden background task
  over ssh is session-agnostic per `win-ssh-vs-mcp.md` (**never** `/it`, a documented DEAD END), and
  the server is pure infra that is NEVER deliberately stopped, so auto-restart can never fight the
  operator. So it DOES auto-restart, then alerts (throttled) so a restart that doesn't take still
  surfaces.

## Non-obvious build/test gotchas hit here

- **Health probe MUST be `curl` FROM dev1** (HTTP 200 + a JSON body), never a bare `:8899` TCP
  connect (that would miss a wedged-but-listening server) and never an MCP-side `Invoke-WebRequest`
  (it hangs even when the server logs a prompt 200 — the ops-SKILL note documents this). The
  JSON-body check strips leading whitespace/BOM before requiring a `{` prefix.
- **A missing `curl` must FAIL LOUD, not read as a measured `:8899 DOWN`** — otherwise every box
  false-classifies DOWN and gets a real restart + page. `require_tools` (curl/ping/timeout) aborts
  the pass with exit 3 (`imag-ssh-remote-tool-preflight.md` #833). `sshpass` is deliberately NOT
  required — its absence degrades safely to alert-only.
- **Do NOT source `scripts/lib/win-ssh-exec.sh`** for the ssh restart — it sets `set -euo pipefail`
  at top, which leaks `-e` into a watchdog that must survive every per-pass failure. Use a
  self-contained `sshpass` call bounded by `timeout` instead.
- **Separation of concerns / no double-paging:** a fully-unreachable box (ping + `:4455` + `:8899`
  all down) → `bundle_state_classify` returns `BOX_UNREACHABLE` → this watchdog DEFERS to
  `network-reach-alert-watchdog` (#1001); it acts ONLY on "box up, `:8899` down".
- **Testing the decision composition offline:** the pure lib has a Tier-0 harness
  (`harness_bundle_state_health_732.rs`); the `handle_box` glue is covered by
  `harness_bundle_state_watchdog_732.rs` via PATH-shimmed `curl`/`ping` + a tempdir state file +
  `--dry-run` (same fixture pattern as `ci-testing-gotchas.md` #836/#975). `cargo` tests do NOT run
  locally here (Tier-0, build-ok DISABLED #477) — prove behaviour by running the watchdog directly
  in bash with the same shims, and rely on CI for the Rust assertions.

## Ships DISABLED — supervisor installs

Units are committed but NOT enabled. Install/live-verify/enable per
`systemd/bundle-state-alert-watchdog.README.md`. This watchdog makes NO Windows-side change — it only
INVOKES `schtasks /run` on the existing task. Recreating the task on re-provision and the
stale-deployed-code drift class remain separate follow-ups.
