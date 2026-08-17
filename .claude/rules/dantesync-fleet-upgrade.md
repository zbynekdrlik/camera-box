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
- **Single-node `dantesync-gate.sh` verification — a SLAVE opts out (`--ntp-master ""`), the MASTER
  is graded master-aware (`--ntp-master <self>`) (#1077 refines the original blanket opt-out).** The
  gate defaults `GATE_NTP_MASTER_NAME=strih` and REFUSES (usage error) when `--win-http`/`--linux`
  is configured but the master name isn't among the configured nodes — so a non-master node MUST
  pass `--ntp-master ""` (the documented opt-out; it forgoes the #1041/#1055 master-step-chase
  widening — the settle poll covers a transient re-lock). But the MASTER node, verified alone, IS
  among its own one configured node, so pass `--ntp-master "<name>"` (`verify_node`'s `master_arg`):
  the gate then applies its #1014 master median+freshness grade (a master-only invocation is
  explicitly supported and pays no priming read) instead of the strict slave offset bound — which
  is what tolerates the master's OWN post-restart step-chase. Blanket `--ntp-master ""` on the
  master measured the restart-induced sawtooth and rolled back a HEALTHY swap (rc=20 twice, live
  v1.8.43).

## #1077 additions — non-root escalation, curl-less staging, master settle window

- **Non-root Linux nodes escalate; the script is run BY FILE, never inline.** The generated
  upgrade/rollback script does root-only ops (`mount -o remount,rw`, `install`, `systemctl`). A
  `root@` node (cam boxes) runs it directly (`dantesync_linux_run_script_cmd` → `bash "$path"`); a
  non-root node (imag-nb `newlevel@`, dev1 `--local`) runs it escalated — `sudo -n` where
  passwordless (dev1), else `printf '%s\n' '<pw>' | sudo -S -p '' bash "$path"`. The password is
  embedded only in the RUN COMMAND (the ssh/`bash -c` arg), NEVER written into the scp'd on-disk
  script FILE. This mirrors the `scripts/lib/imag-presented-frame-check.sh` `sudo -S` convention and
  the Windows `-File` delivery (no nested-quoting hazard). `dantesync_needs_sudo USER` is the
  root-vs-not decision (0 = needs sudo unless USER is `root`).
- **Binary fetch is dev1-staged first, then on-box curl→wget→fail-loud (curl-less boxes: cam3).**
  `ensure_linux_binary_staged` downloads + sha256-verifies the pinned binary ONCE on dev1 (memoized;
  the memo `STAGED_LOCAL_DIR` is published ONLY after the sha passes, so a failed fetch never
  poisons it), and `stage_linux_binary_to` scp's it to `DANTESYNC_LINUX_STAGED=/tmp/dantesync-staged`
  on each node (cp for `--local`). The generated script's fetch resolver is `[ -f staged ]` →
  `command -v curl` → `command -v wget` → `exit 1`, then re-sha-verifies whichever (guards a corrupt
  scp) BEFORE `systemctl stop`. cam3 (no curl, broken apt) upgrades from the pre-placed binary; the
  metered venue LAN pays ONE download, not eight.
- **The master node's settle window is LONGER + bounded** — `MASTER_GATE_WAIT_TRIES`/`_SECS`
  (default 20 × 15s ≈ 5 min) vs the slave `GATE_WAIT_TRIES`/`_SECS` (10 × 6s ≈ 60s). Still a bounded
  `for i in $(seq 1 "$tries")` loop with a clear final PASS/FAIL (`gate_rc`), never a `while true`
  sleep-and-hope. Because the master (strih) is the first Windows canary, waiting it to steady state
  also gates the REST loop — slaves are verified only after the fleet has re-converged. `NTP_MASTER`
  defaults from `DANTESYNC_NTP_MASTER_NAME` (strih), the gate's own single source of truth.

## Testing (Tier-0)

Heavy `cargo test` is CI-only here (#477) — no `# airuleset:build-ok` bypass. Verify the bash logic
by SOURCING the script directly in bash (the same thing `tests/dantesync_fleet_upgrade.rs`'s
`run_sourced` does, minus cargo) and asserting the pure-fn outputs + generated command TEXT; plus
`shellcheck`, `bash -n`, and `rustfmt --check` on the test. The Rust binary compiles on CI.
