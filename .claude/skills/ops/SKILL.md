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

## #132/#445/#452 — `scripts/upgrade-fleet-ndi.sh` canary set + version-scoped backup

Safe, canary-first NDI Linux runtime (`libndi.so.6`) upgrade across the fleet. The fleet is NOT
uniform in HOW it hosts that runtime, discovered incrementally:

- **#445 — cam3 is a distinct "class"**: real-file `libndi.so.6`/`libndi.so` (not symlinks), no
  `strings` binary, and an older "Streaming: X.Y fps" log shape instead of the genlock-report
  line. `ndi_link_kind_remote` detects symlink-vs-regular live; `ndi_read_banner_local` /
  `ndi_active_version_remote` fall back to `grep -a` when `strings` is missing.
- **#452 — a green canary on ONE box proves nothing about a DIFFERENT class.** cam1 (symlink
  layout) passing tells you nothing about cam3 (real-file, no `strings`) — exactly the #132
  history where cam3 needed a manual upgrade after the tool "succeeded". Fix: `ndi_camera_class`
  is a STATIC, hardcoded table (cam3 -> `"cam3class"`, everything else -> `"standard"`) — add any
  future box with cam3's quirks there. `resolve_canary_set(SET, OVERRIDE)` defaults the canary to
  ONE representative per DISTINCT class present in `SET` (first member of each newly-seen class,
  in SET order) — a single-class SET still gets exactly one canary (unchanged #132 behavior); a
  mixed SET like `cam1 cam2 cam3 cam4` defaults to canary set `"cam1 cam3"`. `--canary` now takes
  a space-separated LIST (same shape as `--set`); an override with any non-member is rejected
  whole (never silently drops just the bad one). The main flow loops `for cam in $CANARY_SET`
  BEFORE `for cam in $REST` — every canary is tried (surfacing every class's result in one run)
  and the whole fleet is skipped if ANY canary failed.
- **#452 — version-scoped real-file backup.** The regular/real-file swap branch used to `cp -a
  libndi.so.6 libndi.so.6.bak` with a FIXED name — every upgrade overwrote the same `.bak`, one
  generation of rollback only. Now `ndi_swap_remote` takes a 4th arg `OLD_VERSION` (the caller's
  already-read `cur_ver`, from the SAME `ndi_active_version_remote` call `upgrade_one_camera`
  makes before swapping — never re-derived) and names the backup
  `libndi.so.6.<OLD_VERSION>.bak`, giving the real-file layout the same multi-generation depth the
  symlink layout already has for free (each symlink backup keeps its original versioned
  filename). `ndi_rollback_remote` needed NO code change — it already restores from whatever
  `OLD_BASE` string the caller passes.
- All of the above is pure-function unit-tested (`tests/upgrade_fleet_ndi.rs`, sourced-script
  harness, no real ssh) — never verified by running the script against real cameras (it swaps a
  live NDI runtime under a running service).

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
- **Provisioned by `scripts/setup-device.sh`** (#450): STEP 10 adds `isolcpus=3` to `GRUB_CMDLINE_LINUX`
  (idempotent, INSIDE the #295 initrd-guaranteed grub block — that block is the ONE safe place to touch
  grub, so this does NOT contradict the "never ad-hoc edit grub" rule below); STEP 7 writes the
  `cpu-affinity.conf` (`CPUAffinity=3`) + `genlock.conf` (`CAMERA_BOX_GENLOCK_FPS=${CAMERA_GENLOCK_FPS}`,
  per-cam table, #451) drop-ins. The capture core is AUTO-DERIVED from `isolcpus` — the live fleet sets
  NO `CAMERA_BOX_CAPTURE_CORE` (only `CAMERA_BOX_GENLOCK_FPS=60`, confirmed on cam1). Guard:
  `tests/provisioning_realtime_isolation.rs`.
- **`setup-device.sh` invocation is now NAME-RESOLVED, single-arg (#450 rework, 2026-07-05):**
  `sudo ./setup-device.sh CAM5` (case-insensitive) alone brings a booted box to full fleet parity —
  the OLD 3-positional-arg form (`setup-device.sh CAM5 10.77.9.65 cam5`) is GONE. It sources
  `scripts/camera-set.sh` and resolves `DEVICE_NAME -> DEVICE_IP / VBAN_STREAM / CAMERA_GENLOCK_FPS`
  via a pure `resolve_device_name()` function (unit-tested against the real fleet map in
  `tests/setup_device_pure_functions.rs`, sourced the same way as `setup-imag.sh`'s pure functions —
  its `BASH_SOURCE` guard skips the destructive provisioning flow when sourced). The script is now
  `set -euo pipefail` + fails loud (via a `fail()` helper) on binary/NDI/ALSA/dantesync install
  failure instead of warn-and-continue, and STEP 19 hard-fails (never prints "Setup Complete!") if
  the binary or `/usr/lib/ndi/libndi.so.6` is still missing. Guard:
  `tests/setup_device_provisioner_hardening.rs`.
- Verify on a box: `taskset -acp $(pidof camera-box)` (capture threads on the isolated core),
  `journalctl -u camera-box | grep '#289'` (pin + IRQ log lines), `grep . /proc/irq/*/smp_affinity`.
- **Kernel-cmdline tuning is DEFERRED** (`nohz_full`/`rcu_nocbs`/`irqaffinity=`, #303) — it needs the
  #295 safe-grub work first. **NEVER edit `/etc/default/grub` + `update-grub`** (bricked 2 boxes, #295/#301).
  This deferral is CAM-FLEET-ONLY — imag-nb (below) already has the full kernel-cmdline tuning.

## imag-nb kernel/CPU hardening (#482/#483/#487) — preempt=full + P-core isolation

imag-nb (10.77.9.182) runs 6× NDI decode + render + genlock + audio in OBS (~106 threads) on a
13th-gen-class notebook (10 P-core HT threads cpu0-11 + 4 E-cores cpu12-15). Fully codified in
`scripts/setup-imag.sh` (steps 6-8); a from-scratch provision reproduces this exactly.

**Gotcha (will recur on any modern-kernel low-latency tuning job): there is often NO lowlatency
kernel IMAGE at the current kernel line.** On imag's 6.17.0-35, the newest `linux-image-*-lowlatency`
builds are 6.8/6.11 — installing one would be a DOWNGRADE (loses 13th-gen CPU/iGPU/USB-NIC
support). Check first: **the generic kernel may already be `PREEMPT_DYNAMIC`** — if so,
`apt-get install linux-lowlatency-hwe-24.04` pulls ONLY the `lowlatency-kernel` CONFIG package
(no image change), which drops `/etc/default/grub.d/99-lowlatency.cfg` =
`GRUB_CMDLINE_LINUX_DEFAULT="... preempt=full rcu_nocbs=all"` — full preemption on the kernel
already running, zero downgrade.

**Grub convention for imag differs from the cam-fleet #295 mechanism**: instead of editing
`/etc/default/grub`'s `GRUB_CMDLINE_LINUX(_DEFAULT)` in place, imag writes standalone
`/etc/default/grub.d/*.cfg` drop-ins (`98-imag-isolation.cfg`, `99-lowlatency.cfg` — sourced in
lexical order by `grub-mkconfig`, each appending to `$GRUB_CMDLINE_LINUX_DEFAULT`). A whole-file
`cat > file <<'EOF'` write is trivially idempotent (no grep-before-append needed) — but it is
STILL routed through the shared `safe_grub_regen()` bash function (defined in setup-imag.sh step
6, #487) before trusting it: initrd-guarantee loop → `update-grub` → validate the generated
default menuentry has both a kernel image and an initrd, `fail()` otherwise. Call it ONCE after
ALL grub.d drops for a batch of changes are written — not once per file.

**CPU layout** (HT pairs confirmed via `thread_siblings_list`, NOT `lscpu`'s flat count):
cpu0-1 = P-core0 (kept for GNOME/Xorg/sshd/MCP), cpu2-11 = the OTHER 5 P-cores (isolated, reserved
for OBS's whole thread pool via `isolcpus=2-11`), cpu12-15 = E-cores (no HT, also housekeeping).
`nohz_full`/`rcu_nocbs` are scoped to ONLY cpu10,11 — the single core pair reserved for the future
#484 SCHED_FIFO genlock render-tick thread — never the whole isolated block (spreading it wider
removes load-balancing signal for everything else on the block, per #303's cam-fleet caveat).

**OBS must be `taskset -c 2-11`-pinned on EVERY launch path** or `isolcpus` starves it onto the
tiny cpu0,1,12-15 remainder. Two launch paths exist in setup-imag.sh: the autostart
`~/.config/autostart/obs.desktop` (`sed`-patch `Exec=obs` → `Exec=taskset -c 2-11 obs` — the file
is copied FRESH from `/usr/share/applications/...` every run so the sed always finds its target,
which makes this idempotent for free) and the script's own provisioning-time launcher. The
Desktop *icon* (double-click) is deliberately left unpinned on the live box — don't "fix" that.
`nice -n -5` was tried and dropped: the desktop user lacks `CAP_SYS_NICE`.

**Live status (2026-07-04): only the grub/isolcpus/preempt changes were hand-applied on the box —
the #487 boot-safety net (apt-mark hold, the postinst initrd hook, the Unattended-Upgrade
kernel-blacklist) was NEVER live-applied, only codified.** `apt-mark showhold` on imag currently
shows only `obs-studio`. Don't assume "codified in setup-imag.sh" means "already protecting the
live box" — check `apt-mark showhold` / `systemctl is-enabled unattended-upgrades` if it matters.

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

**#370 — distinct alert body + rate-limit for the partial/KEPT case.**
Two new pure functions in `rig-restore-decision.sh` (unit-tested in `harness_rig_restore_watchdog.rs`):
- `rig_classify_restore(act, obs_unreadable, obs_failed)` → `kind=positive|partial`. Positive = full
  restore (0 unreadable, 0 failed → "AUTO-RECOVERED"). Partial = marker KEPT, lower-urgency body.
- `rig_alert_throttle(kind, current_sig, prior_sig, prior_passes, [N])` → `alert_now=0|1 + new_sig +
  new_passes`. Positive: always alert. Partial: alert on first occurrence or after `N` passes
  (default `RIG_ALERT_THROTTLE_PASSES=5`). Sig = `kind:unreadable_names:failed_count`.
  STATE_FILE extended to `confirm=N / alert_sig=... / alert_passes=N` (key=value; backward-compat:
  bare-number legacy files are transparently upgraded on next write). Two-write pattern:
  early write (crash-safe confirm), late write (alert state, only runs when act=1).

**ENABLED + live-verified on dev1 (#350, 2026-07-01).** The timer is active (~2 min cadence). The
conservative re-do of the #266 watchdog (removed for false positives) — heartbeat gate + 2-confirm +
clear-signal-only. Full procedure: `systemd/rig-restore-watchdog.README.md`. Enable recipe used:

```bash
cp systemd/rig-restore-watchdog.{service,timer} ~/.config/systemd/user/
# strih OBS-WS secret is NOT committed — supply it via a LOCAL drop-in (mode 0600, not git):
mkdir -p ~/.config/systemd/user/rig-restore-watchdog.service.d
printf '[Service]\nEnvironment=OBS_WS_PASSWORD=<strih WS pw — local memory, NOT committed>\nEnvironment=CAM_PW=newlevel\n' \
  > ~/.config/systemd/user/rig-restore-watchdog.service.d/override.conf
systemctl --user daemon-reload
systemctl --user enable --now rig-restore-watchdog.timer
systemctl --user list-timers | grep rig-restore-watchdog   # confirm NEXT is ~2 min out
```

Live-verify (all passed): dry-run on the healthy rig → `no-stranded-signal`; simulate a strand by
`rig_e2e_marker_set` (source `scripts/lib/rig-heartbeat.sh`) with NO heartbeat → pass1 `confirm=1
act=0`, pass2 `confirm=2 act=1` restore both OBS boxes + Discord alert + marker auto-cleared; then
`rig_heartbeat_write` a fresh heartbeat WITH the marker still set → `heartbeat-fresh-legit-e2e act=0`.
Confirm one UNATTENDED timer-driven pass fired (`journalctl --user -u rig-restore-watchdog`). Clear the
test marker/heartbeat + `rm` the state file after.

**GOTCHA — `obs_phase2.py teardown` consults `/tmp/obs_phase2_state.json`; a STALE leftover switches a
healthy box's scene.** teardown restores the `prev_scene`/`prev_preview` that a prior `setup`/`prod-scene`
saved there, and only switches when current≠saved. A leftover state from an OLD session (e.g. the
simulation above ran teardown while a Jun-30 file said stream `prev_scene:"PRO"`) makes teardown move a
healthy box's program to that stale scene. In a REAL strand this is correct (the dead harness saved the
true pre-test scene); when SIMULATING against a healthy rig, `rm -f /tmp/obs_phase2_state.json` first (or
expect the switch and restore after). Prod scenes: strih `Cam 5`; stream `PRO` = live program,
`PRE` = standby (both real scenes — `PRO` is what `recording-e2e.sh` records).

**GOTCHA (observability, #394) — under the systemd unit the per-box observation LOG lines are
intermittently dropped from the journal** (journald racing `log()` in the fast-exiting `$()` subshells
of `obs+="$(probe_obs …)"`). The detect→restore path is UNAFFECTED — the `obs … scene=…` RECORD is
captured in-process, not via journald (proven: dry-run+marker → `restore_obs:strih restore_obs:stream`
even on a pass whose log line is missing). To SEE the reads reliably, run the script directly or with
`bash -x`. Fix tracked in #394 (collect observations without a per-call `$()` subshell).

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

## #369 — auto-grow root partition + fleet disk-space guard (PR #384)

**Root cause:** cam4 shipped 3.5G/92%-full on a 57G disk — the partition was written by the image builder
but NEVER expanded to fill the physical disk. cam1's `/var/cache` tmpfs was 100M/100%-full (pre-#306 era).

**Fix: `systemd/camera-box-grow-root.service` + `scripts/lib/camera-box-grow-root.sh`**

Run-once first-boot service installed + enabled by BOTH builders. Key design decisions:

- **Run-once gate:** `ConditionPathExists=!/var/lib/camera-box/grow-root.done`. Marker written
  unconditionally at end of script regardless of growpart/resize2fs success. The marker ALWAYS lands
  so subsequent boots skip the service entirely and boot is never blocked.
- **Fault-tolerant growpart:** `build-image.sh` creates a 3-partition layout (EFI + root + overlay).
  Root is NOT the last partition, so `growpart` WILL refuse to expand it (non-zero exit). Script handles
  this with `growpart … 2>/dev/null && resize2fs … 2>/dev/null || echo "non-fatal"`. The marker is
  still written. `create-usb-linux.sh` images have root as the last partition — growpart succeeds there.
- **Unit in `[Unit]` section:** `After=local-fs.target` + `Before=multi-user.target` (ordering explicit
  so grow happens before any service that needs space).

**`scripts/lib/` is the canonical shared-helper location for both builders.** Extract any bash helper
that both `create-usb-linux.sh` AND `build-image.sh` need into `scripts/lib/<name>.sh`; then each
builder does `install -m 0755 "$SCRIPT_DIR/lib/<name>.sh" "$MOUNT_ROOT/usr/local/sbin/<name>.sh"`.
Do NOT embed the same script verbatim via heredoc in both builders (the reviewer flagged that as
important and it was refactored out in commit `6ea08a216`). For **service unit files**, always use
`install -m 0644 … "$MOUNT_ROOT/etc/systemd/system/<name>.service"` — NOT bare `cp`; `cp` is
mode-preserving from the source and silently propagates any unusual source mode. Current `scripts/lib/` contents:
`install-grub-efi.sh`, `rig-heartbeat.sh`, `rig-restore-decision.sh`, `camera-box-grow-root.sh`,
`disk-guard-thresholds.sh`.

## #458 — `set -euo pipefail` footguns in one-shot provisioning scripts (setup-*.sh)

Two review passes on `scripts/setup-imag.sh`'s genlock hot-swap step (#460 integrity check)
found the SAME class of bug three separate times, in three different shapes. All three are now
fixed there (and empirically spot-check-verified — revert the fix, confirm the test fails,
restore) but WILL recur in any future `setup-*.sh`/provisioning script unless watched for:

1. **`ls`/`grep`/`curl|grep|grep` piped into `| head -1`, assigned bare — aborts SILENTLY on an
   empty match.** `VAR="$(ls -t DIR/*.txt 2>/dev/null | head -1)"` — if the glob matches nothing,
   `ls` exits non-zero even though `head` succeeds (empty output is not an error for `head`).
   Under `pipefail`, the pipeline's exit status is the RIGHTMOST non-zero command, so the bare
   assignment inherits `ls`'s failure and `set -e` aborts the WHOLE SCRIPT right there — before
   whatever `[ -n "$VAR" ] || fail "..."` check you wrote next ever runs, usually with **zero
   diagnostic** (stderr is typically `2>/dev/null`'d). **Fix: append `|| true` to the whole
   pipeline** (`... | head -1 || true`), then let the following `[ -n "$VAR" ] || fail ...` do its
   job normally. Same fix applies to any `curl | grep | grep | head -1` URL-scraping pipeline.
2. **A `fail()`-calling function invoked via `cmd "$(func)" other-arg` — its abort is silently
   swallowed.** `$(...)` always forks a subshell. A BARE `VAR="$(func)"` assignment correctly
   propagates that subshell's exit status to `set -e` (empirically verified — this DOES abort the
   script). But `cmd "$(func_that_calls_fail)" other-arg` does NOT: the subshell's `exit` only
   kills the command-substitution subshell, and `cmd` still runs with that argument silently
   EMPTY. **Rule: any function that can call `fail()`/`exit` must ALWAYS be captured via a bare
   `VAR="$(func ...)"` assignment on its own line — never embedded as one of several arguments to
   another command.** This is the subtler sibling of the already-documented `| while read; do
   fail; done` pipe-subshell trap (same root cause — a subshell's `exit` doesn't propagate through
   an enclosing command the way `set -e` needs — different shape, easy to reintroduce while fixing
   the first one).
3. **`gh ... -q '.[0].someField'` on an EMPTY JSON array/result returns the literal 4-char text
   `"null"`, not an empty string** (jq semantics: indexing a nonexistent element yields `null`,
   and `-r`/`gh -q` renders it as text). A bare `[ -n "$VAR" ]` guard wrongly treats `"null"` as a
   valid value. **Fix: `-q '.[0].someField // empty'`.**
4. **(#463, `drift-guard.sh gather_and_check_imag`) You need the EXIT CODE of a `$(cmd)` that can
   itself legitimately fail (an `ssh`/`timeout` call whose 255/124 means "unreachable", not just
   pipe-exit-status like point 1) — capturing it on the NEXT line crashes instead.**
   `var="$(ssh ... cmd)"` then `local rc=$?` on the FOLLOWING line looks reasonable, but under
   `set -e` the FAILING ASSIGNMENT ITSELF aborts the whole script the instant `cmd` returns
   nonzero — `rc=$?` never even runs (empirically verified: `bash -c 'set -e; f(){ return 255;
   }; x="$(f)"; echo after'` prints nothing and exits 255). **Fix: put the assignment in an
   OR-list** — `rc=0; var="$(cmd)" || rc=$?` — the one shape `set -e` exempts (only the LAST
   command of an AND/OR list is errexit-checked), so it survives AND captures the real code.
   Different failure shape from point 2 above (that one is about `fail()`/`exit` INSIDE a
   function losing propagation through `$(...)`; this one is about a command that returns a
   plain nonzero you need to READ, not propagate).
5. **(#489, `drift-guard.sh check_imag_report`) A bare `${12}`/`${13}`-style reference to an
   OPTIONAL trailing positional param crashes the whole script under `set -u` when an OLDER
   caller doesn't pass that many args — it does NOT quietly become an empty string.** Adding a
   new arg to a widely-called pure function (`check_imag_report` gained a 12th/13th param for the
   dantesync-lock check) means every EXISTING call site that still passes only 11 args now
   references an unset positional parameter — under `set -uo pipefail` (this file's own top-of-
   file `set`, in effect for the whole test-harness `run_sourced` session too, #463's own
   footgun-comment above) that is a hard `unbound variable` abort, not a graceful empty string
   (verified: `bash -c 'set -u; f(){ local a="$1" c="$3"; }; f one'` → `3: unbound variable`,
   exit 127). **Fix: default-empty expansion on the OPTIONAL trailing params —
   `local exp="${12:-}" obs="${13:-}"`** — mirrors `compare_observed`'s existing
   `o_av_sync_calibrated_ms="${25:-}"` pattern for its own optional trailing args. Whenever you
   extend a pure function's positional-arg contract, grep every existing call site first; if any
   don't pass the new args, the new params MUST use `${N:-}`, never a bare `${N}`.
6. **(#489 review, `tests/drift_guard.rs`) `big="$(yes "$line" | head -n N)"` to manufacture a
   large test blob has its OWN unrelated SIGPIPE hazard — it can crash the WHOLE test harness
   before the function under test is ever called, and is easy to misread as a bug in that
   function.** `head -n N` closes its read end after N lines; `yes` (which never stops producing)
   gets SIGPIPE on its next write; under `set -o pipefail` (this file's own top-of-file `set`,
   inherited by `run_sourced`'s sourced-script session) that non-zero exit aborts the `$(...)`
   assignment itself — verified: `bash -c 'set -euo pipefail; big="$(yes x | head -n 3000)"; echo
   after'` prints nothing, exits 141, `after` never runs. A `_from_log` parser call placed AFTER
   such a line looks like it crashed when in fact the crash happened one line earlier, unrelated
   to anything the parser does. **Fix: build the large blob as a plain string (a Rust `for` loop
   appending to a `String`, or a bash `for`/`printf` loop — never `yes | head`), then either pass
   it directly or (to dodge `ARG_MAX` on a huge value) write it to a temp FILE and `cat` it inside
   the bash body** — this is the file's OWN established, already-proven-correct pattern for
   exactly this scenario: `tests/drift_guard.rs`'s `genlock_parser_reads_running_state_from_log`
   (~line 200, builds via a `for i in 0..5000 { push_str(...) }` loop into a temp file, sourced
   via `cat "$LOGFILE"`) and its #489 sibling `dantesync_locked_from_log_survives_a_large_journal_
   without_sigpipe_489`. Before believing a "confirmed" large-input crash in a `_from_log` parser,
   isolate the blob-construction line ALONE (no function call after it) and confirm it does NOT
   also crash on its own — an incident here (#514, opened then corrected+closed) mistook the
   `yes | head` artifact for a real crash in the already-shipped `genlock_from_log`, when the
   function itself is safe (verified up to 200,000 lines / ~12.5MB via a properly materialized,
   `cat`-read input) and the real underlying bug was a silent WRONG ANSWER, not a crash.

**Test-quality corollary (found 3× in `tests/setup_imag_guards.rs` across both review passes):** a
purely textual `body.contains("some string")` guard can pass even when the real check is gutted,
if that exact substring ALSO appears in unrelated prose (a WARNING fallback echo, a success
banner). Anchor textual guards on the literal FUNCTIONAL invocation (`grep -iE 'the actual
pattern'`), not just "the words appear somewhere" — and for anything with real comparison logic
(not just presence/absence of a string), add a REAL execution test: give the script a
`if [ "${BASH_SOURCE[0]}" != "${0}" ]; then return 0; fi` guard (mirrors
`scripts/launch-obs-genlock.sh`/`scripts/genlock-manifest.sh`/`scripts/drift-guard.sh`), define
pure helper functions BEFORE it, and source it from a Rust test (`tests/genlock_manifest.rs::run_sourced`
is the reference pattern; `tests/setup_imag_pure_functions.rs` is the new sibling for
`setup-imag.sh`). A textual pin proves the code EXISTS; a sourced execution test proves it WORKS —
neither substitutes for the other.

**Fleet disk-space guard (SHIPS DISABLED):** `scripts/cam-disk-guard.sh` + `scripts/lib/disk-guard-thresholds.sh`
+ `systemd/cam-disk-guard.{service,timer}` are committed to the repo but NOT installed/enabled — same
pattern as `systemd/rig-restore-watchdog.{service,timer}` (#281 Fix#3). Enable manually on dev1 when
ready: `systemctl --user enable --now cam-disk-guard.timer`. Checks CAM1/CAM2/CAM4 only (cam3 excluded,
#301). Alert threshold: `CAM_DISK_ALERT_THRESHOLD=80` (env-overridable; pure function in
`scripts/lib/disk-guard-thresholds.sh` makes it Tier-0 unit-testable without SSH).

## #449 — one canonical live-install builder (`scripts/create-usb-linux.sh`)

`scripts/create-ubuntu-vm.sh` (old QEMU-manual install-to-image flow), `scripts/write-image.sh`
(dd a pre-built master image to USB), and `scripts/create-image.sh` (clone a running device's
disk into an image) were deleted (#449, mechanical part) — all three pre-dated the #448 one-shot
`create-usb-linux.sh` installer and were unreachable dead code (confirmed via `git grep`: not
sourced by any script or `scripts/lib/*`, not any script's sole consumer). `SETUP.md` and
`.github/workflows/release.yml` were updated to stop referencing them. Pinned by
`tests/appliance_boot_hardening.rs::dead_image_builders_are_removed` +
`::create_usb_linux_is_the_sole_live_install_builder` — a revert or stray re-add fails CI.

**`scripts/build-image.sh` is DELIBERATELY NOT part of this cleanup.** It is the live, tested
ro-root+overlay builder (`RO_IMAGE_BUILDER`/`IMAGE_BUILDERS[]` in
`tests/appliance_boot_hardening.rs`, `SETUP.md` ro-root section, the "Unified cam-box
provisioning" section above). #449 stays OPEN for a queued user decision on its fate — fold it
into `create-usb-linux.sh` as the sole builder, or keep it as a sanctioned second (RO-ROOT)
builder alongside the live-install one. Do not delete/demote it without that decision.

`.github/workflows/build-image.yml` (a 5th, inline, script-less image-build path) was NOT
retired by #449 — noted as a follow-up.
