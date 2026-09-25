---
paths:
  - "vendor/obs-studio/frontend/widgets/OBSProjector.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSProjector.hpp"
  - "vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSBasic_Preview.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSQTDisplay.cpp"
  - "tests/obs_projector_stock_toplevel_1357.rs"
  - "vendor/obs-studio/frontend/widgets/OBSQTDisplay.hpp"
  - "tests/obs_display_resize_debounce_1358.rs"
---

# OBS projector window path: the stock toplevel on every OS (the Linux child-host from #1352 is RETIRED, #1357)

## Status: RETIRED 24.9.2026 — do not reintroduce a per-OS projector host

Issue 1352 (22.9.2026) hosted every Linux OBS projector in a native CHILD of a plain host
toplevel. On strih-lx, which then ran XWayland + NVIDIA PRIME render offload, a projector whose
GL surface IS the X toplevel stalled the graphics thread ~0.5 s per present: program lag 93 %,
MV 1.8 fps. `xdotool windowreparent` into a child fixed it (lag 0 %, MV 29.8 fps).

Issue 1357 removed it. **Why:**

- **The premise is gone.** On 23.9.2026 strih-lx moved to openbox on plain Xorg, with NVIDIA as
  the primary provider (`xrandr --listproviders`: `NVIDIA-0` Source Output, `modesetting` only
  Sink/Offload). There is no Xwayland process and no `__NV_PRIME_RENDER_OFFLOAD` in the OBS env.
  imag and a future strih PP use the same baseline (`obs-box-baseline.md`), and the Windows boxes
  never used the host.
- **The hosted shape had its own bug (owner ruling on issue 1346, 23.9.2026).** Toggling "always on
  top" at runtime turned the projector BLACK. `SetAlwaysOnTop` (`utility/platform-x11.cpp`) is
  `setWindowFlags(flags); show();` on the host toplevel. Qt recreates the host's native window, and
  the child's native GL window loses its parent.
- **One unified design.** A Linux-only projector path was a per-box divergence with no remaining
  reason (the issue-1357 owner principle).

**What replaced it:** the four vendored files (`OBSProjector.cpp/.hpp`, `OBSBasic_Projectors.cpp`,
`OBSBasic_Preview.cpp`) are restored byte-for-byte to their pre-issue-1352 content. Only the two
issue-1352 commits had touched them. That means:

- the projector is `OBSQTDisplay(widget, Qt::Window)`, created parentless by `OpenProjector`;
- geometry is saved and restored on the projector itself;
- `SetIsAlwaysOnTop` calls `SetAlwaysOnTop(this, ...)`, and `UpdateProjectorAlwaysOnTop` calls
  `SetAlwaysOnTop(projectors[i], ...)`.

This is the same upstream code on Linux and Windows. The runtime `strih-mv-host` X11 re-hosting
helper (the other issue-1352 workaround) is retired too: setup-strih step 8b only removes a leftover
install, and verify-strih item 22 grades it absent.

**Guard:** `tests/obs_projector_stock_toplevel_1357.rs`, a pure-std source anchor that runs
offline with plain `rustc --test` (set `CARGO_MANIFEST_DIR`). It pins:

- the stock ctor, and no `projectorWindowFlags` / `Toplevel()` / host teardown plumbing;
- a creation site with no `__linux__` branch, no `QVBoxLayout`, and no `new OBSProjector(host`;
- that EVERY `SetAlwaysOnTop(` call under `frontend/widgets/*.cpp` passes the window itself,
  never `window()` or `Toplevel(`.

**If a present stall ever comes back** on a Linux OBS box, fix the box's display stack in the
shared baseline (Xorg + a primary GPU, no offload). Never re-add a per-OS projector host. A hosted
GL child cannot survive Qt's native-window recreation on a window-flags change, and the
workaround only ever existed for a display stack no box runs any more.

Deploy: a frontend change ships as a FULL bundle (obs64), never fast-dll (`obs-titlebar-build-id.md`).
Rig acceptance after the deploy, on strih-lx:

- the multiview projector, windowed AND fullscreen, for 5 min: `program-render-audit lagged=0`
  and `multiview-audit rendered_fps >= 28`;
- toggling always-on-top at runtime keeps the picture.

## Display resize is DEBOUNCED (#1358) — never resize the display per Qt event

`OBSQTDisplay::resizeEvent` (the base of every preview AND projector window) no longer calls
`obs_display_resize` itself. A window drag emits a resize per step; `obs_display_resize` only stores
`next_cx/next_cy`, but the graphics thread then runs `gs_resize` (swap-chain reallocation) on its
next render of that display, and the PROGRAM render shares that thread — dragging the strih-lx
multiview projector measured `program-render-audit lagged=80/150`. Now `resizeEvent` restarts a
150 ms single-shot `QTimer` and `ApplyDisplayResize()` applies the final pixel size once + emits
`DisplayResized` (so the preview layout and the swap-chain size change together). The first resize
after `CreateDisplay` stays immediate (`resizeImmediate`). The `visibleChanged`/`screenChanged`
lambdas in the ctor keep their direct resize (create/visibility paths, not a drag storm). Same code
on every platform. The timer is created FIRST in the ctor (before `WA_NativeWindow` makes the
native window); `ApplyDisplayResize` returns when `destroying`, and the inline `DestroyDisplay()`
stops a pending apply (hence the header `#include <QTimer>` — a forward declaration does not compile
there). `resizeImmediate` stays set when the display was created from `paintEvent`/`visibleChanged`
with no resize after it, so the FIRST step of the first drag is applied at once — one extra
swap-chain reallocation per window lifetime, by design; do not "optimise" it into a per-drag one.
Guard: `tests/obs_display_resize_debounce_1358.rs` (incl. the invariant "exactly
3 `obs_display_resize(` call sites in the file" — keep the token out of comment prose there).

## Local type-check of a vendored frontend `.cpp` against the REAL Qt6 headers (Tier-0, no cmake)

dev1 has the Qt6 dev headers (`/usr/include/x86_64-linux-gnu/qt6`), so a frontend widget can be
`g++ -fsyntax-only` checked with tiny scratch stubs for the OBS side (`obs.hpp` with an `OBSDisplay`
wrapper + the `obs_display_*` prototypes, `obs-nix-platform.h`, `utility/display-helpers.hpp`,
`utility/SurfaceEventFilter.hpp`, an empty `moc_<File>.cpp`) — the real `.cpp` and its real `.hpp`
compile against real Qt, so a wrong Qt signature (a `connect` overload, a missing include) fails
locally instead of at CI:
`g++ -std=c++17 -fsyntax-only -fPIC -Wall -Wextra -include <qt6>/QtGui/QScreen -I<stubs> -I<qt6> -I<qt6>/QtCore -I<qt6>/QtGui -I<qt6>/QtWidgets vendor/obs-studio/frontend/widgets/OBSQTDisplay.cpp`.
Two traps: `-include QScreen` is needed (the `screenChanged` connect instantiates
`QMetaTypeId<QScreen*>`, which in the real build comes in transitively), and `-DENABLE_WAYLAND`
cannot be used (dev1's Qt is < 6.9 and has no `qpa/qplatformnativeinterface.h` private header).
A header-level check of `OBSProjector.hpp` (a scratch TU that constructs `new OBSProjector(nullptr,
src, -1, ProjectorType::Multiview)` and calls its public API) needs NO obs.hpp stub: `-I
vendor/obs-studio/libobs` gives the real `obs.hpp`, and the only missing piece is the CMake-generated
`obsconfig.h` (a stub with the four `OBS_*_PATH`/`PREFIX` defines). The full `.cpp` needs
`OBSBasic.hpp`'s whole include tree, so check the header, not the translation unit (issue 1357).
