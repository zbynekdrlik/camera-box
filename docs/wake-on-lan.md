# Wake-on-LAN remote recovery — strih + stream (#1053)

Remote-recovery counterpart to issue 1001's network-outage **detection** watchdog: 1001 ALERTS on an
outage; this makes the outage remotely **recoverable** — if a broadcast OBS box has gone to sleep or
been powered off, a Wake-on-LAN magic packet from dev1 wakes it without hands on the box.

## What was found (STEP-0 live probe, 2026-08-17)

The Windows NIC WoL is **already fully enabled and armed on BOTH boxes** — the remotely-doable half of
this ticket was effectively already satisfied. Verified live with `scripts/enable-nic-wol.ps1
-VerifyOnly` (both returned `VERIFY OK`):

| Box | NIC | MAC | `WakeOnMagicPacket` | `wake_armed` | Notes |
|-----|-----|-----|---------------------|--------------|-------|
| strih | Marvell AQtion Felicity 10GbE (add-in) | `5C:6A:80:F6:6C:F7` | Enabled | yes | `WakeFromPowerOff=1`; only `S3` sleep available, hibernation off |
| stream | Realtek PCIe GbE (onboard) | `E8:9C:25:CE:B6:EA` | Enabled | yes | EEE/Green off; only `S3` sleep available, hibernation off |

The only non-desired NIC state is `*WakeOnPattern=1` on both (the "only allow a magic packet"
hardening — does NOT block a magic-packet wake; apply it with the helper below, elevated).

**So why did the 2026-08-13 magic packet not wake strih?** Not the Windows NIC layer (already enabled).
The remaining gap is the **BIOS/firmware standby-power** layer (see checklist below) — or the box was
powered-on-but-network-isolated during the outage, which WoL cannot fix (that is 1001's domain).

## Tools

### `scripts/wake-box.sh` — send the magic packet (run from dev1)

```bash
scripts/wake-box.sh strih            # wake strih (MAC + subnet broadcast from wol-targets.txt)
scripts/wake-box.sh stream           # wake stream
scripts/wake-box.sh strih --dry-run  # show the packet + targets, send nothing
scripts/wake-box.sh 5C:6A:80:F6:6C:F7 --broadcast 10.77.9.255   # raw MAC form
```

Sends the 102-byte magic packet (6×0xFF + 16×MAC) to the box's subnet-directed broadcast
(`10.77.9.255`) AND the limited broadcast (`255.255.255.255`) on UDP ports 9 and 7 — exactly the
delivery shape the 2026-08-13 cam1 attempt used. Targets come from `scripts/wol-targets.txt`.

dev1 is on the same `10.77.9.0/24` as the rig, so the subnet broadcast reaches the boxes at L2. If
ever run from OFF the rig segment, `scp` this dir to an on-segment box (a cam box) and send from
there — a routed directed-broadcast is commonly dropped by switches.

#### Confirm the box came back — `--wait` (the automated "verify availability after wake")

`--wait[=SECS]` polls the target for reachability AFTER the send, so a recovery is one command
instead of "send, then manually ping":

```bash
scripts/wake-box.sh strih --wait          # wake, then poll strih's ip for up to 120s (default)
scripts/wake-box.sh imag-nb --wait=180    # give a slow box a longer budget
scripts/wake-box.sh strih --dry-run --wait # show the verify plan (host + budget), send/poll nothing
scripts/wake-box.sh 5C:6A:80:F6:6C:F7 --wait --wait-host 10.77.9.202   # raw-MAC target: poll host is explicit
```

It exits **0** and prints `WAKE-VERIFY UP: <box> (<ip>) reachable after ~Ns` once the box answers,
or exits **4** with `WAKE-VERIFY STILL-DOWN: … not reachable after <SECS>s` if the budget elapses
(distinct from exit **2** for a misuse, so a recovery loop can tell "sent but never came up" from
"bad args"). The poll host is the box's `wol-targets.txt` ip; for a raw-MAC target (which carries no
ip) pass `--wait-host <ip>`. The reachability probe is `${WOL_PING_CMD:-ping -c1 -W1}` (override it
with e.g. a TCP-port check) and the interval `${WOL_WAIT_INTERVAL:-3}`s. This pairs with issue
1001's outage detection: detect-down → `wake-box.sh <box> --wait` → confirm-up.

### `scripts/enable-nic-wol.ps1` — enable + verify the Windows NIC WoL (runs on the box)

Idempotent, fail-loud, three modes: default = apply (needs an **elevated** session), `-VerifyOnly`
(assert desired state, no change), `-DryRun` (report-only). It is a session-agnostic NIC/registry
operation, so it runs over the sanctioned ssh transport **or** the win-* MCP Shell.

```bash
# from dev1 over ssh (verify — read-only, no admin needed):
source scripts/lib/win-ssh-exec.sh
win_ssh_upload newlevel newlevel 10.77.9.202 scripts/enable-nic-wol.ps1 'C:\Windows\Temp\enable-nic-wol.ps1'
win_ssh_run    newlevel newlevel 10.77.9.202 '& powershell -NoProfile -ExecutionPolicy Bypass -File C:\Windows\Temp\enable-nic-wol.ps1 -VerifyOnly'
# to APPLY (sets *WakeOnPattern=0 hardening etc.) an elevated session is required — run it from the
# box's own elevated PowerShell, or via a mechanism that elevates; -VerifyOnly / -DryRun do not.
```

## BIOS checklist (hands-on, per box — the remaining gap)

WoL from a full shutdown (S5) needs the firmware to keep the NIC powered in standby. On the box's
BIOS/UEFI setup screen, per box:

**Both boxes**
- [ ] Enable **Wake on LAN** / **Power On by PCI-E / PCIe Device** / **Resume by PCI-E** (name varies).
- [ ] Set **ErP Ready** / **EuP** / **Deep Sleep** to **Disabled** (ErP/deep-S5 cuts standby power to
      the NIC and BREAKS WoL from a full shutdown). If a granular option exists, keep LAN powered.
- [ ] Confirm the onboard/NIC power is not gated by an "Energy Saving" master switch.

**strih (Marvell AQtion 10GbE — an ADD-IN PCIe card)**
- [ ] Confirm the motherboard supplies **standby (5VSB) power to the PCIe slot** in S4/S5. Many boards
      only keep the ONBOARD LAN powered in S5, not add-in slots — if so, the AQtion card **cannot**
      wake strih from a full shutdown regardless of BIOS. Fallbacks in that case:
      - Wake from **S3 sleep** instead of shutdown (S3 is available on strih), OR
      - Enable WoL on the board's **onboard** NIC and add its MAC to `wol-targets.txt`, OR
      - Use an out-of-band path (network PDU / IPMI) for cold power-on.

**stream (Realtek onboard GbE)** — onboard NICs typically wake from S5 once the BIOS options above are set.

## Verify after the BIOS visit

1. `scripts/enable-nic-wol.ps1 -VerifyOnly` on the box → `VERIFY OK` (NIC still armed).
2. Put the box to **sleep** (S3): from dev1 `scripts/wake-box.sh <box>` → box powers on. Record it.
3. Fully **shut down** the box: from dev1 `scripts/wake-box.sh <box>` → box powers on. Record it.
4. Note which power states successfully wake per box (esp. strih S5 vs S3, per the add-in caveat).

---

## imag-nb — Linux WoL (#1103)

`imag-nb` (10.77.9.182) is the imag projection notebook and the imag counterpart of the strih/stream
WoL above. It is normally an always-on box; WoL only matters for the post-event window when it is
powered down or taken away (the issue-1013 scenario). Because it is Linux, the OS half differs from
the Windows boxes — it is provisioned, not a hands-on NIC edit.

### What is provisioned (the OS half — done)

`scripts/setup-imag.sh` **step 1** arms Wake-on-LAN durably via NetworkManager on the same connection
it pins the static IP on:

```bash
nmcli con mod "$CON" 802-3-ethernet.wake-on-lan magic 802-3-ethernet.wake-on-lan-password ""
```

NM re-applies this on every connection-up (every boot), so it survives reboot. `scripts/verify-imag.sh`
check **(x)** asserts it, reading the persisted value sudo-lessly:

```bash
nmcli -g 802-3-ethernet.wake-on-lan connection show imag-lan   # => magic
```

(`imag-lan` is the default NM connection id set by `install-imag-nb.sh`; if the box's active
connection is named otherwise, substitute that name — or read it dynamically:
`nmcli -t -f NAME,DEVICE con show --active`. `verify-imag.sh` check (x) already resolves it
dynamically by the box's static rig IP, so its assertion never depends on the connection id.)

Live-confirmed 2026-08-18: the NDI NIC is a USB **r8152 Realtek** dongle (`enx6c1ff766154b`, MAC
`6C:1F:F7:66:15:4B`), `Supports Wake-on: pumbg`, and after `nmcli con up` the runtime `ethtool
<nic>` shows `Wake-on: g`.

### Send the magic packet (from dev1)

```bash
scripts/wake-box.sh imag-nb            # MAC + subnet broadcast from wol-targets.txt
scripts/wake-box.sh imag-nb --dry-run  # show the packet + targets, send nothing
```

`wake-box.sh` is table-driven — the imag-nb row in `scripts/wol-targets.txt` is all that was needed;
there is no second sender.

### BIOS / standby-power — the remaining hands-on step (per box, owner)

WoL from a full shutdown (S5) needs the firmware to keep the NIC powered in standby. imag-nb's NIC is
a **USB** adapter, so this is even more restrictive than strih's add-in-card caveat: a USB host
controller usually loses power in S5, so a USB-ethernet dongle typically **cannot** wake the box from
a full shutdown unless the BIOS explicitly keeps USB powered in standby. On imag-nb's BIOS/UEFI:

- [ ] Enable **Wake on LAN** / **Power On by PCI-E/USB** / **Resume by USB device** (name varies).
- [ ] Set **ErP Ready** / **EuP** / **Deep Sleep** to **Disabled** (ErP/deep-S5 cuts standby power to
      USB and the NIC and breaks WoL from a full shutdown).
- [ ] Enable **USB power in S3/S4/S5** ("USB Standby Power" / "Wake from USB" / "ErP-exempt USB"), so
      the USB-ethernet dongle stays powered enough to receive the magic packet. Without this, the
      only viable wake is from **S3 sleep** (if the box keeps USB powered in S3), not a full shutdown.

### Verify after the BIOS visit

1. `nmcli -g 802-3-ethernet.wake-on-lan connection show imag-lan` on the box → `magic` (still armed);
   `sudo ethtool <nic> | grep Wake-on` → `g`.
2. Put the box to **sleep** (S3): from dev1 `scripts/wake-box.sh imag-nb` → box powers on. Record it.
3. Fully **shut down** the box: from dev1 `scripts/wake-box.sh imag-nb` → box powers on. Record it.
4. Note which power states successfully wake it (esp. S5 vs S3, per the USB-dongle caveat above). If
   S5 never wakes despite the BIOS options, fall back to waking from S3 sleep, or use an out-of-band
   cold power-on path (network PDU).
