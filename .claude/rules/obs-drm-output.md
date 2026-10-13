---
paths:
  - "vendor/obs-studio/libobs/obs-drm-output.c"
  - "vendor/obs-studio/libobs/obs-drm-output.h"
  - "vendor/obs-studio/libobs/cmake/os-linux.cmake"
  - "tests/drm_output_lease_1152.rs"
  - "tests/drm_output_program_1152.rs"
  - "tests/drm_lease_tolerance_1152.rs"
  - "scripts/imag-obs-start.sh"
  - "scripts/obs_phase2.py"
  - "tests/python/test_obs_phase2_open_projectors_lease_1152.py"
  - "scripts/recording-e2e.sh"
  - "scripts/verify-imag.sh"
  - "scripts/lib/imag-projector-lease-count.sh"
  - "tests/harness_imag_projector_lease_count_1152.rs"
  - "tests/harness_verify_imag_lease_count_1152.rs"
  - "vendor/obs-studio/libobs/obs-drm-output-view.c"
  - "vendor/obs-studio/libobs/obs-drm-output-internal.h"
  - "vendor/obs-studio/frontend/components/DrmOutputView.cpp"
  - "vendor/obs-studio/frontend/components/DrmOutputView.hpp"
  - "tests/drm_output_view_1346.rs"
  - "tests/drm_output_view_mv_budget_1346.rs"
  - "vendor/obs-studio/libobs/obs-drm-output-backend.c"
  - "vendor/obs-studio/libobs/obs-drm-output-vk.c"
  - "vendor/obs-studio/libobs/obs-drm-output-vk.h"
  - "vendor/obs-studio/libobs/obs-drm-output-vk-internal.h"
  - "vendor/obs-studio/libobs/obs-drm-output-vk-setup.c"
  - "tests/drm_output_vk_direct_1346.rs"
  - "tests/c/drm_output_vk_rig_harness.c"
  - "vendor/obs-studio/libobs/obs-drm-output-audio.c"
  - "vendor/obs-studio/libobs/obs-drm-output-audio.h"
  - "vendor/obs-studio/libobs/obs-drm-output-audio-match.h"
  - "src/drm_output_audio_match.rs"
  - "tests/drm_output_audio_1424.rs"
  - "tests/drm_output_vk_program_mode_1424.rs"
  - "scripts/lib/drm-output-audio.sh"
  - "tests/python/test_drm_output_audio_1424.py"
  - "vendor/obs-studio/libobs/obs-drm-output-outputs.c"
  - "tests/drm_output_outputs_1424.rs"
  - "tests/fixtures/drm_output_outputs_parity.tsv"
  - "tests/python/test_drm_output_outputs_1424.py"
  - "scripts/lib/strih-drm-output.sh"
  - "scripts/drm_output_config.py"
---

# In-OBS vendored DRM-lease HDMI output (#1152) — the forked OBS draws Program onto a DRM-leased connector

Owner KOREKCIA (2026-08-20, supersedes the design spec's NDI-loopback P1): the imag HDMI Program
output must leave the Xorg desktop. The binding shape is a **vendored OBS DRM output** — our forked
OBS acquires DRM master of the HDMI connector through an **X RandR output LEASE**
(`xcb_randr_create_lease`) and page-flips onto it **directly** (`drmModePageFlip`), render→scanout,
with **NO** NDI hop and **NO** external presenter. The NDI-loopback / external-presenter variant is
REJECTED (zero-latency mandate); it survives only as an emergency fallback in the spec appendix.

Milestones: **M1** (done, live-verified 2026-08-26: 679 flips ≈ 60/s, clean release) = lease acquire
+ solid-color flip from the OBS process. **M2** (code done, awaiting rig verify) = Program texture →
GBM dma-buf → zero-copy KMS flip (section below). **M3** = vsync/latency measurement vs today's
projector. **M4** = provisioning + `[0/8]` facets.

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
- **fsyntax-only against the REAL in-tree libobs headers — the M2-upgraded net (use THIS, not the
  older stub-obs.h variant):** the whole public libobs header set is self-contained C, so the ONLY
  stub needed is the CMake-generated `obsconfig.h` — write a minimal one into a scratch dir
  (OBS_DATA_PATH/OBS_INSTALL_PREFIX/OBS_PLUGIN_DESTINATION + feature defines; **NO
  `OBS_VERSION`/`OBS_VERSION_CANONICAL` macros** — this tree declares those `extern const char*`
  in `obsversion.h`, and a macro collides the moment a TU pulls obs-internal.h) — then
  `gcc -fsyntax-only -std=gnu11 -Wall -Wextra -Wformat=2 -Wconversion -I<scratch>
  -Ivendor/obs-studio/libobs -I/usr/include/libdrm vendor/obs-studio/libobs/obs-drm-output.c`.
  Every real prototype (`gs_*`, `obs_*`, `os_atomic_*`, gbm/drm/xcb) is then genuinely
  signature-checked locally — this net catches the exact M1-class miss (the stubbed obs.h that
  masked a missing `util/threading.h` include, fda5b2f5d) by construction. `obs-video.c` compiles
  the same way with `-Ivendor/obs-studio/deps/libcaption` added (obs-internal.h needs it).
  Install `libdrm-dev`/`libxcb-randr0-dev`/`libgbm-dev` on dev1 if missing. The old stub-obs.h
  recipe survives only as a fallback for a TU that genuinely cannot include the real headers.

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

**M1 LIVE-VERIFIED 2026-08-26 (new imag-nb) — plus two runbook gotchas the first live pass hit:**
lease acquired on the real HDMI-1, mode set 1920x1080@60, 679 vblank-locked flips (~60/s), and a
DETERMINISTIC clean release under a real SIGTERM (obs_shutdown → `lease released — connector
returned to Xorg` — the review's lifecycle invariant, proven live, not just in code review).

- **Do NOT run the M1/M2 test window under the `imag-obs` user unit's supervision with HDMI-1 off
  in X.** The unit's wrapper (`imag-obs-start.sh`) runs post-launch bootstrap + `--projector`
  steps that FAIL when the HDMI display is out of the X layout → wrapper exit 1 → systemd tears
  OBS down and CRASH-LOOPS the unit (restart counter climbing every ~13 s). For a test window:
  `systemctl --user stop imag-obs`, launch OBS manually, test, then restore the unit — or defer to
  an M2-era wrapper that tolerates the leased-connector state.
- **After a lease, X RandR can stick at `HDMI-1 disconnected`** (kernel status stays `connected`),
  and a per-output `xrandr --output HDMI-1 --auto --primary` throws `BadAccess (RRSetOutputPrimary)`
  while a lease is still held. The reliable restore is a GLOBAL `xrandr --auto` after the release —
  it re-probes and brings HDMI-1 back `connected primary 1920x1080`.

## M2 — Program → GBM dma-buf → zero-copy scanout (the shipped shape)

The chosen path is **render INTO scanout** (the kmscube/wlroots pattern), NOT an EGL export of GL
textures (a render-chosen Intel CCS modifier can be unscannable) and NOT a CPU readback (violates
the zero-copy mandate; ~0.5 GB/s + a GPU stall on the 25W box):

- `start()` allocates 3 `gbm_bo_create(XRGB8888, GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING)` BOs
  on the LEASE fd (scanout-compatible modifier BY CONSTRUCTION) + `drmModeAddFB2WithModifiers`
  FBs. **The GL import is LAZY** — at `obs_startup` autostart time the graphics subsystem does not
  exist, so the first `obs_drm_output_on_frame()` call (the graphics thread) imports each BO via
  the UPSTREAM `gs_texture_create_from_dmabuf` (which returns a `GS_RENDER_TARGET` texture —
  gl-egl-common.c creates them with that flag, and the GL backend attaches any `GS_TEXTURE_2D` to
  an FBO), so M2 needed ZERO new graphics vtable exports.
- The hook sits in `obs_graphics_thread_loop` (obs-video.c) right after `output_frames` under a
  `#if defined(__linux__)` guard, and raw-copies the Program (`obs_get_main_texture`) into the
  mailbox back buffer: **non-sRGB sampling + framebuffer-sRGB OFF + blending OFF** = byte-faithful
  SDR copy (the sRGB decode/encode round-trip of `obs_render_main_texture` is for filtered
  scaling, not a scanout copy). Aspect-fit letterboxes a mode/canvas mismatch; SDR-only (HDR would
  need a tonemap pass — out of scope, imag is SDR).
- **Mailbox triple buffer** (front on scanout / pending flip queued / ready latest-wins): producer
  = graphics thread (~60 fps DanteSync-locked), consumer = the M1 flip thread (HDMI vblank) — two
  INDEPENDENT clock domains, so an occasional repeated/overwritten frame is inherent and correct.
  The pure helpers `drm_output_pick_render_buf` + `drm_output_fit_rect` are truth-tabled in
  `tests/drm_output_program_1152.rs` (std-only, the same lift-compile model as M1).
- The first Program frame does a one-shot `drmModeSetCrtc` onto the GBM FB (a legacy page-flip
  across a modifier change is unreliable — the dumb solid FB and the GBM FB differ), then
  page-flips run among the identical GBM FBs; when the mailbox is empty the flip thread RE-FLIPS
  the front FB (vblank pacing with no condvar). GPU→scanout sync is `gs_flush()` + i915/Xe
  implicit fencing on the BO (the kernel flip waits on dma-resv — no glFinish).
- **Lock order (deadlock rule): graphics context FIRST, then `program_lock`** — and the frame
  hook is CLAIM/RENDER/PUBLISH shaped (review finding): the graphics CONTEXT alone excludes the
  GL teardown for the whole hook body, so `program_lock` is held only for the two mailbox-role
  transactions (claim a role-free buffer; publish `p_ready` with a `program_want` re-check),
  never across the GL command recording or the lazy bind. The flip thread takes `program_lock`
  alone (briefly, never across the flip wait) and never the graphics context; `g_drm.lock` still
  never enters the flip loop. `stop()` order: disarm `program_want` → join → GL teardown (ctx +
  program_lock) → `teardown_locked` (Program FBs/BOs freed BEFORE the fd closes). `obs_shutdown`
  calls stop BEFORE `stop_video()`, so graphics is still alive for the texture destroy.
  **Disarm invariant (review 🔴/🟡): "armed ⇒ buffers exist" holds on EVERY path** —
  `drm_output_program_free_bufs_locked` disarms first (covers the pthread_create-failure start
  path), and the flip loop disarms on SELF-death (lease revoke / failed flip) so the hook never
  keeps rendering ~60/s into a mailbox nobody drains.
- **Fail-open**: any GBM/AddFB2/import failure logs `program bind FAILED`/`staying on the solid
  pattern` and the output keeps the M1 solid behaviour (it also shows solid until OBS's first
  rendered frame). Config: optional `"program": false` in `~/.camera-box/drm-output.json` forces
  the M1 solid diagnostic pattern; absent/true = Program binding (the default).
- New CI/link deps: `libgbm-dev` in linux-genlock.yml `OBS_APT_PACKAGES`;
  `pkg_check_modules(Gbm REQUIRED IMPORTED_TARGET gbm)` + `PkgConfig::Gbm` in os-linux.cmake.
- New log markers (all M1 `drm-output:` substrings stay byte-identical; these are mutually
  non-substring): `program buffers allocated`, `program bind ready` / `program bind FAILED`,
  `program scanout LIVE`, `program-flip #N` (first + ~1/min at 60 Hz). Rig verify (supervisor):
  enable the config → restart imag OBS → expect `lease acquired` → `mode set` → `program buffers
  allocated` → `ACTIVE` (solid) → after the first render tick `program bind ready` → `program
  scanout LIVE` → `program-flip #1`, and the HDMI shows the LIVE Program, not grey.

Scanout tearing stays report-only until the #781 physical HDMI tap; M3 measures vsync/latency
vs today's projector.

## M4 — permanent-deployment tolerance + facets (the shipped shape)

M1/M2 proved the mechanism; M4 makes the ENABLED state deployable unattended. Three pieces, all
keyed on the ONE config file (`~/.camera-box/drm-output.json` — the module's own activation
contract, still DEFAULT-OFF):

- **Lease-tolerant wrapper + seeder.** `imag-obs-start.sh` detects an enabled config BEFORE the
  OBS launch: it announces `drm-lease mode ENABLED` loudly into `/tmp/imag-obs-start.log` and
  takes the config's `connector` out of the X layout (`xrandr --output <connector> --off`,
  best-effort + loud WARN — the idle-connector lease precondition from the M1 runbook, now
  reboot-durable with no openbox-autostart edit). The projector-step tolerance itself lives in
  `imag_scenes.py`: `projector()` consults the BOX's own config (local read on the loopback path,
  the sshpass transport for a dev1 `--host <ip>` call) and in lease mode opens ONLY the panel
  Multiview, closes EVERY restored X `Projector - Program` stray, and NEVER exits non-zero — so
  the crash-loop from the first M1 runbook gotcha above cannot recur, and every seed caller (unit
  boot, operator menu, watchdog relaunch, verify repopulate) inherits the tolerance from one
  place. Dormant config ⇒ both files behave byte-identically to before.
- **The `drm_output` display-path facet + lease-aware `hdmi_primary`** (shared
  `scripts/lib/imag-display-path.sh`, flowing into drift-guard `--check-imag` check 10, the E2E
  `[0/8]` preflight and verify-imag (z) through their generic loops): dormant config = OK (the
  fleet default never false-aborts a run); ENABLED demands the current OBS log's
  `program scanout LIVE` marker = OK, while bind FAILED / solid-only (incl. the `"program": false`
  M1 diagnostic — cam2's grabber taps this HDMI, a grey pattern wrecks a data run) / no marker /
  no readable log = DRIFT fail-loud. With the config enabled `hdmi_primary` accepts the panel
  primary as correct-by-design (HDMI is leased OUT of X), instead of the pre-M4 false DRIFT.
- **Provisioning stays hands-off.** `setup-imag.sh`'s #840 block already fetches
  `imag-obs-start.sh` + `imag_scenes.py` from `ref=dev`, so a re-run refreshes the pair; it never
  writes `drm-output.json` (locked by a negative anchor test — enabling is ALWAYS the deliberate
  runbook step below, never provisioning). verify-imag check (p2) proves the INSTALLED pair is the
  lease-tolerant generation (static greps for `DRM_LEASE_MODE` / `drm_output_lease_enabled`).

### The ENABLE flip (SUPERVISOR/owner runbook — never automated)

1. `scripts/verify-imag.sh` green FIRST, incl. (p2) — an older installed wrapper pair crash-loops
   the unit the moment the DRM output owns the connector. (Check (o) expects the DORMANT 1+1
   X-projector state, so run the acceptance gate BEFORE the flip, not after.)
2. Write the config (the M1 runbook's `printf '{"enabled":true,...}'` block above) — no manual
   `xrandr --off` needed any more; the wrapper does it at every unit start. **The machine-written
   ONE-LINE form with an explicit non-empty `"connector"` is a CONTRACT, not a style**: the C
   module and the shared python classifier (`imag_scenes.drm_output_lease_connector` — the
   wrapper's and the seeder's ONE decision grammar) both require a full-JSON-parseable config
   with a connector to arm, and the facet gather's grep reads a newline-stripped copy. Never
   hand-edit the file into pretty-printed/partial JSON — a config the C would ignore must stay
   dormant EVERYWHERE, and a malformed one shows up as a loud `drm_output` DRIFT, never as a
   half-armed wrapper.
3. `systemctl --user restart imag-obs` (the supervised path, never a direct script call). Expect,
   in order: OBS log `lease acquired` → `mode set` → `program buffers allocated` → `ACTIVE` →
   `program bind ready` → `program scanout LIVE` → `program-flip #1`; wrapper log
   `drm-lease mode ENABLED` and the Multiview-only projector seed.
4. From then on the `drm_output` facet (drift-guard / [0/8] / verify (z)) holds the enabled state
   to the LIVE-marker proof. NB: the facet grades the NEWEST OBS session's LOG — `drm_output|OK`
   does not by itself prove OBS is alive right now (liveness is the sibling gates' job), and a
   read racing a fresh OBS start reads DRIFT until `program scanout LIVE` lands.
5. **Known boundary — CLOSED (M4 follow-up, issue 1152; the M4 lease-tolerance slice closed the
   remaining two).** `obs_phase2.py::open_projectors` (the `[0/8]` preflight gate) is lease-aware —
   it reuses `imag_scenes.drm_output_lease_connector` / `_drm_output_config_text` (lazy
   `_imag_scenes_module()` import, same pattern as `_measurement_pins_module()`) and, when the
   lease is enabled, opens ONLY Multiview and returns — never raising "no HDMI projector monitor
   detected" for the expected/healthy panel-only monitor list. It ALSO raises loud if the lease is
   enabled but `GetMonitorList` still reports an HDMI monitor (the connector never actually left
   X — the wrapper's `xrandr --off` step failed or the config is stale) — a genuinely inconsistent
   state the gate must never silently pass.
   **(a) `recording-e2e.sh`'s `#756` projector-COUNT check** (immediately after `open-projectors`
   in the SAME `[0/8]` sequence) now derives `_lease_connector` the SAME way (via a `python3 -c`
   one-liner calling the shared classifier against `$IMAG_IP`) and branches through the new
   `scripts/lib/imag-projector-lease-count.sh::imag_projector_lease_count_verdict` — dormant keeps
   the exact pre-#1152 1 Multiview + 1 Program contract; lease-enabled expects 1 Multiview + 0
   Program, and hard-fails loud (never a no-op) if a Program X window is STILL present (the
   connector never actually left X, or a stray reappeared).
   **(b) `scripts/verify-imag.sh` check (o)** (`imag_projector_counts_ok`, live flow ~line 1520+)
   derives the SAME `LEASE_CONNECTOR` once (reused for both the before-restart AND the
   after-restart-poll reads, since the config is static across an OBS restart) and now calls the
   new `imag_projector_counts_ok_for_mode` — which composes over the UNCHANGED
   `imag_projector_counts_ok` (dormant, 1+1) and the new `imag_projector_counts_ok_lease` (lease,
   1+0) — at both call sites, instead of the raw dormant-only function. verify-imag.sh still does
   NOT invoke `obs_phase2.py open-projectors` (the pre-existing docstring correction from the prior
   half of this follow-up stands) — it is a fully independent count read, now independently
   lease-aware too.
   Both `[0/8]` and `verify-imag.sh` now tolerate the config enabled end-to-end; the M4 runbook's
   step-1 workaround ("run verify-imag.sh BEFORE the flip") is no longer required, though running
   it first is still good practice. Neither gate weakens the dormant-mode contract at all — a
   dormant box that regresses to a stray/missing projector still hard-fails exactly as before.

### Rollback (ORDER MATTERS — X must get the connector back BEFORE the dormant wrapper runs)

`rm ~/.camera-box/drm-output.json` → `systemctl --user stop imag-obs` → **explicit
`xrandr --output HDMI-1 --auto --primary`** (the second M1 runbook gotcha: RandR sticks at
`disconnected` after a lease, and a global `xrandr --auto` right after teardown can race a
still-held lease) → `systemctl --user start imag-obs`. Skipping the xrandr step crash-loops the
DORMANT unit instead: with the config gone the wrapper touches nothing and `projector()` is back
to hard-failing on a missing HDMI monitor — which is correct for a genuinely unplugged projector
and exactly why the restore must come first.

## The VIEW — Program or the built-in Multiview (issue 1346, strih-snv, owner 24.9.2026)

The output scans out one of two views, chosen by `"view"` in the same config (`program` | `multiview`;
absent = program, so imag is byte-for-byte unchanged in behaviour):

- **PROGRAM** = the M2 path above, unchanged.
- **MULTIVIEW** = the frontend's BUILT-IN `Multiview` (labels, PVW/PGM tally), never a custom
  scene. The frontend registers a renderer with
  `obs_drm_output_set_view_renderer` (the setter takes the graphics context, so after it returns no
  render is in flight); on a multiview tick the view TU (`obs-drm-output-view.c`) renders it into an
  sRGB-capable `GS_BGRA` texrender at the connector mode size, then claims a mailbox buffer through
  the shared seam (`obs-drm-output-internal.h`: claim / publish / texture / mode size / blit) and
  fills it with the ONE raw blit both views use (`drm_output_blit_raw`). The frame hook asks
  `drm_output_view_frame()` before the Program copy; only a PROGRAM tick falls through to it.
- **GOTCHA (review round 1) — never render an OBS scene straight into the dma-buf scanout texture.**
  OBS's own render targets are `GL_SRGB8(_ALPHA8)` storage, and every sRGB-aware draw
  (`obs_render_main_texture` for the Multiview's Program cell, the scene items' linear-sRGB path)
  relies on the TARGET encoding with `GL_FRAMEBUFFER_SRGB` on. The GBM BO imported through
  `gs_texture_create_from_dmabuf` is LINEAR storage (no EGL colorspace attribute), so GL writes the
  decoded values unencoded -> too dark. Render into a texrender, then copy the BYTES raw
  (`gs_effect_set_texture`, which OBS GL samples with `GL_TEXTURE_SRGB_DECODE_EXT = SKIP`, and
  framebuffer sRGB off) — the same shape the Program path always had.
- **Never degrade the Program:** the Multiview renders ONLY in the multiview view, and each tick first
  asks the monitoring-surface budget gate `obs_aux_sender_should_skip` (issue 879 over the
  278/293/756/776 `obs_display_should_skip` + canvas-rate divisor): a skipped tick keeps the last frame
  on scanout, the anti-starvation floor still renders every K+1 ticks.
- **GOTCHA — the view renders INSIDE the tick whose total the gate reads back (main design
  5840501628, live 25.9.2026: `multiview-render rendered_fps=15` on vk-direct).** The #1063 consumed
  term is `max(elapsed, obs->video.last_tick_total_ns)`, and the previous tick's total CONTAINS the
  view's own Multiview render. That cost is also the gate's `ewma_ns`, so it was counted twice: after
  a rendered tick, 15 ms program work + 14 ms Multiview read as 29 ms consumed + 14 ms ewma > the
  30 ms budget, so the next tick skipped. Result: a strict render/skip alternation = 15 fps. The X
  projector never hit it because `render_display()` budgets on `elapsed` only. The view therefore
  calls `obs_aux_sender_should_skip_excluding(..., self_last_ns)` (obs.c). It subtracts the caller's
  previous render from the previous tick's total, saturating. `obs_aux_sender_should_skip()` is its
  `self_last_ns = 0` case, so the ndi_filter aux senders are unchanged.
  - The view leaves its render out ONLY when that render ran in the immediately previous graphics
    tick (the pure `drm_output_view_self_last_ns`, lift-tested in `tests/drm_output_view_1346.rs`).
    The render records its cost and `obs_get_total_frames()` right after `gs_texrender_end()`,
    BEFORE the scanout-buffer claim, because a failed claim or blit still spent that time in the
    tick. `total_frames` advances once per processed tick (by the lag count on a lagged one), so the
    render is excluded exactly when the difference is 1.
  - Anything else excludes nothing: a lagged tick, no render yet, or a frame-hook call a backend
    skipped. The vk-direct hook skips the call while the output is disarmed or its present loop is
    dead (`drm_output_vk_wants_frames()` is a latch set at open and cleared at halt/death, NOT a
    per-READY-image wait). The lease hook skips while `program_want` is false, and both skip on a
    GL bind failure.
  - Review round 1 corrected a first version that read-then-cleared the value per call. That
    version carried a render across a stop/restart into a tick that did not contain it.
  - The `multiview-render` line carries `self_ns=` (the value the gate last used). On a healthy
    30 fps view it reads about the Multiview cost (~14e6).
  - Tier-0 mirror: `src/render_budget.rs` (`display_should_skip` + `aux_consumed_ns` +
    `aux_sender_should_skip_excluding`). C parity + the live numbers (pre 15 + mv 14 = 30/30, the
    wrapper 15/30, pre 25 + mv 14 = the floor 7/30): `tests/drm_output_view_mv_budget_1346.rs`.
  - Any NEW surface that renders inside the graphics tick and asks this gate must pass its own
    previous cost the same way, or it runs at half rate whenever it fits the budget.
- **Logs (all `drm-output:`, mutually non-substring with the M1/M2 markers and with
  `multiview-audit:` / `program-render-audit`):** `view=<v> (from <path>)` at autostart,
  `multiview renderer registered|cleared`, the one-shot `multiview bind LIVE`, the ~5 s
  `multiview-render rendered_fps=… skipped=… avg_ms=… max_ms=… ewma_ms=… cx=… cy=…` cost line (read
  it next to `program-render-audit lagged`), and `view <a> -> <b> (persisted)` on a live switch.
  `program scanout LIVE` still means "a GBM buffer is on the CRTC" for either view.
- **The live switch** is the OBS Tools menu pair (Linux-only `components/DrmOutputView.cpp`) →
  `obs_drm_output_set_view`, which rewrites ONLY `"view"` into the config the autostart read (obs_data
  round-trip, one compact line, `os_quick_write_utf8_file_safe`). The pure helpers
  `drm_output_parse_view` + `drm_output_view_tick_action` are lift-tested in
  `tests/drm_output_view_1346.rs`; the view grammar is ONE table
  (`tests/fixtures/drm_output_view_parity.tsv`) shared with the Python mirror in
  `scripts/strih_scenes.py`.
- **NVIDIA:** the LEASE path is Intel-only in practice. The NVIDIA X driver refuses the RandR lease
  (live on strih-snv 25.9.2026, also with `non-desktop 1`), so an NVIDIA-driven connector uses the
  vk-direct backend below, never this one.
- **imag stays Program:** the Tools switch ships in the whole Linux bundle, and `program scanout
  LIVE` fires for either view, so the imag `drm_output` facet (`scripts/lib/imag-display-path.sh`)
  gathers `DRM_OUTPUT_VIEW` and grades `"view":"multiview"` as DRIFT (audience projection + cam2 tap).
- **Live-verify items the first strih-snv run must read (review round 1):** (1) the laptop Multiview
  projector's `multiview-audit rendered_fps` next to the new `multiview-render` line — the HDMI
  Multiview renders in the frame hook BEFORE `render_displays()`, so it takes budget first; if the
  laptop projector sags under its floor, move the view render after `render_displays()` or share one
  throttle; (2) NVIDIA implicit fencing — `gs_flush()` + publish relies on dma-resv implicit sync
  (proven on Intel only); torn/partial frames on the RTX 5050 would need a CPU fence wait before
  publish (libobs exposes only the GPU-side `gs_sync_wait`, so that is a graphics-vtable addition).
- strih provisioning / verify: `.claude/rules/strih-linux-provisioning.md` (issue 1346 section).

## The vk-direct backend — NVIDIA Vulkan direct display (issue 1346, strih-snv built-in HDMI)

The owner ruled the strih-snv HDMI output must use the BUILT-IN port (NVIDIA RTX 5050, X output
`HDMI-0`) and must be indestructible like imag's (never an X window, no mouse; rulings 5838578002 +
5838662632). The NVIDIA X driver refuses the X RandR lease, so the module has a SECOND backend:
`VK_EXT_acquire_xlib_display` + `VK_KHR_display`, the NVIDIA-supported way to take a display away
from X (SteamVR/Monado direct mode). Selected by `"backend":"vk-direct"` in the SAME config file
(absent / `"lease"` = the lease backend, so imag is byte-for-byte unchanged); on strih boxes it comes
from the per-box fact `STRIH_HDMI_OUTPUT_BACKEND` (strih-snv = `vk-direct`).

- **Structure:** `obs-drm-output.c` routes its public entry points (start/stop/active/the frame hook)
  and its mailbox seam (claim/publish/texture/mode size) to `drm_output_vk_direct_backend` (a
  `struct drm_output_backend_ops` in `obs-drm-output-internal.h`) while that backend OWNS the output;
  the lease code under each branch is unchanged. `obs-drm-output-backend.c` = the backend grammar
  (`drm_output_parse_backend`, one TSV with the Python mirror) + the OBS side (a GS_BGRA
  intermediate at the display mode size that the ONE raw blit fills, the lazy bind, the frame hook,
  the ordered stop). `obs-drm-output-vk.c` = present thread + lifecycle + GL side;
  `obs-drm-output-vk-setup.c` = Vulkan/X setup + teardown. The view TU, the budget gate, the
  Tools-menu switch and the `drm-output:` log family are shared.
- **Build:** Vulkan HEADERS only (`find_package(Vulkan REQUIRED)` + `Vulkan::Headers`, `libvulkan-dev`
  in linux-genlock.yml); `libvulkan.so.1` is `dlopen`ed (`VK_NO_PROTOTYPES`), so a box without the
  loader keeps the output dormant instead of failing to load libobs. The xlib entry points are
  declared locally with RROutput as `unsigned long` (no libxrandr-dev). GL entry points are resolved
  at run time through `eglGetProcAddress` (OBS runs EGL/X11).
- **The X precondition (STEP 0 finding):** `vkAcquireXlibDisplayEXT` returns `-13`
  (VK_ERROR_INITIALIZATION_FAILED) while the output has an X CRTC. `strih-obs-start.sh` already runs
  `xrandr --output <connector> --off` for any armed config, and the kiosk autostart keeps HDMI off.
  After `vkReleaseDisplayEXT` the output comes back to X DISABLED (no CRTC), never onto the desktop.
  OBS closes an X projector whose screen leaves X (`OBSProjector::ScreenRemoved`), and a saved
  projector on a missing monitor index is not restored (`OpenProjector` returns null), so no stray
  window lands on the laptop screen.
- **Data path + sync:** 3 shared `R8G8B8A8_UNORM` optimal-tiling images, exported `OPAQUE_FD` with a
  DEDICATED allocation and imported into the OBS GL context with `GL_EXT_memory_object_fd` (DSA
  `glCreateTextures` + `glTextureStorageMem2DEXT` — no texture binding changes behind the libobs GL
  state cache). Per tick: the raw blit into the intermediate, then `glCopyImageSubData` (a byte copy,
  sRGB8_ALPHA8 -> RGBA8 are copy-compatible) + `glSignalSemaphoreEXT` + `glFlush`. The present thread
  (FIFO: `vkAcquireNextImageKHR` blocks on vblank) blits the newest READY image — or re-blits the FRONT
  one — into the swapchain `B8G8R8A8_UNORM` image, acquiring/releasing it from/to
  `VK_QUEUE_FAMILY_EXTERNAL` on every copy. The GL side never does the matching acquire/release
  (GL_EXT_semaphore carries only layouts), so this leans on the NVIDIA driver being lenient about
  external ownership — proven live on the target GPU, re-check on any other vendor. Roles change only
  after a SIGNALLED per-vblank fence, so a GL write never races a Vulkan read. **A signalled READY
  image is NEVER overwritten:** the GL claim skips while one waits, so every GL signal is waited
  exactly once by the present thread and no GL-side semaphore wait exists. GOTCHA (live 25.9.2026):
  the first design kept the lease's "latest wins" overwrite and consumed the overwritten signal with a
  GL-side `glWaitSemaphoreEXT` — under a publish burst the copy DEADLOCKED (observed; the likely cause
  is the GL wait still pending when Vulkan waited the same binary semaphore): the copy never completed, the teardown leaked
  and the process hung at exit HOLDING the display (`vkAcquireXlibDisplayEXT` then returned -13 and
  `xrandr` could not re-enable HDMI-0 until the process was killed). Never reintroduce a GL-side
  consume wait. A skipped claim drops the NEWER frame (the lease drops the older one): one frame either
  way, and a burst self-paces to the display rate. The bookkeeping is three pure helpers
  (`drm_output_vk_pick_claim` / `_present_pick` / `_present_done`), model-checked over 200000 random
  interleavings in `tests/drm_output_vk_direct_1346.rs` (a mutant restoring the overwrite fails it with
  ~180000 violations); with no READY frame the claim is exactly the lease mailbox rule.
- **Bounded, and a replug is not the end:** the present loop's acquire/fence waits are 1 s and
  re-checked. `VK_ERROR_OUT_OF_DATE_KHR` / `VK_ERROR_SURFACE_LOST_KHR` (an HDMI or grabber replug)
  rebuild the swapchain (+ the display-plane surface when lost, or from the 2nd try on) with 10 tries
  1 s apart, without any device wait: the replaced swapchain (+ surface) is RETIRED (handed over as
  `oldSwapchain`, freed after 4 later signalled fences or at the teardown) and the per-image present
  semaphores live across rebuilds, so a still-queued present never waits a destroyed object; the
  committed mode never changes during a rebuild, and a display
  that now offers a different size gives up by name (the shared images no longer fit — restart OBS).
  The teardown waits an outstanding copy bounded (5 x 1 s); a TIMEOUT LEAKS the Vulkan objects (and
  keeps the loader + X connection) with a loud line instead of hanging `obs_shutdown` — note the
  process may then still hang at exit inside the driver, holding the display (the burst incident
  above); a lost device is destroyed normally. `glFinish` runs before the GL objects are deleted. verify-strih item 4c reads a present
  loop that died on its own (`vk-direct present loop exited` with no `stopped (vk-direct` after it) as
  `present-dead`, because `program scanout LIVE` stays in the log.
- **Swapchain format:** B8G8R8A8 UNORM, else R8G8B8A8 UNORM, else refuse — never an sRGB format (the
  blit would encode the already-encoded frame a second time); the blit features are checked.
- **GL lookup:** `eglGetProcAddress` via `dlsym(RTLD_DEFAULT)`, falling back to the already-loaded
  `libEGL.so.1` / `libGLX.so.0` (`RTLD_NOLOAD`) in case libobs-opengl loaded them privately. The rig
  harness links libEGL directly, so the first in-OBS run must show `program bind ready (vk-direct`.
- **Logs (all `drm-output:`):** `vk-direct display acquired`, `vk-direct mode WxH@HzHz on display
  plane N`, `vk-direct swapchain WxH FIFO images=N`, `solid-present #N` (before the first frame),
  `program bind ready (vk-direct: ...)` / `program bind FAILED (vk-direct: ...)`, **`program scanout
  LIVE (vk-direct: ...)`** (the SAME marker verify-strih greps), `program-present #N`, `vk-direct
  present loop exited`, `vk-direct display released`, `stopped (vk-direct, ...)`. `program-present #`
  / `solid-present #` are mutually non-substring with the lease `program-flip #` / `page-flip #`.
- **Proof without OBS:** `tests/c/drm_output_vk_rig_harness.c` compiles the REAL core (gcc ON the box
  with staged headers — never on dev1) with its own EGL/GL 3.3 core context and publishes red, green,
  blue and 50 % grey phases, then a 2 s BURST that claims with no pause (claim FIRST, render only when
  claimed — like the OBS hook) and requires `gl_skips > 0`, a present loop still alive after a GL drain,
  and a clean release. Live on strih-snv 25.9.2026: 716 phase publishes, 0 skipped; cam2 (which captures
  HDMI-0) read `245/0/0`, `0/255/0`, `0/0/239`, `127/128/127` — channel order right, no gamma step;
  the burst published 121 frames in 2 s (the 60 Hz display rate) with 7835 skipped claims, released
  with VkResult 0 and exited. After ANY harness run check `pgrep -x vk-rig-harness` is empty (NOT `pgrep -f`, which matches the ssh `bash -c` that carries the name) — a hung
  harness holds HDMI-0 acquired. Re-run it on every new NVIDIA strih box (strih PP).
- **Enable (supervisor runbook, after CI + a FULL Linux bundle deploy on strih-snv):** re-run
  `setup-strih.sh --box strih-snv` step 6 with the HDMI monitor/grabber plugged (it writes or upserts
  `"backend":"vk-direct"` and installs `libvulkan1`) — or write the one line by hand as the desktop
  user — then `systemctl --user restart strih-obs.service`. Expect in the OBS log: `autostart ENABLED`
  → `backend=vk-direct` → `vk-direct display acquired` → `ACTIVE (vk-direct)` → `program bind ready`
  → `program scanout LIVE` (+ `multiview bind LIVE` for the multiview view); verify-strih item 4c
  green; the HDMI shows the chosen view and the Tools switch changes it live. Rollback: remove the
  config (or its `"backend"` key) and restart OBS; `xrandr --output HDMI-0 --auto` brings the port
  back to the desktop only if wanted.

## Lifecycle invariants (locked by the #1152 review — keep them if you touch the module)

- `obs_shutdown()` MUST call `obs_drm_output_stop()` before teardown (flip thread must not outlive
  the log sink; lease returned to Xorg deterministically, not by process death).
- `stop()` claims the transition with a `stopping` flag in the FIRST critical section, then joins
  OUTSIDE the lock — a racing stop returns, a racing start rejects (never double-`pthread_join`).
- The flip loop uses `os_atomic_{set,load}_bool(&running)`; its poll wait breaks on
  `POLLERR/HUP/NVAL` and a failing `drmHandleEvent` (no CPU spin on lease revoke / X restart) and
  emits a wedge WARNING after ~5 s of overdue completions.

## GOTCHA (M4 follow-up, issue 1152 review) — a NEW `_conn`/host-consulting helper on `open_projectors` needs its OWN stub in EVERY pre-existing test file's `_patch`/mock helper, or the tests silently degrade to live I/O

`tests/python/test_obs_phase2_open_projectors_lease_1152.py` added `_drm_lease_connector_for_host`
(consults the box's real `~/.camera-box/drm-output.json`, over ssh for a `--host <ip>` call) to
`obs_phase2.py::open_projectors`. The PRE-EXISTING `tests/python/test_obs_phase2_open_projectors_758.py`'s
own `_patch` helper stubbed `_rpc`/`_conn` but had NO reason to know about the new call — so its
tests kept passing, but silently started performing a REAL live ssh dial into the new code path
on every run (measured ~3s/test; `10.77.9.187` happened to be dormant at review time, so the
tests passed by luck — the SAME tests pointed at the real imag-nb `.182`, which resolved
`ENABLED`, would have failed outright). Nothing in the test output flagged this — a slow-but-green
test suite is the only tell. **Whenever `open_projectors` (or any function a `_patch`-style mock
helper wraps) gains a NEW call that consults live host/rig state, grep every OTHER test file in
`tests/python/` that mocks the SAME function for its own `_patch`/mock helper and add the SAME
stub there too** — a shared decision function reused by multiple test files each maintaining
their OWN mock helper is exactly this trap: adding a call inside the function under test is
invisible to a mock helper written before that call existed.

## The output's own HDMI audio + the Program-view mode (issue 1424, owner 13.10.2026)

The fixed HDMI output plays the OBS program mix (mix 0) on ITS OWN connector's HDMI audio, written
straight into that connector's ALSA hw PCM (`hw:<card>,<device>`) -- owner: "Ziadnych milion vrstiev
po ceste": no PipeWire, no OBS monitoring device, no mixer in between, never a per-box setting.

- **The match** (`obs-drm-output-audio-match.h`, pure C, Rust twin `src/drm_output_audio_match.rs`,
  parity gate `tests/drm_output_audio_1424.rs` over the strih-pp fixtures in
  `tests/fixtures/drm_output_audio_1424.tsv`): the connector's EDID (X RandR output property `EDID`,
  read by output name BEFORE the backend takes the connector; readable while vk-direct holds it)
  against the ELD of every HDMI/DP PCM. EDID bytes 8..11 ARE ELD bytes 16..19 (the kernel's /proc
  prints them as little-endian `manufacture_id` / `product_id`), so the raw bytes compare, no swap;
  the monitor name only breaks a tie. Two identical monitors = AMBIGUOUS = no audio.
- **The ELD comes from the per-PCM ALSA control** `iface=PCM,name='ELD',device=<pcm>`
  (`/dev/snd/controlC<card>`, kernel uapi ioctls), NEVER `/proc/asound/card*/eld#C.P`: that index is
  the codec pin (+dev_id), not a PCM number (strih-snv card0 = ONE pin 0x5, dev_ids 0..3 behind PCMs
  3/7/8/9, dynamic PCM assignment). The control is the kernel's own pcm->pin map.
- **GOTCHA -- the NVIDIA HDA driver publishes a connector's ELD only once a mode is driven on it.** The
  leg opens AFTER the backend start succeeded and re-reads for `DRM_OUTPUT_AUDIO_RESOLVE_WINDOW_MS`
  (10 s) before the ONE `drm-output audio: NO AUDIO on '<conn>' -- <reason>` line. Never decide from a
  read taken before the scanout starts.
- **GOTCHA -- `<alsa/asoundlib.h>` and the kernel `<sound/asound.h>` cannot share a TU** (conflicting
  `snd_pcm_format_t` / `snd_ctl_elem_iface_t` typedefs). alsa-lib is dlopen'd (`libasound.so.2`, the
  vk-direct loader pattern) with its few prototypes declared locally; the control ioctls use the kernel
  header.
- **The matched PCM's "IEC958 Playback Switch" is turned on**: no sound server unmutes it any more.
  Its index = the PCM's rank among the card's ELD-carrying PCMs (`drm_output_audio_iec958_index`;
  strih-snv 3/7/8/9 <-> 0/1/2/3) -- written ONLY when the card's switch count equals its HDMI PCM
  count; an extra analog S/PDIF switch on the same card (an Intel HDA laptop) leaves it untouched and
  the open line says `IEC958 switch ambiguous`.
- **Placement** (`drm_output_audio_plan_block`): each block plays at its OBS timestamp + the output's
  measured present delay (render tick noted at the ONE publish entry `drm_output_publish_render_buf`,
  on-screen at the lease flip event / the vk-direct present -- a lower bound by up to one refresh)
  against `snd_pcm_delay`. Early audio is held, a queue grown past the floor (3 blocks) by more
  than the threshold is trimmed at most a block at a time; inside the threshold the clock drift (OBS
  clock vs the HDMI audio clock, tens of ppm) is walked back ONE frame per block outside a 5 ms band
  (`nudge_frames`, inaudible) -- never a silence insert or a cut every ~20 min. **Physics:** the mix leaves the raw callback ~the audio-buffering floor
  (85 ms+) after its timestamp, the picture ~1-2 frames after it, so the audio normally runs BEHIND
  the picture; the open line `drm-output audio: pcm hw:C,D card C device D delay <hold> ms (...
  audio N ms behind the picture ...)` and a 60 s report line name the residual. Closing it would need
  a video hold on the HDMI picture (an owner decision on picture latency).
- **The Multiview view mutes it** (the cutter's own monitor never gets the program audio). It then
  places no block, so its only open line is the resolver's `drm-output audio: '<conn>' matched
  hw:C,D` -- verify counts that line as open too.
- **The resolver stays as the leg's watcher:** every 5 s it re-reads the ELDs (never on libobs's audio
  thread, which neither allocates nor opens devices: the hold's silence buffer is allocated at open);
  a PCM that no longer carries the connector's monitor (a replug or a re-mode moved the NVIDIA map)
  is muted and named until it does again (verify reads that mute, and a `write to hw:` failure, as
  no audio; the last outcome line wins). It never re-resolves to a NEW PCM: a moved map stays muted
  until OBS restarts (follow-up). The 60 s report line counts holds / trims / nudges / xruns /
  `dropped_frames` (what a full ring refused). **Read `nudges` on the first live run:** the 5 ms band
  sees one raw `snd_pcm_delay` sample per block, so a nudge count near one per block means jitter, not
  drift, and the band needs smoothing; a transient lost/again pair around the startup re-mode is
  expected.
- **Known limits:** a runtime audio reset (a profile switch) drops the mix callback without a line;
  the EDID comes only from X RandR (a connector X still lists `disconnected` after an earlier lease
  has none -> NO AUDIO, named); verify-imag has no audio row yet.
- **Box side** (`scripts/lib/drm-output-audio.sh`): ONE WirePlumber rule disabling every ALSA node on
  an `hdmi:` path (0.5 SPA-JSON drop-in in `/etc/wireplumber/wireplumber.conf.d/`, 0.4 Lua in
  `main.lua.d/`, by `wireplumber --version`), so PipeWire leaves the PCM to OBS (EBUSY otherwise).
  Written by setup-strih step 6 (inside `strih_drm_output_provision`) and setup-imag; never restarts
  the running audio session -- it applies at the next session start. verify-strih item 4c (when the
  output is live) grades the rule, `pw-dump`'s HDMI node count and the module's outcome line.
  `DRM_OUTPUT_AUDIO_WP_ROOT` is the test seam for the rule's root.
- **The Program view's MODE (vk-direct):** the vestibule SDI chain does not carry 1080p60, so
  `drm_output_vk_pick_mode` prefers the CANVAS size at the CANVAS rate (within 0.2 %: 30.000 over
  29.970, 29.970 when 30 is absent, never 25), else the issue-1346 rule; the Multiview keeps the
  native ~60 Hz. The canvas exists only after the frontend's video reset (after the autostart), so the
  output opens with the 1346 rule and the frame hook hands the canvas over (`drm_output_vk_want_mode`);
  the present thread re-modes through the surface rebuild only when the pick changes and keeps the
  size (a size change is named, kept until an OBS restart). The lease backend (imag) is unchanged.
  The re-mode line names the pick: `program view: exact canvas rate`, `program view: near-rate, canvas
  30.000 Hz -- one frame dropped every 33 s` (the live strih-pp HDMI-1 -> Blackmagic HDMI-to-SDI sink
  offers 1080 @ 60/59.94/50/29.97/25/23.98 and 720 @ 60/59.94/50, NO 1080 @ 30.000, and 1080p60 does
  not pass its SDI cable), or the native fallback. 720p60 vs 1080p29.97 is the owner's call after a
  measurement, never code.
- **Live acceptance (supervisor, strih-pp):** full Linux bundle deploy, setup-strih step 6 (the rule),
  restart the operator's WirePlumber or re-login, remove the monitoring-device stopgap, restart OBS:
  the OBS log shows `drm-output audio: pcm hw:0,<d>`, `pw-dump` has no hdmi node, the vestibule hears
  the program; with the Program view on a 30 fps canvas the HDMI-1 sink reads `vk-direct: re-mode
  1920x1080@60.000Hz -> @29.970Hz (program view: near-rate ...)`, and the SDI chain reaches cam1.

## Two fixed outputs at once (issue 1424 part 2, owner 13.10.2026)

strih-pp drives HDMI-0 = the cutter's Multiview (BenQ) AND HDMI-1 = the Program + its audio for the
vestibule (Blackmagic HDMI-to-SDI), both vk-direct on the RTX 3060 (main design 6150029770). No
picture hold or delay is ever added for A/V alignment (owner ruling 13.10.2026: zero picture latency).

- **Config grammar** (`obs-drm-output-outputs.c`: OBS's UTF-8 reader, which drops a byte-order mark as
  the old obs_data loader did, then jansson directly): the single-object form
  is unchanged (imag + strih-snv byte-identical); `{"enabled":true,"outputs":[{connector, argb, view,
  backend}, ...]}` drives up to `OBS_DRM_OUTPUT_MAX_OUTPUTS` (2). ALL OR NOTHING: not an array, empty,
  more than 2, a non-object entry, a bad connector (1..63 bytes), an unknown view / backend or a
  duplicate connector starts NO output and logs one `"outputs" is malformed (<verdict> at entry N)`
  line. jansson, not obs_data: obs_data's JSON loader drops a non-object array member silently and
  would start half a pair. The pure grammar is the marked `BEGIN/END pure config grammar` block, lifted
  verbatim and compiled against real jansson by `tests/drm_output_outputs_1424.rs` over
  `tests/fixtures/drm_output_outputs_parity.tsv`, the table the Python twin
  `scripts/drm_output_config.py` is pinned to. That module (split out of strih_scenes.py, which
  re-exports every name; setup-strih step 6 installs it beside strih_scenes.py) reads the file the C's
  way: a BOM dropped, and a NaN / Infinity literal, an integer outside 64 bits or an overflowing real
  makes the WHOLE file unreadable, as jansson does. imag_scenes' classifier agrees on every row except
  those strict-JSON ones (imag reads a machine-written single object). CI's test / coverage / mutants
  jobs install `libjansson-dev` for the lift.
- **Per-instance state** (`struct drm_output_inst`, `obs-drm-output-internal.h`): connector, backend
  ops, view, present clock, audio leg. The lease backend keeps `g_lease[inst->index]`, the vk-direct
  OBS side `g_vkd[inst->index]` and the vk core display slot `inst->index` (`drm_output_vk_open(slot,
  ...)`, refused while that slot is open), so an instance never points at the other one's display; the registry (count published after the instance is fully written)
  routes the entry points and the mailbox seam through `inst->ops`. An output that fails to start never
  stops the other (`output '<c>' did not start — the other output keeps running`). The ONE raw blit +
  `drm_output_fit_rect` moved to the outputs TU.
- **The vk core** splits into ONE `struct drm_output_vk_device` (loader, X connection, VkInstance,
  VkDevice; one queue per display slot when the family offers it, else queue 0 shared, each with a
  host-access mutex taken around every vkQueueSubmit + vkQueuePresentKHR) and a `struct drm_output_vk_disp` per
  output (own VkDisplayKHR, display plane -- the open path, under the device lock, skips a plane another
  open display presents on; a rebuild relies on `currentDisplay` --,
  surface, swapchain, shared images, present thread). A second output on another GPU is refused by name.
  Closing one display drains ITS queue BOUNDED (`drm_output_vk_drain_queue_bounded`: an empty submit's
  fence, 5 x 1 s; never an unbounded vkQueueWaitIdle -- on a one-queue GPU a queue the other display
  wedged would hang `obs_shutdown`); a timeout leaks that display and marks the device wedged (never
  destroyed); the device goes with the last display (`dev->refs`). The core
  reports presents through a callback (`drm_output_vk_shown_fn`), so the rig harness links without the
  audio TU; `./vk-rig-harness HDMI-0 HDMI-1` proves both displays on one device, then closes display 0 while
  display 1 keeps presenting (its count must advance) and re-opens display 0 beside it.
- **Audio + mode per output:** each leg reads its OWN output's view (`const volatile long *view`) and
  present clock, so the Multiview output's leg is muted while the Program output plays (verify grades
  a Multiview leg that gave up, e.g. a BenQ with no audio, as a NOTE); the Program
  view's canvas-rate mode pick (part 1) is per display.
- **An output whose PCM is owned elsewhere (part 3, owner ruling, design comment 6151522810):** an
  "outputs" entry carries an optional boolean `"audio"` (absent / true = the module's own leg; false =
  the connector's PCM belongs to someone else; a non-boolean = `audio-invalid`, all or nothing; the
  single-object form always has its leg). strih-pp's HDMI-1 is such an output: the intercom hub writes
  the vestibule audio into its PCM, so the module reads no EDID, resolves and opens nothing there and
  logs ONE line `drm-output audio: '<connector>' off by config (owned elsewhere)`. The box fact spells
  it `CONNECTOR:VIEW:noaudio` (strih-pp `HDMI-0:multiview HDMI-1:program:noaudio`); step 6 writes
  `"audio":false` from the FACT (never kept from an old config: it is box ownership, not an operator
  choice); verify item 4c grades that line instead of the open line, FAILs an opened PCM or a missing
  line on such an output, and FAILs a config whose flag differs from the fact. The single-object form
  logs one WARNING when it carries an `"audio"` key (ignored there).
  **The PCM owner's contract (the hub lane, not this module):** with the module's leg off, three duties
  move to whoever writes that PCM: (1) find WHICH `hw:C,D` is the connector by its monitor's ELD (the
  NVIDIA PCM-to-pin map is dynamic; never a fixed device number); (2) turn on that PCM's `IEC958
  Playback Switch` (no sound server unmutes it, and the step-6 WirePlumber rule keeps PipeWire off every
  HDMI PCM, so the writer opens hw directly); (3) survive the re-mode shortly after OBS starts (the
  Program view moves HDMI-1 from 60 to 29.97 Hz, and a re-mode can move the NVIDIA PCM map: re-resolve
  when the device disappears or no longer carries the monitor).
- **Log lines name the connector** where verify grades them: `program scanout LIVE (... of '<c>')`,
  `multiview bind LIVE (... of '<c>')`, `vk-direct present loop exited on '<c>'`; the audio lines
  already did. The prefixes the single-output grading greps are unchanged.
- **Frontend:** one Multiview + one Tools-menu pair per output (`HDMI výstup <connector>: Program /
  Multiview` when several; a one-output box keeps `HDMI výstup: Program / Multiview`), persisting
  into that output's entry (`obs_drm_output_set_view_of`). `obs_drm_output_get_view/set_view/
  set_view_renderer` stay as output 0.
- **Box side:** the fact `STRIH_HDMI_OUTPUTS` (`auto` = one output on the first connected HDMI, `none`
  exactly when the backend is none, or 1..2 `CONNECTOR:VIEW`; strih-pp `HDMI-0:multiview
  HDMI-1:program`). setup-strih step 6 (`strih_drm_output_provision`, 5th argument or the loaded
  fact) writes a fresh array config when X RandR names at least one listed connector connected (each
  unplugged one WARNed), or migrates an existing config with `strih_scenes.write_drm_outputs` --
  every connector the config already drives KEEPS its view and argb. `strih-obs-start.sh` takes every
  armed connector out of X (`drm_output_lease_connectors`). The fact grammar is ONE function,
  `strih_box_hdmi_outputs_ok` in the facts lib. verify-strih item 4c dispatches (through
  `strih_drm_outputs_verify_dispatch`) an array
  config (or a single one on a box whose fact is a list) to `strih_drm_outputs_verify_report`: the
  array valid, its connectors = the fact's, and every output graded on ITS OWN log lines (open line,
  multiview bind, present loop, audio leg via `drm_output_audio_verify_report ... "" <conn>`).
  `strih_scenes.py --projector <view> --connector <c>` is the scripted twin. Known limit: on the LEASE
  backend the per-output "connected" check reads any kernel HDMI connector (X RandR sticks at
  disconnected after a lease and names connectors differently); every strih box is vk-direct.
- **Live acceptance (supervisor, strih-pp):** full Linux bundle deploy, re-run setup-strih step 6 (the
  live single config migrates to the array, HDMI-0 keeps multiview), restart OBS: the log shows
  `autostart ENABLED ... 2 outputs`, two `vk-direct display acquired`, `program scanout LIVE` for
  both connectors, `multiview bind LIVE ... of 'HDMI-0'`, `drm-output audio: pcm hw:...` on 'HDMI-1'
  and the HDMI-0 leg `matched ... (muted while the view is multiview)`; verify-strih item 4c green
  for both; the BenQ shows the Multiview and the vestibule the Program with sound. Run the rig harness
  with both connectors first on the new box.
