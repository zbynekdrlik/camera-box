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

**#362 — a FRESH USB clone needs the NDI/audio RUNTIME deps baked in (re-image / #301 checklist).**
A fresh CAM3 clone booted but camera-box crash-looped because libndi could not `dlopen`. If a fresh
box crash-loops camera-box, check these four (the builders now bake all of them — verify if a clone
predates the fix):
1. `libasound2t64` (ALSA) — camera-box exits on missing `libasound.so.2` (intercom).
2. `/etc/ld.so.conf.d/ndi.conf` = `/usr/lib/ndi` (+ `ldconfig`) — without it `dlopen("libndi.so")`
   fails (`cannot open shared object file`) even though the lib is present at `/usr/lib/ndi/`.
3. `libavahi-client3` + `libavahi-common3` — libndi links them; NDI load fails without them.
4. `avahi-daemon` (installed + `systemctl enable`d + running) — libndi browses mDNS via it for NDI
   source discovery; no daemon → `find()` returns nothing → black `--display` receiver even though
   the source is reachable at TCP level. `avahi-utils` (`avahi-browse _ndi._tcp`) diagnoses it.
   avahi-daemon is mDNS only — NO conflict with DanteSync's clock ownership (cam4 runs both; do NOT
   touch chrony/timesyncd/timedatectl).
Baked into `scripts/create-usb-linux.sh` (base image) + `setup.sh` + `setup-device.sh`; pinned by
content-assertion tests in `tests/appliance_boot_hardening.rs`.

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

## #265 — NDI-receive STUCK state (detect by NDI fps, NOT by dantesync CPU)

THE STUCK STATE (intermittent, after long uptime): the cam→OBS NDI receive on a broadcast box
(strih/stream) collapses to ~10 fps (genlock STARVED, underruns CLIMBING). When this genuinely
happens, a full PC reboot clears it (2026-06-26: cam1→strih ~10→30.2 fps, underruns 290k→0 after
reboot). Restarting only the dantesync service does NOT fix the NDI collapse.

**DO NOT use `dantesync.exe` CPU as the stuck signal on strih — it is a RED HERRING (corrected
2026-06-29).** strih is the cluster NTP MASTER (`ntp_server_mode.enabled=true` in
`C:\ProgramData\DanteSync\config.json`); its NTP-serving thread busy-loops ~99% of ONE core
CONTINUOUSLY since process start — present at every boot, healthy or not. The stream box (an NTP
client) does NOT do this. So a 99%-core dantesync on strih is NORMAL and a reboot does NOT fix it
(the busy-loop resumes identically next boot). The 2026-06-29 incident: a black #312 program +
dantesync-core-peg were mis-read as "#265 stuck"; forensics showed NDI receive was actually HEALTHY
(all sources 60 fps, underruns CONSTANT not climbing) — the black program was cam1's NDI
mid-renegotiation after its restart (see #312 self-check fix), not a stuck box. The dantesync
busy-loop itself is a fixable bug in `~/devel/dantetimesync` (NTP-server socket set non-blocking AND
read-timeout → `recv_from` spins) — tracked + fixed separately; once deployed strih's dantesync
idles ~0%.

**Stuck-state detection keys SOLELY on the NDI receive trajectory, never on CPU:** read the OBS
`genlock-fifo audit` lines for the cam sources — STUCK = received-fps dropped to ~10 AND underruns
CLIMBING between samples. Healthy = 60 fps received==consumed, underruns CONSTANT. (On the stream
box, an NTP client, dantesync CPU is still a valid secondary signal; on strih it is not.)

A detect+alert watchdog is tracked in **#266** (split out of PR #268; first cut over-/under-
discriminated STUCK vs benign IDLE). It MUST key on the NDI received-fps + underrun trajectory
(2-live-sample on its own monotonic clock), NOT on dantesync CPU. No watchdog script lives in the
tree yet; until #266 lands, a genuine stuck state is caught by hand (genlock-fifo audit fps +
climbing underruns) and recovered by reboot.

## #281 Fix#3 — rig stranded-in-TEST auto-restore watchdog (heartbeat-gated)

Dead workers leave the rig in a TEST state (prod `camera-box` down + a manual `/tmp` probe holding
the device; OBS program on `PHASE2-PROBE`; burns on). Fix#3 added a conservative safety net.

**Heartbeat = "a legit E2E is running, do NOT auto-restore."** `scripts/lib/rig-heartbeat.sh` owns a
well-known file `${XDG_RUNTIME_DIR}/camera-box-rig-active` (fallback `/tmp/camera-box-rig-active`):
- `recording-e2e.sh` `rig_heartbeat_start` (after the cleanup trap is armed) + `rig_heartbeat_stop`
  (FIRST line of `cleanup()`) → fresh for the whole run, cleared on clean exit OR death.
- `rig-mode.sh` TEST `rig_heartbeat_write`, EVENT `rig_heartbeat_clear`. **One-shot** (rig-mode
  exits) → goes STALE after `RIG_HEARTBEAT_STALE_SEC` (default 600s); an idle TEST rig then becomes
  watchdog-actionable (intended — run an active E2E to keep it fresh).

**Pure decision** `scripts/lib/rig-restore-decision.sh::rig_restore_decide` (no I/O, unit-tested in
`tests/harness_rig_restore_watchdog.rs`): fresh heartbeat → never act; else a CLEAR stranded signal
(cam down / stale probe / the **#353 E2E marker** / — fallback — OBS on a known TEST scene
`RIG_KNOWN_TEST_SCENES`) → act, but only after **2 consecutive confirmations**
(`RIG_CONFIRM_THRESHOLD`, the #266 lesson); acting ALWAYS alerts.

**#353 — the E2E MARKER is now the PRIMARY stranded signal (replaced scene-name scraping).**
`scripts/lib/rig-heartbeat.sh` gained `rig_e2e_marker_{path,set,clear,present}`. The marker
(`${XDG_RUNTIME_DIR:-/run}/rig-in-e2e`) is written ONCE on harness entry and removed ONLY on clean
exit — so an UNCLEAN death (SIGKILL / interrupted trap) leaves it behind. This is **distinct from the
heartbeat**, which the background refresher self-removes the instant the harness dies. So:
- **marker present AND heartbeat absent/stale** = a harness entered a test state and never cleaned up
  = rig stranded, REGARDLESS of which scene OBS is on. When set, the decision restores EVERY observed
  OBS box (`RIG_E2E_MARKER=1` env). The watchdog reads it (`rig_e2e_marker_present`) → exports it →
  **clears it after restoring** so a stale marker can't re-trigger/re-alert each pass.
- Wired: `recording-e2e.sh` sets the marker next to `rig_heartbeat_start`, clears it in `cleanup()`.
- Why: scene scraping was fragile (env-overridden / custom scene names slip through; two files to keep
  in sync). The marker needs no scene-list and is robust to any scene name.

**`RIG_KNOWN_TEST_SCENES` is now a FALLBACK only** (marker-less runs: older harness / manual testing).
Default (both `rig-restore-decision.sh` ~line 60 and `rig-restore-watchdog.sh` ~line 80):

```
RIG_KNOWN_TEST_SCENES="PHASE2-PROBE"
```

- `PHASE2-PROBE` — the `obs_phase2.py` phase2 probe scene on strih (still a live test scene).
- **`REC-STRIH-TMP` was DROPPED** (#343/#353): `recording-e2e.sh` now records the stream box's
  already-active prod scene `PRO` (`STREAM_PROG_SCENE` default), so the stream box never lands on an
  ephemeral scene — the marker, not a scene name, detects a stranded stream box now.

**INVARIANT (post-#353)**: prefer the MARKER for new rig-touching harnesses — wrap them so they
`rig_e2e_marker_set` on entry + `rig_e2e_marker_clear` on clean exit (and the heartbeat too). The
scene-list fallback only matters for marker-less callers; if you DO add a scene to it, add it to BOTH
default strings (decision + watchdog — the two-file sync the marker eliminates) and a test.

**GOTCHA — fallback scene names must not contain spaces**: the match loop uses IFS word-split
(`for ks in $known; do`) — a name like `"NDI 2ME PGM"` would split into three tokens and silently
fail to match. All current names are hyphenated. Locked by #352 (code comment + lock test).

**Watchdog** `scripts/rig-restore-watchdog.sh` (runs on dev1 from a `systemd --user` timer,
session-independent): probes cam1/2/4 (`systemctl is-active camera-box` + stale-probe `pgrep`) + the
E2E marker + OBS program scene (`obs_phase2.py program-scene` reader, reuses `_conn`/`_rpc`), persists
the confirm counter across runs in a state file, restores prod (`systemctl restart camera-box` /
`obs_phase2.py teardown`), clears the marker **only after a POSITIVE full OBS restore** (pure
`rig_marker_should_clear`: marker present + acted + no OBS box unreadable + every teardown succeeded —
else KEEP so a later pass retries; clearing while an OBS box was unreadable/failed would mask a
still-stranded box), and ALWAYS `airuleset.py notify`. `--dry-run` = observe+decide+log only (never
clears the marker).

**SHIPS DISABLED** — `systemd/rig-restore-watchdog.{service,timer}` committed but NOT installed/
enabled. The **supervisor** installs, live-verifies (real E2E heartbeat → no act; simulated stranded
state → detect→restore→alert), then `systemctl --user enable --now rig-restore-watchdog.timer`.
Full procedure: `systemd/rig-restore-watchdog.README.md`. This is the conservative re-do of the
#266 watchdog (removed for false positives) — heartbeat gate + 2-confirm + clear-signal-only.

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
mitigation already on survivors (.61/.62/.64): kernels dpkg-held, `/var/cache` freed.

**#307 SHIPPED (PR #322):** the hardening now also covers the two builders the setup scripts didn't —
`create-usb-linux.sh` (master base-image builder: `apt-mark hold` the kernel + unattended-upgrades off
via `20auto-upgrades` periodic=0 + `GRUB_DEFAULT=saved`/`grub-set-default 0`, was the hardcoded
boot-newest default) and `build-image.sh install_bootloader` (fail-closed guard: validate the generated
grub.cfg default menuentry has BOTH a kernel image AND an initrd before pinning). All four scripts
(`setup.sh`, `setup-device.sh`, `create-usb-linux.sh`, `build-image.sh`) are now content-asserted by
`tests/appliance_boot_hardening.rs` (10 tests). These are BUILD-time scripts — verified by
content-assertion tests in CI, NOT a rig deploy (runtime boot-verify happens at the #301 re-image).

**#344 SHIPPED (PR #345) — USB/image actually BOOTS (the historical "USB never boots" pain):**
`grub-install --removable` on Ubuntu 24.04 embeds a signed-grub memdisk core whose live-media probe
(`search /.disk/info`) FAILS on an installed root → never chains to the real grub.cfg → drops to the
`grub>` rescue prompt → USB never boots. **Fix:** a shared `scripts/lib/install-grub-efi.sh`
`build_grub_efi_core` rebuilds `EFI/BOOT/BOOTX64.EFI` with `grub-mkimage` + a minimal embedded config
(`search.fs_uuid <root> → set prefix=($root)/boot/grub → configfile $prefix/grub.cfg`) — topology-
independent, no live-media probe (819 KB clean core vs 2.3 MB broken Ubuntu core). Wired into BOTH
`create-usb-linux.sh` AND `build-image.sh` (after `grub-set-default 0`) + `GRUB_DISABLE_OS_PROBER=true`.
**Secure Boot must be OFF** on the target (the grub-mkimage core is UNSIGNED — not a regression, the
old --removable core wasn't shim-chained either; cam boxes run SB off). Boot regression test
`scripts/test-usb-grub-boot.sh` (on-demand, root+qemu+OVMF): builds a loopback GPT image, installs the
core, boots it headless over a USB-storage controller, asserts a marker reaches the serial. The cam3
USB stick (clone of cam4 + cam3 identity, static 10.77.9.63) was built + boot-proven this way before
hand-off.
