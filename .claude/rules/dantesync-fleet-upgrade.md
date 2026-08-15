---
paths:
  - "scripts/dantesync-fleet-upgrade.sh"
  - "tests/dantesync_fleet_upgrade.rs"
---

# dantesync fleet-upgrade mechanism (#876)

`scripts/dantesync-fleet-upgrade.sh` is the REMEDIATION half of the dantesync version-drift
problem — `dantesync-version-gate.sh` (#862) is the DETECTION half. It is operator/agent-INVOKED,
never a scheduled task (a task that silently stops scheduling is as bad as no task — the exact
#876 root cause: the Windows `DanteSyncUpdate` task died at the DanteTimeSync→DanteSync rename and
sat `Enabled`/`Next=N/A`/`Result=0` for months). Structure mirrors the tested
`scripts/upgrade-fleet-ndi.sh`: pure fns above a `BASH_SOURCE != $0` source-guard (unit-tested by
sourcing), network/mutating flow below.

## Design invariants (do not regress)

- **Target the PIN, never `releases/latest`.** Default target = `DANTESYNC_VERSION_PIN`, sourced
  from `dantesync-version-gate.sh` (single source of truth). "latest" would chase docs-only
  version bumps and schedule pointless clock-master redeploys (the #876 follow-up comment).
- **Canary-first, one representative per OS CLASS present** (`dantesync_resolve_canary`). A green
  Linux canary must NEVER authorize touching a Windows box — the class here is the OS (the #452
  per-class insight from the NDI upgrader). Any canary failure ABORTS the whole roll (rest
  untouched, exit 10); a non-canary failure is recovered + recorded but the loop continues (exit 20).
- **Reuse, never reinvent:** the version PARSER (`dantesync_version_from_version_output`) + the PIN
  come from sourcing `dantesync-version-gate.sh`; canary VERIFY is `dantesync-gate.sh` (PTP-lock +
  fresh in-bound offset); offline exclusion is `scripts/lib/cambox-offline-ack.sh` + `rig-fleet.txt`.

## Three traps a review caught (2026-08-15) — keep them fixed

- **Self-heal the swap; NEVER blind-rollback on an upgrade-command failure.** The remote upgrade
  script downloads + sha256-verifies BEFORE stopping the service, backs up the current binary, then
  arms a restore trap (Linux `trap '_dantesync_restore' ERR` disarmed on success; Windows
  try/catch that restores the `.bak` + rethrows) — so a failure PAST the point of no return
  self-heals ON THE BOX. The orchestrator therefore only externally `rollback_node`s on the
  VERIFY-failure path (swap provably completed). Blind-rolling-back on ANY non-zero upgrade rc
  would, with a pre-existing `.bak`, stop a HEALTHY master and downgrade it (a failed *download*
  exits before the service is ever touched).
- **Windows path SENDS A `.ps1` (scp -O) and runs it with `-File`** — never a nested
  `powershell -Command "..."` over ssh, which fails SILENTLY (exit 0, no output) per
  `.claude/rules/rig-state-inspection.md` §2. `dantesync_windows_upgrade_ps`/`_rollback_ps` return
  the `.ps1` CONTENT; `dantesync_windows_run_ps_file_cmd` is the `-File` invocation.
- **Single-node `dantesync-gate.sh` verification MUST pass `--ntp-master ""`.** The gate defaults
  `GATE_NTP_MASTER_NAME=strih` and REFUSES (usage error) when `--win-http`/`--linux` is configured
  but the master name isn't among the configured nodes — so verifying any non-strih node (e.g.
  `stream`, the box the ticket exists to converge) always failed. `--ntp-master ""` is the
  documented opt-out; it forgoes the #1041/#1055 master-step-chase widening (the post-restart
  settle poll covers a transient re-lock).

## Testing (Tier-0)

Heavy `cargo test` is CI-only here (#477) — no `# airuleset:build-ok` bypass. Verify the bash logic
by SOURCING the script directly in bash (the same thing `tests/dantesync_fleet_upgrade.rs`'s
`run_sourced` does, minus cargo) and asserting the pure-fn outputs + generated command TEXT; plus
`shellcheck`, `bash -n`, and `rustfmt --check` on the test. The Rust binary compiles on CI.
