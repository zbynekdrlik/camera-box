---
paths:
  - "scripts/lib/obs-box-baseline.sh"
  - "scripts/lib/obs-box-kiosk.sh"
  - "scripts/lib/obs-box-baseline-verify.sh"
  - "scripts/setup-imag.sh"
  - "scripts/setup-strih.sh"
  - "scripts/verify-imag.sh"
  - "scripts/verify-strih.sh"
  - "scripts/strih-obs-start.sh"
  - "tests/obs_box_baseline_1357.rs"
  - "tests/obs_box_brightness_1357.rs"
---

# The shared OBS-box appliance baseline (issue 1357 scope A)

**Owner rulings (ticket comments 5793367455 + 5793384168):** the imag notebook's provisioning is
the reference starting position. Every Linux OBS box (imag, strih-lx, a future replacement) is the
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
- **rtprio stays OFF in the baseline** (comment 5793075833: the render-tick SCHED_FIFO pin assumed a
  reserved core and its FIFO + affinity leaked to every NDI receiver thread). imag's own issue-484
  limits.d line stays in setup-imag.sh outside the lib (behaviour-identical); setup-strih removes a
  leftover grant and verify-strih item 33 FAILs while one exists.
- **The dpkg lock wait is the FIRST action of every provisioning run.** `obs_box_apt_lock_timeout` writes `/etc/apt/apt.conf.d/90camera-box-lock-timeout` (`DPkg::Lock::Timeout "600";`) right after the root check in BOTH setup scripts, before any apt-get.
  - It exists because a periodic apt run held the lock and failed the strih-lx deploy twice at step 4 (24.9.2026). apt-get's default lock wait is 0.
  - Ubuntu's `Version::2.0::Dpkg::Lock::Timeout` applies only to the `apt` front end, never to apt-get.
  - Never add a per-call `-o DPkg::Lock::Timeout`. A real apt failure must still fail loud after the wait, through a `|| fail` or the caller's `set -e`.
  - The file is written 0644 through a `.dpkg-tmp` sibling, a name apt ignores silently.
  - It covers the DPKG lock only (install/remove/purge). `apt-get update` takes the separate lists lock (`/var/lib/apt/lists/lock`), which this setting does not govern.
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
  its test-pinned step-16 heredoc, strih-lx uses `strih_openbox_autostart_text`. Both MUST carry the
  preamble lines verbatim and `systemctl --user start <obs unit>` — that is what the verify grades.

## python3-websocket (kiosk package install, issue 1361)

Both OBS launchers run a websocket scene seeder (`strih-obs-start.sh` refuses to start OBS without
`python3-websocket`; `imag_scenes.py` imports it). It was only a hand install on strih-lx, so
`obs_box_kiosk` step (a) installs it with the other kiosk packages, and the grader row `websocket`
(after `kiosk`, before `brightness`) is OK only when the package is installed AND the system python3
can `from websocket import create_connection`. setup-imag.sh's own step-11b install stays (a no-op).

## The panel-brightness keys (kiosk facet, issue 1357)

Openbox has no brightness handler, so a notebook's Fn brightness keys did nothing on the kiosk
(strih-lx, 24.9.2026 production; imag the same). `obs_box_kiosk` step (f) runs
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

- **The helper and the rule text are the live strih-lx files byte-for-byte**
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
  - Merging the strih-lx stock rc.xml reproduces its live rc.xml byte-for-byte (checked under mawk,
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
  text changed, and runs `usermod` only when the user is not yet in `video`. The strih-lx genlock
  deploy re-runs setup-strih every time.
- **`obs_box_write_if_changed` details:** an exclusive `mktemp` sibling (never a predictable root
  temp inside a user-writable config dir); content, mode AND owner are all part of the unchanged check.

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
- strih-lx: PL1 = `strih_lx_pl1_watts <firmware µW>` = max(55 W spec base, the firmware's own
  package-0 long_term constraint read at provisioning through the envelope's identity gather). The
  issue-1357 review read **80 W** live on strih-lx (23.9.2026). The envelope reads and pins the
  **MMIO** zone only (`intel-rapl-mmio:*`; the MSR zone is ALSO named `package-0` and can differ), so
  strih keeps whatever the MMIO package-0 long_term reads -- 80 W if that is where the review's number
  came from; the step-11 echo prints `firmware <uW>`, confirm it at the conversion. Pinning the 55 W
  spec base under the firmware would cut the cutter's sustained power by a third (the imag 25 W-clamp
  starvation class). A RE-RUN reads back our own pin, or the guard's step-down on a hot box, so a
  stepped-down read (guard state `stepped`) is discarded and the `IMAG_PL1_W` already baked into
  `imag-power-envelope.service` (`strih_lx_baked_pl1_watts`) is a floor too. The guard step-down is
  `strih_lx_pl1_stepdown_watts` (45 W), passed as `obs_box_power_envelope`'s third argument.
  Overrides: `STRIH_LX_PL1_W`, `STRIH_LX_PL1_STEPDOWN_W`.

## strih-lx conversion (supervisor runbook)

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
3. Run `verify-strih.sh`: every `(baseline:*)` row must be PASS, and item 33 (rtprio-off) must PASS
   (setup-strih removed the leftover grant; the reviewer found it still present on the box).

Live conversion results (strih-lx, 23.9.2026, first kiosk boot on 7.0.0-31 + `preempt=full`):

- Every `(baseline:*)` row and item 33 (rtprio-off) PASS; OBS on the RTX via NVIDIA-primary Xorg,
  program `avg_frame_ms` ~19-20 `lagged=0`, MV 30 fps.
- **power-profiles-daemon 0.30 resets every core's governor to `powersave` when it starts** -- the
  first kiosk boot came up powersave. The max-performance item now stops + masks it and the perf row
  FAILs while it is enabled; `<BOX>-maxperf.sh` writes `platform_profile` itself.
- The kiosk's service-disable list turned **bluetooth** off, which dropped the operator's Bluetooth
  mouse. `obs_box_kiosk ... keep-bluetooth` is the strih-lx box fact (imag keeps the plain call). A
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
