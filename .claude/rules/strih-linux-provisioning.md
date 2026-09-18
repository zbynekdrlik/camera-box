---
paths:
  - "scripts/setup-strih.sh"
  - "scripts/verify-strih.sh"
  - "scripts/lib/strih-provision.sh"
  - "systemd/strih-obs.service"
  - "systemd/strih-bundle-state-server.service"
  - "tests/strih_provision_pure_functions.rs"
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
  lesson); NO DRM lease; NO scene-seeder preflight (the strih seeder is a separate follow-up, so the
  launcher must not import one). `--profile` / `--collection` are passed only when those OBS dirs
  exist (a fresh box has none → default).
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
- **A bespoke `strih_scenes.py`** WS seeder (sibling of `imag_scenes.py`) that seeds the 10 inputs
  (genlock_fifo + floor 3) + the `STRIH-LX (...)` outputs from `/opt/camera-box/strih-lx-seed.json`
  and enforces Studio Mode. `setup-strih.sh` installs the seed manifest + `obs_phase2.py` primitives;
  the full seeder is deferred.
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
