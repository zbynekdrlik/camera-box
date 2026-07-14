---
name: provision
description: >
  New-cam-box provisioning runbook (#448-#454) — the canonical build-USB → boot → setup-device.sh
  → verify-device.sh flow, plus the install gotchas found bringing up cam5/cam6. Load before
  building/flashing a new camera box, running setup-device.sh, or touching scripts/verify-device.sh.
---

# New-cam-box provisioning runbook (#448-#454)

There is no more tribal knowledge here — this is the ONE canonical way to bring a fresh camera
box (CAM5, CAM6, or a re-flash of an existing one) to full fleet parity. The method was
unified across #448 (create-usb-linux.sh), #449 (dead builders removed), #450 (setup-device.sh
name-resolved), #451 (camera-set.sh cam1-6), #452 (upgrade-fleet canary), #453 (cam3 convergence)
and this ticket, #454 (the runbook + the acceptance gate below). See also `.claude/skills/ops`
for the DanteSync clock, realtime CPU isolation, and rig-recovery background this runbook builds
on top of.

**cam7 does NOT exist (#593)** — the user only expressed FUTURE interest in a 7th camera; no
box has ever been built or connected. It is deliberately absent from `camera-set.sh`'s active
fleet map below; add it (one line) when a real cam7 box exists.

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
| (d) | dantesync PTP servo LOCKED + a **FRESH** clock offset within bound | `journalctl -u dantesync -o short-iso`; `dantesync_offset_verdict()` reads the FRESHEST `[NTP] offset:` line, rejects a stale boot-step line (`DANTESYNC_OFFSET_FRESHNESS_S` default 300s), FAILs on a fresh offset outside ±`CLOCK_GUARD_BOUND_US` (2000µs). A fresh out-of-bound offset = a real desync (#550/#591); stale/absent + PTP LOCKED passes (gate (r) guarantees sole authority). Supersedes the age-blind `dantesync_offset_ok` |
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
| (p) | `config.toml`'s `[display]` section matches `camera-set.sh`'s `CAMERA_DISPLAY_SOURCE` table entry, PLUS the `ExecStart` `--display` flag matches `CAMERA_DISPLAY_EXECSTART_SOURCE` | `cat /etc/camera-box/config.toml` over SSH + `systemctl show -p ExecStart --value camera-box` over SSH, each compared against its own per-cam table (#528/#557/#558/#562) — catches a box that lost either mechanism's HDMI-preview config, or wrongly gained one |
| (q) | no stale `.bak`/`.bak-*` cruft under `/usr/lib/ndi` or the systemd drop-in dir | `ls` both dirs over SSH — **WARNING only, never a FAIL** (#453). Inert leftovers (ldconfig/systemd never load a `.bak`) are surfaced as drift but don't fail an otherwise-healthy box; `setup-device.sh`'s `cleanup_bak_cruft` self-heals them on the next provisioning pass |
| (r) | dantesync is the **SOLE** timesync authority — NO competing daemon | one SSH call gathers, per `systemd-timesyncd`/`chrony`/`ntp`/`ntpsec`/`openntpd`, its `dpkg -s` install state + `systemctl is-active` + `is-enabled` into a `NAME\|DPKG\|ACTIVE\|ENABLED` block; `timesync_authority_verdict()` HARD-FAILs any that is INSTALLED (even masked) / ACTIVE / enabled (#591 — cam5/6 ran `systemd-timesyncd` alongside dantesync → a real 5.28s desync). A minimalist appliance PURGES them (`setup-device.sh` STEP 17 + `create-usb-linux.sh` chroot); masking is only a backstop |

Every check **except (q)** is a hard FAIL on an unreachable/unreadable signal too (test-strictness —
no silent pass on "couldn't tell"); **(q) is the sole WARNING-only check** — inert `.bak` cruft is
drift to surface, not a functional defect, so it never fails the gate.
Both `setup-device.sh` and `verify-device.sh` now source `scripts/lib/cli-log.sh` for their
color/log helpers (#568) — do NOT re-add a local `RED=…`/`log()` block; extend the shared lib. `verify-device.sh`'s pure decision functions are unit-tested offline in
`tests/verify_device_pure_functions.rs` (source the script, call the functions directly — same
convention as `tests/setup_device_pure_functions.rs` / `tests/clock_offset_guard.rs`); the live
SSH flow itself can only be proven against a real box (the supervisor runs it against a live
camera as the #454 acceptance proof).

**A `#445`-class outlier (cam3, or any future manual-patch box) will legitimately FAIL check (g)**
— that is by design: the gate certifies the CANONICAL build produced by phases 1-3 above, not a
hand-patched box. Converging an outlier onto the canonical layout is separate work (#453), not
something `verify-device.sh` should be loosened to tolerate.

## Known gotchas (found bringing up cam5/cam6 — all fixed IN THE SCRIPTS, never hand-patched)

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
- **Purging an OLD kernel goes apt-BROKEN via the held metapackages (#547, distinct from the
  PREVENTION note above)** — `apt-mark hold linux-image-generic linux-headers-generic
  linux-generic` (see "#295 brick-proofing" below) is applied BEFORE an upgrade to stop apt from
  auto-installing a newer kernel. But once the fleet has TWO installed kernels (check (k) failed)
  and you go to CLEAN UP the old one, `apt-get purge <old-kernel-pkgs>` fails: the three HELD
  metapackages still depend on the kernel you're removing, so apt refuses and leaves itself
  BROKEN (blocks ALL later `apt-get`/`apt` operations on the box, not just the kernel purge — this
  blocked a live `avahi-utils` install on cam1/2/4 mid-session). Fix, in order:
  1. `apt-mark unhold linux-image-generic linux-headers-generic linux-generic`
  2. `apt-get purge -y --allow-change-held-packages linux-image-generic linux-headers-generic
     linux-generic` — this ONLY removes the 3 metapackages (confirmed with `-s`/`--simulate`
     first), never the running kernel itself.
  3. `apt-get purge -y <old-kernel-image-and-headers-pkgs>` — now succeeds.
  4. `apt-mark hold linux-image-<pinned>-generic linux-headers-<pinned>-generic` — re-pin the
     CURRENT (surviving) kernel so a future `apt-get upgrade` can't silently pull in a new one
     again. Never leave the box unheld after cleanup.
- **A DIFFERENT apt-BROKEN signature on cam3 (#743/#750, 2026-07-14) — `linux-image-generic`
  depends on a kernel version that was never installed, blocking ANY `apt-get install` fleet-wide,
  not just kernel ops:**
  ```
  E: Unmet dependencies. Try 'apt --fix-broken install' with no packages (or specify a solution).
   linux-image-generic : Depends: linux-image-6.8.0-124-generic but it is not going to be installed
  ```
  Distinct from the held-metapackage-on-purge gotcha above (that one is triggered by REMOVING an
  old kernel; this one blocks EVERY normal install, unprovoked). **Do NOT run
  `apt --fix-broken install` blind** on a #295/#547-hardened appliance — it may try to install/
  remove a kernel under the brick-hardening guards. For installing ONE unrelated package (the
  live case: `psmisc`) while the real fix is pending, bypass the full dependency-graph resolver
  entirely: `cd /tmp && apt-get download <pkg> && dpkg -i <pkg>*.deb` — downloads + installs just
  that .deb without touching the broken kernel chain. Root cause not yet diagnosed (tracked #750)
  — investigate `dpkg -l | grep linux-image`, `apt-mark showhold`, `uname -r` before attempting a
  real fix.
- **`setup-device.sh` re-run against an already-booted ro appliance now self-remounts (#599)** —
  STEP 15-18 (fwupd purge, package install, timesync/linuxptp purge, fstab rewrite) all need a
  writable root; `ensure_root_writable()`/`restore_root_mode()` detect a ro root, remount rw for
  the whole STEP 15-18 window, then remount back to ro, stopping/masking `packagekit` +
  `unattended-upgrades` around the cycle (the same PackageKit-EBUSY blocker as the fwupd gotcha
  above). Both fail loud on a remount failure. A manual `mount -o remount,rw /` before re-running
  the script is no longer required for THIS purpose — it self-heals. Verified live on CAM1
  (booted `ro,relatime`): sourcing the fixed script (no positional arg -> the `BASH_SOURCE` guard
  skips the destructive flow) and calling `ensure_root_writable` / `restore_root_mode` directly
  flipped `ro -> rw -> ro` correctly with `camera-box.service` undisturbed throughout.
- **Verifying a setup-device.sh fix live WITHOUT running the destructive flow** — fetch the raw
  script + `scripts/lib/cli-log.sh` + `scripts/camera-set.sh` from
  `raw.githubusercontent.com/zbynekdrlik/camera-box/main/...` onto the box (same relative layout
  under one tmp dir, e.g. `/tmp/setup-device.sh` + `/tmp/lib/cli-log.sh` + `/tmp/camera-set.sh` --
  `HERE` resolves from `BASH_SOURCE`), then `. /tmp/setup-device.sh` (no positional arg -> the
  source-guard returns before the destructive provisioning flow) and call the specific pure/helper
  function(s) directly. Proves the REAL shipped function against REAL hardware state (findmnt,
  systemctl, mount) without re-provisioning the box. Clean up the fetched temp files afterward.
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
- **`#295` boot-hardening (kernel pinning + guaranteed initrd)** is baked into
  `setup-device.sh` (the sole provisioning script since `scripts/setup.sh` was retired in #563) —
  never hand-edit `/etc/default/grub` + `update-grub` on a live box; that is exactly what bricked
  CAM3/CAM4 historically. If GRUB needs a change, change it in the provisioning script so every
  future box gets it too.

## The fleet map (`scripts/camera-set.sh`, #24/#451)

Single source of truth for NAME → IP / NDI source name / genlock FPS, used by `setup-device.sh`,
`verify-device.sh`, `deploy-fleet.sh`, `upgrade-fleet-ndi.sh`, and every E2E orchestrator:

```
cam1 -> 10.77.9.61 / "CAM1 (usb)"     cam5 -> 10.77.9.65 / "CAM5 (usb)"
cam2 -> 10.77.9.62 / "CAM2 (usb)"     cam6 -> 10.77.9.66 / "CAM6 (usb)"
cam3 -> 10.77.9.63 / "CAM3 (usb)"
cam4 -> 10.77.9.64 / "CAM4 (usb)"

cam7 -> NOT BUILT (#593) — no box exists; add a row here (mirroring the six above) when it does.
```

All six emit genlock at 60fps today (`CAMERA_GENLOCK_FPS`, per-cam table in `camera_resolve()`).
Adding cam7 (or any further camera) means editing `camera-set.sh` ONCE — every script downstream
(including `verify-device.sh`) picks it up automatically.

**Gotcha — removing/adding a fleet camera changes behavior DIFFERENTLY across resolvers (#593),
historically.** `camera_resolve()` here fails LOUD (nonzero exit) on any name not in its
`case` table — `setup-device.sh`'s `resolve_device_name` and `verify-fleet.sh`'s per-box loop both
propagate that as a hard reject/"invalid" verdict, since their whole job IS fleet provisioning/
verification. (Historical note: `scripts/setup.sh` — retired in #563 — used to carry its own
DELIBERATELY lenient `resolve_display_source()`, whose hostname argument wasn't required to be a
fleet camN name at all; that second resolver no longer exists, so this divergence is now moot.)

`camera_resolve()` also carries a per-cam `CAMERA_DISPLAY_SOURCE` table (#528) — the HDMI
cameraman-preview NDI source (empty when a box has no preview configured). `setup-device.sh`
STEP 6 wires it into `config.toml`'s optional `[display]` section (never baked into the
canonical, always-plain `ExecStart`), so a box's preview survives a re-provision instead of
being a manual, non-persistent SSH edit. **cam1 only** resolves to `"STRIH-SNV (interkom)"`
today.

**RESOLVED (#563, 2026-07-09): `scripts/setup.sh` (the separate `curl | sudo bash` quick-install
path) is RETIRED — deleted from the repo, dropped from the release artifact packaging, and
`setup-device.sh` is now the ONE canonical provisioning path.** Historical context, for anyone
reading old issue history: `setup.sh` had joined the `[display]`-section mechanism in #557 (used to
hardcode `--display "STRIH-SNV (interkom)"` into EVERY box's `ExecStart`; since it was a standalone
script with no local repo checkout when curl-piped, it downloaded `scripts/camera-set.sh` at
provision time rather than keeping a second, hand-copied table). Its `config_toml_display_section()`
duplication with `setup-device.sh` was mooted by the #528 design pivot (both copies removed when
the per-box display-config mechanism was dropped in favor of an unconditional default), so nothing
needed to be deduplicated at retirement time — see #563 for the full decision record.

**cam1's own live box still shows check (p) FAIL as of 2026-07-06** (`camera-box --version
1.7.0-dev.258`, predating #528) — it has not yet been RE-PROVISIONED with the new
`setup-device.sh` to pick up the `[display]` section. That is #528's own remaining scope, not a
bug in check (p) — it's the acceptance gate correctly detecting the still-open condition. **cam1
is READ-ONLY (do not re-provision it casually)** — verify a provisioning-script change against a
live box via `verify-device.sh`'s READ-ONLY acceptance check, never by actually re-running
`setup-device.sh` against cam1.

**cam2 is deliberately EXCLUDED from the `CAMERA_DISPLAY_SOURCE` (config.toml) table**, even though
its live box already runs the same interkom preview — cam2's preview is a manual `--display` flag
baked into `ExecStart`, and `scripts/rig-mode.sh`'s TEST/EVENT mode toggle (the QR-painter E2E
harness) specifically flips that flag via a systemd drop-in and verifies restoration by grepping
`ExecStart` for `--display`. `config.toml`'s `[display]` section is read INDEPENDENTLY of any
`ExecStart` flag, so giving cam2 a `CAMERA_DISPLAY_SOURCE` table entry would make a future
re-provision keep the preview active via `config.toml` regardless of `rig-mode.sh`'s drop-in
override — silently breaking its fb0-arbitration checks. Add a `CAMERA_DISPLAY_SOURCE` entry
(never a per-box `setup-device.sh` edit) for any OTHER box that needs a config.toml-mechanism
preview.

**cam2's ExecStart mechanism is now provisioner-persistent via a SEPARATE table (#562,
`CAMERA_DISPLAY_EXECSTART_SOURCE`)** — before #562, cam2's manual `--display` edit was NOT
tracked anywhere in `camera-set.sh`, so re-running `setup-device.sh` against cam2 (STEP 7's
unconditional bare `ExecStart=`) silently erased it (the #379 recurrence risk), and
`verify-device.sh` check (p) — which only ever read `config.toml` — verdicted "ok" regardless
(structurally blind to cam2's real mechanism). The fix keeps `rig-mode.sh` completely UNTOUCHED
(it already keys on `ExecStart`'s `--display` flag however that flag got there) and instead:
`camera_resolve()` gained a mirror-image table, `CAMERA_DISPLAY_EXECSTART_SOURCE` (cam2 =
`"STRIH-SNV (interkom)"`, every other box empty — exactly one of the two tables is non-empty for
any given box); `setup-device.sh` STEP 7 builds its `ExecStart` line via the new
`execstart_display_flag()` pure helper (mirrors `config_toml_display_section()`'s escaping) instead
of a hardcoded literal, so a box with no ExecStart-mechanism entry still renders the exact
pre-#562 canonical plain `ExecStart=/usr/local/bin/camera-box`; `verify-device.sh` gained
`execstart_display_source()` (parses `systemctl show -p ExecStart --value camera-box`, the SAME
command `rig-mode.sh` already uses) and extended check (p) to compare it via the EXISTING, reused
`display_config_verdict()`. **Design decision (mechanism (a), issue #562, issuecomment-4898996033):**
keep cam2 on ExecStart (not migrate it to config.toml, which would have required teaching
`rig-mode.sh` a second mechanism) — the conservative choice that touches zero rig-mode.sh code.

## Keeping the fleet converged — `scripts/verify-fleet.sh` (#552)

`verify-device.sh` accepts (and certifies) ONE box at a time. `verify-fleet.sh` runs it across
the WHOLE fleet in one pass — the fleet-wide drift-guard loop:

```bash
scripts/verify-fleet.sh                 # cam1..cam6 (or camera-set.sh's CAMERA_SET override)
CAMERA_SET="cam1 cam3" scripts/verify-fleet.sh   # a subset
```

Each box in the set is checked for SSH reachability FIRST — an offline box (mid-reboot/deploy)
is reported **SKIPPED**, never a hard FAIL; only a reachable box that fails
`verify-device.sh`'s own acceptance gate counts as a fleet FAIL. An unresolvable camera NAME
(e.g. `cam7` — never built, #593) is a distinct **invalid** verdict, not SKIPPED. Exit
status is nonzero iff at least one reachable box FAILed (a fleet of all-SKIPPED/all-PASS boxes
exits 0). Run it periodically (or after any fleet-wide change) to catch drift before it becomes
a live-event surprise, instead of re-deriving each box's state by hand.

**Test gotcha — a stubbed `sshpass` MUST scan for the `@` token, never index by position.**
`tests/harness_verify_fleet.rs`'s first `sshpass` stub extracted the ssh target as `"${@: -1}"`
(the last positional arg) — but the real invocation's last arg is the trailing remote COMMAND
(`true`), not `user@host` (which sits one arg earlier, after three `-o` options). That stub
therefore never matched any offline IP and silently reported every box reachable regardless of the
test's `offline_ips` list — the two tests meant to pin "offline box is SKIPPED, never FAIL" passed
only VACUOUSLY (satisfied by the always-present "SKIPPED: none" summary label). Caught by a
code-review pass, not by the tests themselves. Fix: scan ALL remaining args for the one containing
`@` (`for arg in "$@"; do case "$arg" in *@*) target="${arg#*@}" ;; esac; done`) instead of
indexing by position — robust to the exact `-o` option count, which any of these scripts could
change. Any future `sshpass`-stubbing test in this repo should use the scan form, and assert on the
EXACT per-name summary line (not a loose `contains("SKIPPED")`) to prove the stub genuinely drove
the offline path.

**Shell-scripting gotchas found fixing #557/#558 (setup.sh + verify-device.sh check (p)) — kept as
historical wisdom even though `scripts/setup.sh` itself was retired in #563; the curl-piped-script
gotcha below still applies to any future curl-piped script in this repo:**

- **A `BASH_SOURCE[0] != $0` source-guard is WRONG for a script whose real invocation is
  `curl | sudo bash -s NAME` (stdin-piped).** `setup-device.sh`'s guard form works because it's
  always invoked as a FILE (`./setup-device.sh` or a downloaded path) — but under `bash -s`,
  `BASH_SOURCE[0]` is EMPTY and `$0` is literally `"bash"`, so a bare `!=` comparison ALSO matches
  and would silently skip `main "$@"` on every real production install. Verified against all
  three invocation modes (piped, direct-file, sourced) before landing #557's guard on `setup.sh`.
  The fix: require `[ -n "${BASH_SOURCE[0]:-}" ] && [ "${BASH_SOURCE[0]}" != "${0}" ]` — non-empty
  AND different, not just different. Any future testable pure-function refactor of a curl-piped
  script in this repo needs this exact form, not a copy-paste of `setup-device.sh`'s guard.
- **A `warn()`/`log()` helper that writes to STDOUT is dangerous to call inside a function whose
  own stdout IS its return-value channel** (any function called as `x="$(some_func ...)"`). Adding
  a `warn "fetch failed"` call inside `resolve_display_source()` (#557-review fix) would have
  silently corrupted the captured return value with the warning text itself on every failure path
  — caught only because the new test asserted on the captured stdout, not because it "looked
  wrong". Redirect the specific call to stderr (`warn "..." >&2`) instead of changing the shared
  `warn()` function; every OTHER call site in the script legitimately wants it on stdout.
- **A "wiring" test that does `body.contains("function_name")` is tautological** — it ALSO matches
  the function's own `function_name() {` definition line, so it can never catch a dead/deleted
  call site. Search for the exact CALL-SITE substring instead (the function name PLUS its real
  argument, e.g. `body.contains(r#"resolve_display_source "$DEVICE_HOSTNAME""#)`), or slice the
  script body past a marker (`verify_device_pure_functions.rs`'s
  `check_p_is_wired_into_the_live_flow_and_usage_doc` does this by slicing past the source-guard
  comment) before searching. This bug landed once in this exact ticket (#557's first wiring test)
  and was only caught by a later review pass — write the call-site form from the start.
- **An exact-line-anchored TOML section-header match (`/^\[display\][[:space:]]*$/`) misses a
  header with a trailing inline comment** (`[display]  # note`, valid TOML that a hand-edit could
  introduce). If that header drives a strip-then-append idempotent rewrite, the miss means the OLD
  section never gets stripped while a NEW one still gets appended — an invalid DUPLICATE TOML
  table that fails to parse and crash-loops the consuming service. Use a prefix match
  (`/^\[display\]/`, no end-anchor) instead — verified it still can't false-match an unrelated
  section like `[displayfoo]` (the literal substring `[display]` isn't present in it).

## After acceptance

Once `verify-device.sh NAME` reports `ALL CLEAR`, the box is fleet-identical and ready to plug
into the rig (capture card + HDMI/USB source) and add to the relevant OBS box's NDI source list.
That step is manual (scene/source wiring) and out of scope for this runbook — see
`.claude/skills/genlock` / `.claude/skills/obs-ops` for the OBS side.
