---
paths:
  - "scripts/obs-release.pin"
  - "scripts/lib/obs-release.sh"
  - "scripts/lib/strih-snv-deploy.sh"
  - "scripts/lib/genlock-windows-pin.sh"
  - "tests/python/test_obs_release_windows_1505.py"
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/lib/e2e-linux-bytes.sh"
  - "scripts/lib/version-integrity-vendor-pin.sh"
  - "tests/obs_pin_1487.rs"
  - "tests/python/test_obs_release_1487.py"
  - "tests/verify_strih_drm_scanout_marker_1504.rs"
---

# The OBS boxes run a pinned release of the forked OBS (issues 1487 + 1505)

`zbynekdrlik/obs` owns the forked OBS (its release workflow + `docs/HANDOVER.md`). camera-box pins a
release, installs it on the Linux OBS boxes (every obs-fleet `linux-genlock` row with a fact file:
strih-snv, stream-snv, strih-pp, stream-pp) and on the Windows OBS boxes (resolume-snv, and
resolume-pp + grading-pp when home), and grades them against the pin. An OBS change goes to the obs
project; camera-box moves the pin. Only imag still runs camera-box's own `linux-genlock.yml` build of
`vendor/`.

- **One pin:** `scripts/obs-release.pin`: the tag, the commit, the sha256 of the strih tarball
  (`obs-strih-linux-x86_64-<tag>.tar.zst`) and of its `BUNDLE_MANIFEST.json` (the release publishes no
  `.sha256` for the manifest), plus the OBS / DistroAV versions drift-guard grades a Linux box against.
- **`scripts/lib/obs-release.sh`:** `obs_release_bundle ROOT DEST` fetches by the exact tag into
  `<tree>/obs-release/<tag>/` (git-ignored, a cached tarball needs no network), proves the tarball
  against the pin AND its `.sha256`, the manifest against the pin (the tarball's own copy must be the
  same bytes), unpacks with `zstd -dc | tar -xf -` (unix modes kept), and checks every manifest file's
  sha256, the required files (`bin/obs`, `libobs.so.30`, `RUNTIME_PACKAGES.txt`, `STRIH_BUILD_FLAGS.txt`,
  `dante-input.so`, the two markers) and that `GENLOCK_BUILD_SHA.txt` + `OBS_RELEASE.txt` + the manifest's
  `release` block name the pinned tag + commit. Any failure removes DEST. No fallback, ever.
- **The deploy** (`deploy-genlock-fleet.sh` -> `strih_lx_prepare`): the setup-strih boxes take the pin's
  commit as their canonical SHA (never the anchor run's); `--run-id` is optional for a Linux-only run
  (the fleet log names `obs-<tag>`). `genlock_stats_abi_stage` writes the manifest's stats pair. The
  read-back adds the `OBS_RELEASE.txt` tag to the marker + libobs bytes + render tick + MainPID settle +
  `:8899` checks.
- **setup-strih from a checkout:** `STRIH_LX_BUNDLE_SRC=pin` fetches + installs the pinned release; unset
  still installs no OBS (the "few steps on a live box" path).
- **The gates:** `e2e-linux-bytes.sh` grades a Linux box's bytes against the PINNED manifest (a box on
  another commit gets no manifest: UNKNOWN, refused); the vendor-pin alarm reads a box on the pin commit
  as OK without touching `vendor/`; drift-guard's `--compare strih_linux=1` takes the OBS / DistroAV
  versions from the pin. The cross-box parity still compares the boxes' markers.
- **The Windows boxes take the release's Windows zip (issue 1505).** The pin also carries
  `OBS_RELEASE_WINDOWS_SHA256` (`obs-windows-x64-<tag>.zip`) and `OBS_RELEASE_WINDOWS_MANIFEST_SHA256`
  (`WINDOWS_MANIFEST.json`). Their `.sha256` files read `<sha> *<name>` (git-bash); the first token is the
  hash. `obs_release_windows_bundle ROOT DEST` is the `obs_release_bundle` twin: fetch by tag, the zip
  proved against the pin AND its `.sha256`, the manifest against the pin, `unzip`, the zip's own
  `BUNDLE_MANIFEST.json` must be the pinned manifest's bytes, every manifest file, the Windows required
  files (`obs64.exe`, `obs.dll`, distroav / obs-vban / vlc-video, the two markers), the markers naming tag
  + commit. Any failure removes DEST; there is no other source.
  - The zip's layout is the old windows-genlock `stage/` one (`bin/64bit`, `data`, `obs-plugins/64bit`,
    markers, `BUNDLE_MANIFEST.json`), so the ONE Windows program `build_windows_deploy_program` reads
    the unzipped tree unchanged (obs.dll verify, distroav to ProgramData + verify, obs-vban/vlc-video).
  - The routing is `scripts/lib/genlock-windows-pin.sh` (the deploy script is at its line budget):
    `fleet_win_split` sends every Windows box to the zip (a pin without one refuses it); its markers name the pin
    commit, `--run-id` / `--sha` are needed only for imag, `--fast` is refused (exit 2: the release ships
    the full bundle only), the stats pair comes from the pinned manifest only when its release block
    names the pin (plan mode reads it from `--stage`; another release = the marker is removed), and the
    fleet log records the pinned boxes as `obs-<tag>`.
  - STEP 0 of a pinned box names the cached, sha-proved zip `<tree>/obs-release/<tag>/obs-windows-x64-<tag>.zip`
    (`fleet_win_bundle_source`): scp it to `C:\stage-genlock-<commit>`, check its sha256 on the box, `Expand-Archive`
    it there (the zip's root is the stage layout). The unzipped tree the plan verified is a mktemp dir, gone on exit.
    Then the deploy `.ps1` and the `launch-obs-genlock.sh --box resolume-snv --force` `.ps1` run by `-File`
    (resolume-snv on v0.4.28, 13.11.2026: every DLL byte-verified, one obs64 in session 1, `:8899` LOCKED).
  - A pin WITHOUT the two fields REFUSES every Windows box by name (`fleet_win_split` rc 3: "the pin
    carries no Windows zip"), in plan and execute mode, before any box is touched; it never falls back
    to another build. An imag-only run is unaffected.
- **camera-box's own Windows build is RETIRED (issue 1505, 14.11.2026)**, after resolume-snv ran the
  pinned zip and was read back (13.11.2026). Gone: `windows-genlock.yml`, `windows-genlock-fast.yml`,
  `windows-manifest-gate.yml`, the planner's non-pin Windows branch (`fleet_windows_workflow` /
  `fleet_windows_artifact`), the E2E's Windows FAST/FULL manifest auto-source
  (`manifest_autosource_fetch_win_full`, `win_manifest_pair_*`; an operator can still pass
  `VERSION_GATE_MANIFEST` / `VERSION_GATE_ALT_MANIFEST`) and every pwsh lock-step mirror those
  workflows carried. `--fast` is refused for a Windows box, and issue 1522 removed what that made
  unreachable: the FAST deploy program, its stats-ABI gate (0f) (`genlock_fast_abi_*`), the refusal
  branch of `genlock_stats_abi_resolve` and their tests; `build_windows_deploy_program` refuses a
  fast MODE with rc 2.
  `linux-genlock.yml`'s header comment still names the two moved tests: an edit to that workflow
  triggers its full Linux build, so the comment waits for the next real change there.
- **Deploy-test fixtures carry the Windows zip too:** `tests/python/obs_release_fake_1487.py`
  (`fake_obs_pin_env`, `fake_obs_release_env`) and the strih-snv execute harness
  (`fake_windows_zip` in `tests/deploy_genlock_fleet_strih_lx_exec_1317.rs`) build a pin that names
  one, so a test listing a Windows box is not refused. A test of the refusal strips the
  `OBS_RELEASE_WINDOWS_*` lines (`STUB_NO_WIN_ZIP=1` in the Rust harness).
- **Still on the vendor build:** imag (`linux-genlock.yml`) and the `vendor/**` tests below. The
  vendored OBS is removed only after a green E2E on the pin, in a separate change.

## Bumping the pin

1. The tag, the commit, the two strih sha256 (`.sha256` of the tarball, `sha256sum BUNDLE_MANIFEST.json`),
   the two Windows sha256 (`.sha256` of the zip and of `WINDOWS_MANIFEST.json`),
   the manifest's `release.versions` obs-studio / distroav, and `OBS_RELEASE_DSTACK_VERSION` = the dstack
   `plugins/dante-input/ffi/Cargo.lock` locks at the commit (issue 1485: dtray's `--print` threshold,
   `scripts/lib/dtray.sh`).
2. `cargo test --test obs_pin_1487` reads the release over HTTPS and fails until every value holds.
3. Deploy: `scripts/deploy-genlock-fleet.sh --boxes strih-snv` (then stream-snv, the PP boxes when home).

## Vendored-only tests live in the obs repo (issues 1504 + 1505)

Since obs v0.4.25 the obs repo's CI job `vendored-tests` (`tests/vendored`, its release gated on it)
runs every test that reads only the vendored OBS/DistroAV/plugin sources, plus their C harnesses
(`tests/c`). camera-box dropped its copies; a reference written `obs:tests/vendored/tests/<name>.rs`
or `obs:tests/c/<file>` in these rules points there. A vendored-source change belongs in the obs
repo with its test. camera-box keeps the tests whose subject it owns: anything that also reads
`scripts/**`, `src/**` or `linux-genlock.yml` (imag), or calls `camera_box::` (the `genlock_audio_pairing_lift` / `genlock_n1_lift`
parity users, the av-sync dock twins), and the consumer side of an OBS log contract, such as
`tests/verify_strih_drm_scanout_marker_1504.rs` (verify-strih greps `drm-output: program scanout LIVE`).

- **Issue 1505 deleted the five of the seven tests issue 1504 kept** that read no camera-box workflow
  once the Windows ones were gone, each byte-identical to its obs copy except the manifest-dir path:
  `genlock_audio_buffering_freeze_wiring_1425`, `genlock_fifo_relabel_wiring_1372`,
  `obs_crash_handler_no_modal_1378`, `obs_titlebar_newlevel`, `obs_updater_disabled`.
  `drm_output_program_1152` and `jxs_strih_build_1387` stay (they guard `linux-genlock.yml`).
- **Not yet in obs, so they stay here** (checked against obs main's `tests/vendored/tests/`,
  13.11.2026): the `distroav_*` / `vban_*` source tests, the vendor-only `av_sync_dock_*`,
  `obs:tests/vendored/tests/genlock_lock_indicator_guards.rs` and `obs:tests/vendored/tests/distroav_stale_duplicate_retarget_1367.rs` (obs has only its
  replay half). imag's `linux-genlock.yml` still compiles that `vendor/` tree, and nobody else runs
  them. Delete each one here only after obs carries it.
