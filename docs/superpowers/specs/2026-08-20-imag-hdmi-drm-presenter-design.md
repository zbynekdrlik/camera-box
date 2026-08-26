# imag HDMI Program output OUTSIDE Xorg — DRM/KMS presenter (production isolation) — Design

> Implements GitHub issue **#1152** (owner request, 2026-08-20). DESIGN-only spec; no rig
> implementation lands with it. Continues the imag HDMI vsync line (issue 1146 observability /
> issue 1107 OBS EGL projector-vsync — both merged & deployed) by attacking the *architectural*
> gap those left open: HDMI is still a second Xorg desktop that a window can land on.

## 1. Summary

Take the imag HDMI (projector) output **out of the Xorg desktop entirely** and drive the OBS
Program onto it from a **dedicated non-X process** that owns the HDMI connector through a
**DRM/KMS lease** and page-flips with its own vblank lock. The operator desktop (OBS UI +
Multiview) stays on the internal eDP panel. No X window can ever land on HDMI because X no
longer has an output or CRTC there; the projector carries the Program image and nothing else.

The presenter is **not greenfield**: camera-box already owns a tear-free DRM/KMS page-flip
engine (`src/probe/kms.rs`, the cam2 painter presenter) and an NDI receive stack
(`src/ndi.rs` / `src/ndi_display.rs`); this design adapts them to run under a running X via a
RandR output lease, fed by an NDI loopback of the OBS Program.

## 2. Motivation (the problem, live-verified 2026-08-20 on 10.77.9.182)

- imag drives **one** Intel i915 iGPU (`/dev/dri/card1`, Raptor Lake-P UHD, `modesetting` DDX,
  xorg-server 21.1.12) with **two** active CRTCs/outputs: `eDP-1` (panel — operator: OBS UI +
  Multiview) and `HDMI-A-1` (projector — Program). `DP-1` is disconnected. The whole desktop is a
  single `3840×1200` root screen (`xrandr`: `eDP-1 1920x1200+1920+0`, `HDMI-1 primary
  1920x1080+0+0`).
- The Program reaches HDMI **as an OBS fullscreen-projector X window** (`Projector - Program`
  at `0,0 1920x1080 → HDMI-1`, per #1146 STEP-0). Because HDMI is an ordinary X output, **any**
  window (OBS main after a restart, the Ctrl+Tab switcher, a dialog) can be placed on it — and
  did (owner 2026-08-20: OBS UI landed on the projector after an OBS restart; moved by hand).
- Owner's literal ideal (2026-08-20): *"keby HDMI výstup nebol vôbec časť Xorg ako druhá plocha,
  ale ako nejaká video-out karta… malo by sa to maximálne správať produkčne… alebo úplne nejako
  to spraviť cez framebuffer."*
- **Residual smoothness:** the #1107 OBS EGL projector-vsync is deployed and killed steady-state
  line tearing (owner-confirmed), picom was reverted OFF (render-budget 21.57% skip incident,
  #1146 2026-08-20 09:35), but an **occasional hitch that momentarily tears part of the square**
  remains (owner 2026-08-20 11:50). A dedicated presenter with **full page-flip control** is the
  named candidate to close it.

## 3. Acceptance (from the ticket)

1. No window or UI element may EVER land on HDMI — it carries only the Program image.
2. The operator desktop (OBS UI + Multiview) stays on the internal eDP panel.
3. Smoothness/vsync at least as good as today; the residual hitch is a candidate to fix.
4. Survives reboot AND OBS restart.
5. Scanout tear cannot be machine-measured until the #781 physical HDMI tap exists (`ops-wait`
   hardware) — so the design claims a *mechanism* (leased + page-flipping + armed), never
   "tearing solved". Honest report-only verification until #781.

## 4. Hardware / software reality that shapes the design (all live-verified)

| Fact | Value (live) | Consequence |
|---|---|---|
| GPU | single Intel i915, `/dev/dri/card1` | HDMI **cannot** move to a "separate card" — one DRM device, multiple CRTCs. The only real isolation is handing the connector to a non-X process. |
| DDX | `modesetting` on xorg-server 21.1.12; `xrandr --listproviders` → `modesetting crtcs:4 outputs:3 cap:0x9` | RandR 1.6 output leasing (`RRCreateLease`) is wired into modesetting since xserver 1.20 → the DRM-lease path is supported here. |
| CRTCs | 4 total, 2 in use (eDP+HDMI) | ≥2 free CRTCs → a lease can pair a free CRTC with the HDMI output. |
| Wayland kiosk | `cage`/`weston` **absent** | a Wayland variant is a large new dependency + provisioning rewrite. |
| v4l2loopback | module **not loaded** | the v4l2 Program-feed variant needs a new kernel module + OBS venc. |
| NDI | DistroAV present, 10 NDI inputs, **no** Program NDI output yet | the NDI-loopback feed needs one new OBS NDI output; the receive side reuses `NdiReceiver`. |
| Existing engine | `src/probe/kms.rs` (1074 lines): double dumb-BO `drmModePageFlip` blocking on vblank → tear-free 1:1 60 Hz | the page-flip core exists and is tested; adapt its open path to a lease fd. |
| dev2 "presenter" | church-lyrics web app (Leptos/WASM; NDI *ingest* via `ndi_whep`, no DRM output) | shares only the name — **not** a reusable NDI→DRM presenter. |

## 5. The two coupled sub-decisions

**(A) How to give the HDMI connector to a non-X process exclusively (isolation).**
**(B) Where the Program pixels come from for that process (feed).**

### 5A — Isolation mechanism

- **A1 (CHOSEN) — DRM lease via X RandR output leasing.** X stays DRM master of the card, the
  desktop is configured **eDP-only** (HDMI out of the X layout), and the presenter requests a
  lease of `{free CRTC + HDMI-A-1 output + primary plane}` via `xcb_randr_create_lease`,
  becoming DRM master of *only* those resources. This is the SteamVR/Monado VR-HMD "direct mode"
  pattern (`RRCreateLease` returns a KMS/DRM fd controlling the leased objects directly through
  the kernel). No window can land on HDMI — X has no output/CRTC there. Lowest disruption to the
  operator's eDP desktop; retires the #1146 HDMI-primary + picom complexity for the projector.
  **Bring-up caveat:** a generic HDMI output is not auto-marked *non-desktop* (that kernel quirk
  is VR-HMD-specific), so the robust production config is to keep HDMI **out of the X desktop
  layout** (eDP-only) and lease the idle connector, rather than lease a connector X is actively
  displaying (which would live-reconfigure the root screen).
- **A2 (REJECTED) — Wayland kiosk (cage/wlroots) with `wp_drm_lease_v1`.** Replace the whole imag
  desktop with a Wayland compositor, run OBS on Wayland, lease HDMI to the presenter. Rejected:
  cage/weston are absent (new big dependency); cage is single-output (imag needs eDP AND HDMI →
  a multi-output wlroots compositor would have to be written/deployed); OBS-on-Wayland + a full
  provisioning rewrite is a disproportionate new failure surface on a 25 W production box during
  the season, for the same goal a lease reaches without touching the eDP desktop.
- **A3 (REJECTED as the solution; kept only as an emergency stopgap) — Xorg fuses.** Keep Program
  on HDMI via the OBS projector, but forbid other windows there (openbox per-app rules / a
  second WM-less X screen / HDMI off in the layout). Rejected: HDMI **stays** part of X (exactly
  what the owner wants gone); window-rule isolation is best-effort (a misconfig / new app /
  OBS re-open defeats it — the "raz dobre raz zle" nondeterminism the owner rejects); no full
  page-flip control, so the residual hitch stays.

### 5B — Program feed (folded into the chosen isolation)

- **B1 (CHOSEN for v1) — NDI loopback.** Add a dedicated "imag Program" NDI output in imag OBS
  (DistroAV already present); the presenter receives it via the existing `NdiReceiver` and
  page-flips. Reuses the whole existing NDI stack.
- **B2 (REJECTED for v1) — v4l2loopback.** OBS virtual-camera → `/dev/videoN` raw → presenter.
  No compression, local, but needs the `v4l2loopback` module (not loaded) + OBS venc + a copy;
  less integrated with the NDI-native stack.
- **B3 (FUTURE optimization) — DMA-BUF / EGL zero-copy.** A bespoke vendored-OBS output plugin
  exports the Program GL texture as a dma-buf the presenter imports zero-copy — lowest latency,
  but high complexity and tight lifecycle coupling. Noted as an M5 latency optimization, not v1.

## 6. Chosen architecture (A1 + B1) — structure and topology

**Topology (target state):**
- `eDP-1` = the only X output. Operator desktop: OBS UI + the Multiview projector window. X
  vsyncs it with no dual-output conflict → the #1146 HDMI-primary doctrine and the picom
  compositor complexity **retire for the projector** (picom already off).
- `HDMI-A-1` = leased **out of X**, driven exclusively by a dedicated `imag-presenter` process
  with its own vblank-locked page-flip (`KmsPresenter`).
- Program feed: OBS "imag Program" NDI output → `NdiReceiver` → presenter → page-flip.

**Code structure (reuse, not greenfield — investigate-existing-first, real sources read):**
- `src/probe/kms.rs` — the page-flip engine. Today `KmsPresenter::open()` takes **full** DRM
  master (needs fbcon detached; used on cam2 where no X runs). Add a lease-open path, e.g.
  `KmsPresenter::open_leased(lease_fd, crtc_id, connector_id, canvas)`, that skips the
  master-acquire/fbcon-detach (X already owns the console) and drives only the leased CRTC. The
  pure mode-select / BO-copy / back-buffer logic (already Tier-0 unit-tested) is unchanged.
- `src/presenter_kind.rs` + `src/probe/presenter.rs` — the `Presenter` trait + `/dev/dri/card*`
  enumeration (#854) reuse as-is.
- `src/ndi.rs` / `src/ndi_display.rs` — the NDI receive stack reuse for the Program feed.
- **NEW** crate-root pure module (Tier-0), e.g. `imag_presenter_lease`: the pure decision "which
  output + which free CRTC to lease" from a RandR provider/resources dump — unit-tested, no
  hardware. Mirrors `order_drm_card_candidates` (`src/presenter_kind.rs`).
- **Framework check (verify in the implement phase, do not assume):** the X lease request goes
  through `x11rb`/`xcb-randr` (`randr::create_lease`) if it covers RandR 1.6 leases, otherwise a
  thin FFI to `libxcb-randr` `xcb_randr_create_lease` — **not** a hand-rolled X protocol. Confirm
  `x11rb`'s RandR 1.6 coverage before choosing.

**Bring-up / provisioning topology:**
- X layout → eDP-only (autostart / xorg.conf.d, the existing boot-durable authority per #522).
- `imag-presenter` = a user systemd unit (enable-only, never `--now` at provisioning) that, at
  session start, requests the lease and starts page-flipping the Program; re-requests the lease
  on OBS restart / connector re-plug.
- Supervision: a dead-man / wedge watchdog reusing the `camera-box-deadman` +
  `wedge-watchdog-pattern` recipes; reboot + OBS-restart durability.

## 7. Latency / genlock analysis (honest)

- The current path already has latency (OBS render → GL → X window present → i915 scanout) and
  an occasional hitch. The presenter path **replaces** the X-window present with a dedicated
  vblank-locked page-flip (a better shape against tear/hitch) but **adds** a Program-feed hop:
  NDI speedHQ encode → localhost → decode → a small receive FIFO in the presenter (exactly the
  "own FIFO" the ticket anticipates).
- Therefore latency is **not automatically lower**. The Program-feed hop must be **measured**
  against today's baseline; no latency-improvement claim without measurement (and objective
  scanout tear needs the #781 tap).
- The **imag 3 ms NDI mandate is unchanged** — it governs the camera NDI *inputs* into OBS. The
  Program feed is a **separate new budget** to define and tune with a low-latency NDI receive
  (small FIFO). If the measured Program-feed latency is unacceptable, B3 (dma-buf zero-copy) is
  the escalation.

## 8. E2E impact

- **imag leg recording is UNCHANGED.** The imag-leg recording-verdict records the OBS Program
  **canvas inside OBS**, independent of how Program reaches HDMI.
- **`[0/8]` projector preflight CHANGES.** Today it checks the OBS fullscreen Program projector
  window on HDMI. With the presenter it becomes: "presenter process alive + successful DRM
  page-flips on the leased HDMI CRTC" (mirrors cam2-painter liveness + the `presenter:` log-line
  in `.claude/rules/presenter-drm-selection.md`).
- **New drift facets** `hdmi_leased` / `presenter_pageflip` flow into the shared
  `scripts/lib/imag-display-path.sh` verdict (the #780/#1040 shared-verdict framework), consumed
  by `drift-guard --check-imag`, the `[0/8]` preflight, and `verify-imag.sh` acceptance — with
  the same two-tier UNKNOWN discipline (missing tool → UNKNOWN by name; empty gather → UNKNOWN,
  never a false DRIFT).
- Verification of actual scanout tear stays **report-only** until the #781 tap.

## 9. Milestones (phased, each independently verifiable)

- **M1 — DRM-lease bring-up.** A standalone `imag-presenter` binary requests a lease of
  `HDMI-A-1` + a free CRTC and drives a static test pattern via the adapted `KmsPresenter`.
  Verify: HDMI shows the pattern, the eDP desktop is untouched, no window can land on HDMI (X has
  no output there). Tier-0 pure lease-select logic + rig bring-up.
- **M2 — Program feed live.** Add the "imag Program" NDI output in OBS; the presenter receives it
  and displays the live Program on HDMI. Verify: Program on HDMI, latency measured vs baseline,
  imag leg recording unchanged.
- **M3 — Provisioning + supervision.** Fold into `setup-imag.sh` (X eDP-only layout, presenter
  systemd unit enable-only); dead-man/wedge watchdog; reboot + OBS-restart durability.
- **M4 — E2E integration.** `[0/8]` preflight facet flip (presenter liveness + page-flip vs the
  OBS projector window), drift-guard facets, `verify-imag.sh` acceptance. Honest report-only
  tear stance until #781.
- **M5 (future).** dma-buf zero-copy Program feed as a latency optimization; scanout
  tear-detection once the #781 physical HDMI tap lands.

## 10. Open questions / risks

- **`x11rb` RandR 1.6 lease coverage** — confirm before committing to it; FFI fallback otherwise.
- **Lease of an idle-but-owned connector on modesetting** — confirm the exact flow (eDP-only
  layout + lease the free HDMI output) behaves deterministically across a lightdm/X restart; the
  presenter must re-request the lease at each session start.
- **Program-feed latency** — the one real numeric risk; measured at M2, escalated to B3 only if
  the NDI-loopback budget is unacceptable.
- **Doctrinal reversals to record with evidence when implementing:** the #1146 HDMI-primary
  persistence and the picom facet retire for the projector (HDMI leaves X); the #1107 OBS EGL
  projector-vsync becomes irrelevant to imag HDMI (no OBS projector there) but stays for the
  Windows boxes.

## 11. References

- Issue #1152 (this ticket) — owner request + STEP-0 validation + design comment.
- Issue 1146 / issue 1107 — the deployed vsync line this continues.
- Issue 1147 — the single-display fallback (rejected direction; MV on panel must run).
- Issue 781 — physical HDMI tap (`ops-wait`); gates objective tear measurement.
- `src/probe/kms.rs`, `src/probe/presenter.rs`, `src/presenter_kind.rs` — the page-flip engine.
- `src/ndi.rs`, `src/ndi_display.rs` — the NDI receive stack.
- `.claude/rules/presenter-drm-selection.md` — DRM card numbering + `presenter:` log-line facts.
- `.claude/rules/imag-display-path.md` — the shared display-path drift-verdict framework.
- RandR 1.6 output leasing (`RRCreateLease`) + modesetting DDX support — X.Org / Keith Packard
  patch series (2017–2018), Monado direct-mode docs.
