---
name: provision
description: >
  New-cam-box provisioning runbook (#448-#454) — the canonical build-USB → boot → setup-device.sh
  → verify-device.sh flow, plus the install gotchas found bringing up cam5/cam6/cam7. Load before
  building/flashing a new camera box, running setup-device.sh, or touching scripts/verify-device.sh.
---

# New-cam-box provisioning runbook (#448-#454)

There is no more tribal knowledge here — this is the ONE canonical way to bring a fresh camera
box (CAM5, CAM6, CAM7, or a re-flash of an existing one) to full fleet parity. The method was
unified across #448 (create-usb-linux.sh), #449 (dead builders removed), #450 (setup-device.sh
name-resolved), #451 (camera-set.sh cam1-7), #452 (upgrade-fleet canary), #453 (cam3 convergence)
and this ticket, #454 (the runbook + the acceptance gate below). See also `.claude/skills/ops`
for the DanteSync clock, realtime CPU isolation, and rig-recovery background this runbook builds
on top of.

## The 4-phase flow

```
1. BUILD USB     scripts/create-usb-linux.sh --target-disk /dev/sdX --yes
2. BOOT + REACH  power on the box, wait for it to come up, confirm :22 is reachable
3. PROVISION     sudo ./setup-device.sh NAME          (on the box itself, over SSH)
4. ACCEPT        scripts/verify-device.sh NAME         (from dev1, over SSH — the gate)
```

Every step is idempotent-ish and fails LOUD (script-failure-policy) — a half-configured box never
silently reports success. If any step fails, fix the root cause and re-run that step; do not
hand-patch the live box (hand patches are exactly how cam3 became the fleet's outlier — see
"Known gotchas" below).

### Phase 1 — build the USB installer

```bash
# From a Linux box with the target USB stick attached (or run FROM the box's own live-USB,
# targeting its internal disk):
sudo scripts/create-usb-linux.sh /dev/sdX                       # a separate USB stick
sudo scripts/create-usb-linux.sh --target-disk /dev/sda --yes   # ON the box's own live-USB,
                                                                 # non-interactive
```

- `--target-disk /dev/sdX` is REQUIRED (not just positional) to target `/dev/sda` — the script
  refuses a bare positional `/dev/sda` as a safety guard against nuking the wrong disk; pass
  `--target-disk` explicitly when you KNOW `/dev/sda` is the box's own internal disk (the normal
  case when running the installer from the box's own live-USB session).
- `--yes` / `-y` skips the interactive "type yes" confirmation — needed for a scripted/remote
  session with no TTY prompt round-trip.
- The script is a ONE-SHOT installer (#448): a fresh box installs → boots → is SSH-reachable with
  NO manual post-install patching. It debootstraps Ubuntu 24.04 (noble), user `newlevel` / root
  SSH enabled, and bakes in the boot-critical fixes below (all of which bit the fleet live).
- `scripts/create-ubuntu-vm.sh` / `write-image.sh` / `create-image.sh` are GONE (#449) —
  `create-usb-linux.sh` is the SOLE canonical live-install builder. `scripts/build-image.sh` (the
  ro-root+overlay builder) is a deliberately separate, still-live path — do not confuse the two or
  "helpfully" delete it; its fate is a queued user decision on #449, not this runbook's concern.

### Phase 2 — boot + reach

Power on the box. Wait for it to reach multi-user + start sshd. **Do not assume the box keeps the
live-USB's IP or immediately gets its future static IP** — a freshly-installed OS often gets its
OWN fresh DHCP lease on first boot, distinct from both the live-USB's lease and the eventual
static address `setup-device.sh` will assign (the cam6 bring-up: installed OS got `10.77.9.166`,
a lease never seen before, while everyone kept polling the stale `.181`/`.66`). Scan for the new
lease (router DHCP table, or `journalctl --directory=<mounted-disk>/var/log/journal -b 0 | grep
DHCPv4` from the live-USB before the first real boot) rather than guessing.

```bash
ssh root@<discovered-ip>   # password: see local memory / DEVICE_ROOT_PW, never committed
```

If the box appears dark (no ping, no SSH): see "Known gotchas" below before assuming hardware
failure — most "won't boot" incidents on this fleet were a masked/blocked boot stage, not a dead
box.

### Phase 3 — provision (`setup-device.sh`)

```bash
# ON the box itself, as root:
sudo ./setup-device.sh CAM5        # case-insensitive: cam5 / Cam5 / CAM5 all resolve identically
```

**Single positional argument — NAME only** (#450 rework). `setup-device.sh` sources
`scripts/camera-set.sh` and resolves `NAME -> DEVICE_IP / VBAN_STREAM / CAMERA_GENLOCK_FPS` itself
— there is no more free-text 3-positional-arg form (`setup-device.sh CAM5 10.77.9.65 cam5` is
GONE). An unknown name fails loud through `camera-set.sh`'s own fail-closed `case` — it will never
silently provision the wrong box.

`--binary <url|path>` optionally pins a specific camera-box build; by default the script fetches
the latest successful CI dev-build artifact for the fleet's dev branch (#457) — never a GitHub
release (the fleet runs CI dev-builds, e.g. `1.7.0-dev.157`, not tagged releases).

The script performs 19 steps (hostname, static IP, binary install, NDI library, ALSA, camera-box
systemd unit + `cpu-affinity.conf`/`genlock.conf` drop-ins, auto-login, capabilities, GRUB
hardening, network-wait timeout, power-button remap, sleep/power-saving disable, network tuning,
service pruning, package install, dantesync install, read-only root, final verification) and
FAILS LOUD (via its own `fail()` helper) at the first failure — it never prints "Setup Complete!"
on a half-configured box (STEP 19's belt-and-braces check).

After it finishes: `netplan apply` (if not already applied) then `reboot` — the box comes back up
on its final static IP with every drop-in and service enabled.

### Phase 4 — accept (`verify-device.sh`, #454)

**This is the NEW acceptance gate this ticket adds.** Run it from dev1 (or anywhere with SSH
reach + `sshpass`) AFTER the box has rebooted post-provisioning:

```bash
scripts/verify-device.sh CAM5
```

`verify-device.sh` is a RUNTIME check — distinct from `setup-device.sh` STEP 19's INSTALL-TIME
(pre-reboot, still-inside-the-live-session) file-presence check. It connects fresh over SSH,
AFTER reboot, and re-derives every fact from live signals rather than trusting the installer's own
claim of success — the honest proof a box is "identically built" to the rest of the fleet.

It checks (all must pass, exit 0 only if every check is OK):

| # | Check | How |
|---|---|---|
| (a) | `camera-box --version` is a valid, well-formed fleet version | regex on the binary's own `--version` output |
| (b) | `camera-box.service` is active | `systemctl is-active camera-box` |
| (c) | NDI sender is actually streaming, no crash | `journalctl -u camera-box`, reuses `scripts/lib/ndi-alive.sh`'s `emit_ok_grep_pattern()` / `fatal_grep_pattern()` (shared with `deploy-fleet.sh` / `upgrade-fleet-ndi.sh`) |
| (d) | dantesync PTP servo LOCKED + clock offset within bound | `journalctl -u dantesync`, reuses `scripts/clock-offset-guard.sh`'s `ptp_locked_from_journal()` / `offset_us_from_journal()` / `offset_check()` (#8) |
| (e) | `genlock.conf` drop-in present, `CAMERA_BOX_GENLOCK_FPS` matches `camera-set.sh`'s per-cam table | `cat` the drop-in, compare against `camera_resolve()`'s `CAMERA_GENLOCK_FPS` |
| (f) | `cpu-affinity.conf` drop-in present (`CPUAffinity=<isolated core>`) | `cat` the drop-in (#289) |
| (g) | `/usr/lib/ndi/libndi.so.6` is a root-owned SYMLINK to a root-owned regular file | `ls -la /usr/lib/ndi` — the CANONICAL layout; the #445 cam3-outlier real-file/user-owned layout deliberately FAILS this check |
| (h) | avahi mDNS sees this box's NDI source | `avahi-browse -tp _ndi._tcp` |
| (i) | capture-chroma metric reports "colour", not "grayscale" | `journalctl -u camera-box`, the #299 regression signal (`src/main.rs`'s periodic `capture chroma: ... -> colour\|grayscale` line) |
| (j) | root filesystem mounted **read-only** | `findmnt -no OPTIONS /` — the ro appliance (#547); a `rw` box FAILs |
| (k) | exactly **ONE** installed kernel, equal to the running one | `ls -1 /boot/vmlinuz-*` vs `uname -r`; two kernels (the cam4 drift) or a mismatch FAILs (#547). Optional exact fleet pin via `KERNEL_PIN=6.8.0-134-generic` |
| (l) | **fwupd purged** | `systemctl is-enabled fwupd` must be gone/not-found (#547 — fwupd holds a write handle that blocks the `ro` remount) |
| (m) | `systemd-networkd-wait-online` **masked** | `systemctl is-enabled …` == `masked` (#547 — unmasked it stalled boot ~120s) |
| (n) | core-isolation kernel cmdline | `/proc/cmdline` carries `isolcpus=3` (#289) + `nohz_full=3` + `rcu_nocbs=3` + `irqaffinity=0-2` (#303), each a whole token |
| (o) | NDI runtime pinned to the fleet version | version of the `libndi.so.6` symlink target; `NDI_VERSION_PIN` (default `6.3.2`, #132/#547) |

Every check is a hard FAIL on an unreachable/unreadable signal too (test-strictness — no silent
pass on "couldn't tell"). `verify-device.sh`'s pure decision functions are unit-tested offline in
`tests/verify_device_pure_functions.rs` (source the script, call the functions directly — same
convention as `tests/setup_device_pure_functions.rs` / `tests/clock_offset_guard.rs`); the live
SSH flow itself can only be proven against a real box (the supervisor runs it against a live
camera as the #454 acceptance proof).

**A `#445`-class outlier (cam3, or any future manual-patch box) will legitimately FAIL check (g)**
— that is by design: the gate certifies the CANONICAL build produced by phases 1-3 above, not a
hand-patched box. Converging an outlier onto the canonical layout is separate work (#453), not
something `verify-device.sh` should be loosened to tolerate.

## Known gotchas (found bringing up cam5/cam6/cam7 — all fixed IN THE SCRIPTS, never hand-patched)

- **Boot hangs with no console/SSH** was `systemd-networkd-wait-online.service` blocking
  `network-online.target` on a base image with no bound timeout — `setup-device.sh` STEP 11 now
  **masks** the unit outright (#547; the box has a static IP, so camera-box never needs to wait for
  "online"). Unmasked it stalled boot ~120s → camera-box started ~123s late (observed on cam3). If
  a fresh box pings but `:22` never comes up, this is the first thing to suspect, not dead hardware.
- **`ro` remount fails with EBUSY, box stuck `rw` (#547)** — `fwupd` holds an open write handle on
  `/var/lib/fwupd/pending.db`, so `mount -o remount,ro /` fails and the box never becomes a proper
  ro appliance (hit on cam1/cam4). The appliance never firmware-updates itself → `setup-device.sh`
  STEP 15 **purges fwupd**. On an already-live box: `apt-get purge -y fwupd fwupd-signed` (or
  `dpkg --purge --force-depends fwupd fwupd-signed` if apt is wedged), then remount ro (or reboot,
  which restores ro from fstab).
- **Kernel update on the ro appliance fails with "No space left on device" (#547)** — `mkinitramfs`
  builds the initramfs in `/var/tmp`, a **50M tmpfs**, far too small for a ~400M initramfs → no
  initrd → a half-installed, unbootable kernel (the `#295` brick-guard aborts the reboot, correctly,
  but the install is stuck). The `#295` `zz-camera-box-initrd-guarantee` postinst hook now builds
  with `TMPDIR=/root/.itmp` (the real ~51G disk) so the guarantee actually works on the ro box. To
  update a kernel manually: `mount -o remount,rw /` → `TMPDIR=/root/.itmp dpkg --configure -a` (or
  the install) → confirm `/boot/initrd.img-<ver>` exists → `mount -o remount,ro /` / reboot. NEVER
  run `update-grub` without confirming every `/boot/vmlinuz-*` has a matching initrd first.
- **No `curl` on the base image** silently failed the STEP 3 binary download and STEP 17 dantesync
  download. `create-usb-linux.sh` now ships `curl`; `setup-device.sh` also has a pre-flight
  curl-install step before either download runs.
- **`create-usb-linux.sh` refuses `/dev/sda` unless `--target-disk` is passed explicitly** — see
  Phase 1 above. This is a deliberate safety guard, not a bug.
- **`grub-install --removable` writes no NAMED NVRAM boot entry** — firmware may keep booting a
  stale Windows EFI entry, or the live-USB itself, ahead of the fresh install. Use
  `efibootmgr -n <entry>` (BootNext, one-shot) + a hard `sysrq` reboot to force the install to
  boot once, or physically remove the live-USB.
- **A live-USB session may have NO sftp subsystem** (`scp` fails: "subsystem request failed") —
  use an ssh-pipe instead: `cat file | ssh host 'cat > /path'`. Installed (post-reboot) boxes DO
  support `scp -O` (legacy) normally.
- **`HandlePowerKey` defaults to `poweroff` until `setup-device.sh` STEP 12 runs** — a box left
  running before provisioning can be powered off by an accidental power-button press, looking like
  a dead box. Run `setup-device.sh` promptly after first boot.
- **Journal reads as EMPTY over SSH as `newlevel` on a read-only-root / volatile-journal box**
  (`journalctl -u camera-box` returns nothing, "insufficient permissions") — this is a READ
  artifact (no `/var/log/journal`, `newlevel` not in `systemd-journal`), NOT proof the service is
  silent. Read as root (`sudo journalctl ...`), or check sockets (VBAN udp/6980 bound = intercom
  up; NDI :596x bound = sender up). `verify-device.sh` runs its journal reads over the `root@`
  SSH session specifically to avoid this trap.
- **`/usr/lib/ndi` runtime deps (#362)** — a fresh box crash-loops camera-box on `dlopen` failure
  unless `libasound2t64`, `/etc/ld.so.conf.d/ndi.conf` (+`ldconfig`), `libavahi-client3`,
  `libavahi-common3`, and a running `avahi-daemon` are all present. `create-usb-linux.sh` +
  `setup-device.sh` bake all four in; `tests/appliance_boot_hardening.rs` pins it.
- **Auto-grow-root (#369)** — the root partition is NOT expanded to fill the physical disk by
  default; `systemd/camera-box-grow-root.service` (run-once first-boot) handles it. See
  `.claude/skills/ops` "#369 — auto-grow root partition" for the fault-tolerant `growpart` details
  (root is not always the last partition, depending on which builder made the image).
- **`#295` boot-hardening (kernel pinning + guaranteed initrd)** is baked into BOTH
  `setup.sh`/`setup-device.sh` — never hand-edit `/etc/default/grub` + `update-grub` on a live box;
  that is exactly what bricked CAM3/CAM4 historically. If GRUB needs a change, change it in the
  provisioning script so every future box gets it too.

## The fleet map (`scripts/camera-set.sh`, #24/#451)

Single source of truth for NAME → IP / NDI source name / genlock FPS, used by `setup-device.sh`,
`verify-device.sh`, `deploy-fleet.sh`, `upgrade-fleet-ndi.sh`, and every E2E orchestrator:

```
cam1 -> 10.77.9.61 / "CAM1 (usb)"     cam5 -> 10.77.9.65 / "CAM5 (usb)"
cam2 -> 10.77.9.62 / "CAM2 (usb)"     cam6 -> 10.77.9.66 / "CAM6 (usb)"
cam3 -> 10.77.9.63 / "CAM3 (usb)"     cam7 -> 10.77.9.67 / "CAM7 (usb)"
cam4 -> 10.77.9.64 / "CAM4 (usb)"
```

All seven emit genlock at 60fps today (`CAMERA_GENLOCK_FPS`, per-cam table in `camera_resolve()`).
Adding an 8th camera means editing `camera-set.sh` ONCE — every script downstream (including
`verify-device.sh`) picks it up automatically.

## After acceptance

Once `verify-device.sh NAME` reports `ALL CLEAR`, the box is fleet-identical and ready to plug
into the rig (capture card + HDMI/USB source) and add to the relevant OBS box's NDI source list.
That step is manual (scene/source wiring) and out of scope for this runbook — see
`.claude/skills/genlock` / `.claude/skills/obs-ops` for the OBS side.
