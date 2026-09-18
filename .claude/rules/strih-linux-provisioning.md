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
- **strih-lx joins the cluster clock as a dantesync CLIENT** (`--ntp-server strih.lan`), never a
  server/master — the Windows PC keeps the single NTP-master role. `setup-strih.sh` self-checks the
  invocation with `strih_lx_dantesync_is_client_not_master` (fail-closed: an empty/ambiguous mode
  refuses) so a mis-provision can never spawn a second master.

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
- **CAVEAT — `latency: 3` in the certified dict.** The seeder sets the DistroAV `latency` key to `3`
  exactly as the issue-1317 design specified ("use latency 3 not 0 per the manifest"). Note the stock
  DistroAV `latency` field is a receive-buffer MODE enum (`imag_scenes.py` sets it to `1` = Low), and
  the 3 ms genlock FLOOR is otherwise carried by the SEPARATE per-source `genlock_latency_ms_src` pin
  (owned by the per-run aligner, `latency-pins-verify.md`), NOT by this `latency` field. Confirm on the
  live box that `latency: 3` is accepted/meaningful when the supervisor first runs `--bootstrap`; the
  `--verify-parity` report deliberately does NOT gate on `latency` (a live receiver may normalise it).
- **obs_phase2 is imported LAZILY** (`_obs_phase2_module`, the #1156 class): an older box may lack it,
  so the read-back verify degrades to a direct set rather than crashing the boot seed. The top-level
  `from websocket import create_connection` IS the dep the launch preflight validates.
- **Install + launch wiring.** `setup-strih.sh` step 6 `install -m 0755 "${HERE}/strih_scenes.py"
  /usr/local/bin/strih_scenes.py` (fail-loud if absent next to the script), alongside the manifest +
  `obs_phase2.py`. `strih-obs-start.sh` preflights `import strih_scenes` BEFORE launch and runs
  `--bootstrap` AFTER the `:4455` wait. Tests: `tests/python/test_strih_scenes_1317.py` (pure helpers +
  the two static-anchor asserts on the launcher/setup wiring — a python assert over the script text,
  so it runs in Tier-0 with no cargo).

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

## Audio — the one BLOCKER, fail-loud until wired

There is NO Dante on the strih PC (owner 15.9.): program audio is `MiniFuse 4` USB → VB-Matrix
(VASIO-8) ASIO on Windows. On Linux the class-compliant MiniFuse 4 is a native PipeWire node and
**PipeWire replaces VB-Matrix**. The graph that routes the mastered program mix into the MiniFuse 4
capture is not yet wired — `setup-strih.sh` step 12 **FAILS LOUD** (`strih_lx_audio_route_wired`)
until an operator wires it and re-runs with `STRIH_LX_AUDIO_WIRED=1`. Arena stays on the Windows PC
(Spout has no Linux); its cg feed reaches the notebook over NDI (`RESOLUME-SNV (cg-obs)`).

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

Recording retention, the NIC self-heal watcher, Companion Satellite re-pairing, WoL, and the 4K
multiview projector budget are all listed in the issue-1317 inventory as NEEDS-WORK but are not part
of the initial provisioning scaffolding — they follow once the hardware is in hand.

### Anchor-test literal vs an expanded path (18.9.2026, CI 35390761239)

`tests/strih_provision_pure_functions.rs` pins the step-4 fail-closed message by the LITERAL
`RUNTIME_PACKAGES.txt missing`; a message that expands `${SRC_RUNTIME_PKGS} missing` reads identically
at runtime but never matches the source-text anchor. Keep the file name literal in the message and put
the expanded path in parentheses after it — the same rule for every future fail-closed marker a
static-anchor test pins.
