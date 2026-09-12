---
paths:
  - "scripts/dantesync-maintenance-gate.sh"
  - "scripts/dantesync_config_patch.py"
  - "scripts/dantesync-fleet-upgrade.sh"
  - "scripts/dantesync-version-gate.sh"
  - "tests/dantesync_maintenance_gate.rs"
  - "tests/dantesync_fleet_upgrade.rs"
  - "tests/python/test_dantesync_config_patch_1297.py"
---

# RESOLUME-SNV dantesync maintenance (issue 1297)

RESOLUME-SNV (the CG / graphics PC — Resolume Arena → strih via Spout/NDI, plus a `cg-obs`) is a
**traveling** `windows-genlock` box in `scripts/lib/obs-fleet.sh`, under the fleet dantesync
clock-discipline umbrella since issue 811. It runs dantesync (1.8.53 = the fleet pin) and answers
`:8898/status` whenever it is up. Issue 1297 added the CODE + DOCS half of closing a specific
drift; the on-box config flip + the dantesync failover FEATURE are NOT in this repo (see below).

## The problem (supervisor read-back 2026-09-12)

`C:\ProgramData\dantesync\config.json` = `{"http_status":{"enabled":true,"port":8898},"ntp_server":
"strih.lan","ntp_server_mode":{"enabled":false,...}}` with **no `system.phase_slew` key**.
`:8898/status` → `mode=NANO is_locked=true gm_source_ip=10.77.9.138 ntp_failed=true
ntp_sample_count=0 accumulated_phase_us=-14020 phase_slew_enabled=false`.

`ntp_server: "strih.lan"` does NOT resolve from the box while strih is powered off (most of the
week), so the NTP subsystem never takes a sample (`ntp_failed=true`, 0 samples). PTP keeps the
FREQUENCY disciplined, but with no UTC-phase reference and `phase_slew` OFF the accumulated phase
free-runs (−14 ms) and the box STEPS it in one jump the instant strih returns — the issue-1130 step
storm the rig boxes + mbc already cured by enabling `phase_slew`.

## What this repo ships vs what it does NOT

- **Ships (issue 1297):** a pure config-patch helper, a report-only maintenance gate, resolume in
  the fleet-upgrade traveling path, and this runbook.
- **NOT this repo:** the on-box config flip + service restart + read-back is the **SUPERVISOR's rig
  step** (run the EMIT-only program below via the `win-resolume` MCP; do NOT ssh/MCP a box from a
  code lane). The NTP-FAILOVER feature (a secondary master / failover list so the box keeps a phase
  reference while strih is off) is **zbynekdrlik/dantesync#111** — until it lands, the `ntp_server`
  repoint target (dev1 as a chained secondary master vs a public pool) is an OPEN design decision;
  `dantesync_config_patch.py` PARAMETRISES `ntp_server` and defaults to LEAVING IT UNCHANGED, so
  the phase_slew flip can ship now without touching the master chain.

## Components

- `scripts/dantesync_config_patch.py` — PURE (pytest Tier-0). `patch_config(text, ntp_server=None)`
  sets `system.phase_slew.enabled=true` (preserving every other key + order), optionally repoints
  `ntp_server`, and REFUSES to touch `ntp_server_mode` (a hard round-trip invariant — the "never
  two masters" rule, `rig-timesync-single-authority`). `emit_apply_program(...)` / `--emit-apply`
  returns the EMIT-ONLY PowerShell the supervisor pastes (backup → write → `Restart-Service
  dantesync` → read back `:8898/status`), mirroring the mbc phase_slew flip on issue 1265.
- `scripts/dantesync-maintenance-gate.sh` — REPORT-ONLY, NEVER in the blocking `[0/8]` set. Reuses
  the shared parsers (`ptp_locked_from_pipe_json` / `ntp_freshness_verdict` /
  `offset_us_from_pipe_json` / `phase_slew_enabled_from_pipe_json` from `clock-offset-guard.sh`,
  `dantesync_version_from_version_output` + `DANTESYNC_VERSION_PIN` from `dantesync-version-gate.sh`)
  and `obs_fleet_is_home` from `obs-fleet.sh`. Prints ONE honest row — **SKIP when away (never a
  false red)**, OK / ALARM / UNKNOWN when home. Exit 0 OK|SKIP, 30 ALARM, 11 UNKNOWN.
- `scripts/dantesync-fleet-upgrade.sh` — resolume added via `dantesync_resolume_win_spec <ip>`
  (`resolume=newlevel@<ip>`); `dantesync_skip_away_traveling` SKIPS a traveling box that is away
  (`obs_fleet_is_home` false) from a roll instead of failing it — so a fleet roll may always list it.

## SUPERVISOR RUNBOOK (the on-box flip — run only when the box is HOME + its identity is CONFIRMED)

resolume.lan's DHCP lease drifts and COLLIDES with `bridge` on 10.77.9.201 (`targets.md`). ALWAYS
confirm identity first (`getent hosts resolume.lan` + its OBS profile, `rig-state-inspection.md`
§2). All on-box steps run through the `win-resolume` MCP Shell — NOT ssh, NOT from a code lane.

1. **Read the current config on-box** (win-resolume MCP Shell):
   `Get-Content 'C:\ProgramData\dantesync\config.json' -Raw`
2. **Generate the APPLY program on dev1** from that text (phase_slew only; ntp_server unchanged):
   `printf '%s' '<the config text from step 1>' | python3 scripts/dantesync_config_patch.py --emit-apply`
   (add `--ntp-server <host>` ONLY once dantesync#111 lands and the owner has decided the target.)
3. **Paste the emitted PowerShell into the win-resolume MCP Shell.** It backs up
   `config.json.bak-<date>`, writes the patched config (no-BOM UTF-8), `Restart-Service dantesync`,
   waits ~8 s, and prints `:8898/status`.
4. **Verify the read-back** (the emitted program already prints it; or from dev1 once home:
   `curl -fsS http://resolume.lan:8898/status`). Then run the maintenance gate from dev1:
   `scripts/dantesync-maintenance-gate.sh --box resolume`.

### Acceptance (issue 1297 #4)

- **With strih OFF** (the common case): resolume `:8898/status` shows `is_locked=true`,
  `phase_slew_enabled=true`, AND — the real cure target, pending dantesync#111 — `ntp_failed=false`,
  `ntp_age_s < 120`, `|ntp_offset_us| < 2000`. Until dantesync#111 repoints `ntp_server` to a source
  that EXISTS when strih is off, `ntp_failed` may STILL be true with strih off; the `phase_slew`
  flip alone converts the eventual re-lock from a STEP storm into a smooth slew, which is the
  shippable-now half. The maintenance gate then reads OK once all fields are green, ALARM while
  `ntp_failed`/`phase_slew` are still wrong, SKIP while the box is away.
- **With strih ON**: the same fields green, and NO `[NTP] Stepped` storm on either box during the
  hand-back (the phase_slew slew replaces the step).
- The upgrade script lists resolume: `dantesync_resolume_win_spec "$(getent hosts resolume.lan |
  awk 'NR==1{print $1}')"` → a `resolume=newlevel@<ip>` `--win` entry.

**Do NOT wire the maintenance gate to a PAGING dev1 watchdog until dantesync#111 lands.** Until the
`ntp_server` repoint ships, a correctly-phase_slew-fixed resolume STILL reads `ntp_failed=true`
whenever strih is off (its only NTP master) — so the gate grades ALARM on most runs BY DESIGN. That
is fine for a MANUALLY-run maintenance check, but a paging watchdog over it would be chronic Discord
noise (the "Discord objem blízko nuly" rule). This gate ships as a standalone manual check ONLY
(nothing in this lane wires it to a watchdog); promote it to a paged facet only once dantesync#111
gives the box a phase reference that exists while strih is off — then a strih-off ALARM becomes a
genuine anomaly, not the expected state.

## Tier-0 verification (this lane)

`bash -n` + `shellcheck -S warning` on the `.sh`; source the libs and exercise the pure verdict /
helpers over fixtures (via a scratch `bash <file>`, never the guarded `bash -c '…source…'` from a
worktree); `pytest tests/python/test_dantesync_config_patch_1297.py`; `cargo fmt --all --check`. The
Rust harnesses (`tests/dantesync_maintenance_gate.rs`, the issue-1297 block in
`tests/dantesync_fleet_upgrade.rs`) compile + run at CI (cargo is Tier-0-blocked locally).
