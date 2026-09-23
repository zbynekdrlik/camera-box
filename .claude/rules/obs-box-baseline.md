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
  lightdm -> openbox kiosk with the owner's GNOME purge list, touchpad, and the kiosk openbox
  autostart preamble + root-menu printer (`user_bus_alive`/`gs` live here too).
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
- **Adding an item** = a function in the right half + a call in BOTH setup scripts (imag in the step
  that owns it, strih inside step 11 in imag's order) + a verdict row + its gather keys. The
  `gather_and_verdict_share_one_key_set` test fails if the two halves of the grader drift.
- **Autostart CONTENT stays per box** (which supervised OBS unit, which projector layout): imag keeps
  its test-pinned step-16 heredoc, strih-lx uses `strih_openbox_autostart_text`. Both MUST carry the
  preamble lines verbatim and `systemctl --user start <obs unit>` — that is what the verify grades.

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
   effect together. On 26.04 the lowlatency meta also pulls the series' newest HWE image (7.0.0-34
   over the running -31), so the boot runs a NEWER kernel and the NVIDIA DKMS module rebuilds for
   it: confirm `nvidia-smi` and `prime-select query` = nvidia after the boot. OBS starts from
   `~/.config/openbox/autostart` via `strih-obs.service`; the autostart pins both outputs to
   1920x1080@60 (`--auto` only as a fallback).
3. Run `verify-strih.sh`: every `(baseline:*)` row must be PASS, and item 33 (rtprio-off) must PASS
   (setup-strih removed the leftover grant; the reviewer found it still present on the box).

Unverified on the live 26.04 box when this landed (check during the conversion): lightdm/Xorg on
26.04, whether rc-local.service exists and runs /etc/rc.local on 26.04 (the perf row reports it), the kernel 7.0.0-34 boot + DKMS rebuild, the 80 W PL1 under a thermal soak (the guard steps
down to 45 W on a hot TCPU), the 1920x1080@60 panel-primary + HDMI-right-of layout, the new OBS CPU
pin (`/etc/strih-isolated-cpus.conf`, 2-11 on the i5-13450HX — `strih-obs-start.sh` now taskset-pins
OBS where it ran unpinned before; re-measure the render/genlock ladder against the 11b NIC-IRQ E-core),
and whether `strih-mv-host` + the vendored child-host projector are still needed on NVIDIA-primary
Xorg (the XWayland-PRIME present stall they work around should be gone — re-measure, then remove
them if so).
