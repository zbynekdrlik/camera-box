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
