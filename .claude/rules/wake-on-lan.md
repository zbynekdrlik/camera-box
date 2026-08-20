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
- `scripts/wake-box.sh --wait[=SECS]` (#1053, 2026-08-20) — the automated "verify availability after
  wake": after the send it polls the target for reachability (exit 0 UP / 4 STILL-DOWN / 2 misuse /
  3 send-error), so recovery is ONE command composable with issue 1001 (detect-down → wake →
  confirm-up). The poll HOST is resolved by the pure `wol_verify_host` in `wol.sh` (box → table ip;
  `--wait-host` override; raw-MAC/unknown → fail-loud, so a verify never polls the wrong/no host).
  Two seams make the I/O deterministically testable with NO real network: the reachability probe is
  `${WOL_PING_CMD:-ping -c1 -W1}` (a test passes `true`/`false` for a fixed UP/DOWN verdict) and the
  poll interval `${WOL_WAIT_INTERVAL:-3}`s (validated positive-int); a test also puts a stub `python3`
  (drains stdin, exit 0) on PATH so the send broadcasts NOTHING. Reuse that stub-python3 + injectable-
  probe pattern for ANY future wake-box.sh I/O test (`tests/harness_wol_verify_1053.rs`).
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

## imag-nb — the LINUX WoL path (#1103), the NM-managed counterpart

imag-nb (10.77.9.182) extends WoL to a Linux box; the mechanism differs from the Windows boxes above.
Reuse this pattern for any other NM-managed Linux target:

- **Persist via NetworkManager, not a systemd/udev mechanism.** `setup-imag.sh` step 1 arms it on the
  SAME `$CON` it already pins the static IP on: `nmcli con mod "$CON" 802-3-ethernet.wake-on-lan magic
  802-3-ethernet.wake-on-lan-password ""`. NM re-applies it on every connection-up (every boot), so
  it survives reboot, and it keeps ONE source of truth (the connection profile) rather than a second
  ethtool-in-a-systemd-unit mechanism. `nmcli con mod` only writes the profile (no hardware
  validation), so it is safe under the script's `set -euo pipefail` — it won't abort a provision on a
  box whose driver lacks WoL; the verify gate catches that instead.
- **Verify SUDO-LESSLY off the persisted NM value, not the runtime ethtool line.** `verify-imag.sh`
  check (x) reads `nmcli -g 802-3-ethernet.wake-on-lan connection show <con>` (== `magic`) — the
  durable source of truth, readable as the plain SSH user. The runtime `ethtool <nic> | grep Wake-on`
  (`g`) needs root, so it is NOT the gate's signal. The pure `imag_wol_enabled_ok` requires EXACTLY
  `magic` (a `magic secureon` = password-protected wake our passwordless `wake-box.sh` cannot trigger,
  so it must FAIL).
- **Resolve the rig NIC by the box's OWN static rig IP, never the default route.** imag-nb is a
  notebook — a Wi-Fi default route could point at a DIFFERENT connection than the one setup armed. The
  (x) check resolves the NIC via `ip -o -4 addr show | awk` matching `$IMAG_IP` (the address the gate
  is SSHed in over), which is unambiguous on a multi-homed box.
- **The sender needs NO code change — it is table-driven.** Adding the `imag-nb 10.77.9.182 <mac>` row
  to `scripts/wol-targets.txt` is all `wake-box.sh imag-nb` needs (the `wol.sh`/`wake-box.sh` core is
  reused, never a second sender). The MAC is the box's NDI-NIC MAC, live-read (`cat
  /sys/class/net/<nic>/address`).
- **USB-dongle S5 caveat (worse than strih's add-in-card caveat).** imag-nb's NDI NIC is a USB r8152
  Realtek dongle. `Supports Wake-on: pumbg` includes `g` (magic), but a USB host controller usually
  loses power in S5, so the dongle typically CANNOT wake the box from a full shutdown unless the BIOS
  keeps USB powered in standby (ErP/EuP/Deep-Sleep disabled + a "USB power in S3/S4/S5" option). That
  BIOS layer is the owner's hands-on step — the OS half (NM `magic`) is all the repo can provision.
