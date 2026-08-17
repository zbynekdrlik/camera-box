---
paths:
  - "scripts/lib/wol.sh"
  - "scripts/wake-box.sh"
  - "scripts/enable-nic-wol.ps1"
  - "scripts/wol-targets.txt"
  - "docs/wake-on-lan.md"
---

# Wake-on-LAN remote recovery for strih + stream (#1053)

The remote-recovery counterpart to issue 1001's outage-detection watchdog: a magic packet from dev1
wakes a slept/off broadcast OBS box. See `docs/wake-on-lan.md` for the operator runbook + BIOS checklist.

## Architecture (mirror it if you extend WoL)

- `scripts/lib/wol.sh` — source-only PURE lib (mirrors `scripts/lib/network-reach-health.sh`): MAC
  normalize, 102-byte magic packet (6×0xFF + 16×MAC), box→ip→mac table lookup. No I/O — unit-tested
  exhaustively by `tests/harness_wol_1053.rs` (sourced-bash behavioral, the `harness_v4l2_neutral_744.rs`
  pattern). Keep all deterministic logic HERE; keep I/O in the thin wrappers.
- `scripts/wake-box.sh` — dev1 sender. dev1 has NO `wakeonlan`/`etherwake`, and bash `/dev/udp` cannot
  set `SO_BROADCAST`, so the actual send is a tiny inline `python3` (already a repo dep) that sets
  `SO_BROADCAST` and sends the hex packet built by the pure lib. Sends to subnet-directed (`a.b.c.255`)
  + limited (`255.255.255.255`) broadcast on UDP 9 AND 7 (the 2026-08-13 delivery shape). dev1 is on
  the same `10.77.9.0/24` as the rig, so the subnet broadcast reaches at L2; from off-segment, run it
  from a cam box instead (routed directed-broadcasts are commonly dropped).
- `scripts/enable-nic-wol.ps1` — Windows NIC enable+VERIFY. Session-agnostic NIC/registry op (NOT
  desktop-dependent), so run it over `scripts/lib/win-ssh-exec.sh` `win_ssh_run` (PowerShell
  `-EncodedCommand`) OR the win-* MCP Shell — per `.claude/rules/win-ssh-vs-mcp.md` this is Context-B
  ssh-fine. Idempotent, fail-loud (`$ErrorActionPreference='Stop'`), `-DryRun`/`-VerifyOnly` (both
  read-only, no admin; applying needs elevation). It splits CRITICAL drift (blocks a magic-packet
  wake → fails verify) from HARDENING drift (`*WakeOnPattern`="only a magic packet" — box still wakes
  → WARN only). `-VerifyOnly` from dev1 is the safe way to probe live state without changing it.

## STEP-0 finding (2026-08-17) — the NIC half is ALREADY DONE on both boxes

Live `-VerifyOnly` proved both boxes are magic-packet-enabled + `wake_armed` TODAY: strih=Marvell
AQtion Felicity 10GbE (add-in) MAC `5C:6A:80:F6:6C:F7`; stream=Realtek onboard GbE MAC
`E8:9C:25:CE:B6:EA`. Both: only S3 sleep, hibernation off. So the remaining WoL gap is BIOS
standby-power, NOT Windows NIC settings — if a future magic packet does not wake a box, check BIOS
("Power On by PCI-E" / "Wake on LAN") + the S5-vs-S3 power state, not the NIC advanced properties.

## GOTCHA — Realtek `*EEE` `RegistryValue` is a MULTI-ELEMENT array, not a scalar

`Get-NetAdapterAdvancedProperty -RegistryKeyword '*EEE'` on stream's Realtek returns
`RegistryValue` as a `[string[]]` like `@('0','0')` — `[string]$prop.RegistryValue` renders `"0 0"`,
which fails a scalar `-eq "0"` compare (false DRIFT). Normalize to the first element with
`[string]@($prop.RegistryValue)[0]` (`@(...)` forces array context so a scalar stays whole; a bare
`[0]` on a scalar string returns its first CHARACTER). strih's Marvell returns a plain scalar for the
same keyword, so this only shows up on the Realtek — test any NIC-advanced-property compare on BOTH
drivers, never just one.

## GOTCHA — the design-marker recorder needs GitHub GraphQL (a broad GitHub outage stalls it)

`enable-nic-wol.ps1`/etc. carry no CI compile locally, so the durable design/validated/reviewed
comments (via `gh issue comment`) are the only gate artifacts. Their marker recorder re-reads via
`gh issue view --json comments` (GraphQL). During a GitHub GraphQL incident the POST can succeed while
the recorder's re-read 503s → no marker (and the commit stays blocked). To register a SPECIFIC
still-missing marker, post ONLY that kind's comment (the recorder classifies the FRESHEST authored
comment in the last 180s — a later comment of another kind shadows it). Retry until the marker file
appears, then DELETE the dup comments a flapping outage left behind (`gh api -X DELETE
.../issues/comments/<id>`). git-over-https (push) keeps working even when the api.github.com REST/
GraphQL endpoints flap, so the `refs/autopilot-wip/*` backup push is unaffected.
