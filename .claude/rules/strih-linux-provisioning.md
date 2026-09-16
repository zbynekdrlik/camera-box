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
- **`strih-obs-start.sh` / `strih-obs-stop.sh`** launcher pair (sibling of `imag-obs-start.sh`) that
  `strih-obs.service` ExecStart references.
- **The 2ME self-feedback INPUT names.** issue 1317's spec lists the multiview-feedback inputs as
  `STRIH-SNV (2ME PGM/PVW)` (what the Windows box publishes). For a fully independent strih-lx the
  box's OWN multiview feedback should read `STRIH-LX (2ME PGM/PVW)` (self-loop); while parallel and
  tuning, either is fine and trivially re-seedable. Re-confirm the intended feedback source when the
  box goes live.

## What is NOT in scope

Recording retention, the NIC self-heal watcher, Companion Satellite re-pairing, WoL, and the 4K
multiview projector budget are all listed in the issue-1317 inventory as NEEDS-WORK but are not part
of the initial provisioning scaffolding — they follow once the hardware is in hand.
