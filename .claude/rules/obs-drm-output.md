---
paths:
  - "vendor/obs-studio/libobs/obs-drm-output.c"
  - "vendor/obs-studio/libobs/obs-drm-output.h"
  - "vendor/obs-studio/libobs/cmake/os-linux.cmake"
  - "tests/drm_output_lease_1152.rs"
---

# In-OBS vendored DRM-lease HDMI output (#1152) — the forked OBS draws Program onto a DRM-leased connector

Owner KOREKCIA (2026-08-20, supersedes the design spec's NDI-loopback P1): the imag HDMI Program
output must leave the Xorg desktop. The binding shape is a **vendored OBS DRM output** — our forked
OBS acquires DRM master of the HDMI connector through an **X RandR output LEASE**
(`xcb_randr_create_lease`) and page-flips onto it **directly** (`drmModePageFlip`), render→scanout,
with **NO** NDI hop and **NO** external presenter. The NDI-loopback / external-presenter variant is
REJECTED (zero-latency mandate); it survives only as an emergency fallback in the spec appendix.

Milestones: **M1** (done) = lease acquire + solid-color flip from the OBS process (mechanism proof,
NOT bound to the render texture). **M2** = Program texture → dma-buf/EGL export → zero-copy KMS flip.
**M3** = vsync/latency measurement vs today's projector. **M4** = provisioning + `[0/8]` facets.

## Why the module lives in libobs, not a plugin

`linux-genlock.yml` (the FIRST compiler of any vendored change) configures OBS with
`-DENABLE_FRONTEND=OFF -DENABLE_PLUGINS=OFF`, so of the whole OBS it builds only
`libobs`+`obs-frontend-api`+DistroAV. A bundled OBS *plugin* (`plugins/…`) would therefore NOT be
compiled by the CI compile-gate. So a new OBS output that needs CI-first-compile verification MUST
be a file in `libobs/` added via `libobs/cmake/os-linux.cmake`. The deps are already present:
`linux-genlock.yml`'s `OBS_APT_PACKAGES` already lists `libxcb-randr0-dev` + `libdrm-dev` +
`libx11-xcb-dev`, and `FindLibdrm.cmake` is on `CMAKE_MODULE_PATH` (bootstrap.cmake) + `XCB::RANDR`
resolves via ECM's FindXCB. The 3 os-linux.cmake edit sites: `find_package(Libdrm REQUIRED)` +
widen `find_package(XCB … RANDR …)`; add the `.c`/`.h` to `target_sources(libobs …)`; add
`Libdrm::Libdrm`+`XCB::RANDR` to `target_link_libraries(libobs …)`.

## GOTCHA — the X RandR output name ≠ the DRM kernel connector name

`xrandr` shows the output as **`HDMI-1`**; the DRM kernel connector is **`HDMI-A-1`**
(`/sys/class/drm/card1-HDMI-A-1`). The lease is requested by the **X RandR output XID** (selected by
the `HDMI-1` name), and the DRM connector id is discovered from the LEASED fd
(`drmModeGetResources(leasefd)->connectors[0]`), never from the config string. So the config
`connector` value is the **X RandR output name** (`"HDMI-1"`). The pure CRTC-selection
(`drm_output_pick_free_crtc`, Tier-0 truth-tabled) picks a free candidate CRTC to include in the
lease request; on the leased fd there is then exactly one crtc+connector.

The lease fd IS DRM master of the leased objects — so **no `SET_MASTER` ioctl and no fbcon-detach**
(unlike the cam2 full-master path in `src/probe/kms.rs`, which is only the page-flip SEQUENCE
reference here). Linux/EGL/DRM only — strih+stream are libobs-d3d11 where xcb/libdrm/lease don't
exist; the whole TU is under `#if defined(__linux__)` and the obs.c call site is `__linux__`-guarded,
so there is **no `windows-genlock*.yml` pwsh mirror** (nothing to assert on Windows).

## Local verification (CI is the first real type-check, but buy a compile check back)

- **Anchor + lift-compile gate** `tests/drm_output_lease_1152.rs` (std-only, runs via
  `CARGO_MANIFEST_DIR=<abs> rustc --test --edition 2021 tests/drm_output_lease_1152.rs -o /tmp/t && /tmp/t`).
  Facet A source-anchors the mechanism (lease/free_lease/pageflip/`drm-output:`/pick-helper, the
  os-linux.cmake link, the `__linux__`-guarded obs.c autostart + obs_shutdown stop). Facet B lifts
  `drm_output_pick_free_crtc` and cc-compiles it under `-Werror -Wconversion` over a truth table —
  mutate the free-test on a scratch copy to prove it bites (9/10 vectors diverge).
- **fsyntax-only against REAL headers** (stronger than "CI is first compiler"): stub `obs.h`
  (blog with `__attribute__((format(printf,2,3)))` → `-Wformat=2` checks every blog format string;
  stub `os_atomic_{set,load}_bool`/`obs_data_*`), stub `util/c99defs.h` (`#define EXPORT`), copy the
  real `.c` into the stub dir so its `"obs.h"` resolves to the stub, then
  `gcc -fsyntax-only -std=gnu11 -Wall -Wextra -Wformat=2 -Wconversion -I<stub> -I/usr/include/libdrm
  $(pkg-config --cflags xcb xcb-randr libdrm) obs-drm-output.c`. Install `libdrm-dev`/`libxcb-randr0-dev`
  on dev1 if missing. This caught nothing on the clean pass but is the net that would catch a wrong
  xcb/drm signature or a `%zu`/`%llu` mismatch before a wasted CI cycle.

## DEFAULT-OFF activation + M1 rig runbook (SUPERVISOR step, after CI-green + full-bundle deploy)

Activation is a config file, not env (respects the "no env" doctrine): `obs_startup()` calls the
`__linux__`-guarded `obs_drm_output_maybe_autostart()`, which reads `~/.camera-box/drm-output.json`
(`{"enabled":true,"connector":"HDMI-1","argb":2105376}`). File absent → `access()` fails → one
`drm-output: autostart disabled` log line, dormant (zero behaviour change). Rig bring-up:
1. `DISPLAY=:0 xrandr --output HDMI-1 --off` (take HDMI out of the X layout — the spec §5A bring-up
   caveat: lease the IDLE connector, never one X is actively displaying).
2. `mkdir -p ~/.camera-box && printf '{"enabled":true,"connector":"HDMI-1","argb":2105376}\n' > ~/.camera-box/drm-output.json`
3. Restart imag OBS. Verify: `grep 'drm-output:' <obs-log>` → `lease acquired` → `mode set …` →
   `ACTIVE` → `page-flip #1` then ~1/min; HDMI shows solid grey, no X window lands on it, eDP untouched.
4. Rollback: `rm ~/.camera-box/drm-output.json` + restart OBS; `xrandr --output HDMI-1 --auto`.

M1 is SOLID color; the **M2 HOOK** (where the solid fill is replaced by a dma-buf import of the
Program GL texture) is marked in `drm_output_setup_scanout()`. Scanout tearing stays report-only
until the #781 physical HDMI tap; M1 claims only the MECHANISM (leased + page-flipping + armed).

## Lifecycle invariants (locked by the #1152 review — keep them if you touch the module)

- `obs_shutdown()` MUST call `obs_drm_output_stop()` before teardown (flip thread must not outlive
  the log sink; lease returned to Xorg deterministically, not by process death).
- `stop()` claims the transition with a `stopping` flag in the FIRST critical section, then joins
  OUTSIDE the lock — a racing stop returns, a racing start rejects (never double-`pthread_join`).
- The flip loop uses `os_atomic_{set,load}_bool(&running)`; its poll wait breaks on
  `POLLERR/HUP/NVAL` and a failing `drmHandleEvent` (no CPU spin on lease revoke / X restart) and
  emits a wedge WARNING after ~5 s of overdue completions.
