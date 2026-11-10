---
paths:
  - "scripts/shading-release.pin"
  - "scripts/shading-release-rest.pin"
  - "scripts/shading-release-boxes.txt"
  - "scripts/lib/bkshading-camera-link.sh"
  - "tests/python/test_shading_rest_1452.py"
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

- **The pins:** `scripts/shading-release.pin` = the fleet DEFAULT (the tag, the commit, the sha256 of each
  of the four assets); since issue 1452 the relay pin is chosen PER BOX (below), and the service keeps the
  default pin.
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
  verify-device (ao) FAILs a relay off the pin; verify-strih item 31 FAILs a service off the pin; step 16c
  restarts a running service when its binary changed. By equality: a `1.x` relay left on a box is simply not the pin.
- **The first roll to v0.1.0** replaces relays that print a camera-box version; RELAY-STALE until then.
- **Seed:** `tests/python/test_shading_seed_check_config_1487.py` runs `bkshading --check-config` from the
  pinned release on `strih_bkshading_config_text` and compares the camera COUNT: an unknown table is
  ignored, so a misspelled `[[cameras]]` exits 0 with `ok: 0 cameras`.
- **`scripts/bkshading.example.toml`** is camera-box's seed for the Windows install.

## The relay pin is chosen per box (issue 1452, shading#6, owner 9.11.2026)

shading v0.2.x drives the camera over the Blackmagic REST API on its USB-C link (CDC-NCM) with NO
gphoto2 fallback, and REST answers only on a Pocket 6K at fw >= 8.6 / a Pocket 4K at fw >= 9.8 with
Blackmagic Camera Setup -> Network access -> "Web media manager" ON (else 404 = shading offline).
- **`scripts/shading-release-boxes.txt`** lists `<box> <pin file>`; a box not listed takes the default.
  Today `cam3` + `cam5` -> `scripts/shading-release-rest.pin` (v0.2.2); PP stays on v0.1.2 until the
  owner turns Web media manager on there. Add a box only after its camera answers
  `GET /control/api/v1/lens/iris` on the link. The box = the relay host's own name (lower case, no
  domain): a cambox camN, a handheld SBC by its hostname.
- **`shading_pin_file_for_box ROOT BOX`** resolves it; every relay consumer sets the lib's
  `SHADING_RELEASE_PIN_FILE` from it, so the fetch and the two sha256 proofs are the same code for
  every pin. Seams: `SHADING_RELEASE_PIN_FILE` alone = that ONE pin for every box (the deploy tests);
  `SHADING_RELEASE_BOXES_FILE` = another map (its pin files relative to the map's dir).
- **Consumers:** deploy-fleet fetches every pin its set needs before any box and passes `--box camN`;
  `bkshading-deploy-relay.sh --box <name>` takes that box's pin and, before anything changes, reads
  the box's own `uname -n`: a name whose pin differs is REFUSED ("pass --box cam3"); an unreadable name
  is REFUSED without `--box` and keeps the given `--box` pin with a NOTE; setup-device takes `$DEVICE_NAME`'s pin; verify-device (ao) and
  rig-dev-handover item 15 grade each box against its own pin (the handover also against origin/main's
  map + pin, `shading_pin_version_for_box_at_ref`).
- **The service takes its strih box's pin** (issue 1452): setup-strih step 16c sets
  `SHADING_RELEASE_PIN_FILE` from `shading_pin_file_for_box <tree> $STRIH_FACT_BOX` and verify item 31's
  `strih_bkshading_version_grade` reads `shading_pin_version_for_box` the same way. A v0.2.x panel sends
  `apertureStep` for +/-, which a v0.1.x relay ignores, so a venue moves when its cameras are on v0.2.x:
  strih-snv is on the REST pin since 10.11.2026 (owner, cam3 aperture steps; cam2's gphoto2 relay keeps
  the one-way gap), strih-pp stays on the default pin.
- **The camera link file** every relay box carries: `scripts/lib/bkshading-camera-link.sh`
  (`05-bkshading-camera.network`, the contract's text byte for byte, pinned against
  `tests/fixtures/shading_pinned_rest/CONTRACT.md`): setup-device `[bkshading-relay]`, the SBC
  `--install` + `--check` row (2c), verify-device (bb). A pin declaring `SHADING_RELAY_TRANSPORT=rest`
  (`shading_pin_transport`) makes `bkshading-deploy-relay.sh` run the live-box program after the swap
  on a cambox (amd64), so every REST relay deploy carries its link file. Any other live cambox:
  `bash scripts/lib/bkshading-camera-link.sh --apply-program | ssh root@<box> bash -s` (one verified ro
  window, `networkctl reload` only after the close; never on an SBC, whose root may be rw by design).
  setup-device's write is enable-only: the link comes up at the next boot or with that program. systemd-networkd is the cambox netplan renderer, so the
  file takes effect; its match (`Driver=cdc_ncm` + `ID_VENDOR_ID=1edb`) never claims the `enp*` LAN.

## Bumping the pin

1. `scripts/shading-release.pin`: the tag, the commit, the four sha256 from the release's `.sha256` files.
2. Copy the tag's `docs/CONTRACT.md` and `bkshading/service/src/preview/ndi_paths.rs` into
   `tests/fixtures/shading_pinned/` (for the REST pin: `docs/CONTRACT.md` into
   `tests/fixtures/shading_pinned_rest/`; `tests/shading_pin_1487.rs` `PINS` lists every pin file).
3. `cargo test --test shading_pin_1487` reads the release and the tag over HTTPS and fails until the pin
   and the copies hold; `tests/python/test_bkshading_ndi_provision_1157.py` compares
   `scripts/lib/bkshading-ndi-runtime.sh` with the copy. Read the contract's diff: a gphoto2 key,
   a port or an env name is a camera-box change too.
