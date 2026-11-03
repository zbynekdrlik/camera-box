---
paths:
  - "scripts/shading-release.pin"
  - "scripts/lib/shading-release.sh"
  - "scripts/bkshading-deploy-relay.sh"
  - "scripts/bkshading-deploy-service.sh"
  - "scripts/lib/bkshading-relay-version.sh"
  - "scripts/bkshading.example.toml"
  - "tests/fixtures/shading_pinned/**"
  - "tests/shading_pin_1487.rs"
  - "tests/python/test_shading_*_1487.py"
---

# bkshading is a pinned release (issue 1487)

`zbynekdrlik/shading` owns the bkshading code, its tests and its releases; `docs/CONTRACT.md` there is
what camera-box reads. camera-box pins a release, deploys it and keeps the deploy and provisioning
layer: every `scripts/bkshading-*`, the `systemd/bkshading-*` units, the rig gates, the strih seed
generator. A relay or service change goes to the shading project.

- **One pin:** `scripts/shading-release.pin`: the tag, the commit, the sha256 of each of the four assets.
- **`scripts/lib/shading-release.sh`:** `shading_release_fetch` by the exact tag into
  `<tree>/shading-release/<tag>/` (git-ignored); the sha256 must equal the pin AND the release's
  `.sha256`; `shading_release_relay` / `shading_release_service` also run the binary for its version
  (dev1 cannot run arm64: the two sha256 proofs stand there). An asset already there needs no network.
  No fallback, ever.
- **Only the pinned bytes deploy.** `--binary` (deploy-relay, deploy-service) and setup-device's
  `--relay-binary` go through `shading_release_file_ok`. No CI artifact, no `--run`. To test another
  relay on the rig, pin it.
- **Who fetches:** deploy-fleet before any box is touched (a fetch failure deploys nothing; `--no-relay`
  needs no release); deploy-relay when called alone; the strih deploy on dev1 into its staged tree (the
  box needs no network); setup-strith step 16c from a checkout; setup-device on the cambox with `curl`
  (else stage the asset and pass `--relay-binary`).
- **The relay version is the pin's, never camera-box's.** deploy-fleet leaves a box already on the pin
  alone (no relay stop, no remount) and reads the version back after a swap. rig-dev-handover item 15
  grades against the tree's pin or origin/main's (a pin bump on dev reaches the boxes with the merge).
  verify-device (ao) FAILs a relay off the pin; verify-strih item 31 is a NOTE, a running service
  keeps its old inode until restarted. By equality: a `1.x` relay left on a box is simply not the pin.
- **The first roll to v0.1.0** replaces relays that print a camera-box version; RELAY-STALE until then.
- **Seed:** `tests/python/test_shading_seed_check_config_1487.py` runs `bkshading --check-config` from the
  pinned release on `strih_bkshading_config_text` and compares the camera COUNT: an unknown table is
  ignored, so a misspelled `[[cameras]]` exits 0 with `ok: 0 cameras`.
- **`scripts/bkshading.example.toml`** is camera-box's seed for the Windows install.

## Bumping the pin

1. `scripts/shading-release.pin`: the tag, the commit, the four sha256 from the release's `.sha256` files.
2. Copy the tag's `docs/CONTRACT.md` and `bkshading/service/src/preview/ndi_paths.rs` into
   `tests/fixtures/shading_pinned/`.
3. `cargo test --test shading_pin_1487` reads the release and the tag over HTTPS and fails until the pin
   and the copies hold; `tests/python/test_bkshading_ndi_provision_1157.py` compares
   `scripts/lib/bkshading-ndi-runtime.sh` with the copy. Read the contract's diff: a gphoto2 key,
   a port or an env name is a camera-box change too.
