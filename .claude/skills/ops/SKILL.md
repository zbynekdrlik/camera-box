---
name: ops
description: >
  Camera-box rig operations — DanteSync cluster clock, device deployment, and rig recovery.
  Load when: deploying camera-box to CAM1-4, diagnosing time-sync issues, checking DanteSync
  status, recovering wedged cameras, or any infra work on the cam/strih/stream cluster.
---

# Camera-box Ops

## DanteSync Cluster Clock

DanteSync (`~/devel/dantetimesync`) is the cluster wall-clock basis for genlock (#8/#42).

**Topology:** strih.lan (10.77.9.202) = NTP master (`ntp_server_mode`, stratum 3).
All nodes sync to strih: stream, cam2 (`--ntp-server strih.lan`), dev1.

**PTP (primary):** grandmaster 10.77.9.184 — all nodes LOCK to µs-grade parity.
**NTP fallback** (when GM absent): ±0.3-1 ms stepping sawtooth — genlock must degrade gracefully.

**Verified node status (2026-06-15 — do NOT re-doubt):**

| Node | IP | Version | Mode | Measured offset to strih |
|---|---|---|---|---|
| cam1 | 10.77.9.61 | v1.8.11 | LOCK | 0.50 ms |
| cam2 | 10.77.9.62 | v1.8.11 | NANO | 0.23 ms |
| cam3 | 10.77.9.63 | v1.8.16 | LOCK | 0.07 ms |
| cam4 | 10.77.9.64 | v1.8.11 | — | — |
| dev1 | — | v1.8.16 | LOCK | 0.16 ms |

`systemctl is-active dantesync` = active+enabled on all.

**HARD RULE:** DanteSync OWNS the clock on cam boxes AND dev1. NEVER install/enable
chrony / ptp4l / systemd-timesyncd / any other NTP/PTP tool, and NEVER run
`timedatectl set-ntp true` (DanteSync's `install.sh` disables them).

**`timedatectl` LIES here** — reports "System clock synchronized: no / NTP inactive"
because DanteSync disciplines the clock DIRECTLY (not via the kernel NTP path timedatectl
watches). This trap has recurred TWICE (2026-06-15, 2026-06-17). Trust DanteSync's own
log/IPC, never `timedatectl` or ad-hoc NTP probes. SSH-RTT offset probes to cam boxes
are also worthless (cam2 SSH RTT 0.4-0.8s while painting → ±0.4s error swamps the real offset).

**Status query:**
- Windows: named pipe `\\.\pipe\dantesync` (length-prefixed JSON; PowerShell NamedPipeClientStream)
- Linux: `journalctl -u dantesync` + `/var/log/dantesync/dantesync.log`
  - Lines: `[PTP] LOCK Drift … µs/s`, `[NTP] offset:…us`

**Gotchas (will bite again):**
- **BOM kills the config:** PowerShell 5 `Set-Content -Encoding UTF8` writes a BOM;
  serde_json then fails → DanteSync silently falls back to defaults (`10.77.8.2` +
  ntp_server_mode DISABLED — kills the cluster's time source). Always write with:
  `[System.IO.File]::WriteAllText($p, $s, (New-Object System.Text.UTF8Encoding $false))`
- **Hostname → IPv6:** `time.cloudflare.com` resolves AAAA-first; DanteSync's resolver
  picks IPv6 (no route) → timeout while `w32tm` (IPv4) worked. Use IPv4 literals in
  `ntp_server` (e.g. `162.159.200.1` for Cloudflare).

Fixed 2026-06-12: strih's upstream was dead `10.77.8.2` (old network); set to `162.159.200.1`.

## Device Deployment

Camera devices (CAM1-4) run x86_64 Ubuntu. Build in CI (never locally); download artifact, deploy over SSH.

```bash
# 1. Download the CI-built binary for the commit to deploy
gh run download --repo zbynekdrlik/camera-box -n camera-box-linux-amd64 --dir ./dist
chmod +x ./dist/camera-box

# DEVICE_ROOT_PW: the box root password (NOT committed — export it from your password store before deploying)
# 2. Deploy to device (device root pw — NOT committed)
sshpass -p "$DEVICE_ROOT_PW" ssh root@10.77.9.6X "mount -o remount,rw / && systemctl stop camera-box"
sshpass -p "$DEVICE_ROOT_PW" scp ./dist/camera-box root@10.77.9.6X:/usr/local/bin/
sshpass -p "$DEVICE_ROOT_PW" ssh root@10.77.9.6X "systemctl start camera-box && mount -o remount,ro / 2>/dev/null; true"
```

Use IP addresses — `.lan` DNS may not resolve.

## Realtime CPU + IRQ isolation of the grab (#289)

The cam-box grab/emit must run ALONE on the `isolcpus`-reserved core or it wobbles under box load
(USB kworkers, ssh, the QR painter on .62) → fps dips + underruns + head_skew. The logic lives in
`src/affinity.rs` (pure, unit-tested) + its call sites:
- **Capture+emit** thread → the isolated core; **painter / `--display` / intercom** → OFF it (cores 0-2).
- The isolated core is DERIVED from `/sys/devices/system/cpu/isolated` (highest online isolated;
  fallback last online; `CAMERA_BOX_CAPTURE_CORE` override) — **never hardcode a core number**.
- USB capture IRQ routed onto the isolated core via `/proc/irq/<n>/smp_affinity`, discovered from
  `/proc/interrupts` — run by `camera-box --setup-irq-affinity` from the unit's `ExecStartPre`.
- `systemd/camera-box.service` carries `CPUAffinity=3` (soft belt-and-braces; the binary refines).
- Verify on a box: `taskset -acp $(pidof camera-box)` (capture threads on the isolated core),
  `journalctl -u camera-box | grep '#289'` (pin + IRQ log lines), `grep . /proc/irq/*/smp_affinity`.
- **Kernel-cmdline tuning is DEFERRED** (`nohz_full`/`rcu_nocbs`/`irqaffinity=`, #303) — it needs the
  #295 safe-grub work first. **NEVER edit `/etc/default/grub` + `update-grub`** (bricked 2 boxes, #295/#301).

## Rig Recovery Policy

The camera-box rig (cam1-4, strih.lan, stream.lan) is a **dev rig, not production**.
Fix infra breakage autonomously — including host reboot — without asking.

When the stream box GPU wedges (`DXGI_ERROR_DEVICE_REMOVED` / TDR on the RTX 4060),
an OBS restart alone often does NOT clear it; a full reboot does.
User directive: "Nie sme v produkcii — mas to opravit ci uz tak alebo onak kludne aj
restartom pc a pokracovat vo vyvoji!!!"

Do NOT gate, classify, or ask before recovering dev rig infra.
The user interrupts Claude when the rig is needed live.

## #265 — NDI-receive STUCK state (recovery = reboot)

THE STUCK STATE (intermittent, after long uptime): the cam→OBS NDI receive on a broadcast box
(strih/stream) collapses to ~10 fps (genlock STARVED, huge underruns) while `dantesync.exe` pegs
~1 of 16 cores. Restarting the dantesync service does NOT fix it — only a full PC reboot does
(2026-06-26: cam1→strih ~10→30.2 fps, underruns 290k→0 after reboot). Recovery = reboot (#265).

A detect+alert watchdog for this state is tracked separately in **#266** (split out of PR #268 for a
solo redesign — the first cut over-/under-discriminated STUCK vs benign IDLE). No watchdog script
lives in the tree yet; until #266 lands, the stuck state is caught by hand (genlock-fifo audit fps +
dantesync CPU) and recovered by reboot per above.

## #297 — NDI sender re-announce (OBS discovery reliability across reboots)

OBS/DistroAV mDNS NDI discovery is flaky on this LAN: the source dropdown does NOT reliably list
all live senders (observed live: finder returned only {CAM2} while CAM1/3/4 were up + emitting; a
rebooted box appears "gone"). Setting `ndi_source_name` by hand still connects → the source IS
reachable, only DISCOVERY fails.

**Sender-side fix (shipped, code):** the camera-box NDI sender (`NDIlib_send_create`, `src/ndi.rs`)
used to be created ONCE at startup and never re-register. It now **re-announces** (re-creates the
sender → re-runs the mDNS announce on the CURRENT network) when the host's usable **LAN** address
set CHANGES or RECOVERS from an outage. Pure trigger in `src/reannounce.rs`
(`should_reannounce(announced, current, saw_down_since_announce)`, `is_discoverable_interface()`
LAN-NIC denylist, `REANNOUNCE_POLL_INTERVAL=2s`); Linux IO (`getifaddrs`) +
`NdiSender::maybe_reannounce()` in `src/ndi.rs`; called each report tick by the capture loop.
Steady state never re-creates (would drop connected OBS receivers) — only a real change does.

**GOTCHA — re-announce MUST destroy before create (#297 dev.139 infinite loop):** `NDIlib_send_create`
refuses to register a SECOND sender whose name is already live in this process → returns null. So a
same-name re-create while the old handle still exists ALWAYS fails. `reannounce_now` must
**`send_destroy(old)` FIRST, then `send_create`** (guard the null handle on destroy — on a retry the
field is already null, same as `Drop`). The shipped dev.139 created-first → null → `bail!` every 2s
without ever advancing the announced signature → infinite WARN loop, box never rediscovered. Two
invariants: (1) advance the trigger (`announced`) ONLY on a SUCCESSFUL create — `ReannounceState::record_reannounce_attempt(current, created_ok)`; a failed create leaves state unchanged so the next
poll retries. (2) the emit path (`send_frame_data_with_timecode`) must GUARD a null `self.sender` (drop
the frame, don't call `send_send_video_v2(NULL)` = UB) for the brief destroy→create window. Convergence
+ retry are unit-tested purely in `src/reannounce.rs`.

**FFI ORDERING is now LOCKED by a test seam (#317):** the pure `src/reannounce.rs` tests never call the
FFI, so a revert to create-first kept them green while reintroducing the loop. `src/ndi.rs` has a private
`trait NdiSendOps { send_create; send_destroy }` separating the two FFI calls from `NdiLib`'s
`libloading::Library`; `NdiLib` impls it as a thin `#[inline]` pass-through (live path byte-identical),
and `fn reannounce_dance<O: NdiSendOps>(ops, &mut sender, settings, &mut trigger, current)` holds the
destroy-first logic that `reannounce_now` delegates to. `mod ffi_seam_tests` drives the dance with a
`FakeNdi` that records `[Op::Destroy(ptr)|Op::Create]` order AND models the SDK one-live-sender-per-name
rule (create while a same-name handle is live → null). To unit-test ANY other NDI FFI ordering, add the
method to `NdiSendOps` + `FakeNdi`, never construct a real `NdiLib`. Run cheap: `cargo test --lib
ffi_seam_tests` (append `# airuleset:build-ok` — the Tier-0 hook blocks `cargo test` otherwise).

**SCOPE LIMIT (do NOT overstate):** re-announce fixes the **boot-race / late-DHCP / link-flap**
cases. It does **NOT** fix a STABLE box whose mDNS registration was simply lost/missed by OBS
(stable network, no change → no trigger) — and the cam boxes have STATIC IPs, so a clean reboot may
present a stable sig from process start. That persistent-flakiness case is the **central NDI
Discovery Server** (`NDI_DISCOVERY_SERVER` on every cam box + both OBS) or a LAN multicast fix
(avahi reflector / IGMP snooping) — a **fleet-infra decision raised to the user** (pending; file a
follow-up if chosen). When diagnosing "camera missing from OBS dropdown", check WHICH case it is
(boot-race/flap → re-announce should cure within ~2s; stable-lost-announce → needs the discovery
server).

**Rig verify (supervisor):** reboot a box, confirm it reappears in the OBS NDI dropdown within
seconds (and on a link flap). The unit tests prove the trigger logic only — discovery is a rig check.

## #295 — cam-box boot hardening (what bricked CAM3/CAM4 + the durable fix)

**How the boxes ACTUALLY boot:** the live cam boxes run a plain **RW root on `/dev/sda2`**
(ext4 rw,relatime), provisioned MANUALLY — NOT the repo's ro-root+overlay image. `build-image.sh`
designs a read-only root + 512M overlay, but the deployed boxes don't use it (long-term target;
operational re-image tracked in **#301**).

**The brick (NOT fs corruption):** `unattended-upgrades` was active → auto-installed a `6.8.0-124`
kernel; a FULL 100M `/var/cache` tmpfs broke apt with ENOSPC so its **initrd never generated**; a
later `update-grub` (the #289 isolcpus edit) made that initrd-less kernel the default → can't mount
root → unbootable. Recovery was a dev1 chroot: `update-initramfs -c -k <ver>` + `update-grub`.

**Durable fix (PR #306, in provisioning `setup.sh` + `setup-device.sh`):**
- `apt-mark hold linux-image-generic linux-headers-generic linux-generic` — **BEFORE** `apt-get upgrade`.
- unattended upgrades OFF (`/etc/apt/apt.conf.d/20auto-upgrades` periodic=0 + kernel blacklist + masked).
- every kernel gets an initrd BEFORE `update-grub` (loop + `/etc/kernel/postinst.d/zz-camera-box-initrd-guarantee`).
- `GRUB_DEFAULT=saved` + validate the default grub.cfg menuentry has BOTH a kernel image AND an initrd
  (abort loudly otherwise) + `grub-set-default 0`.
- `/var/cache` tmpfs sized **512M** uniformly (was the 100M that ENOSPC'd).

**Live-box rules (unchanged):** NEVER edit `/etc/default/grub` + `update-grub` on a live box — that
is what bricked them. Kernel-cmdline tuning (#303 nohz_full/rcu_nocbs) stays DEFERRED. Safe live
mitigation already on survivors (.61/.62/.64): kernels dpkg-held, `/var/cache` freed. Extending the
hardening to the base-image builder (`create-usb-linux.sh`) + a `build-image.sh` fail-closed grub
guard is **#307**.
