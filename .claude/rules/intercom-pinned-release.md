---
paths:
  - "scripts/intercom-release.pin"
  - "scripts/lib/strih-intercom-release.sh"
  - "scripts/lib/strih-intercom-hub-unit.sh"
  - "scripts/lib/strih-interkom-public.sh"
  - "tests/fixtures/intercom_pinned/**"
  - "tests/intercom_pin_1487.rs"
---

# The intercom is a pinned release (issue 1487, airuleset#1280)

The intercom lives in `zbynekdrlik/intercom`: the hub, its unit, the routing TOMLs, the PWA, the
converter, their tests and their rules (`strih-intercom*.md`). camera-box pins a release, installs it on
the boxes and keeps the glue: `strih-intercom-hub-unit.sh`, `strih-interkom-public.sh`,
`janus_turn_rest_cfg.py`, the interkom nginx conf. A hub or routing change goes to the intercom project.

- **One pin:** `scripts/intercom-release.pin` (tag + tarball sha256). Root `Cargo.toml`'s `intercom-vban`
  git tag must equal it (`tests/intercom_pin_1487.rs`).
- **The bundle** `intercom-hub-linux-amd64.tar.gz` carries the binary, the unit and the four TOMLs.
  `--check-config` makes TOML and binary a pair, so step 13 installs all three from one directory,
  `<tree>/intercom-release/<tag>/` (git-ignored).
- **`strih_intercom_release_ensure`:** by the exact tag only; the sha256 must equal the pin AND the
  release's `.sha256`; the binary's `--version` must equal the tag. No fallback, ever: a stale hub is
  issue 1467's silent FOH. A tarball already in the tree needs no network.
- **Who fetches:** the strih deploy on dev1, into its staged tree (the box needs no network for it); else
  setup-strih from a checkout, before step 12 (the purge decision reads the routing file).
- **Another installed version:** replaced by rename (a running hub stays on its old inode), the old one
  kept as `intercom-hub.pre-<version>`. Provisioning never restarts a running hub: restart it after a pin
  bump, or the crash restart loads the new pair.
- **verify item 16:** the running hub (else `<bin>.version`) against the pin, a difference = a NOTE with
  the reinstall line. Plus the capability row (ROZHODNUTÉ 6628785864, 4.11.2026: the hub's dstack device
  follows Dante PTP in process on UDP 319): once the pin's `INTERCOM_RELEASE_DSTACK_VERSION` >= v0.6.23 the
  unit must grant EXACTLY `AmbientCapabilities=CAP_NET_BIND_SERVICE` + `CapabilityBoundingSet=CAP_NET_BIND_SERVICE`
  and the binary no file capability (release 1.9.12's unit); missing or any other capability FAILs, below the
  threshold a NOTE. No camera-box drop-in may set either key or `SecureBits=` (an empty one resets the list and
  takes 319 away; pinned by `tests/python/test_strih_dante_ptp_1485.py`). Plus `(intercom-dante) media clock`
  (`scripts/lib/strih-dante-ptp.sh`, `.claude/rules/strih-dante-input.md`). The dsync service stays the only
  writer of the system clock (`audio-clock-map.md`).
- **`StateDirectory=intercom-hub`** in the released unit: `/var/lib/intercom-hub/dlink.toml` keeps what Dante
  Controller sets. The drop-in's `DynamicUser=no` + `User=` make it owned by the operator.

## Bumping the pin

1. `scripts/intercom-release.pin`: the tag and the new tarball's sha256, and `INTERCOM_RELEASE_DSTACK_VERSION`
   = the dstack the tag's Cargo.lock locks (issue 1485: dtray's `--print` NOTE/FAIL threshold reads it,
   `scripts/lib/dtray.sh`); `Cargo.toml` tag; `cargo update -p intercom-vban`.
2. Copy the four TOMLs and the unit into `tests/fixtures/intercom_pinned/`.
3. `cargo test --test intercom_pin_1487`: it reads cargo's git checkout of the tag and fails until the
   copy, `src/drm_output_audio_match.rs` (apart from the hub's doc note), the TURN credential contract and
   the Dante ports all hold at the tag. A routing change moves the golden's `intercom-toml` sha.
4. Python tests that grade camera-box logic against a routing file read that pinned copy.
