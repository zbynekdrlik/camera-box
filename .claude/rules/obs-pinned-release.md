---
paths:
  - "scripts/obs-release.pin"
  - "scripts/lib/obs-release.sh"
  - "scripts/lib/strih-snv-deploy.sh"
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/lib/e2e-linux-bytes.sh"
  - "scripts/lib/version-integrity-vendor-pin.sh"
  - "tests/obs_pin_1487.rs"
  - "tests/python/test_obs_release_1487.py"
  - "tests/verify_strih_drm_scanout_marker_1504.rs"
---

# The Linux OBS boxes run a pinned release of the forked OBS (issue 1487)

`zbynekdrlik/obs` owns the forked OBS (its release workflow + `docs/HANDOVER.md`). camera-box pins a
release, installs it on the Linux OBS boxes (every obs-fleet `linux-genlock` row with a fact file:
strih-snv, stream-snv, strih-pp, stream-pp) and grades them against the pin. An OBS change goes to the
obs project; camera-box moves the pin.

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
- **Still on the vendor build:** the Windows boxes (resolume-snv: `windows-genlock*.yml`), imag
  (`linux-genlock.yml`), the Windows FAST/FULL manifest auto-source, and every `vendor/**` test. The
  vendored OBS is removed only after a green E2E on the pin, in a separate change.

## Bumping the pin

1. The tag, the commit, the two sha256 (`.sha256` of the tarball, `sha256sum BUNDLE_MANIFEST.json`),
   the manifest's `release.versions` obs-studio / distroav, and `OBS_RELEASE_DSTACK_VERSION` = the dstack
   `plugins/dante-input/ffi/Cargo.lock` locks at the commit (issue 1485: dtray's `--print` threshold,
   `scripts/lib/dtray.sh`).
2. `cargo test --test obs_pin_1487` reads the release over HTTPS and fails until every value holds.
3. Deploy: `scripts/deploy-genlock-fleet.sh --boxes strih-snv` (then stream-snv, the PP boxes when home).

## Vendored-only tests live in the obs repo (issue 1504)

Since obs v0.4.25 the obs repo's CI job `vendored-tests` (`tests/vendored`, its release gated on it)
runs every test that reads only the vendored OBS/DistroAV/plugin sources, plus their C harnesses
(`tests/c`). camera-box dropped its copies; a reference written `obs:tests/vendored/tests/<name>.rs`
or `obs:tests/c/<file>` in these rules points there. A vendored-source change belongs in the obs
repo with its test. camera-box keeps the tests whose subject it owns: anything that also reads
`scripts/**`, `src/**` or camera-box's own build workflows (the pwsh lock-step mirrors), or calls `camera_box::` (the `genlock_audio_pairing_lift` / `genlock_n1_lift`
parity users, the av-sync dock twins), and the consumer side of an OBS log contract, such as
`tests/verify_strih_drm_scanout_marker_1504.rs` (verify-strih greps `drm-output: program scanout LIVE`).
