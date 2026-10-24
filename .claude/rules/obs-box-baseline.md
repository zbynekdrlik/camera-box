---
paths:
  - "scripts/lib/obs-box-baseline.sh"
  - "scripts/lib/obs-box-kiosk.sh"
  - "scripts/lib/obs-box-baseline-verify.sh"
  - "scripts/lib/rig-timezone.sh"
  - "scripts/setup-imag.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-imag.sh"
  - "scripts/verify-strih.sh"
  - "scripts/strih-obs-start.sh"
  - "tests/obs_box_baseline_1357.rs"
  - "tests/obs_box_brightness_1357.rs"
  - "tests/obs_box_cpu_latency_1357.rs"
  - "scripts/obs-box-cpu-latency-hold.sh"
  - "systemd/obs-box-cpu-latency.service"
---

# The shared OBS-box appliance baseline (issue 1357 scope A)

**Owner rulings (ticket comments 5793367455 + 5793384168):** the imag notebook's provisioning is
the reference starting position. Every Linux OBS box (imag, strih-snv, a future replacement) is the
SAME OBS-only appliance: lightdm autologin into **openbox on plain Xorg** (no GNOME Shell, no
Wayland/XWayland), the low-latency kernel, the de-jitter masks, max-performance persistence, the
power envelope. A difference between boxes is a defect, not a per-box feature.

## Where it lives

- `scripts/lib/obs-box-baseline.sh` — the ONE entry point both setup scripts source. The system
  half: pure helpers (`obs_box_cpu_isolation_plan`, `obs_box_has_discrete_nvidia`,
  `obs_box_same_unit`, `obs_box_kernel_series`), `safe_grub_regen`, and one function per item:
  network tuning, max-performance (governor + rc.local), boot safety net, preempt=full lowlatency
  kernel, AFFINITY-ONLY core reservation, NVIDIA 595-open + PRIME nvidia-primary, power envelope,
  maxperf persistence. It sources its kiosk half:
- `scripts/lib/obs-box-kiosk.sh` — never-sleep, de-jitter + the crash-popup item, the
  lightdm -> openbox kiosk with the owner's GNOME purge list (plus its panel-brightness keys facet),
  touchpad, and the kiosk openbox autostart preamble + root-menu printer (`user_bus_alive`/`gs` live
  here too). `obs_box_write_if_changed` (the compare-then-rewrite install) lives in the system half.
- `scripts/lib/obs-box-baseline-verify.sh` — the ONE grader: `obs_box_baseline_gather_snippet`
  (read-only bash, runs as any user, locally or over ssh) + the pure `obs_box_baseline_verdict`
  (one `item|OK|FAIL|detail` row per item, fail-closed on a gather that never completed). verify-imag
  check `(bb)` and verify-strih item 32 both run it.

## Conventions (keep them when adding an item)

- **Bodies moved VERBATIM, column-0.** The function bodies keep setup-imag.sh's layout so every
  heredoc writes byte-identical files for imag. Bind the original variable names as `local` from the
  arguments (`local NIC="$1" BOX="$2"`) instead of rewriting the body.
- **Box facts are arguments, never imag literals.** BOX (file/unit name prefix: `imag` | `strih`),
  desktop user, NIC, SERIES (from `/etc/os-release` via `obs_box_kernel_series`), OBS config dir,
  PL1 watts, FETCH (installer function). Generated files whose quoted heredoc must keep `$` literal
  carry an `@BOX@` placeholder expanded by `sed "s/@BOX@/${BOX}/g" > file <<'EOF'`.
- **The power-envelope tool + unit names stay `imag-power-envelope*` on every box.** The shared
  gather/verdict lib `scripts/lib/imag-power-envelope.sh` grades exactly those names; renaming them
  fleet-wide is its own ticket, not a baseline edit.
- **rtprio stays OFF on every box** (comment 5793075833: the render-tick SCHED_FIFO pin assumed a
  reserved core and its FIFO + affinity leaked to every NDI receiver thread). `obs_box_rtprio_off`
  (step 8 on imag, step 11 on strih) removes every retired `95-<box>-genlock-rtprio.conf` grant, and
  the grader row `rtprio` FAILs while one exists, or while the running OBS still holds a non-zero
  realtime limit from a grant removed after it started. imag's issue-484 grant was DROPPED, not gated: the
  pin now serves only isolated nohz_full cores, which the `affinity` row forbids, so a gated grant
  could never fire. Full story: `genlock-render-tick-pin.md`.
- **The dpkg lock wait is the FIRST action of every provisioning run.** `obs_box_apt_lock_timeout` writes `/etc/apt/apt.conf.d/90camera-box-lock-timeout` (`DPkg::Lock::Timeout "600";`) right after the root check in BOTH setup scripts, before any apt-get.
  - It exists because a periodic apt run held the lock and failed the strih-snv deploy twice at step 4 (24.9.2026). apt-get's default lock wait is 0.
  - Ubuntu's `Version::2.0::Dpkg::Lock::Timeout` applies only to the `apt` front end, never to apt-get.
  - Never add a per-call `-o DPkg::Lock::Timeout`. A real apt failure must still fail loud after the wait, through a `|| fail` or the caller's `set -e`.
  - The file is written 0644 through a `.dpkg-tmp` sibling, a name apt ignores silently.
  - It covers the DPKG lock only (install/remove/purge). `apt-get update` takes the separate lists lock (`/var/lib/apt/lists/lock`), which this setting does not govern.
  - It is an APT setting: a bare `dpkg -i` never reads it and fails at once on a held lock. Install a local `.deb` with `apt-get install -y <absolute path>.deb` (issue 1440's shim).
- **Every list refresh is `obs_box_apt_update`, never a bare `apt-get update`** (main ruling 5821855428).
  - It retries ONLY while the lists lock is HELD (`Could not get lock /var/lib/apt/lists/lock`, the same text on apt 2.8 and 3.2), logs each wait, and gives up after `OBS_BOX_APT_UPDATE_BUDGET_S` (600 s) total.
  - apt runs under `LC_ALL=C`. The match reads apt's English text, and an ssh session forwards the operator's locale; apt ships translations.
  - The wait log names the lock line itself, because apt can print `W:` lines before it. Every wait sleeps at least 1 s, never a busy loop.
  - Any other error fails loud at once, with apt's output shown. A permission failure prints `Could not open lock file` and is not retried.
  - `add-apt-repository` always passes `-n`, so it never refreshes the lists itself.
  - `no_bare_apt_get_update_on_the_obs_box_provisioning_paths` sweeps a hand-kept list of files. It covers both setup scripts, the libs they source, and the libs sourced one level down (`obs-fleet.sh`, `camera-set.sh`). It flags any `apt`/`apt-get` with an `update` sub-command, whatever the flag order. A new lib on either path must be added to that list.
- **Adding an item** = a function in the right half + a call in BOTH setup scripts (imag in the step
  that owns it, strih inside step 11 in imag's order) + a verdict row + its gather keys. The
  `gather_and_verdict_share_one_key_set` test fails if the two halves of the grader drift.
- **Autostart CONTENT stays per box** (which supervised OBS unit, which projector layout): imag keeps
  its test-pinned step-16 heredoc, strih-snv uses `strih_openbox_autostart_text`. Both MUST carry the
  preamble lines verbatim and `systemctl --user start <obs unit>` — that is what the verify grades.

## python3-websocket (kiosk package install, issue 1361)

Both OBS launchers run a websocket scene seeder (`strih-obs-start.sh` refuses to start OBS without
`python3-websocket`; `imag_scenes.py` imports it). It was only a hand install on strih-snv, so
`obs_box_kiosk` step (a) installs it with the other kiosk packages, and the grader row `websocket`
(after `kiosk`, before `brightness`) is OK only when the package is installed AND the system python3
can `from websocket import create_connection`. setup-imag.sh's own step-11b install stays (a no-op).
`xinput` rides the same package line (issue 1418): verify-strih item 43 reads the LIVE touchpad
tapping value and the X input devices with it, and a missing tool is named there, never read as a hung X.

## The panel-brightness keys (kiosk facet, issue 1357)

Openbox has no brightness handler, so a notebook's Fn brightness keys did nothing on the kiosk
(strih-snv, 24.9.2026 production; imag the same). `obs_box_kiosk` step (f) runs
`obs_box_brightness_keys "$DESKTOP_USER"`, so BOTH setup scripts get it with no extra call site:

- `/usr/local/bin/obs-box-brightness up|down` (root 0755): the first sysfs backlight, a tenth of
  `max_brightness` per step (at least 1), clamped to max and to a 5 % floor; exit 2 on a bad argument,
  exit 1 (logged) on a box with no backlight.
- `/etc/udev/rules.d/90-obs-box-backlight.rules`: `chgrp video` + `chmod g+w` on every backlight node
  at `add`, then `udevadm control --reload-rules` + `udevadm trigger --subsystem-match=backlight
  --action=add`. A failed trigger is a WARNING (the rule still applies at the next boot).
- The desktop user in group `video` (`usermod -aG`).
- The two `XF86MonBrightnessUp/Down` keybinds in `~/.config/openbox/rc.xml`.

Rules for the facet:

- **The helper and the rule text are the live strih-snv files byte-for-byte**
  (`obs_box_brightness_helper_text`, `obs_box_backlight_udev_rule`, sha256 checked 25.9.2026).
  Change them only together with the box.
- **rc.xml is MERGED, never replaced.** `obs_box_openbox_rc_with_brightness_keys` is a pure stdin
  filter and the kiosk's only rc.xml writer.
  - It inserts only the missing lines of `obs_box_brightness_keybinds_xml` (the comment only when a
    keybind is missing) right before the first `</keyboard>` line.
  - An rc.xml that already has both keybinds comes back unchanged. Without `</keyboard>` it returns 1
    with NO output, and the install fails loud.
  - The source is the user's rc.xml when present, else the stock `/etc/xdg/openbox/rc.xml` (openbox
    reads the user file first). The merged text is written through `obs_box_write_if_changed`.
  - Merging the strih-snv stock rc.xml reproduces its live rc.xml byte-for-byte (checked under mawk,
    Ubuntu's default awk).
  - This is additive, and the only exception to verify-imag's "never rewrite operator rc.xml"
    doctrine (issue 1095). Operator content and the Root right-click binding stay untouched, so that
    reachability check keeps its meaning.
- **Every file goes through `obs_box_write_if_changed`** (obs-box-baseline.sh): compared, rewritten
  only on a content, mode or owner difference, logged `unchanged` / `written`, one atomic rename. It is the
  shared install for rendered config files (setup-strih step 12 uses it for the audio drop-ins).
- **Grader row `brightness`** (after `kiosk`, before `autostart`). It is OK only when all four hold:
  - the helper is executable AND byte-identical to `obs_box_brightness_helper_text`;
  - the udev rule is byte-identical to `obs_box_backlight_udev_rule`;
  - every keybind line of `obs_box_brightness_keybinds_xml` is in the rc.xml openbox loads. The check
    counts the renderer's lines and FAILs when there are none, so a missing or broken renderer is
    never a pass;
  - the desktop user is in `video`.

  The gather embeds all three renderers via `declare -f`, so the grader and the install read the same
  text. `tests/obs_box_brightness_1357.rs` runs the real gather against fixture files by rewriting
  only its three paths. A box provisioned before this facet FAILs the row until setup re-runs (imag
  included).
- **Re-runs stay quiet.** `obs_box_brightness_keys` reloads and re-triggers udev only when the rule
  text changed, and runs `usermod` only when the user is not yet in `video`. The strih-snv genlock
  deploy re-runs setup-strih every time.
- **`obs_box_write_if_changed` details:** an exclusive `mktemp` sibling (never a predictable root
  temp inside a user-writable config dir); content, mode AND owner are all part of the unchanged check.

## lightdm waits for every display GPU (kiosk facet, issue 1454)

strih-snv booted to the lightdm LOGIN SCREEN on 21.10.2026 (owner: no login prompt, ever, on any
Linux rig). lightdm's autologin X server started 0.5 s after the amdgpu iGPU finished its DRM init,
before udev had finished that card; X added it later as a hotplugged GPU screen and Xorg 21.1 aborted
in `glamor_init` (`dixRegisterPrivateKey: Assertion '!global_keys[type].created' failed`, an Xorg core
dump), so lightdm fell back to the greeter. The autologin config was correct; it is a boot race, and
any dGPU + iGPU notebook has it.

- `obs_box_kiosk` runs `obs_box_gpu_ready_install`: `/usr/local/sbin/obs-box-gpu-ready-wait` (0755)
  and `/etc/systemd/system/lightdm.service.d/50-obs-box-gpu-ready.conf` (`ExecStartPre=<waiter> 30`),
  both through `obs_box_write_if_changed`, then a daemon-reload (never a lightdm restart).
- The waiter: every PCI display-class device (sysfs class `0x03*`) needs a DRM card whose udev record
  is finished (`USEC_INITIALIZED`; the card directory alone is not enough, X enumerates from the udev
  database), then one `udevadm settle`. Bounded in microseconds (`EPOCHREALTIME`; `SECONDS` ended a
  1 s budget early), never fatal: past the bound one WARNING names the pending GPU and lightdm starts.
  Its journal line (`journalctl -b -u lightdm`) says how long it waited.
  Every `udevadm` call is under `timeout` (a wedged udevd must never hold lightdm past systemd's 90 s
  start timeout). A display device that never gets a driver costs the full 30 s + a WARNING each boot;
  devices without a bound driver are NOT skipped, because the late iGPU binds its driver late too.
- Rejected: `AutoAddGPU false` alone (no abort, but a late iGPU never joins and the panel stays
  black); amdgpu in the initramfs (narrows the race, does not close it).
- Grader row `kiosk` also needs `gpu_wait=1` (both files byte-identical to the renderers, embedded by
  `declare -f`) and `x_core=0`: the crashed Xorg PIDs (`terminated abnormally` or `dumped core`, each
  PID once) in this boot's systemd-coredump lines. A journal the gather cannot read (no PID 1 line this
  boot) prints `x_core=` empty and FAILs, never 0. The boot of 21.10.2026 FAILs the row until the box
  boots again with the wait.
- Tests: `tests/python/test_obs_box_gpu_ready_1454.py` (the waiter against a fake sysfs + udevadm:
  ready, late card, card udev has not finished, never-ready bound, non-display devices, bad budget;
  the install; the real gather with its paths moved; the row), the `gpu_wait` / `x_core` rows in
  `tests/obs_box_baseline_1357.rs`.

## The iGPU panel sink on the modesetting DDX (item `obs_box_igpu_sink_modesetting`, grader row `igpu-sink`, issue 1418)

On an NVIDIA-primary notebook (PRIME nvidia-primary) NVIDIA renders the X screen and the iGPU scans
the panel out as a reverse-PRIME SINK. On strih-snv (AMD Radeon 780M) Xorg ran that sink on
xf86-video-amdgpu, and the panel's `commit_work` blocked forever on an NVIDIA fence on 11/12.10.2026:
the panel froze for 18 h while OBS kept rendering. The returned Intel notebook had the same topology
with the sink on the Xorg `modesetting` DDX and was stable (ROZHODNUTE 6132283178).

**DEAD END, do not retry: an xorg.conf.d OutputClass does not move the sink.** An
`/etc/X11/xorg.conf.d` `OutputClass` with `MatchDriver "amdgpu"` + `Driver "modesetting"` was tried
live on strih-snv (12.10.2026) as `20-`, as `05-`, and as a same-named `10-amdgpu.conf` shadow. Every
time Xorg still applied the package's own `/usr/share/X11/xorg.conf.d/10-amdgpu.conf`
(`OutputClass "AMDgpu"`, `Driver "amdgpu"`) first and loaded the amdgpu DDX.

**What works: remove the DDX package.** `apt-get remove xserver-xorg-video-amdgpu
xserver-xorg-video-all` (the meta package is the DDX's only reverse dependency, and the DDX package
owns that 10-amdgpu.conf). Xorg then logs `Failed to load module "amdgpu"` (harmless) and
`modeset(G0): glamor X acceleration enabled on AMD Radeon 780M`; `xrandr --listproviders` reads
`Provider 1: ... Sink Output, Source Offload ... name:modesetting`. The panel output is renamed
(eDP-1-1 -> eDP-1-2); the kiosk autostart resolves the panel dynamically, so that is fine.

- **Install, in TWO places** (the functions live in the kiosk half; the system half is at its line
  budget):
  - `obs_box_kiosk`'s package install (`apt-get install ... xserver-xorg ...`) is followed by
    `obs_box_amdgpu_ddx_remove` on a discrete-NVIDIA box. On a fresh 26.04 box that install brings
    xserver-xorg, whose `xserver-xorg-video-all | xorg-driver-video` dependency pulls the amdgpu DDX
    back in AFTER the NVIDIA step (which runs first in both setup scripts) removed it. Without this a
    fresh box needs a second setup run (re-review finding);
  - `obs_box_nvidia_prime` calls `obs_box_igpu_sink_modesetting` right after `prime-select nvidia`,
    inside the discrete-NVIDIA branch only (an iGPU-only box gets nothing). It:
    - removes the ones of `obs_box_amdgpu_ddx_packages` that are on the box (`xserver-xorg-video-amdgpu`,
      `xserver-xorg-video-all`) through `obs_box_amdgpu_ddx_remove`: an `apt-get -s remove` dry run
      first, and a removal that would take ANY other package (xserver-xorg, lightdm) is REFUSED by name,
      never done; nothing installed = nothing touched; an apt error fails loud, and so does a
      `dpkg-query` that cannot answer (`obs_box_pkg_status`: a status, `absent`, or rc 1; any unpacked
      state, half-installed included, counts as present). No list refresh is needed; the dpkg lock wait is
      `obs_box_apt_lock_timeout`'s;
    - still writes `/etc/X11/xorg.conf.d/20-igpu-sink-modesetting.conf` (the OutputClass, belt and
      braces, its own comment says so) through `obs_box_write_if_changed` (0644 root:root). Never rely
      on it alone.
  - Both take effect at the next X start (which restarts OBS).
- **Grader row `igpu-sink`** (right after `gpu`): on a dGPU box it is OK only when
  `obs_box_pkg_status` of `obs_box_amdgpu_ddx_package` reads `absent` / `... not-installed` /
  `... config-files` (gather key `amdgpu_ddx`; `declare -f` embeds both functions). Any unpacked state
  FAILs, and an empty (unread) status FAILs: never a pass. An
  iGPU-only box is OK whatever is installed (no reverse-PRIME sink). A dGPU box provisioned before this
  item with the DDX installed FAILs the row until setup re-runs. Verify-strih's operator item 43
  grades the EFFECT, the running sink provider's DDX name (`(panel-gpu)`, `strih-operator.md`).
- **Tests:** `tests/python/test_obs_box_igpu_sink_1418.py` runs the real remover with `dpkg-query` /
  `apt-get` shadowed by functions (none / both / one installed, an over-wide removal refused, an apt
  failure), the real `obs_box_nvidia_prime` with its host tools stubbed, the real gather with
  `dpkg-query` shadowed, and the verdict; `tests/obs_box_baseline_1357.rs` carries the row in ITEMS
  and the broken-fact table.
- **Live (supervisor):** strih-snv was fixed by hand on 12.10.2026 (packages removed, the OutputClass
  file left). The next setup-strih run there logs `not installed -- unchanged` and rewrites the
  OutputClass file to the baseline text. Before the next deploy, read `apt-get -s remove
  xserver-xorg-video-amdgpu xserver-xorg-video-all` (read-only) on imag-nb and strih-snv: a leftover
  meta that depends on the meta (an old desktop package) makes the dry run refuse and stops setup at the
  NVIDIA step by design. On any other NVIDIA box, after setup + an X restart,
  `xrandr --listproviders` must name the sink `modesetting`; then soak overnight and read `(drm-hang)`.

## Swap (item `obs_box_swap`, grader row `swap`, issue 1428)

Owner 14.10.2026 (verbatim): "To ze tam nieje swap je hrube a opakovane porusenie pravidiel pre linux
boxy". stream-pp, strih-pp and strih-snv ran with NO swap; on stream-pp the OOM killer took the strih
OBS browser source and the restreamer (14.10.2026 09:37).

- **Install:** `obs_box_swap` (`scripts/lib/obs-box-swap.sh`, sourced by the baseline: the system half is at its line budget) creates `/swapfile` = RAM rounded up to whole GiB
  (`fallocate`, chmod 600, `mkswap`), `swapon` when not active, ONE `/swapfile none swap sw 0 0` line
  in `/etc/fstab`. Idempotent. A root that is not ext4 fails loud and writes nothing. Both setups call
  it on the `obs_box_rtprio_off ;` line (setup-strih is at its 1000-line budget; keep the space before
  `;` -- tests anchor `\nobs_box_rtprio_off `).
- **Grader row `swap`** (last): active swap >= 90 % of RAM AND an fstab swap line; an unread count
  FAILs. Gather keys `swap_bytes` / `mem_bytes` / `swap_fstab`.
- **Camboxes are NOT this item:** read-only root on a stick, so their swap is zram (RAM-backed, no
  disk writes) from setup-device / create-usb, graded by verify-device.
- Tests: `tests/python/test_obs_box_swap_1428.py` (stubbed tools, path seams `OBS_BOX_SWAPFILE` /
  `OBS_BOX_FSTAB` / `OBS_BOX_MEMINFO`), the `swap` rows in `tests/obs_box_baseline_1357.rs`.

## The rig time zone (item `obs_box_timezone`, grader row `timezone`, issue 1415)

Every fresh box used to run the image default `Etc/UTC`, so the browser-source clocks (Camera info,
Interkom, Odpocet) ran 2 h behind. The zone is ONE declaration, `RIG_TIMEZONE` in
`scripts/lib/rig-timezone.sh` (Europe/Bratislava), shared with setup-device.sh / create-usb-linux.sh
/ verify-device.sh `(as)`. Never retype it.

- **Install:** `obs_box_timezone` (kiosk half, which sources the lib) runs in STEP 1 of BOTH setup
  scripts (imag's static-IP step, strih's static-IP + hostname step), not in step 11. It installs
  tzdata when the zoneinfo file is missing, then `rig_timezone_apply`.
- **Never `timedatectl set-timezone`.** The writer writes the `/etc/localtime` symlink +
  `/etc/timezone` itself, and every reader reads `readlink /etc/localtime`. On a read-only-root
  cambox systemd-timedated keeps its own mount namespace, so set-timezone fails inside a correct rw
  window and `timedatectl show -p Timezone` then reports a zone the link does not have (live
  12.10.2026). `timedatectl set-ntp false` (setup-imag, dantesync owns the clock) is a different call
  and stays.
- **Grader row `timezone`** (FIRST, before `net`): the gather embeds `rig_timezone_read` by
  `declare -f` and prints `timezone=<zone>`; the verdict is `rig_timezone_verdict`. A copied
  `/etc/localtime` file (no link) reads empty and FAILs.
- `RIG_TIMEZONE` is the one override for a box outside Slovakia.
- **A zone change needs a restart of what runs.** glibc `localtime_r` (and so Python) and OBS's CEF
  sources keep the zone they loaded. When `obs_box_timezone` changed the zone it prints a note: reboot,
  or restart the OBS unit, the :8899 bundle-state server and the session apps TOGETHER. A restarted
  OBS beside a UTC bundle-state server reads its own log ~2 h old (`obs_log_head_age_s`), which the
  audio-mixer watchdog reads as STALE / a CLOCK page. The strih-snv genlock deploy restarts OBS only.
- **The swap is atomic:** the link and `/etc/timezone` are built as `*.rig-new` beside their names and
  `mv -Tf`'d over them, so a live reader never sees a missing `/etc/localtime` (glibc: UTC).
- **Known side effect, accepted:** the `apt-daily-upgrade` off-hours drop-in (`OnCalendar=04:00`,
  local time) now fires at 04:00 Bratislava = 02:00-02:30Z in summer, the strih-snv dantesync nightly
  date-step window. 04:00 local is still off-hours; apt is not timing-sensitive.

## The CPU idle wake-up latency bound (item `obs_box_cpu_latency`, grader row `cstate`)

strih-snv runs the ACPI idle driver: POLL / C1_ACPI 1 us / C2_ACPI 127 us / C3_ACPI 1048 us, and C3
was entered ~106 M times on cpu0 (finding 5839598285). A ~1 ms wake-up can land on the genlock
render tick, the audio thread and the NDI receive threads. The governor/EPP pins do not touch this.

- **Mechanism = a PM QoS request, never a C-state index.** `systemd/obs-box-cpu-latency.service`
  (a SYSTEM unit) runs `/usr/local/sbin/obs-box-cpu-latency-hold.sh ${OBS_BOX_CPU_LATENCY_US}`.
  - The holder opens `/dev/cpu_dma_latency`, writes the bound, then `exec sleep infinity`. The
    sleep inherits fd 3, and the kernel honours the request while that fd stays open.
  - Every cpuidle governor then skips the states whose exit latency is longer. Stopping the unit
    drops the request: reversible, no reboot.
  - Rejected: a kernel cmdline max_cstate (reboot, GRUB edit, and the ACPI driver ignores
    `intel_idle.*`), and `/sys/.../stateN/disable` (a state INDEX that differs per box and can be
    renumbered by a new kernel).
- **The bound is ONE baseline value**, `obs_box_cpu_latency_bound_us` = 150 us (C1/C2 stay, C3 is
  out). The unit's `Environment=OBS_BOX_CPU_LATENCY_US=150` is test-pinned to it; never per box.
- **Write format trap.** The kernel reads a write of EXACTLY 4 bytes as a raw binary s32, and any
  other length as a hex number. The holder writes `printf '0x%08x'` (10 bytes).
  - A short hex string like `0x96` is 4 bytes, so it would become a huge garbage bound.
  - The holder also strips leading zeros with `10#`, so `0150` never reads as octal.
- **Install:** `obs_box_cpu_latency FETCH`, after `obs_box_maxperf_persistence` in BOTH setup
  scripts (imag step 26 with `imag_fetch_repo_file`, strih step 11 with `strih_fetch_repo_file`).
  - It fetches the holder and the unit from the repo, then runs daemon-reload and enable.
  - It restarts when the installed text changed, else starts, then fails loud unless the unit is
    active. The apply is the baseline's `enable --now` pattern.
  - It fails before touching anything when `/dev/cpu_dma_latency` is missing.
  - imag fetches `?ref=dev`, so the files must be on dev before an imag re-provision.
- **`Type=notify` + `NotifyAccess=all`.** The holder runs `systemd-notify --ready` only after the
  bound is written (only when `NOTIFY_SOCKET` is set). So `systemctl start` returns once the bound is
  held, and a holder that cannot open/write the device FAILS the start. With `Type=simple` the
  installer's `is-active` check passed a holder that died a moment later. Checked on dev1's systemd
  255 with a transient `systemd-run --user -p Type=notify -p NotifyAccess=all` unit: a scratch device
  started `active`/`NRestarts=0`, an unopenable one failed the start.
- **First install under errexit.** The before/after text hash reads files that do not exist yet on a
  first install. It is written `{ cat … || true; } | sha256sum`; the bare pipeline aborted setup
  silently under the callers' `set -euo pipefail`. `tests/obs_box_cpu_latency_1357.rs` runs the real
  installer under `set -euo pipefail` (paths moved into a temp dir, a logging `systemctl` stub).
- **Unit hardening:** `DevicePolicy=closed` + `DeviceAllow=/dev/cpu_dma_latency w`,
  `CapabilityBoundingSet=` (root owns the 0600 device, no capability needed),
  `RestrictAddressFamilies=AF_UNIX` (the notify socket), `SystemCallArchitectures=native`. Never
  `PrivateDevices=`, which would hide the device.
- **imag acceptance (supervisor).** imag runs intel_idle, where 150 us also keeps out C6/C8/C10
  (dev1, the same Intel family, lists C6 at 195 us). Idle cores then sit in C1E: more idle power
  inside the imag PL1 envelope. Read the RAPL package power + TCPU and the power-envelope guard
  state before/after the first imag install.
- **Grader row `cstate` (after `perf`).** It runs unprivileged, and the device is root-only 0600,
  so it grades the unit AND the kernel effect.
  - The unit must be `enabled` + `active`, and its `Environment=` bound (read via
    `systemctl show -p Environment`) must equal the baseline constant.
  - At least one cpuidle state must be readable.
  - The summed `usage` of every state whose `latency` exceeds the bound must not advance over a
    1 s sample (`cstate_deep_delta=0`).
  - `/sys/.../cpu0/power/pm_qos_resume_latency_us` is NOT this aggregate; it is the per-device
    resume latency, so never grade it.
  - The deepest state stays LISTED and enabled while the bound is held (the governor just skips it),
    so "deepest enabled state <= bound" would FAIL a healthy box.
  - dev1 (no holder) reads ~1400 deep entries per second, so the signal discriminates.
  - The gather's `_cs_root` path and its single `sleep 1` line are the test seams:
    `tests/obs_box_cpu_latency_1357.rs` rewrites the path to a fixture tree and swaps the sleep
    for a counter write, so it never races the gather's timing.

## Tests + the Tier-0 equivalence net

The imag content guards (`setup_imag_guards.rs`, `setup_imag_maxperf_791.rs`, ...) read
setup-imag.sh PLUS the baseline libs; box-literal anchors are the `${BOX}` forms plus an anchor that
setup-imag passes `imag`. The equivalence proof used on the move: extract every generated heredoc
from the pre-move setup-imag.sh and from the libs rendered with `BOX=imag`, and diff them (only two
comment lines differ). Re-run it for any further move. Before trusting a moved anchor, run the
occurrence-count sweep (old vs new) AND an order-preservation check over each test's `.find` pairs
(a first-occurrence anchor can move into a lib and flip an ordering assertion — the two issue-504
kiosk tests are scoped to `obs_box_kiosk` for exactly that reason).

## The grader is the shared contract

verify-imag check `(bb)` and verify-strih item 32 run the SAME `obs_box_baseline_verdict`, so a
row that passes on one box means the same thing on the other. It grades what the baseline WRITES,
not only what it says it did: the rc.local EEE hook, the apt kernel holds (a generic kernel package
+ `lowlatency-kernel`), the iGPU max-frequency pin unit on an iGPU-only box, OBS
`ProcessPriority=High` in global.ini, every `obs_box_dejitter_user_units` member masked for the
desktop user (a `--user` mask silently no-ops when the user bus is down, which is exactly why it is
graded), and lightdm + openbox installed.

## Per-box power facts

- imag: PL1 `${IMAG_PL1_W:-45}`, guard step-down 25 W (the historic iGPU values).
- strih-snv: PL1 = `strih_lx_pl1_watts <firmware µW>` = max(55 W spec base, the firmware's own
  package-0 long_term constraint read at provisioning through the envelope's identity gather). The
  issue-1357 review read **80 W** live on strih-snv (23.9.2026). The envelope reads and pins the
  **MMIO** zone only (`intel-rapl-mmio:*`; the MSR zone is ALSO named `package-0` and can differ), so
  strih keeps whatever the MMIO package-0 long_term reads -- 80 W if that is where the review's number
  came from; the step-11 echo prints `firmware <uW>`, confirm it at the conversion. Pinning the 55 W
  spec base under the firmware would cut the cutter's sustained power by a third (the imag 25 W-clamp
  starvation class). A RE-RUN reads back our own pin, or the guard's step-down on a hot box, so a
  stepped-down read (guard state `stepped`) is discarded and the `IMAG_PL1_W` already baked into
  `imag-power-envelope.service` (`strih_lx_baked_pl1_watts`) is a floor too. The guard step-down is
  `strih_lx_pl1_stepdown_watts` (45 W), passed as `obs_box_power_envelope`'s third argument.
  Overrides: `STRIH_LX_PL1_W`, `STRIH_LX_PL1_STEPDOWN_W`.

## strih-snv conversion (supervisor runbook)

1. Stop OBS (`strih-obs-stop.sh`), then run `setup-strih.sh` from a checkout (it copies the
   power-envelope tools from the repo). The kiosk item purges GNOME and switches the display manager
   to lightdm, so the running GNOME session will not survive it — never re-run it expecting the
   GNOME desktop to keep working; reboot right after it finishes. Its final step only REPORTS the
   verify while `strih_lx_reboot_pending` (lowlatency drop-in present, no running `preempt=full`).
2. Reboot once: the kernel, PRIME nvidia-primary and the lightdm -> openbox Xorg session all take
   effect together. The lowlatency config meta is pinned to the installed generic-hwe version, so
   the boot stays on the running HWE kernel. Confirm `nvidia-smi` and `prime-select query` = nvidia after the boot. OBS starts from
   `~/.config/openbox/autostart` via `strih-obs.service`; the autostart pins both outputs to
   1920x1080@60 (`--auto` only as a fallback).
3. Run `verify-strih.sh`: every `(baseline:*)` row must be PASS, including `rtprio`
   (setup-strih removed the leftover grant; the reviewer found it still present on the box).

Live conversion results (strih-snv, 23.9.2026, first kiosk boot on 7.0.0-31 + `preempt=full`):

- Every `(baseline:*)` row and the then-separate item 33 (rtprio-off, now the baseline `rtprio` row) PASS; OBS on the RTX via NVIDIA-primary Xorg,
  program `avg_frame_ms` ~19-20 `lagged=0`, MV 30 fps.
- **power-profiles-daemon 0.30 resets every core's governor to `powersave` when it starts** -- the
  first kiosk boot came up powersave. The max-performance item now stops + masks it and the perf row
  FAILs while it is enabled; `<BOX>-maxperf.sh` writes `platform_profile` itself.
- The kiosk's service-disable list turned **bluetooth** off, which dropped the operator's Bluetooth
  mouse. `obs_box_kiosk ... keep-bluetooth` is the strih-snv box fact (imag keeps the plain call). A
  paired + trusted mouse that stays `Connected: no` with the adapter powered is asleep / on another
  Easy-Switch channel -- a click reconnects it; nothing on the box side to fix.
- The lowlatency config meta must be installed at the INSTALLED generic-hwe version (an unpinned
  install pulled a newer HWE image and conflicted); `obs_box_lowlatency_kernel` pins it.
- MV projector on Xorg: iconified (`WM_STATE Iconic`) keeps `rendered_fps=30` (GNOME/XWayland fell to
  7 fps), and a 40-step drag-resize keeps program `lagged=0`. The `strih-mv-host` helper and the
  vendored child-host projector (both XWayland + PRIME workarounds) are retired on this baseline
  (issue 1357): the stock toplevel projector runs on every box.
- verify item 6 (dantesync offset) can FAIL `unstable` on the fleet NTP master: its upstream is an
  internet NTP server (`ntp_server` 162.159.200.1), so the spread tracks WAN jitter (dev1 saw ping
  mdev 1.6 ms to the same server at the same time). The PTP lock (`mode=LOCK`) is the fleet signal;
  compare against dev1's ping jitter to the upstream before treating it as a box regression.
