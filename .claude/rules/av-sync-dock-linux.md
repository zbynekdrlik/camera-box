---
paths:
  - "scripts/obs_dock_state.py"
  - "scripts/lib/obs-box-avsync-dock.sh"
  - "scripts/lib/obs-box-loudness-dock.sh"
  - "scripts/lib/obs-box-plugin-dock.sh"
  - "tests/python/test_obs_box_loudness_dock_1436.py"
  - "tests/qt/**"
  - "tests/python/test_obs_dock_state_1470.py"
  - "tests/python/test_obs_box_avsync_dock_1470.py"
  - "tests/python/fixtures/obs_dock_state_1470/**"
  - "vendor/av-sync-dock/CMakeLists.txt"
---

# The A/V-sync dock + the LUFS meter on the Linux OBS boxes (issues 1470, 1436)

The stream OBS lost the camera-box A/V-sync dock when stream-snv went from Windows to Linux: only
windows-genlock.yml built `vendor/av-sync-dock`. Since issue 1470 the linux-genlock strih job builds
it too and stages `obs-audio-video-sync-dock.so` + its `data/` next to obs-vban.so, so setup-strih
step 4's bundle install carries it to every Linux OBS box (no second install path).

## The build

- Configured like obs-vban: `cmake -B build_ubuntu -DCMAKE_PREFIX_PATH=$GITHUB_WORKSPACE/obs-sdk
  -DQT_VERSION=6`. The SDK's Development component exports `obs-frontend-apiConfig.cmake`; Qt6 is the
  system `qt6-base-dev` OBS itself built with.
- **C++ standard:** the dock's CMakeLists says `CMAKE_CXX_STANDARD 11`, but the sources need C++17
  (`std::complex(1.0f, -1.0f)` CTAD, `emplace_back()` returning a reference). CMake raises the
  standard to what Qt6::Core's `INTERFACE_COMPILE_FEATURES cxx_std_17` requires (Windows relies on
  the same). A local `g++ -std=gnu++11 -fsyntax-only` fails, `-std=gnu++17` is clean: syntax-check
  the TUs at gnu++17 with the stub `plugin-macros.generated.h` + `obsconfig.h`
  (`av-sync-dock-decode-worker.md`) and `-I/usr/include/x86_64-linux-gnu/qt6{,/QtCore,/QtWidgets,/QtGui}`.
- The dock's source anchors stay the ci.yml Rust tests (`tests/av_sync_dock_*.rs`, the twins of the
  Windows pwsh steps). `tests/linux_genlock_workflow_gate.rs` forbids `shell: pwsh` in linux-genlock.yml,
  so never copy the pwsh dock steps there. Its `CMAKE_PREFIX_PATH` count (5) counts this configure.
- The CEF strih job is ~2 h: a change to `scripts/obs_dock_state.py`, `tests/qt/**` or the fixtures
  re-runs it (the trigger list), because the real-Qt proof runs there on the boxes' Qt release.

## OBS hides a plugin dock; the saved DockState decides

- `obs_frontend_add_dock_by_id` (OBSStudioAPI.cpp) ends `setVisible(false); setFloating(true)`. The
  plugin adds its dock in `obs_module_post_load`; `OBSBasic.cpp` restores `[BasicWindow] DockState`
  (base64 of `QMainWindow::saveState()`) AFTER that. A saved layout without the dock = hidden.
- **A blob Qt refuses resets the operator's WHOLE dock layout** (`on_resetDocks_triggered`). So
  `scripts/obs_dock_state.py` parses the Qt 6 format exactly and refuses anything else, and
  `tests/qt/run_dock_state_restore_1470.sh` restores every seed with the REAL Qt (linux-genlock strih
  job; locally with dev1's Qt 6.4 dev files). A seed mutant that leaves the area count stale makes Qt
  refuse the restore: that is what the runner catches.
- The Qt 6 stream format (qdockarealayout.cpp): int `0xff` + int version, uchar `0xfd`, int area
  count; per area int pos (0 left, 1 right, 2 top, 3 bottom) + QSize + an info. An info: uchar `0xfc`
  (or `0xfa` + int current tab), uchar orientation, int item count; a widget item = `0xfb`, QString
  (uint32 byte length + UTF-16BE), uchar flags (1 visible, 2 floating), 4 ints (pos, size, 2 dummies;
  floating: x, y, w, h); a nested item = `0xfc` + 4 ints + its own info. Then central QSize, 4 corner
  ints (1/2/4/8), floating tab groups (`0xf9` + QRect + info), then the toolbar state (left as is).
- Qt keeps a PLACEHOLDER for a named dock that does not exist at restore and writes it back on save:
  stream-snv's ported Windows layout still holds the dock, so the dock shows there with no seed.
- Reading a box's layout (read-only): `grep '^DockState=' ~/.config/obs-studio/user.ini` over ssh,
  then `obs_dock_state.parse()` on the decoded blob lists every dock + its flags.

## The seed and its marker (setup-strih step 16g, stream role)

- absent -> inserted at the end of the right area (a new right area before any higher-numbered one);
  hidden + floating (OBS's own default) -> re-docked in place, same byte length, ONLY before the
  first seed; anything else is the operator's and kept.
- OBS saves the same hidden + floating state when the operator floats the dock and closes it, so the
  first seed decision writes `[CameraBox] AvsyncDockSeeded=true` INTO user.ini in the same atomic
  rewrite (never a side file: a restored user.ini must lose the marker together with its layout,
  review round 2). libobs `config_save` and strih-operator's configparser rewrite both keep the section.
- The rewrite: one line changed (`newline=""` keeps CRLF), a temp file renamed over the name, the mode
  AND (as root) the owner kept before the rename; never through a symlinked user.ini; never while OBS
  runs (`strih_operator_obs_running`, the ONE probe).

## A second seeded dock: the LUFS meter (issue 1436)

- Owner 4.11.2026: the Windows stream OBS had a LUFS meter; the Linux migration lost it. The pinned obs
  release (v0.4.13+) carries norihiro obs-loudness-dock 0.5.1 (`obs-plugins/loudness-dock.so` +
  `share/obs/obs-plugins/loudness-dock/locale/`), installed by step 4 with the bundle. Its dock object
  name has TWO dots: `net.nagater.obs-loudness-dock..main`.
- ONE engine, a spec per dock: `scripts/lib/obs-box-plugin-dock.sh` holds the seed / loaded verdict /
  grade rows / grade report, parametrized by `OBS_BOX_DOCK_<KEY>_{MODULE,ID,MARKER,LABEL,TAG,STEP,ISSUE,INSTALL}`
  (read by indirect expansion); `obs-box-avsync-dock.sh` and `obs-box-loudness-dock.sh` are the two
  specs + thin wrappers (the A/V-sync output byte-identical to before). A third dock = a third spec,
  never a copy of the engine.
- Each dock has ITS OWN first-seed marker (`obs_dock_state.py --marker KEY`, default
  `AvsyncDockSeeded`; the meter `LoudnessDockSeeded`), in the same `[CameraBox]` section. One shared
  marker would make an already-seeded box treat the meter's hidden + floating default as the
  operator's and never re-dock it. `--marker` must be a plain key (letters + digits).
- Placement: step 16g seeds the meter right AFTER the A/V-sync dock, so the absent-insert path appends
  it to the right area under the A/V-sync dock (stream-pp: both in the new right area). stream-snv's
  ported Windows layout already holds the meter visible (a Qt placeholder): `present`, marker only.
  Stream role only (`obs_box_step_applies loudness-dock`). The Qt runner's case 6 restores it.

## verify-strih item 47

Every role: `(avsync-dock)` the .so + `locale/en-US.ini` in the /usr prefix, `(avsync-dock-loaded)`
the newest OBS log's `Loaded Modules:` list (NOTE with no log). Stream role only (`obs_box_step_applies
avsync-dock`): `(avsync-dock-layout)`. The strih role loads the same plugin from the same bundle, and
the dock auto-starts its measurement output there too (#690 auto-start); its cost on the production
strih OBS (`publish_max_us=` / `decode_dropped=`, OBS output skipped frames) is a post-deploy read.
The same line grades the LUFS meter (issue 1436): `(loudness-dock)`, `(loudness-dock-loaded)` on every
role, `(loudness-dock-layout)` on the stream role (verify-strih.sh is at its 1000-line budget, so the
second call shares item 47's line).
