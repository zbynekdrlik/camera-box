---
paths:
  - "vendor/obs-studio/frontend/widgets/OBSProjector.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSProjector.hpp"
  - "vendor/obs-studio/frontend/widgets/OBSBasic_Projectors.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSBasic_Preview.cpp"
  - "vendor/obs-studio/frontend/widgets/OBSQTDisplay.cpp"
  - "tests/obs_projector_child_host_1352.rs"
  - "vendor/obs-studio/frontend/widgets/OBSQTDisplay.hpp"
  - "tests/obs_display_resize_debounce_1358.rs"
---

# Linux OBS projector hosted in a CHILD window (#1352) — the present-stall class + how to change it safely

## The fix in one paragraph

On strih-lx (XWayland + NVIDIA PRIME render offload) an OBS projector whose GL surface IS the
X toplevel window (`OBSProjector : OBSQTDisplay`, constructed `OBSQTDisplay(widget, Qt::Window)`)
blocks the graphics thread ~0.5 s **per present** — avg render 513 ms, program lag 93 %, multiview
1.8 fps, identical windowed / fullscreen / no-always-on-top / 640×360; `__GL_SYNC_TO_VBLANK=0`
changes nothing; `glxgears` under PRIME runs fine. The property that matters is "the NVIDIA GL
surface is not the toplevel X window" — proven live by `xdotool windowreparent <projector>
<obs-main>` → lag 0 %, avg render 23 ms, MV 29.8 fps. Cure (LIVE since #1352): on Linux the single
creation site `OBSBasic::OpenProjector` wraps the projector in a plain host toplevel and builds the
projector as its native **child** (`Qt::Widget`); Windows/macOS stay a toplevel projector.

## Why hosting the display as a child actually moves the GL surface (the load-bearing fact)

`OBSQTDisplay`'s ctor sets `Qt::WA_NativeWindow` **unconditionally** (+ `WA_DontCreateNativeAncestors`),
so the display ALWAYS owns its own native GL window whether it is `Qt::Window` or `Qt::Widget`. With
`Qt::Widget` under a host toplevel, that native GL window becomes a native CHILD X window of the host
— exactly the "GL surface is not the toplevel" shape. This is the same shape the OBS main window's
preview/program displays already use (native children of the QMainWindow) and they present fine.

## The seam: a `Toplevel()` accessor = `window()`, unconditional, `== this` when unhosted

Do NOT `#if` every toplevel call. Add `QWidget *OBSProjector::Toplevel() { return window(); }` and
route EVERY toplevel-only call inside `OBSProjector` through it. `window()` returns `this` when the
projector is a top-level (Windows / no-host — **behaviorally byte-identical**) and the host toplevel
when it is a hosted child (Linux). Only TWO things are `#if defined(__linux__)`-gated: the window-flag
choice (`projectorWindowFlags(host)` → `host ? Qt::Widget : Qt::Window`) and the host construction at
the creation site.

## The toplevel-only calls that must route — INCLUDING the runtime ones (the #1352 review miss)

The ctor calls are easy to find. The trap (a real [major] review finding) is the RUNTIME toggle
paths, which live in DIFFERENT functions and even a DIFFERENT file:

- `OBSProjector::SetIsAlwaysOnTop` → `SetAlwaysOnTop(this, ...)` — `SetAlwaysOnTop`
  (`utility/platform-x11.cpp`) does `setWindowFlags + show`, so on a child it would try to make it a
  toplevel again (re-introducing the stall). Route to `SetAlwaysOnTop(Toplevel(), ...)`.
- `OBSBasic::UpdateProjectorAlwaysOnTop` (`OBSBasic_Preview.cpp`) → `SetAlwaysOnTop(projectors[i], ...)`
  — route to `projectors[i]->window()`.
- Geometry PERSISTENCE in `OBSBasic_Projectors.cpp` (`saveGeometry`/`restoreGeometry`/`normalGeometry`/
  `setGeometry` on `projector`) → `projector->window()->...`, or a windowed projector's saved position
  is lost (a child's saveGeometry is meaningless).

When you touch this area, grep BOTH files for every window op on `this`/`projector`/`projectors[i]`
(`setWindowFlags|setWindowTitle|setWindowIcon|showFullScreen|showNormal|setGeometry|restoreGeometry|
saveGeometry|normalGeometry|windowHandle|isFullScreen|isMaximized|resize\(|geometry\(\)|screen\(\)|
SetAlwaysOnTop`) — the ctor is NOT the whole surface. `activateWindow()` is fine bare (Qt documents it
to act on the toplevel containing the widget).

## `isOBSProjectorWindow` is Windows-only — null-guard the toplevel handle

The `windowHandle()->setProperty("isOBSProjectorWindow", true)` is read ONLY in
`OBSBasic::SetDisplayAffinity`, and `SetDisplayAffinitySupported()` returns **false** on X11
(`utility/platform-x11.cpp`), true on Windows. On the Linux hosted path the host toplevel is not
realized at ctor time (its `windowHandle()` may be null), so route it as
`if (QWindow *h = Toplevel()->windowHandle()) h->setProperty(...)` — harmless-skip on Linux, still
set on Windows (Toplevel()==this, handle non-null).

## Host teardown must be wired BOTH directions (or you leak a toplevel / dangle a pointer)

The host is a plain `QWidget(nullptr, Qt::Window)` with `WA_DeleteOnClose`, a zero-margin
`QVBoxLayout`, `setFocusProxy(projector)` (so the Escape QAction still fires) and
`installEventFilter(projector)`. Two directions, one `bool closing` guard:

- **Projector deleted first** (Escape→DeleteProjector, monitor-replace, CloseAllProjectors,
  source-destroyed): `~OBSProjector` does `if (Toplevel() != this && !closing) Toplevel()->deleteLater();`
  — tears the host down. (`window()` in the destructor is only COMPARED, never dereferenced, so a
  mid-destruction host pointer is safe.)
- **Host closed first** (WM "X"): the projector's `eventFilter` catches `QEvent::Close` on the host,
  sets `closing = true` and runs `DeleteProjector(this)` (so multiviewProjectors/SaveProjectors
  bookkeeping runs), then returns false to let the host delete itself; `closing` makes `~OBSProjector`
  skip the redundant host teardown. Qt cancels the projector's pending `deleteLater` when the host
  deletes it as a child, so no double-free either ordering.

## Testing: CI is the first compile — buy verification back with the standalone-rustc anchor recipe

The vendored Qt/C++ compiles ONLY on `linux-genlock.yml` (Tier-0, `# airuleset:build-ok` disabled).
Verify a change with:
- `tests/obs_projector_child_host_1352.rs` — a pure-`std` source-anchor guard, run offline via
  `CARGO_MANIFEST_DIR=<worktree-abs> rustc --test --edition 2021 tests/<file>.rs -o /tmp/x && /tmp/x`
  (exit 101 = RED, 0 = GREEN). A `count(method) == count(Toplevel()->method)` invariant proves every
  toplevel call is routed. **Substring-collision traps for the count invariant:** `geometry()`
  collides with `screens()[m]->geometry()` and `setGeometry(`, and prose in a nearby COMMENT that
  names a method (`windowHandle()`, `geometry()`) inflates the count — anchor those precisely
  (`prevGeometry = Toplevel()->geometry()`, `Toplevel()->windowHandle()`) instead of counting, and
  keep the method token out of comment prose (write "its window handle" not "windowHandle()").
- `cargo fmt --all --check` parses + brace-checks the Rust test; a `{}`/`()` delta count vs
  `git show origin/dev:<file>` catches an unbalanced C++ edit in one second.
- This is a #773-class defensive/behavioral guard, NOT a rig-critical divergence that a subtree pull
  would silently revert while still compiling — so it is a **Rust anchor only, no pwsh mirror** in the
  windows-genlock ymls (the change is `#if __linux__`-gated; Windows compiles the unchanged path).

## Deploy: FRONTEND change = FULL-BUNDLE deploy (obs64), never fast-dll

This lands in `obs64`/the frontend, not `obs.dll` — see `obs-titlebar-build-id.md` /
`rig-state-inspection.md`. Acceptance (supervisor, strih-lx): deploy the CI strih bundle,
`systemctl --user stop strih-mv-host.service`, open the multiview projector windowed AND fullscreen,
5 min: `program-render-audit lagged=0`, GetStats render lag < 0.5 %, `multiview-audit rendered_fps
>= 28`, 0 restarts; then provisioning flips the belt-and-braces helper unit to disabled-by-default.

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
on every platform. Guard: `tests/obs_display_resize_debounce_1358.rs` (incl. the invariant "exactly
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

