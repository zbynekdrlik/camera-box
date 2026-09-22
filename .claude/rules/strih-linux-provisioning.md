---
paths:
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "scripts/lib/strih-provision.sh"
  - "scripts/lib/ndi-runtime.sh"
  - "scripts/genlock-runtime-packages.sh"
  - "systemd/strih-obs.service"
  - "systemd/strih-bundle-state-server.service"
  - "tests/strih_provision_pure_functions.rs"
  - "tests/ndi_runtime_lib.rs"
---

# strih-lx — the Linux notebook replacing the Windows strih PC (issue 1317)

The strih cutter/mix role is migrating from the Windows STRIH-SNV PC to a Linux notebook
(`strih-lx`). The owner ruling (16.9.2026): the notebook **runs IN PARALLEL** with the Windows PC
until everything is tuned on it — the Windows PC stays the production cutter meanwhile. This rule
covers the provisioning scaffolding built during that preparation.

## The parallel-run contract (why the namespacing + the client-clock matter)

While both boxes run, TWO strih senders coexist on the NDI wire and must never collide:

- **strih-lx's NDI OUTPUTS are namespaced `STRIH-LX (...)`** — `STRIH-LX (2ME PGM)` /
  `STRIH-LX (2ME PVW)` + the `STRIH-LX (interkom|MULTIVIEW|Grading)` republishes. The stream box +
  the receivers must **never** see a second `STRIH-SNV (...)` sender. `verify-strih.sh` fail-closes
  on any live output that is a `STRIH-SNV (...)` name (`strih_lx_no_second_strihsnv_sender`).
- **The clock role is now a PARAMETER, and post-M4 the notebook IS the fleet NTP master (updated
  #1317 remainder, 22.9.2026 — the parallel run ENDED with the M4 cut-over on 20.9.).** The old
  "dantesync CLIENT only, the Windows PC keeps the single NTP-master role" contract was true ONLY
  during the parallel run; it is STALE. Since M4, `strih.lan` = the notebook (10.77.9.202) and the
  cam boxes take NTP from it, so `setup-strih.sh` step 2 provisions dantesync with a ROLE
  (`STRIH_LX_DANTESYNC_ROLE`, default **`server`**): `strih_dantesync_unit_text ROLE [ARGS]` renders
  the bare NTP-master ExecStart for `server` (folding the live hand `dantesync.service.d/10-ntp-master.conf`
  drop-in INTO the unit, and removing any stale drop-in), or `--ntp-server <host>` for the historical
  `client` role. The self-check is now the role-aware `strih_lx_dantesync_role_ok ROLE ARGS`
  (fail-closed on an AMBIGUOUS shape — server WITH args, client with a master/empty arg, an unknown
  role — but NEVER on a plain server role, which is correct post-M4); the internal
  `strih_lx_dantesync_is_client_not_master` is retained as the client-branch classifier. `verify-strih`
  adds a role live-check: `:8898/status` reachable + `mode` LOCK/NANO + (server role) an ntp UDP :123
  listener. To run a future box as a client again: `STRIH_LX_DANTESYNC_ROLE=client`.

  The NDI-output `STRIH-LX (...)` namespacing above is a SEPARATE matter (issue 1347 owns the rename
  back to non-namespaced production names now the Windows strih is retired); it is untouched here.

## Ubuntu 26.04 (owner ROZHODNUTÉ 18.9.2026)

The strih-lx notebook runs **Ubuntu 26.04 LTS (resolute)**, NOT 24.04 (owner: „chcem prejst na 26
to je nova lts verzia … mal by byt este lepsi support"). This is a deliberate divergence from
imag-nb (retired to the owner, noble) — strih-lx is the only Linux OBS box, so its bundle builds on
a different image than imag's.

Box facts (live on 10.77.9.187, 26.04.1 LTS `resolute`, i5-13450HX / RTX 5050 Mobile GB207M):

- `linux-generic-hwe-26.04` (7.0.0-31) EXISTS on 26.04, so the derived HWE kernel name works
  directly; `linux-generic` is the GA fallback.
- `nvidia-driver-595-open` (595.91.07-0ubuntu0.26.04.1) is recommended for the RTX 5050 — the SAME
  driver `setup-imag.sh` step 9 installs (the proprietary 595 does not init Blackwell).
- Runtime sonames differ from noble: `libavcodec62` (ffmpeg 8.0.1), `libqt6core6t64` 6.10.2 —
  noble's `libavcodec60` / `libqt6core6` do NOT exist there, so a **24.04-built bundle cannot load**.
  `libgles2-mesa-dev` / `libpipewire-0.3-dev` / `qt6-base-dev` exist under the same names.

What this decision changed (issue 1317, owner 18.9.):

1. **CI: the strih variant builds on the `ubuntu-26.04` hosted runner.** `linux-genlock.yml` job
   `linux-genlock-build-strih` is `runs-on: ubuntu-26.04` (ONLY that job — the imag-parity
   `linux-genlock-build` full build and `linux-distroav-compile-check` stay on `ubuntu-24.04`). The
   apt dependency list stays the shared `OBS_APT_PACKAGES` (the names above exist on 26.04); if a
   package name ever differs on resolute the job fails loud and the fix is a strih-scoped apt
   override, never dropping a dependency. The CEF pin is unchanged (CEF 6533 is glibc-portable).
2. **The release-parity marker + gate.** The Stage step writes `TARGET-RELEASE: ubuntu-26.04` into
   `STRIH_BUILD_FLAGS.txt`, single-sourced from the job env `STRIH_TARGET_RELEASE: 'ubuntu-26.04'`
   (`runs-on` cannot read job env, so `tests/linux_genlock_workflow_gate.rs` pins the two literals
   EQUAL). The pure predicate `strih_lx_release_parity_ok BUNDLE_FLAGS_TEXT BOX_VERSION_ID`
   (`scripts/lib/strih-provision.sh`) → 0 iff the flags text carries `TARGET-RELEASE:
   ubuntu-<BOX_VERSION_ID>`; a mismatch OR an absent marker OR an empty VERSION_ID → 1 (fail-closed).
   `setup-strih.sh` step 4 gates the STAGED bundle's marker vs the box `/etc/os-release` VERSION_ID
   BEFORE the `cp -a` install (`fail` on mismatch — never install a bundle built for another
   release), and `verify-strih.sh` item 15 asserts the SAME on the installed bundle. So a
   24.04-built bundle can never silently run on the 26.04 box (it would crash OBS at load).
3. **The installer kernel is release-derived.** `scripts/install-imag-nb.sh` gains a pure
   `imag_kernel_meta_package VERSION_ID AVAILABLE` (`linux-generic-hwe-<VERSION_ID>` when that exact
   name is in the target's `apt-cache pkgnames linux-generic` list, else `linux-generic`; empty
   VERSION_ID → non-zero). The chroot step reads `VERSION_ID` from the target's `/etc/os-release`
   and the available list inside the chroot (after its apt-get update), then installs the derived
   meta — so the ONE installer serves both a 24.04 and a 26.04 live stick, never a hardcoded noble
   literal. The `ls /boot/vmlinuz-*` fail-loud checks are unchanged.

Static IP **10.77.9.203** (free; .202 strih, .204 stream); `strih-lx.lan` DNS is a MikroTik static
entry = owner step (the router is read-only for me), scripts dial the IP / hostname.

## Module structure (the source-of-truth split)

- **`scripts/lib/strih-provision.sh`** — the ONE place the strih-lx role FACTS + pure decisions live
  (the `obs-fleet.sh` / `camera-set.sh` source-only convention: `# airuleset:script-ok`, no
  top-level statements, no `set -euo pipefail` leak). Sourced by `setup-strih.sh`, `verify-strih.sh`
  AND `tests/strih_provision_pure_functions.rs` (rustc-free Tier-0). Pure fns: the 10 NDI inputs,
  the namespaced outputs/republishes, the floor-3 latency, the strih CI artifact name, the
  dantesync-client args + client-not-master check, the profile facts, the MiniFuse-4/PipeWire audio
  route + fail-loud TODO predicate, the output-name + no-2nd-STRIH-SNV guards, and the verify
  predicates (render-tick / distroav-loaded / nvenc-available / single-timesync-authority).
- **`scripts/setup-strih.sh`** — numbered, enable-only, fail-loud, idempotent provisioning flow
  (13 steps) run ON the box as root; models `setup-imag.sh`. BASH_SOURCE-guarded so the tests can
  source it. Reuses `genlock_write_markers` (never re-implemented) and the canonical remoteos-mcp /
  bundle-state tooling.
- **`scripts/verify-strih.sh`** — the acceptance gate; feeds live reads into the lib predicates and
  exits non-zero on any failed item. Latency pins are REPORT-ONLY vs
  `scripts/latency-pins-baseline.json`'s `strih-lx` key (`_all_camera_ndi_inputs_ms` = 3).

## OBS supervision launcher pair — strih-obs-start.sh / strih-obs-stop.sh (issue 1317, DONE)

`systemd/strih-obs.service` (`ExecStart=/usr/local/bin/strih-obs-start.sh`,
`ExecStop=/usr/local/bin/strih-obs-stop.sh --exec-stop`) supervises OBS on strih-lx — the sibling of
`imag-obs.service` / `imag-obs-start.sh` (issue 882). The launcher pair now EXISTS
(`scripts/strih-obs-start.sh` / `scripts/strih-obs-stop.sh`); before it landed the enabled user unit
flapped `203/EXEC` (executable not found, `Restart=on-failure`) and `verify-strih.sh` failed at "OBS
not running under the supervisor".

- **strih-lx facts the imag launcher does NOT cover** (why it is a bespoke pair, never a symlink to
  the imag one): a GNOME **Wayland** session on 26.04 (`DISPLAY` is unset in the user session — the
  start script resolves `WAYLAND_DISPLAY` from the `wayland-*` socket under `XDG_RUNTIME_DIR`, else
  XWayland `DISPLAY=:0` when an X socket exists, else FAILS LOUD, via `strih_resolve_session_env`);
  the OBS binary at `/opt/obs-genlock/bin/obs` (not on `PATH`); NO taskset pin unless
  `STRIH_ISOLATED_CPUS` / `/etc/strih-isolated-cpus.conf` exists (never a guessed pin, the imag #841
  lesson); NO DRM lease. `--profile` / `--collection` are passed only when those OBS dirs
  exist (a fresh box has none → default).
- **The scene-seeder preflight + launch-time seed (issue 1317, DONE — was a follow-up).** After the
  `[ -x "$OBS_BIN" ]` check and BEFORE launching OBS, the start script preflights the seed's Python
  import chain (`python3 -c "… import strih_scenes"`) and FAILS the unit if it is broken (a missing
  `python3-websocket` etc.) — the imag issue 1156 pattern, so a broken seed never `Restart`-loops a
  live OBS. Then, AFTER the `:4455` WS-up wait (+ a 2 s ident settle), it runs
  `python3 /usr/local/bin/strih_scenes.py --bootstrap`. See the "OBS input/scene/Studio-Mode seeder"
  section below.
- **The unit lifetime IS OBS's lifetime** (`Type=simple`, the imag #882 pattern): the start script
  `wait`s on the OBS pid and propagates its exit — a segfault → non-zero → `Restart=on-failure`; an
  operator quit → 0 → left alone. The stop script routes a PLAIN invocation through
  `systemctl --user stop` (so a deliberate stop is never mistaken for a crash and re-launched), and
  runs the SIGTERM → 15 s → SIGKILL ladder only under `--exec-stop` (the unit's own `ExecStop` mode).
- **The install + lock-step gate.** `setup-strih.sh` step 8 `install -m 0755`s BOTH launchers into
  `/usr/local/bin` BEFORE `systemctl --user enable`. `scripts/lib/strih-provision.sh`
  `strih_launcher_pair_ok <bin-dir>` (both present + executable; prints the missing/non-executable
  name) is checked by `verify-strih.sh` FIRST — before the "OBS running under the supervisor" item —
  so a dangling launcher names itself. A `tests/strih_provision_pure_functions.rs` lock-step anchor
  pins the unit's ExecStart/ExecStop basenames EQUAL to the installed launcher basenames, so a future
  rename can never re-dangle the ExecStart target.

## OBS input/scene/Studio-Mode seeder — strih_scenes.py (issue 1317, DONE)

strih-lx's OBS boots EMPTY (no scenes/inputs) — it cannot receive or switch any fleet source until
something seeds it. `scripts/strih_scenes.py` is the focused strih-lx sibling of `imag_scenes.py`
(seeded on every launch), reusing the on-box `obs_phase2.py` certified-genlock primitives instead of
importing imag's 70 KB DRM-lease/encoder/picom machinery that strih-lx does not need.

- **SCOPE: the 10 NDI INPUTS + one per-input scene + Studio Mode**, read from
  `/opt/camera-box/strih-lx-seed.json` (setup-strih.sh step 6). The **5 STRIH-LX NDI OUTPUTS are a
  SEPARATE ticket** — the seeder never touches `outputs`.
- **Pure helpers (Tier-0 testable, no rig):** `parse_seed_manifest(text) → (inputs, outputs, latency)`;
  `certified_genlock_settings(latency)` = the locked baseline `{ndi_bw_mode:0, genlock_fifo:True,
  ndi_sync:2, latency:<floor>}` (latency rides the manifest floor **3**, NOT obs_phase2's probe
  default 0); `seed_inputs(inputs, latency)` → one `{scene, input, ndi_source_name, settings}` per
  input (scene = the source display name e.g. `CAM1 (usb)`; input = `NDI ` + name, kept DISTINCT from
  the scene; `ndi_source_name` is a TOP-LEVEL field, not inside `settings`); `scene_order`;
  `input_parity_problems`. Duplicate/empty names are dropped so a repeated name never double-creates.
- **`--bootstrap` (the default action):** connect WS (`127.0.0.1:4455`, no-auth), per input
  `CreateScene` + `CreateInput` (kind `ndi_source`) ignoring "already exists", THEN re-apply the
  certified settings over an EXISTING input (`SetInputSettings overlay:True`) — because `CreateInput`
  on an existing input fails "already exists" and would NOT update settings, so genlock_fifo could
  otherwise silently drift off across a relaunch (genlock is not a forgettable toggle). Read-back-verify
  each `ndi_source_name` via `obs_phase2.reenforce_ndi_name` (the #795-safe #1158 shape) when
  obs_phase2 is importable, else a direct overlay set + read-back. Finally `SetStudioModeEnabled true`.
- **`--verify-parity` (read-only):** prints ONE whole line `strih ndi inputs: OK` (or the problem
  list) that `verify-strih.sh` item 4b greps with `grep -qxF`, **report-only** so a not-yet-launched
  box (OBS/WS down) is a NOTE, never a hard FAIL — the seed is a LAUNCH-time action, not a
  provisioning artifact.
- **The camera class carries the floor on `genlock_latency_ms_src`, NEVER on the stock `latency`
  key (live finding 19.9.2026).** DistroAV's `latency` is a receive-buffer MODE enum (0 NORMAL /
  1 LOW — `imag_scenes.py` and the feedback class use `1`), and the genlock build's certified
  coercion forces it back to `0` on every `genlock_fifo` input. The original seed wrote `latency: 3`
  there, so the effective read-back never matched the class and the first `--bootstrap` after the 2ME
  fix reported all 8 camera inputs "healed" on a healthy box (a SetInputSettings + ndi_source_name
  re-enforce on every launch — never the pure read the update-only path promises). The 3 ms floor is
  the per-source `genlock_latency_ms_src` pin (issue 235 single knob; `latency-pins-verify.md`), which
  the genlock build defaults to 3, so a healthy camera reads back `latency 0` + `genlock_latency_ms_src
  3` and `settings_update_needed` is False. When a class key looks "always dirty", read the effective
  settings on the live box FIRST (`_effective_input_settings`) — a coerced key is not a drifted key.
- **obs_phase2 is imported LAZILY** (`_obs_phase2_module`, the #1156 class): an older box may lack it,
  so the read-back verify degrades to a direct set rather than crashing the boot seed. The top-level
  `from websocket import create_connection` IS the dep the launch preflight validates.
- **Install + launch wiring.** `setup-strih.sh` step 6 `install -m 0755 "${HERE}/strih_scenes.py"
  /usr/local/bin/strih_scenes.py` (fail-loud if absent next to the script), alongside the manifest +
  `obs_phase2.py`. `strih-obs-start.sh` preflights `import strih_scenes` BEFORE launch and runs
  `--bootstrap` AFTER the `:4455` wait. Tests: `tests/python/test_strih_scenes_1317.py` (pure helpers +
  the two static-anchor asserts on the launcher/setup wiring — a python assert over the script text,
  so it runs in Tier-0 with no cargo).

## 2ME feedback inputs are NOT genlocked — per-input settings class (issue 1317, DONE)

The seed is split into TWO settings CLASSES keyed on the input display NAME, not one uniform genlock
dict:

- **`camera` class** — the certified genlock dict (`genlock_fifo=True, ndi_sync=2, ndi_bw_mode=0,
  latency=<manifest floor>`), for the `CAMn (usb)` grabbers AND the `RESOLUME-SNV (cg-obs)` sender
  (a genlocked sender per issue 1300).
- **`feedback` class** — `ndi_sync=1` (SOURCE_TIMING), `latency=1`, `ndi_bw_mode=0`, and
  `genlock_fifo=False` (**explicit `False`, not omitted** — so `SetInputSettings(overlay=True)` CLEARS
  a mis-seeded `genlock_fifo=True` off an already-existing input; an omitted key would leave the stale
  `True`), for any name ending `(2ME PGM)` / `(2ME PVW)` regardless of the `STRIH-SNV` / future
  `STRIH-LX` self-loop prefix (issue 1347 inherits it by suffix).

**Why:** the `STRIH-SNV (2ME PGM)` / `(2ME PVW)` inputs are the strih's own POST-RENDER 30 fps
program/preview outputs received back as monitoring feedback. A post-render output is off the camera
boundary grid and every program CUT is a timecode discontinuity, so a genlock FIFO underruns and
relocks on every cut — **110,222 underruns / 899 relocks measured live on strih-lx 19.9.** against 4–73
underruns total on the genlocked cameras. This mirrors the Windows strih, which receives these
NON-genlocked (`light.json` `NDI 2ME PGM`/`PVW`: `ndi_sync:1, latency:1`, no `genlock_fifo`).

- **Seams:** `input_class_for(name) → "camera"|"feedback"`; `input_settings_for(name, latency)` returns
  the class dict; `seed_inputs` applies it per input (the real manifest → exactly **8 camera + 2
  feedback**). `input_parity_problems` is class-aware (a feedback input left `genlock_fifo=True` is
  flagged; a camera without genlock is flagged as before).
- **`--bootstrap` is UPDATE-ONLY.** Per input it reads the effective settings (`_effective_input_settings`,
  the obs_phase2 defaults-merged shape, reusing the file's own `Obs.req`) and emits
  `SetInputSettings(overlay=True)` ONLY on a class mismatch (`settings_update_needed`), re-enforcing the
  `ndi_source_name` only after a real change — so a healthy relaunch is a pure read and a mis-seeded box
  self-heals on the next `--bootstrap`. Never deletes/recreates an input.
- **verify-strih.sh item 4b** prints a report-only `seeded input classes -- N camera, M feedback (...)`
  NOTE from the seeder's `strih ndi input classes:` line; the `strih ndi inputs: OK` verdict line
  (`grep -qxF` anchor) is unchanged.

## Fixed HDMI fullscreen projector — the strih_scenes.py projector seed (issue 1346, DONE)

**The owner ROZHODNUTÉ (19.9.2026 11:05):** the strih-lx HDMI output is an **OBS fullscreen
projector** on the HDMI display, **selectable in OBS between Program and Multiview**, and **persisted
across relaunches** — **NOT** Xorg + a DRM-lease scanout (the imag-1152 path was the rejected
alternative). This is Prístup 1 of the issue-1346 design.

- **Why OBS's own projector.** OBS already has the whole mechanism: right-click preview → Fullscreen
  Projector (Program) / Multiview (Fullscreen) → the operator's monitor. `SaveProjectors=true`
  persists that choice in the scene collection's `saved_projectors` and OBS re-opens it on every
  launch. No window-manager scripting / compositor plugin needed. The **OBS UI projector menu stays
  the primary operator switch**; `strih_scenes.py --projector program|multiview` is the scripted twin
  — **run it with `sudo`** (it rewrites the root-owned `/opt/camera-box/strih-lx-projector.json`; a
  non-root invocation fails loud with `PermissionError`).
- **ProjectorType numbers (OBS `saved_projectors` `type`):** **3 = StudioProgram, 4 = Multiview.**
  strih runs Studio Mode always, so a Program projector persists as StudioProgram (3). The Windows
  strih's current `saved_projectors` is exactly one entry `{"monitor":0,"type":4}` = Multiview → so
  **Multiview is the default**, Program the alternative. `/opt/camera-box/strih-lx-projector.json`
  (`{"type":"multiview"|"program"}`) selects it; `setup-strih.sh` step 6 writes it default-multiview
  **guarded by `[ ! -f ... ]`** so it never overwrites the operator's later choice.
- **`SaveProjectors=true` is the OPPOSITE of imag.** imag (`setup-imag.sh` #522) uses
  `SaveProjectors=false` + an openbox-autostart boot hook that RE-OPENS the projectors (re-applying
  settings). strih-lx has **no such boot hook**, so it relies on OBS's own `SaveProjectors` restore +
  the `seed_projector` idempotency check. `setup-strih.sh` step 7 pre-seeds `[BasicWindow]
  SaveProjectors=true` + `ProjectorAlwaysOnTop=true` in the desktop user's `user.ini` (idempotent
  `RawConfigParser` upsert; the OBS default is `SaveProjectors=false`, so without this a projector
  would never come back after `strih-obs.service` relaunches).
- **`seed_projector(obs)` runs AFTER the input seed inside `--bootstrap`** (and standalone via
  `--projector`): read the type → `GetMonitorList` → `projector_monitor_index` (the first monitor
  whose `monitorName` does NOT start with `eDP` — the internal panel) → **no external monitor ⇒ log
  "projector: no external monitor, skipping" and RETURN, NEVER fall back to the eDP panel** (that
  would cover the operator UI; the next launch re-checks) → read the current collection's
  `saved_projectors` (resolved robustly: the authoritative user.ini `[Basic] SceneCollectionFile`
  base → the `GetSceneCollectionList` name → a glob of every `basic/scenes/*.json` as a last resort,
  since OBS slugifies a display-name with spaces into a DIFFERENT filename — an exact-name-only read
  would fail-open and stack a duplicate window) → `projector_already_saved`
  skips if that type is already saved on that monitor (OBS re-opens saved ones itself; a second
  `OpenVideoMixProjector` opens a DUPLICATE window, imag #756 class) → else `OpenVideoMixProjector`
  with the mapped `videoMixType` (`projector_type_to_mix`: program→PROGRAM, multiview→MULTIVIEW).
- **Live facts today:** `GetMonitorList` = one monitor `eDP-2(0)` (1920×1080); every HDMI connector
  disconnected. So the seed SKIPs cleanly until a display is plugged into HDMI (the owner rig step for
  acceptance). The seed never opens a projector on the live box from CI — the supervisor runs the seed
  on strih-lx.
- **verify-strih.sh item 4c — REPORT-ONLY** (pure `strih_projector_verdict` in
  `scripts/lib/strih-provision.sh`): `SaveProjectors=true` present in `user.ini`; and — when an
  external HDMI/DP monitor is connected (a `/sys/class/drm/card*-HDMI*/status` or `DP*` = `connected`)
  — a saved `ProjectorType` 3/4 entry exists → PASS, else NOTE. `hdmi-absent` NOTEs "HDMI display not
  connected". Never a hard FAIL (the live open needs a display).
- **Tests:** `tests/python/test_strih_projector_1346.py` (pure helpers + fake-WS `seed_projector` +
  setup-strih anchors) and the `strih_projector_verdict` case in
  `tests/strih_provision_pure_functions.rs`.

**Follow-up (MEASURED, its own ticket if it bites): HDMI-Multiview tearing.** The issue-1107
present-vsync arming covers ONLY the fullscreen **non-multiview Program** projector
(`OBSProjector.cpp` `savedMonitor > -1 && !isMultiview`, `.claude/rules/obs-projector-vsync.md`). The
**Multiview** projector is NOT vsync-armed. On the notebook (no compositor) this may tear on the HDMI
output; **measure it once a display is on the notebook** and, if tearing shows, extend the arming to
the multiview projector when it is the only fullscreen projector — that is a **vendored OBS change**
(`vendor/obs-studio`), so its own ticket, never bolted onto this provisioning lane.

## Runtime packages + the /usr prefix install (issue 1317, DONE)

The genlock bundle is a tarball BUILT for the `/usr` prefix and links release-specific Qt6 /
ffmpeg 8 / libOpenGL runtime libraries. Before this, `setup-strih.sh` step 4 only `cp -a`d the tree
to `/opt/obs-genlock` (on no loader path) and installed NO runtime packages, so on a fresh 26.04 box
`obs` died at exec with `libavcodec.so.62: cannot open shared object file` (13 unresolved sonames:
`libQt6{Core,DBus,Gui,Network,Svg,Widgets,Xml}.so.6`, `libavcodec.so.62`, `libavformat.so.62`,
`libavutil.so.60`, `libOpenGL.so.0`, plus the bundle's own `libobs.so.30` / `libobs-frontend-api.so.30`).

- **CI records the runtime package list into the bundle.** `scripts/genlock-runtime-packages.sh
  --stage <dir> --out <file>` walks `ldd` over the staged `bin/obs` + every `*.so` (with the stage
  lib dirs on `LD_LIBRARY_PATH` so bundle-internal sonames resolve INSIDE the tree and are DROPPED),
  maps each remaining resolved SYSTEM library path to its apt package via `dpkg -S`, and writes the
  sorted-unique package list to `RUNTIME_PACKAGES.txt`. It **fails loud on any `=> not found`**. The
  runner that BUILT the binary is the only authority on what it links (a hand-curated 26.04 list rots
  on every soname bump). Its pure half `runtime_packages_from_ldd STAGE` is sourceable + guarded for
  Tier-0 testing with fake `ldd`/`dpkg` on PATH. BOTH Linux jobs' Stage steps run it BEFORE
  `genlock-manifest.sh --stage` (a `Record runtime packages (#1317)` step) so `RUNTIME_PACKAGES.txt`
  is in the bundle manifest; it travels in BOTH bundles (a re-provisioned imag-nb hits the same gap).
- **setup-strih.sh step 4 installs the packages, then the bundle into its /usr prefix.**
  `strih_runtime_packages_from_file` (pure, comment/blank-safe parser in `scripts/lib/strih-provision.sh`)
  → `DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends <list>` BEFORE the
  bundle install; a bundle WITHOUT `RUNTIME_PACKAGES.txt` **fails the step** (the same fail-closed
  contract as `TARGET-RELEASE`). Then `strih_install_bundle_prefix <bundle> <libdir> <bindir>
  <sharedir>` installs libs → `/usr/lib/x86_64-linux-gnu` (root:root, dirs 0755, files a+rX — the
  issue-1236 perms normalize), `bin/obs` → `/usr/bin/obs` (0755 root), `share/obs` → `/usr/share/obs`,
  then `ldconfig`. `/opt/obs-genlock` STAYS the staged copy + marker home (`verify-strih.sh` reads
  the markers there).
- **The launcher runs `/usr/bin/obs`.** `scripts/strih-obs-start.sh` launches
  `${STRIH_OBS_BIN:-/usr/bin/obs}` (was `/opt/obs-genlock/bin/obs`).
- **verify-strih.sh gates both** (a new item BEFORE the "OBS running under the supervisor" check):
  `strih_ldd_unresolved` (pure parser of `ldd` output) over `/usr/bin/obs`,
  `/usr/lib/x86_64-linux-gnu/libobs.so.30` and `.../obs-plugins/distroav.so` must be EMPTY, AND
  `/opt/obs-genlock/RUNTIME_PACKAGES.txt` must exist with every listed package `dpkg -s` installed
  (the item names the first missing one).
- **Duplication to consolidate (deploy-arm follow-up).** `strih_install_bundle_prefix` deliberately
  duplicates ~30 lines of the imag on-box install program (the templated heredoc inside
  `scripts/deploy-genlock-fleet.sh`, its `cp -a` + issue-1236 perms-normalize + `ldconfig` block).
  That program has its own probe-gated anchors, so extracting a shared prefix-install helper is the
  `deploy-genlock-fleet.sh` strih-lx EXECUTE-arm follow-up's job — do it THEN, not now.

## Baseline completeness — the six live-found gaps, now durable (issue 1317, DONE)

Bringing the 26.04 notebook up by hand (18.9.2026) surfaced six provisioning steps `setup-strih.sh`
/ `verify-strih.sh` were missing (each hand-applied to get the box green, now made durable so a
fresh box reaches a green baseline like imag-nb from a single `setup-strih.sh` run):

1. **dantesync installed as a SERVICE (step 2).** step 2 used to only VALIDATE the client args, so
   the box had NO timesync. It now writes `/etc/systemd/system/dantesync.service` via the pure
   `strih_dantesync_unit_text` (the EXACT cambox shape: `Type=simple`, `Restart=always`,
   `RestartSec=5`, `ExecStart=/usr/local/bin/dantesync --ntp-server strih.lan`,
   `WantedBy=multi-user.target`), `daemon-reload`, clears a stale `/var/run/dantesync.lock`, enables
   and (if the binary is present) restarts it. `strih_dantesync_unit_text` **fail-closes** on a
   server/master invocation (never a 2nd NTP master while parallel); `--service` is a run mode, NOT
   an installer flag, so it never appears in the unit.
2. **The NDI 6.3.2 runtime (new step 4b).** `setup-strih.sh` had NO NDI-runtime step, so DistroAV
   loaded UI-only (`ERR-404 NDI library not found`). The imag step-10 recipe is now the shared
   **`scripts/lib/ndi-runtime.sh`** (`ndi_runtime_install_cmds NDI_PEER CAM_PW [USER] [NDI_DIR]`,
   emits the on-box statements for the caller to `eval`), reused by BOTH `setup-imag.sh` step 10
   (replacing its inline copy — behaviour unchanged, plus an idempotent perms-normalize) AND
   `setup-strih.sh` step 4b (before the OBS launch). It copies `libndi.so.*.*.*` from a cam box
   (default cam1; `STRIH_NDI_PEER=<ip>` overrides, `CAM_PW` required when the runtime is absent),
   **normalizes it root:root a+rX** (a 0600 copy gives the obs user `Permission denied` at dlopen),
   writes `/etc/ld.so.conf.d/ndi.conf` + `ldconfig` (NO `grep -q` on the pipe — the SIGPIPE footgun),
   the `/usr/local/lib/libndi.so.6` symlink DistroAV's Linux loader scans, and avahi-daemon.
3. **qt6-wayland (a RUNTIME_PACKAGES `--extra`).** OBS is a Wayland Qt app on 26.04 GNOME and aborts
   `Could not find the Qt platform plugin "wayland"` without it; it is DLOPEN'd at runtime so `ldd`
   never sees it. `genlock-runtime-packages.sh` gained a `--extra <pkg>` always-include flag (union
   helper `runtime_packages_with_always_include`); the strih CI recorder step passes
   `--extra qt6-wayland` so it rides `RUNTIME_PACKAGES.txt` and setup-strih step 4 apt-installs it.
   The non-Wayland imag bundle passes no `--extra`.
4. **OBS config dir owned by the desktop user (steps 5/7).** step 5's `mkdir -p "$OBS_CFG"` ran under
   sudo and root-owned `~/.config/obs-studio`, so the obs user could not create `.sentinel`
   (`Permission denied`). It is now `install -d -o "$DESKTOP_USER" -g "$DESKTOP_USER"`, and step 7
   chowns `$OBS_CFG` to the desktop user (catches an earlier root-seeded run).
5. **bundle-state :8899 server tree (step 9).** step 9 installed only the unit and WARNed the server
   tree was absent. It now installs `bundle-state-server.py` + `bundle_state_gather.py` +
   `obs_phase2.py` under `/opt/camera-box` (the setup-imag step-28 pattern, via `curl`+`GH_TOKEN`)
   BEFORE enabling `strih-bundle-state-server.service`, so the unit's `ExecStart` imports resolve.
6. **verify-strih.sh two false FAILs.** (a) item 6 read `/etc/dantesync/config.json` — a
   Windows/imag artifact a flag-based Linux client never creates; it now asserts the dantesync UNIT
   is `active` + a FRESH in-bound offset via the SHARED `dantesync_offset_verdict` /
   `ptp_locked_from_journal` (the cambox verify-device `(d)` shape, sourced from
   `scripts/clock-offset-guard.sh`). (b) item 11's
   `[ "$(systemctl is-enabled sleep.target || echo masked)" = masked ]` DOUBLE-appended (a masked
   unit makes `is-enabled` print `masked` AND exit 1, so `|| echo masked` yields `masked\nmasked`) —
   a FALSE FAIL on a correctly-masked box. It now grades via the pure `strih_verify_sleep_masked`
   (FIRST line only), with the `|| echo masked` dropped.

Pure helpers added to `scripts/lib/strih-provision.sh`: `strih_dantesync_unit_text` +
`strih_verify_sleep_masked` (unit-tested in `tests/strih_provision_pure_functions.rs`); the new
shared lib `scripts/lib/ndi-runtime.sh` is unit-tested in `tests/ndi_runtime_lib.rs`; the
`--extra`/always-include mechanism in `tests/genlock_runtime_packages_1317.rs`.

## GOTCHA — an emit-and-eval `_cmd` helper's `${N:?}` fires on an EMPTY optional arg and silently no-ops the whole recipe (issue 1317 review)

The `ndi_runtime_install_cmds` / `strih_lx_chrome_sandbox_fix_cmd` family EMITS on-box bash for the
caller to `( eval "$(...)" ) || fail`. A parameter written `pw="${2:?cam pw required}"` fires the
`:?` guard on an **empty** value, not only an unset one — and callers pass optional args as
`"${CAM_PW:-}"` (empty when unset). So on an idempotent re-run where the arg is legitimately not
needed (the NDI runtime is already present, so no fetch, so no password), the emitter returns 1 with
**empty stdout**; `( eval "" ) || fail` then runs `eval ""` → exit 0, `|| fail` never triggers, and
the ENTIRE recipe — including the parts that should run unconditionally (perms-normalize, ldconfig,
symlinks, avahi) — is silently skipped while the caller prints success. A false-SUCCESS on re-runs,
invisible to a fresh-box test (which always supplies the arg).

**Fix:** make the arg `"${N-}"` (no colon, empty allowed) at the function header, and gate it ONLY
where it is actually used — move a `[ -n <arg> ] || { echo '…required' >&2; exit 1; }` INSIDE the
branch that needs it (the fetch `if`), not at the top. The unconditional tail then always emits. Bake
the arg `%q`-quoted so the emitted `[ -n <arg> ]` is safe for empty / spaces / shell-metachars (verify
all three parse via `ndi_runtime_install_cmds peer '' user | bash -n`). This preserves byte-equivalence
with an old inline block that ran its tail unconditionally.

## Staging + ssh gotchas for setup-strih (live, 18.9.2026)

- **The tree rsynced to the box must carry `scripts/` AND `systemd/`.** `setup-strih.sh` steps 8/9
  read `${HERE}/../systemd/strih-obs.service` (a sibling of `scripts/`), so a scripts-only sync FAILS
  at step 8 (`systemd/strih-obs.service not found next to this script`). Stage the repo `scripts/`
  and `systemd/` dirs together (run 2 on 18.9. failed exactly here until `systemd/` was synced).
- **Launching setup-strih over ssh needs the sudo password on stdin.** A bare `sudo … nohup … &` over
  ssh has no tty → the log holds only `sudo: A terminal is required to authenticate`. Use
  `echo "$PW" | sudo -S -p "" bash -c "nohup … &"`.
- **Poll with `pgrep -x setup-strih.sh`, never `pgrep -f`.** `pgrep -f setup-strih.sh` matches its OWN
  command line and reports RUNNING forever (run 1 on 18.9. "ran" for 20 min this way while setup had
  actually never started).

## Audio — the local PipeWire graph (issue 1344, WIRED)

There is NO Dante on the strih PC (owner 15.9.). **Program audio is a NETWORK stream, NOT the
MiniFuse** (design 20.9., corrected): the OBS program source `ASIO zvuk` = VB-Matrix slot VASIO8,
fed by `fohabl-strih` + `lv1-strih` VBAN (the mastered FOH mix). **The MiniFuse 4 carries only the
operator TALKBACK mic.** On Linux **PipeWire replaces VB-Matrix**: the intercom hub (issue 1345)
writes the summed program mix to a `strih-program` null sink OBS captures via
`strih-program.monitor` (the OBS input `ASIO zvuk`, now `pulse_input_capture`), and reads the
MiniFuse capture as the operator talkback into the N-1 mix. Full design + the DynamicUser decision:
`.claude/rules/strih-intercom.md` "M2 — the local PipeWire audio bridge".

`setup-strih.sh` step 12 now **INSTALLS** the graph (no more fail-loud TODO / `STRIH_LX_AUDIO_WIRED`):
the operator-session `strih-program` null sink + a WirePlumber rule pinning the MiniFuse to its
pro-audio profile @48 kHz + the intercom-hub audio drop-in (runs the hub as the operator so its
pw-cat children reach the operator PipeWire session). The OBS `ASIO zvuk` input is seeded by
`strih_scenes.py --bootstrap` (the ONE allowed create in update-only mode). `verify-strih.sh` derives
the audio verdict (`strih_lx_program_audio_verdict`: sink present + OBS input pulse_input_capture +
hub program-rx; FOH-live level is a supervisor NOTE). **Supervisor live steps:** confirm the MiniFuse
capture node name against `wpctl status`, restart the operator PipeWire/WirePlumber for the sink to
appear, and do the FOH-live level acceptance. Arena stays on the Windows PC (Spout has no Linux); its
cg feed reaches the notebook over NDI (`RESOLUME-SNV (cg-obs)`).

## CI — the strih FULL-build variant, with obs-browser + CEF (issue 1317)

`linux-genlock.yml` has a `linux-genlock-build-strih` job (artifact
`obs-genlock-linux-x86_64-strih`) copying the imag-parity full build byte-for-byte. The full build
already ships obs-websocket / obs-ffmpeg+NVENC / x264 / outputs / filters / v4l2 / DistroAV (the
ubuntu-ci preset defaults). The strih role additionally needs **obs-browser** (the 4 browser
sources: VDO.ninja interkom, Ableset, camera crew, 1pixel), which needs **CEF**.

**CEF is now WIRED** (issue 1317). The vendored ubuntu-ci preset does NOT auto-fetch CEF on Linux
(no `cmake/linux/buildspec.cmake` — only macos/windows call `_check_dependencies`), so the strih job
fetches it the same way upstream OBS's own Linux CI does:

- **The pin.** `STRIH_ENABLE_BROWSER: 'ON'` plus a CEF pin block in the job `env`: `CEF_VERSION`,
  `CEF_ARCHIVE`, `CEF_SHA256`, `CEF_BASE_URL`. These MUST match
  `vendor/obs-studio/CMakePresets.json` → `configurePresets[dependencies].vendor."obsproject.com/obs-studio".dependencies.cef`
  (the `ubuntu-x86_64` hash). For OBS 32.2.0 that is version `6533`, archive
  `cef_binary_6533_linux_x86_64_v6.tar.xz`, sha256 `7963335519a19ccdc5233f7334c5ab023026e2f3e9a0cc417007c09d86608146`,
  base `https://cdn-fastly.obsproject.com/downloads`. The `revision` (6) is the `_v6` archive
  suffix. `tests/python/test_linux_genlock_strih_cef_1317.py::test_workflow_cef_hash_matches_vendored_pin`
  guards against the workflow pin drifting from the vendored one.
- **The fetch step** (`if: env.STRIH_ENABLE_BROWSER == 'ON'`): download → `sha256sum -c` against the
  pin → `tar -xf` → locate the extracted `cef_binary_*` top dir by NAME (`find … -name 'cef_binary_*'`,
  never guessing the `_v<rev>` suffix) → export its ABSOLUTE path as `CEF_ROOT_DIR` to `$GITHUB_ENV`.
  The OBS configure passes `-DCEF_ROOT_DIR=${{ env.CEF_ROOT_DIR }}`.
- **Cache.** `actions/cache` keyed `cef-${CEF_VERSION}-${CEF_SHA256}`, so the ~325 MB download is
  paid once and a version bump busts it automatically.
- **Marker.** `STRIH_BUILD_FLAGS.txt` is env-conditional: `BROWSER-ON: obs-browser + CEF <version>`
  when ON, the original `BROWSER-OFF` text when OFF.
- **Verify.** `verify-strih.sh` reads the installed `/opt/obs-genlock/STRIH_BUILD_FLAGS.txt`; when it
  says `BROWSER-ON` it fails loud unless `obs-browser.so` AND the CEF runtime `libcef.so` are present
  under the bundle root (pure predicates `strih_lx_browser_bundle_required` /
  `strih_lx_browser_bundle_ok` in `scripts/lib/strih-provision.sh`).

**To flip browser OFF again:** set `STRIH_ENABLE_BROWSER: 'OFF'` in the job `env` — the ONLY change
needed (the CEF fetch step, the `CEF_ROOT_DIR` arg, the marker, and the verify item all follow it).

## chrome-sandbox setuid-root — the CEF sandbox helper (issue 1317 F6, DONE)

CEF ships a **SUID sandbox helper** `chrome-sandbox` alongside `obs-browser.so`/`libcef.so` (in the
strih bundle: `lib/x86_64-linux-gnu/obs-plugins/chrome-sandbox`). In the CI artifact it is mode `700`
owned by the runner uid; Chromium's SUID-sandbox contract requires it **owned root:root, mode 4755
(setuid root)** or the sandbox helper aborts at browser-source launch (`Running without the SUID
sandbox!` → the render process dies). `verify-strih.sh` item 13 only checks the *presence* of
`obs-browser.so`/`libcef.so`, not the sandbox's launchability — F6 closes that gap.

- **The rejected shortcut.** Starting OBS with the sandbox disabled at launch (`--no-sandbox`) would
  make the sources start with no chmod, but it disables the Chromium sandbox for EVERY browser source
  for the whole session — a session-wide security downgrade of all 4 web-rendering sources. The
  setuid helper is Chromium's sanctioned shape and is applied once at provisioning.
- **Pure helpers** (`scripts/lib/strih-provision.sh`): `strih_lx_chrome_sandbox_fix_cmd <bundle-root>`
  emits the idempotent `chown root:root` + `chmod 4755` statements (locates the helper by NAME under
  the install root, every statement `;`-terminated per the `v4l2-neutral.sh` `_cmd`-helper gotcha,
  runs as an `if` so a missing binary is a no-op / set-e safe). `strih_lx_chrome_sandbox_verdict
  <owner> <mode> <present>` → `ok` / `missing` / `wrong-owner` / `wrong-mode`.
- **`setup-strih.sh` step 4** (right after the bundle install, as root): when the installed
  `STRIH_BUILD_FLAGS.txt` says `BROWSER-ON`, it FAILS LOUD if `chrome-sandbox` is absent, applies the
  fix, then re-reads owner+mode through the verdict — so a chown/chmod that failed to record
  `root:root` mode `4755` in the inode (e.g. a `cp -a` that preserved the runner uid and a chown that
  did not run, or a fix that errored) is caught by name. (It does NOT detect a `nosuid` mount — that
  is an execve-time property, invisible to `stat`; the S_ISUID bit is still stored and read back.)
  A `BROWSER-OFF`/absent marker is a loud SKIP.
- **`verify-strih.sh` item 14** (read-only, on the live box): `stat -c '%U:%G'` / `-c '%a'` the same
  `find`-path and PASS only on the `ok` verdict; `BROWSER-OFF`/absent NOTE-skips. On a live box the
  PASS line reads `chrome-sandbox setuid-root (root:root 4755) -- CEF sandbox launchable`; a regressed
  helper prints e.g. `chrome-sandbox not setuid-root (wrong-owner: owner=newlevel:newlevel mode=700)`.

## CPU performance governor + Bitfocus Companion Satellite (issue 1317, this lane, DONE)

Two provisioning steps the owner caught missing LIVE on the notebook (20.9.2026 — no performance
mode, dead Stream Deck) so the ONE `setup-strih.sh` procedure brings a fresh strih-lx to full
parity in a single run (owner: „mas predsa mat jeden postup dany pre setupovanie"). Prístup 1 of
the issue-1317 design — two additive idempotent steps mirroring the existing fleet mechanism, not a
new framework.

- **CPU performance governor (setup-strih.sh step 15).** `strih_performance_mode_apply` (pure emitter
  the step `eval`s): PREFER `powerprofilesctl set performance` when power-profiles-daemon is present
  (GNOME desktop 26.04 ships it), ELSE write the `performance` governor to every
  `/sys/devices/system/cpu/cpu*/cpufreq/scaling_governor` — the setup-device.sh STEP-13 fallback —
  then re-mask the sleep/suspend targets (step 11 already masks them; the re-mask is idempotent and
  keeps the (perf) item self-contained). `strih_cpu_performance_unit_text` (the fleet
  `cpu-performance.service` oneshot: `Type=oneshot`, `RemainAfterExit=yes`,
  `WantedBy=multi-user.target`) is written to `/etc/systemd/system/` + enabled as the reboot
  persistence backstop. WHY: the distro-default `powersave`/`schedutil` is wrong for the low-latency
  genlock OBS cutter (`.claude/rules/realtime-isolation.md` + the imag power-envelope precedent).
  **verify-strih.sh item 18 `(perf)`**: `strih_verify_governor_ok` (stdin = all cores' governors,
  fail-closed on empty/unreadable) AND the existing `strih_verify_sleep_masked` — FAILs loud.
- **Bitfocus Companion Satellite (setup-strih.sh step 16) — DESKTOP TARBALL MODEL (reworked after
  the integration-review bounce).** The notebook exposes its locally-attached Stream Deck to the
  VENUE's Companion CONTROLLER (`10.77.9.205`, the strih-autorecord-coupling box) as a headless
  **SATELLITE** — NOT full Companion (a second controller would fork the venue's button/page state).
  **The bounce cause:** the first draft pinned version `1.11.0` (does not exist) and a GitHub-release
  `.deb` — Bitfocus GitHub releases carry **no `.deb` assets**. Linux x64 is an **Electron desktop
  app shipped as a `.tar.gz` from the Bitfocus CDN**. The corrected flow: `strih_companion_satellite_install`
  (pure emitter) installs the deps (`libusb-1.0-0-dev libudev-dev libfontconfig1`), then — if
  `/opt/companion-satellite/companion-satellite` is absent — downloads the **PINNED** `.tar.gz`
  (`cf-pub.bitfocus.io`, build `722-8bc2f14`), **verifies its sha256 (fail-loud on mismatch)**,
  extracts, and runs the tarball's OWN `install.sh --system --force` (idempotent; installs to `/opt`
  + the app-menu entry + the `50-satellite-desktop.rules` uaccess udev rule). **No `.deb`, no
  `systemctl start`/`enable`** — the desktop build has no system unit. The setup step then:
  (a) writes the operator-login **autostart** at `~/.config/autostart/companion-satellite.desktop`
  via `strih_companion_satellite_autostart_text` (owner rule: a needed feature is always-ON, never a
  forgettable manual launch); (b) **pre-seeds the controller** into the operator's
  `~/.config/Companion Satellite/config.json` (electron-store, note the literal SPACE in the dir)
  via `strih_companion_satellite_appconfig_json` — keys **`remoteIp`/`remotePort`** (`remoteProtocol`
  `tcp`), confirmed from the Satellite v3.4.0 source (`satellite/src/config.ts`), NOT
  `host`/`companionAddress`; `ensureFieldsPopulated` only fills MISSING keys, so a pre-written
  `config.json` is respected before first launch; (c) records the intended controller in
  `/etc/companion-satellite/host.conf` (human-readable paper trail). The version/tarball/sha/host/port
  are single-source pins overridable by `COMPANION_SATELLITE_VERSION` /
  `COMPANION_SATELLITE_TARBALL_URL` / `COMPANION_SATELLITE_SHA256` / `COMPANION_SATELLITE_HOST` /
  `COMPANION_SATELLITE_PORT` (default `3.4.0` / the cf-pub tarball / the pinned sha256 / `10.77.9.205`
  / `16622`). **verify-strih.sh item 19 `(companion)`**: `strih_companion_verdict` grades `/opt`
  binary + `50-satellite-desktop.rules` present + operator autostart present + the seeded
  `config.json` `remoteIp` == the controller — FAILs loud, fail-closed
  (`not-installed`→`no-udev-rule`→`no-autostart`→`wrong-host`→`ok`).
  - **SUPERVISOR CONFIRM on the live box (why the LIVE RUN is a followup, not a gap):** this lane is
    code-only (no ssh/deploy). The pins are verified against the live Bitfocus distribution (the
    tarball URL + sha256 were downloaded and hashed during the rework — 263 565 407 bytes,
    sha256 `32b8b443…a2a953`), but the actual `setup-strih.sh` provisioning RUN + `verify-strih.sh`
    green + the operator's Stream Deck lighting up must be done once the notebook is in hand. A wrong
    pin fails LOUD at the live run (sha mismatch / 404), never silently; the CDN build hash can rotate,
    so `COMPANION_SATELLITE_TARBALL_URL` + `COMPANION_SATELLITE_SHA256` let the supervisor repin (in
    lock-step) with no code change.

Tests: pure fns + wiring anchors in `tests/strih_provision_pure_functions.rs`; the `TOTAL_STEPS=17`
bump is reflected in both `tests/intercom_hub_provisioning.rs` and the janus test.

## GOTCHA — an emit-and-eval `_cmd`/`_install` helper that `exit 1`s needs `( eval … ) || fail`, never a bare `eval … || fail` (issue 1317 review)

When a `strih-provision.sh` emitter's failure path is `exit 1` (as `strih_companion_satellite_install`
does on a curl/apt failure — short-circuiting the rest of the emitted block), the caller MUST run it
in a SUBSHELL: `( eval "$(strih_companion_satellite_install)" ) || fail "…"`. A bare
`eval "$(…)" || fail` runs the emitted `exit 1` in setup-strih.sh's OWN shell, terminating the whole
script with the emitter's terse stderr line BEFORE `|| fail` can print its actionable message — the
`|| fail` is dead code. This is the SAME pattern step 4b already uses for `ndi_runtime_install_cmds`
(`( eval "$(…)" ) || fail`). An emitter whose failure path is instead a NO-OP `if … fi` with no
`exit` (like `strih_lx_chrome_sandbox_fix_cmd`) does NOT need the subshell. Rule: if the emitted
block can `exit`, wrap the `eval` in `( … )` so the exit is contained and the caller's `|| fail`
fires.

## Post-cut-over: the seeder targets the OPERATOR collection (strih role, this lane, 20.9.2026)

The notebook now runs the OWNER's **migrated production ("operator") collection** — the box became
THE strih. Its OBS inputs are the canonical strih names the WHOLE E2E/rig toolchain addresses
(`obs_burn_filter` / `obs_phase2` / recv-timing taps / genlock-audit / latency-pins baseline all key
on `NDI camN`): **`NDI cam1..7`, `NDI 2ME PVW`, `NDI 2ME PGM (mv)`, `cg`, `CG-obs`** — NOT the
parallel-phase derived `NDI CAMn (usb)` the old bare-string manifest produced. So the seeder gained a
**strih ROLE**:

- **The manifest carries EXPLICIT-name OBJECT entries** `{sender, input, scene}` (a DATA name-map)
  alongside the legacy bare-string shape (`_entry_fields` normalizes both). `sender` = the NDI source
  received; `input` = the OBS input the tooling addresses; `scene` = the operator's scene.
  `strih_lx_seed_manifest_json` (`scripts/lib/strih-provision.sh`) is the single source of that DATA
  map; `setup-strih.sh` step 6 writes it. **Why `NDI camN` is canonical on the production strih:**
  renaming the operator's collection to fit the seeder would break every tool that already addresses
  `NDI camN` (Prístup 2, rejected) — the seeder conforms to the operator, not the other way round.
- **`"mode": "update-only"`** makes `--bootstrap` heal the certified genlock CLASS onto inputs that
  ALREADY exist and **NEVER `CreateScene`/`CreateInput`** — a missing declared input is REPORTED, not
  created (`bootstrap(..., update_only=True)`; it also NEVER re-enforces `ndi_source_name`, since the
  operator's source binding is authoritative). `--verify-parity` skips the `ndi_source_name` check in
  this mode (`input_parity_problems(..., check_source=False)`). The legacy `create` mode (absent
  `mode`, the parallel/imag shape) is unchanged.
- **Class by SUBSTRING.** The operator's `NDI 2ME PVW` / `NDI 2ME PGM (mv)` inputs do not END in
  `(2ME PGM)`/`(2ME PVW)`, so `input_class_for` now matches the `2ME PGM`/`2ME PVW` marker as a
  SUBSTRING, and `seed_inputs` classifies feedback when EITHER the sender OR the input name carries
  it — so a supervisor-confirmable sender guess never mis-classifies (the input name already decides).
- **DATA the supervisor re-confirms live.** The 2ME-feedback source (`STRIH-SNV (2ME PGM/PVW)` during
  the parallel run; `STRIH-LX` self-loop once issue 1347 lands) and the CG-pair senders are a
  best-known DATA guess (the notebook's live collection JSON is not reachable from a worktree). Because
  update-only never renames a source, a wrong sender is harmless (drives only CLASS detection, which
  the input name provides) and is a MANIFEST edit, never a code change. The supervisor re-reads the
  live collection's input settings on the notebook and repins the senders if needed.

### The four (five) live gotchas caught bringing the notebook up as strih

1. **Duplicate receivers from the parallel-phase seed.** The old bare-string manifest derived
   `NDI CAMn (usb)` and `--bootstrap` `CreateInput`'d them alongside the operator's `NDI camN` — TWO
   DistroAV receivers per camera (both at 60 fps), duplicate CG receivers, and `NDI STRIH-SNV (2ME …)`
   inputs pulling the OLD strih's feeds (25 scenes / 30 inputs on first launch). The update-only mode
   fixes it structurally: the launch seed never creates an input again.
2. **No `curl` on a fresh box.** A fresh strih-lx has NO `curl`, and the Companion Satellite install
   emitter downloads with `curl -fsSL` → step 16 FAILed on the first run. `curl` is now in the step-16
   dep line (`deps='curl libusb-1.0-0-dev …'`), the setup-device.sh precedent.
3. **intel_pstate governor naming under ppd.** On intel_pstate ACTIVE (this Lenovo Raptor Lake)
   `powerprofilesctl set performance` sets `energy_performance_preference=performance` but leaves
   `scaling_governor=powersave` — so an `if ppd; then …; else <governor>; fi` shape NEVER wrote the
   governor and `verify (perf)` (governor==performance on all cores) FAILed while the step logged the
   self-contradicting "set to performance (now: powersave)". `strih_performance_mode_apply` now runs
   ppd AND the governor write UNCONDITIONALLY (they are complementary — ppd owns EPP, the loop owns the
   governor), and step 15 logs the EFFECTIVE triple `governor=… / EPP=… / ppd=…`
   (`strih_perf_effective_line`). The boot oneshot `cpu-performance.service` already writes the
   governor, so boot state ≠ first-run state — always check `scaling_governor` on intel_pstate, ppd is
   not enough.
4. **Satellite REST apply (:9999).** The electron-store `config.json` file seed (`remoteIp`/
   `remotePort`/`remoteProtocol:tcp`) alone left a RUNNING Satellite's EFFECTIVE controller host at
   `127.0.0.1` until a `POST /api/config {"host","port","protocol":"tcp"}` on its local REST
   (`:9999`) — after which `GET /api/status` = `connected:true`. Step 16 now runs
   `strih_companion_satellite_rest_apply_cmd` after seeding the file (best-effort: a no-op when the
   Satellite is not running), and `verify (companion)` grades the LIVE `connected` state via
   `strih_companion_status_verdict` when the REST answers (file-only when it does not). The POST uses
   the REST keys `host`/`port`/`protocol`, NOT the electron-store `remoteIp`/`remotePort`.

### GOTCHA — OBS resolves its helper processes relative to its OWN binary; a prefix install must copy the WHOLE bin/ dir

OBS spawns `obs-ffmpeg-mux` (the recording muxer) and `obs-nvenc-test` (the NVENC probe) from the
directory of its OWN executable (`os_get_executable_path`), NOT from `$PATH`. The prefix install
(`strih_install_bundle_prefix`) originally copied only `${bundle}/bin/obs` into `/usr/bin`, so those
helpers were absent beside `/usr/bin/obs`: the notebook OBS logged `[NVENC] Failed to launch the
NVENC test process` → `NVENC not supported` (while `/opt/obs-genlock/bin/obs-nvenc-test` run by hand
reported NVENC 13.0 / CUDA 13.20 / Blackwell OK), and RECORDING would fail outright (obs-ffmpeg-mux
is the muxer). The fix: `strih_install_bundle_prefix` now installs EVERY file in `${bundle}/bin/*`
(enumerated by the pure `strih_bundle_bin_files`, so a future helper rides along), root-owned 0755,
next to obs. `verify-strih.sh` item 20 FAILs loud (recording-critical) unless
`/usr/bin/obs-ffmpeg-mux` AND `/usr/bin/obs-nvenc-test` exist+executable beside `/usr/bin/obs`, and —
when OBS is running — grades the newest OBS log's NVENC state (`strih_nvenc_log_verdict`:
`[obs-nvenc] NVENC version:` = healthy, `NVENC not supported` = the missing-helper signature). The
bundle dir on the box is user-private (`drwx------ newlevel`); the install runs as root, so `find`
traverses it fine. General rule: a `/usr`-prefix install of a self-contained app bundle copies the
whole `bin/`, never a single hardcoded main binary — the app's sibling helpers must travel with it.

### GOTCHA — a pure helper that iterates the manifest `inputs` MUST normalize via `_entry_fields` (object entries are dicts, unhashable)

Once `inputs` entries can be OBJECTS `{sender,input,scene}` (the operator collection) as well as bare
strings, EVERY helper that walks `inputs` must go through `_entry_fields` — a raw `for src in inputs:
if src in seen` / `input_class_for(src)` raises `TypeError: unhashable type: 'dict'` on an object
entry. This bit `input_classes_summary` (fixed): it is called by `verify_parity` BEFORE the verdict
line, so the crash made `--verify-parity` **always** fail on the operator collection — the genlock-
class drift check was silently dead on the production strih (report-only, so it degraded to a NOTE
instead of aborting, which is exactly why the 3040-green suite missed it — no object-entry test hit
that helper). `seed_inputs`/`scene_order`/`input_classes_summary` all now share `_entry_fields`; any
NEW `inputs`-walking helper must too, and must carry an object-entry test.

### GOTCHA — a report-only bash verdict that greps a LARGE input must use a here-string, never `printf | grep -q`

`strih_nvenc_log_verdict` graded a real (100s-of-KB) OBS log with `printf '%s' "$text" | grep -q …`
inside an `if`. On an EARLY match grep closes the pipe, printf takes SIGPIPE, and under the caller's
`pipefail` the whole pipeline goes non-zero → the `if` reads false → a HEALTHY large log misgraded
`nvenc-unknown` (and a real `NVENC not supported` buried in a big log could flip to unknown too,
hiding a fault). This is the SAME SIGPIPE-under-pipefail class `drift-guard-log-parsers.md` documents
for `printf | consumer`. Fix: `grep -q PATTERN <<<"$text"` (a here-string reads the whole stdin with
no upstream pipe to break). The lib's small-input test passed while the large-input path was broken —
any log-grading verdict needs a >64 KB fixture in its test, not just a one-line sample.

## Follow-ups (not done in the preparation lane)

- ~~obs-browser / CEF in the strih CI variant~~ — **DONE (issue 1317, CEF now wired)**, see the CI
  section above.
- ~~A bespoke `strih_scenes.py` WS seeder (sibling of `imag_scenes.py`) that seeds the 10 inputs
  (genlock_fifo + floor 3) + per-input scenes + Studio Mode~~ — **DONE (issue 1317, INPUTS/scenes/
  Studio only)**, see the "OBS input/scene/Studio-Mode seeder" section above. The **5 STRIH-LX NDI
  OUTPUTS** (`STRIH-LX (2ME PGM/PVW)` + the `interkom/MULTIVIEW/Grading` republishes) are a SEPARATE
  ticket — they need a study of the Windows-strih DistroAV output+republish config (imag has one
  output, no reference); the seeder never touches outputs.
- **`deploy-genlock-fleet.sh` strih-lx EXECUTE deploy** (scp the strih artifact + ssh-run the on-box
  program). The pure helpers + the PLAN arm exist; execute reuses the imag transport with
  `fleet_linux_bundle_artifact_for strih-lx` + `STRIH_LX_IP` once the box exists.
- ~~**`strih-obs-start.sh` / `strih-obs-stop.sh`** launcher pair (sibling of `imag-obs-start.sh`) that
  `strih-obs.service` ExecStart references~~ — **DONE (issue 1317)**, see the "OBS supervision
  launcher pair" section above.
- **The 2ME self-feedback INPUT names.** issue 1317's spec lists the multiview-feedback inputs as
  `STRIH-SNV (2ME PGM/PVW)` (what the Windows box publishes). For a fully independent strih-lx the
  box's OWN multiview feedback should read `STRIH-LX (2ME PGM/PVW)` (self-loop); while parallel and
  tuning, either is fine and trivially re-seedable. Re-confirm the intended feedback source when the
  box goes live.

## What is NOT in scope

Recording retention, the NIC self-heal watcher, WoL, and the 4K multiview projector budget are all
listed in the issue-1317 inventory as NEEDS-WORK but are not part of the initial provisioning
scaffolding — they follow once the hardware is in hand. (Companion Satellite INSTALL — desktop
tarball + sha256 + `install.sh --system --force` + operator autostart + controller-host seed — is now
DONE — see the "CPU performance governor + Bitfocus Companion Satellite" section above; only the live
`setup-strih.sh` RUN + `verify-strih.sh` green remains, a supervisor step.)

### Anchor-test literal vs an expanded path (18.9.2026, CI 35390761239)

`tests/strih_provision_pure_functions.rs` pins the step-4 fail-closed message by the LITERAL
`RUNTIME_PACKAGES.txt missing`; a message that expands `${SRC_RUNTIME_PKGS} missing` reads identically
at runtime but never matches the source-text anchor. Keep the file name literal in the message and put
the expanded path in parentheses after it — the same rule for every future fail-closed marker a
static-anchor test pins.

## GOTCHA — the genlock OBS on strih-lx (Ubuntu 26.04) needs `qt6-svg-plugins` + a setuid `chrome-sandbox`, or the operator UI is half-broken (M4, 21.9.2026)

Two hand-patches applied live to the strih-lx notebook OBS during the M4 cut-over that must be
BAKED INTO `setup-strih.sh` (currently NOT provisioned — both were discovered only because the
owner reported symptoms on the production box days before a service):

1. **Missing Qt6 SVG plugins → "half the OBS icons are gone".** OBS 32's default **Yami** theme is
   SVG-based. On a fresh Ubuntu 26.04 `libqt6svg6` (the library) is installed but the Qt PLUGINS
   are split into a SEPARATE package `qt6-svg-plugins` (provides
   `/usr/lib/x86_64-linux-gnu/qt6/plugins/iconengines/libqsvgicon.so` +
   `imageformats/libqsvg.so`). Without them the SVG toolbar/dock/settings icons render BLANK while
   PNG bits (checkboxes) still show — the exact "polku ikoniek nevidim" symptom. Fix:
   `apt-get install -y qt6-svg-plugins`, then restart OBS. Diagnose: `ls
   /usr/lib/x86_64-linux-gnu/qt6/plugins/iconengines/` — an EMPTY dir is the tell.
2. **`chrome-sandbox` not setuid → CEF browser sources' sandbox broken.** The /usr-prefix genlock
   OBS uses `/usr/lib/x86_64-linux-gnu/obs-plugins/chrome-sandbox`, which shipped `0755`; CEF needs
   it `root:root 4755`. Fix: `chmod 4755` that path (standalone `sudo`, not a compound call).

Both are `setup-strih.sh` provisioning steps to add. Until then a re-flashed strih-lx OBS will
show broken icons + broken browser sources again.

3. **obs-shaderfilter plugin missing → every OBS start pops "Failed to create source 'User-defined shader'"** (the operator's "errory vyskakujú", 22.9.2026). The strih-lx collection carries 10 `shader_filter` ("User-defined shader") filters from exeldro's obs-shaderfilter, which the Linux genlock build does not ship. The prebuilt `obs-shaderfilter-<ver>-ubuntu-22.04.tar.gz` release LOADS on OBS 32.2.0 / Ubuntu 26.04 (2.6.0 verified live: `[obs-shaderfilter] loaded version 2.6.0`, all filters created) — install `bin/64bit/obs-shaderfilter.so` → `/usr/lib/x86_64-linux-gnu/obs-plugins/` and `data/` → `/usr/share/obs/obs-plugins/obs-shaderfilter/`. No libobs headers on the box, so a local build is not an option; the prebuilt tarball is the path.
4. **A migrated Windows collection carries a dead `scripts-tool` Lua** (`D:/_APPS/vban-output.lua`, VBAN→stream.lan — the VB-Matrix era). It logs `[Lua: vban-output.lua] Error opening file: (null)` every start; VBAN is the intercom hub's job now, so drop it from `modules.scripts-tool` in the collection JSON (OBS stopped, backup kept).

All four (qt6-svg-plugins, chrome-sandbox /usr path, obs-shaderfilter, the dead Lua) are `setup-strih.sh` provisioning items; 1+2 are baked (#1317 slice). 3+4 are COLLECTION concerns the owner's data owns — provisioning never rewrites the collection, so they are now covered REPORT-ONLY by verify item 28 (`strih_collection_hygiene_verdict` counts the `shader_filter` filters + the dead `scripts-tool` Lua so a re-import that re-introduces the boot popups is visible), #1317 remainder.

## The #1317 remainder — baking the last live-applied strih-lx hand-patches into provisioning (22.9.2026)

The five hand-patches that survived ONLY on the live box (a re-flash would revert each) + the E2E ffprobe gap are now provisioned (design comment 5778538537, Prístup 1 — five idempotent enable-only items + a report-only verify + a tool-dep install, no `vendor/`, no `recording-e2e.sh`/`rig-mode.sh`, the collection never rewritten):

| Item | setup-strih | verify-strih | pure helper(s) |
|---|---|---|---|
| (A) `[General] BrowserHWAccel=false` in global.ini (CEF crash-loop guard, OBS stopped) | step 7 | item 26 (FAIL) | `strih_lx_obs_global_ini_cmds` |
| (B) prune `decklink*.so`/`obs-qsv11.so`/`obs-vst.so` from both plugin dirs | step 4 tail | item 27 (FAIL) | `strih_lx_obs_plugin_prune_list` + `strih_lx_obs_plugin_dirs` |
| (C) collection hygiene (never rewrites) | — | item 28 (REPORT-ONLY) | `strih_collection_hygiene_verdict` |
| (D) RustDesk pinned .deb + sha256 + enable --now + 0600-file password | step 16b (gated on the pw file) | item 29 (FAIL / NOTE) | `strih_rustdesk_version`/`_deb_url`/`_deb_sha256`/`_install_cmds` |
| (G) dantesync ROLE (`server` default post-M4) | step 2 | item 6b (FAIL) | `strih_dantesync_unit_text ROLE`, `strih_lx_dantesync_role_ok`, `strih_lx_dantesync_status_role_verdict` |
| (H) ffmpeg/ffprobe for the on-box E2E verdict | step 4b | item 30 (FAIL) | (apt install, no pure helper) |

`TOTAL_STEPS=17` unchanged (RustDesk is the lettered sub-step 16b). Tests: `tests/strih_provision_pure_functions.rs` (source-and-call for every pure helper + static-anchor wiring). Supervisor: place the 0600 RustDesk password file at `STRIH_LX_RUSTDESK_PW_FILE`, then run `setup-strih.sh` + `verify-strih.sh` on the notebook.

5. **CEF browser sources crash-loop OBS on Wayland+NVIDIA (`libcef.so` int3 trap → exit 133 every ~60s) — the cause is `BrowserHWAccel=true`, NOT the sandbox or shaderfilter** (22.9.2026, operator "stále crashuje", whole interkom scene black). CEF is already launched `--no-sandbox` (so chrome-sandbox setuid + the apparmor userns knob are red herrings — reverted). The GPU-accelerated CEF path crashes on the RTX 5050 / GNOME-Wayland stack. Fix: `BrowserHWAccel=false` in `~/.config/obs-studio/global.ini [General]` (OBS down, edit, restart) → CEF software-renders via libvk_swiftshader, 0 crashes. **BAKED (#1317 remainder)** — `setup-strih.sh` step 7 seeds `[General] BrowserHWAccel=false` into `global.ini` (OBS stopped) via `strih_lx_obs_global_ini_cmds`; verify item 26 asserts it.
6. **obs-shaderfilter was a RED HERRING for the crash** — it only silenced the "User-defined shader" start popup; removing it (owner OK 22.9.) does NOT affect stability. With it removed the 10 `shader_filter` filters in the collection re-log the popup; strip them from the collection JSON if the popup should stay gone, or leave shaderfilter installed — cosmetic either way. **The collection is the owner's data — provisioning REPORTS, never rewrites it:** verify item 28 (REPORT-ONLY, `strih_collection_hygiene_verdict`) counts `shader_filter` filters + `scripts-tool` entries in the active collection so a re-import that re-introduces the boot popups is visible (#1317 remainder).
7. **Prune-safe plugins on strih-lx** (errors/unused every boot): `decklink*.so` (no DeckLink hardware — removed 22.9.), `obs-qsv11.so` (Intel QSV, box is NVIDIA → nvenc), `obs-vst.so` (unused). KEEP everything else, especially `distroav.so` (NDI) + `obs-browser.so`/`libcef.so`/`libvk_swiftshader.so` (browser sources, now software-rendered). **BAKED (#1317 remainder)** — `setup-strih.sh` step 4 tail prunes the three basenames (`strih_lx_obs_plugin_prune_list`) from BOTH the /opt bundle copy AND the /usr load path (`strih_lx_obs_plugin_dirs`), idempotent, re-runs each install (the bundle install re-copies the whole tree); verify item 27 asserts absence via the SAME source of truth.

8. **RustDesk remote-desktop on strih-lx** (owner request 22.9.2026): install the pinned `rustdesk-1.4.9-x86_64.deb` via `apt-get install -y ./rustdesk.deb` (pulls libxdo3 etc.), then `systemctl enable --now rustdesk.service`, set the permanent password (`rustdesk --password <fleet-pw>`), read the connect ID with `rustdesk --get-id`. GNOME/Wayland caveat: screen capture goes through the xdg-desktop-portal (pipewire) — the FIRST inbound connection may raise a one-time screen-share approval on the box's own session. **BAKED (#1317 remainder)** — `setup-strih.sh` step 16b installs from the PINNED .deb (URL + sha256 in `strih_rustdesk_deb_url`/`_deb_sha256`, confirmed == the box's installed deb byte-for-byte), fail-loud on a sha256 mismatch, `systemctl enable --now rustdesk`, and applies the permanent password read INSIDE the emitted block from a 0600 file the supervisor places at `STRIH_LX_RUSTDESK_PW_FILE` (default `/etc/rustdesk/permanent-password.secret`) — the `@JANUS_ROOM_SECRET@` discipline: the password value is never in git, in an argv, or in a log. Step 16b is GATED on the pw file's presence (SKIP + warn if absent — the supervisor must place the secret file). Verify item 29: unit active + `rustdesk --get-id` non-empty.

9. **ffmpeg (provides `ffprobe`) NOT installed by default on 26.04** (22.9.2026 15:06): the on-box release-E2E verdict (`recording-verdict-on-strih-lx.sh` runs `recording-verdict --extract-partial strih` ON the notebook) spawns `ffprobe` to demux the strih recording; a fresh box has no ffmpeg, so the `[8/8]` on-box verdict died `spawn ffprobe (install ffmpeg: apt install ffmpeg)`. **BAKED (#1317 remainder)** — `setup-strih.sh` step 4b apt-installs `ffmpeg` (a TOOL dependency of the E2E verdict, same idempotent apt family as `avahi-utils`; NOT the bundle's RUNTIME_PACKAGES.txt); verify item 30 fails loud unless `ffprobe` + `ffmpeg` are both present.

## 22.9.2026 live session — GPU, projector, Janus, NDI naming (issue 1352 + the #1317 findings comment)

9. **OBS renders on the RTX 5050 via XWayland PRIME, NEVER on the Intel iGPU and NEVER native-Wayland NVIDIA.** Measured: the iGPU (Mesa) is saturated by OBS alone (`intel_gpu_top` render 85–90 % obs), program render lag 7–20 %, multiview 6–7 fps — no scene-graph trimming fixes it. Native Wayland + `__EGL_VENDOR_LIBRARY_FILENAMES=10_nvidia.json` crash-loops (`eglSwapBuffers failed` → `The Wayland connection experienced a fatal error: Protocol error`). The working env block in `strih-obs-start.sh` (line ~110): `QT_QPA_PLATFORM=xcb __NV_PRIME_RENDER_OFFLOAD=1 __GLX_VENDOR_LIBRARY_NAME=nvidia __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json` → `Loading up OpenGL on adapter NVIDIA GeForce RTX 5050`, program 9–23 ms, `lagged=0`, GPU ~22 %. `__GL_SYNC_TO_VBLANK=0` is irrelevant. Still to bake into `strih_lx_*` provisioning (issue 1352).
10. **Under XWayland+PRIME a projector whose GL surface IS the X toplevel stalls the graphics thread ~0.5 s per present** (`OBSProjector` = `OBSQTDisplay(widget, Qt::Window)`): any MV projector open → avg render 513 ms, lag 93 %, MV 1.8 fps; windowed/fullscreen/no-above/640×360 all the same, minimized 129 ms. The main window's preview/program displays are native CHILD windows and present fine. PROVEN fix: make the projector a child window — `xdotool windowreparent <projector> <obs-main>` → lag 0.0 %, MV 29.8 fps. Live workaround = `strih-mv-host.service` (`--user` unit, `/usr/local/bin/strih-mv-host.py`, python3-xlib): re-hosts every `Projector` toplevel in a plain managed host window, keeps the child sized, forwards focus + WM close to OBS; verified adopt/resize/close/reopen. The durable fix is the vendored child-display projector (issue 1352). A re-flash without this unit = a 93 %-lag strih.
11. **Multiview divisor on a 30 fps canvas is 1 BY DESIGN** (`obs-display.c` #776: `effective_divisor = round(33.3 ms / frame_interval)` clamped to the frontend's 2) — do not chase `divisor=1` in the audit on strih; on a 60 fps canvas (imag) it is 2.
12. **Janus binds the plain-RTP port to the IP it auto-detected at START.** After the .203→.202 renumbering every `janus_audiobridge_plainrtp_allocate_port` bind was `EADDRNOTAVAIL` on 10.77.9.203 → `No ports available in range 10000-60000` → the hub's `janus: session establish failed — backing off` forever → phones heard an empty room (their own WebRTC leg was fine). `systemctl restart janus` re-detects. Any IP change → restart janus (provisioning: pin `local_ip` in `janus.plugin.audiobridge.jcfg general:{}` or restart janus after netplan). Diagnose with `strace -f -p <janus> -e trace=bind` during the hub's 60 s retry — the hub log alone never names the IP.
13. **`avahi-utils` is NOT installed by default on 26.04** — `avahi-browse … 2>/dev/null` reads as "0 entries" (command not found swallowed) and looks exactly like blind mDNS. `apt-get install avahi-utils` (provisioning + a verify-strih item); libndi 6.3.2 links `libavahi-client` so discovery works whenever avahi-daemon runs. Cambox hub levels `cam1..7=-120 dBFS` are BY DESIGN (the cambox intercom starts MUTED, power-button unmute) — not a routing fault.
14. **DistroAV prepends the hostname to the main/preview output name.** user.ini must carry `MainOutputName=2ME PGM` / `PreviewOutputName=2ME PVW` (+ `…Enabled=true`) to announce `STRIH-LX (2ME PGM)` / `STRIH-LX (2ME PVW)`; `MainOutputName=STRIH-LX (2ME PGM)` announces the doubled `STRIH-LX (STRIH-LX (2ME PGM))`. Edit user.ini with OBS STOPPED (it rewrites the file on exit). The `NDI 2ME PVW` / `NDI 2ME PGM (mv)` inputs receive those two names (the seed's `STRIH-SNV` parallel-phase names are dead).
15. **Scene-collection trims applied 22.9.** (owner-reversible via right-click → Show in Multiview / filter toggle): `MULTIVIEW` + `Test` hidden from the MV grid; the `MULTIVIEW` scene's `ndi_filter` disabled (its only consumer was OBS's own loop); `Browser` (presenter stream) 60→30 fps. `Interkom` MUST stay in the grid: the vendored `ndi_filter` renders its scene only when a display draws it — hiding Interkom kills `STRIH-LX (interkom)` (the hub's phone video).

## GOTCHA — baking the 22.9 findings into provisioning (issue 1352): the `#@MARKER@` install-time substitution + the user.ini `_cmds` upsert printer

Two reusable patterns from wiring findings 9–14 above into `setup-strih.sh` / `strih-provision.sh` / `verify-strih.sh` (all provisioning items land as pure printers + an idempotent step + a verify item, `vendor/` untouched):

- **The GPU-env exports (finding 9) are ONE source of truth in a printer + SUBSTITUTED into the wrapper at install** — `strih-obs-start.sh` is `install`ed near-verbatim, so to route the 4 XWayland-PRIME exports through a single `strih_lx_obs_gpu_env` printer (never hard-coded twice), the repo wrapper carries a MARKER line `#@STRIH_LX_OBS_GPU_ENV@` and setup-strih step 8 does `TEXT="$(cat wrapper)"; TEXT="${TEXT//#@STRIH_LX_OBS_GPU_ENV@/$(strih_lx_obs_gpu_env)}"; printf '%s\n' "$TEXT" > /usr/local/bin/...; chmod 0755` (the `@JANUS_ROOM_SECRET@` idiom, extended to a whole exports line). The marker is `#`-PREFIXED so the repo wrapper stays a valid bash comment (sourcing/running it is harmless), and the substitution drops the `#`. **GOTCHA: the wrapper's own explanatory comment MUST mention the token as ` @STRIH_LX_OBS_GPU_ENV@` (space-prefixed prose), NEVER `#@…`** — the verify item greps `grep -qF '#@STRIH_LX_OBS_GPU_ENV@'` (the un-substituted marker LINE) so a space-prefixed prose mention survives substitution without false-failing verify. Confirmed live: substituting leaves the prose token (1 occurrence of the bare token) but ZERO `#@`-prefixed occurrences, so `grep -qF '#@…'` = 0 = "substituted" on a healthy deployed wrapper, and the box's hand-edited wrapper (4 exports, no marker) also passes. This changed the launcher-install anchor test (`setup_strih_installs_both_launchers_before_enabling_the_unit`) from the `install -m 0755 "${HERE}/strih-obs-start.sh"` literal to `> /usr/local/bin/strih-obs-start.sh` + `chmod 0755`.

- **user.ini section seeding (finding 14) = a `strih_lx_ndi_output_ini_cmds USER_INI` printer emitting a python RawConfigParser upsert, `eval`-consumed** — mirror the step-7 SaveProjectors upsert (`optionxform=str`, `strict=False`, `exit(3)` on unparseable → never clobbers). Emit it as an UNQUOTED outer `cat <<EOF` that interpolates ONLY `${ini}`, with the python body inside an inner `<<'PYNDI'` (quoted); the python body MUST stay free of `$`/backtick or the outer heredoc would expand them. Consume it as `if eval "$(strih_lx_ndi_output_ini_cmds "$USER_INI")"; then echo …; else warn …; fi` — the `if`/`eval` keeps it set-e-safe AND gates the "seeded" echo on success (a bare `eval … || warn` then an UNCONDITIONAL "seeded" echo overstates on failure — a review-caught nit; the same applies to any `apt-get … || warn` + unconditional "installed" echo, gate it with `if apt-get …; then echo; else warn; fi`).

- **Janus `local_ip` (finding 12)** = a 3rd optional `LOCAL_IP` arg on `strih_janus_audiobridge_jcfg_text`, mirroring the `ws_ip` blank-omit idiom (`local ip_line=""; [ -n "$local_ip" ] && ip_line="    local_ip = \"$local_ip\""`), fed `"$STATIC_IP"` (= `strih_lx_ip`, the SAME source the netplan/ws step uses — never a 2nd hard-coded literal). The blank-arg case renders `general: {\n\n}` (a harmless blank line — `strih_janus_room_jcfg_ok` grades only the `room-<N>` block).

- **The mv-host helper (finding 10) is copied BYTE-VERBATIM from the box** (`sha256sum` the local file against `/usr/local/bin/strih-mv-host.py` + `~/.config/systemd/user/strih-mv-host.service` to confirm) and installed as a lettered sub-step `step "8b"` so `TOTAL_STEPS=17` (test-pinned) never changes; enable-only via `systemctl --user enable` (never `start`); verify reads the `default.target.wants/strih-mv-host.service` symlink (no `--user` session bus needed) + `python3 -c "import Xlib"`. The unit's hard-coded `/run/user/1000` + `DISPLAY=:0` stay verbatim (byte-identical mandate; UID 1000 = newlevel).

## GOTCHA — verify-strih.sh acceptance run on the live box (issue 1352, 22.9.2026): three ways a gate lies under `set -euo pipefail`

1. **A `VAR="$(verdict_fn …)"` assignment WITHOUT `|| true` aborts the whole script on a FAIL verdict** — `strih_lx_program_audio_verdict` prints its text AND returns 1 on FAIL; under `set -e` the failing command substitution terminated verify-strih.sh SILENTLY (exit 1, no line) right after the remoteos item, so every item after it (mv-host, gpu-env, avahi, ndi-outputs…) never ran. Every verdict assignment carries `|| true` and lets the `case` grade the TEXT (the sibling pattern at the projector/chrome-sandbox/companion items).
2. **`producer_a || producer_b | predicate` never reaches producer_b when producer_a exits 0 with no useful output** — `ffmpeg -hide_banner -encoders || cat "$LOG"`: the distro ffmpeg has no nvenc encoder but exits 0, so the OBS log (which carried `[obs-nvenc] NVENC version:`) was never read → FAIL with NVENC live. Concatenate evidence sources with `;`.
3. **A `grep -q` predicate fed a LARGE log SIGPIPEs the producer under `pipefail` (141) → the gate reads "no match"** — the same class as the `strih_nvenc_log_verdict` here-string gotcha above, hit AGAIN in `strih_lx_nvenc_available_ok`. A lib predicate consumed by `{ …; cat "$LOG"; } | predicate` must read to EOF (`grep -i … >/dev/null`, never `grep -q`), and its test must feed a >1 MB early-match input under `set -o pipefail` (`nvenc_predicate_is_drain_safe_under_pipefail_with_a_large_early_match`). Local RED/GREEN proof needs no cargo: `bash -c 'set -uo pipefail; . scripts/lib/strih-provision.sh; { printf "…nvenc…\n"; head -c 3000000 /dev/zero | tr "\0" a; } | strih_lx_nvenc_available_ok; echo $?'` → 141 before, 0 after.

Also: the janus items read the root:root 0640 jcfg via a `sudo -n cat` fallback — a non-interactive sudo ticket does NOT propagate into a tty-less child process tree, so run as the operator without NOPASSWD they honestly print `(or jcfg unreadable)`; grade the pin with a valid ticket or as root. And the on-box `/usr/local/bin/strih_scenes.py` had gone STALE behind dev (no `--audio-input-kind`) — `verify-strih.sh` cannot see that by itself; compare `sha256sum` against the repo before trusting the seeder-driven items.
