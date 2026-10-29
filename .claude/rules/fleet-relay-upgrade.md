---
paths:
  - "scripts/deploy-fleet.sh"
  - "scripts/lib/bkshading-relay-version.sh"
  - "scripts/rig-dev-handover-check.sh"
  - "scripts/rig_dev_handover_decision.py"
  - "scripts/lib/camera-box-parity-align.sh"
---

# The fleet upgrade also upgrades the bkshading relay (issue 1452)

Owner, 29.10.2026: "nechapem ako tam moze byt nejaka stara verzia ved vcera pred eventom som ziadal
aby sa spravili upgrady!". `deploy-fleet.sh` updated camera-box and the cam2 painter, never the
relay: every cambox ran an old relay (dev.755-810) at the 28.10 event and cam7's shading did not set
values. Now ONE fleet upgrade brings camera-box AND the relay to the same CI run.

## What `deploy-fleet.sh` does with the relay

- **Source = the camera-box run.** `--run <id>` / the resolved run: the run's own
  `bkshading-linux-amd64` (`bkshading_deploy_artifact_name`), downloaded into `$DIST/relay`; a run
  without it refuses before any box ("pass --no-relay"). `--binary` takes `--relay-binary <path>`
  next to it (ci.yml downloads the same run's artifact to `dist-relay/`). The relay must print
  camera-box's version (one workspace version), else exit 2 before any box: not the same build.
- **The swap is `scripts/bkshading-deploy-relay.sh --host <ip> --binary <relay>`, per box, as a
  subprocess.** Never a copy of it: it owns the rig-lease preflight, the rig-busy guard, the relay
  stop + previous-state restore, the staged copy + atomic rename, the byte-verify and the verified ro
  close (`ro-window.md`).
- **The relay phase runs after the camera-box loop (and the painter), over every box that answered**
  (`REACHED`). One ssh read per box gives both versions (`bkshading_relay_versions_cmd`):
  - `relay=absent` (no binary, no unit file) = "not installed": reported in the summary, never a
    failure, nothing is installed there (setup-device / `bkshading-provision-relay.sh` own that);
  - a box with a `<cam>(root-…)` FAILED entry (its camera-box close left the root unverified) gets
    no relay deploy: nothing more is written to a writable root;
  - `--binary` without `--relay-binary` on a box WITH a relay = FAILED
    `camN-relay(no relay binary: …)`, never a silently old relay;
  - a relay failure is a FAILED entry, the next box still runs, the exit folds it (the issue-868
    pattern): `camN-relay(rig lease held by another run: not upgraded)` (rc 6),
    `camN-relay(deploy-failed rc N)`, `camN-relay(version=X)` (read back after the deploy).
- **The final summary line names every box:** `FLEET ALIGNED: … ; versions: cam2 camera-box=1.8.2
  relay=1.8.2, cam3 camera-box=1.8.2 relay=not installed` (also on NOT FULLY ALIGNED and `--venue`).
- **`--no-relay`** = camera-box only, the versions still read. Its one caller is the E2E `[0/8]`
  parity align (`cambox_align_deploy`): that E2E holds the rig lease and pauses the relay on the
  measurement boxes, and its job is the camera-box parity the gate grades. A bisect (`--run` of an
  old run) brings the relay of that run too; an old run without the artifact needs `--no-relay`.
- **Frame-probe-only mode** (`--frame-probe` alone) never touches the relay.

## The lease: the caller's own run goes through

The post-merge ci.yml deploy job takes the rig lease itself (`rig-busy-gate.sh`) before
`deploy-fleet.sh`. `bkshading-deploy-relay.sh`'s lease preflight (issue 1371, exit 6) therefore
accepts a live lease whose holder is the caller's OWN identity: `rig_lease_held_by` (rig-lease.sh)
against `RIG_LEASE_REPO`/`RIG_LEASE_RUN_ID`, else `GITHUB_REPOSITORY`/`GITHUB_RUN_ID` (the same
defaults rig-busy-gate.sh writes). An operator shell has neither, so a foreign holder still refuses.
`--force-live` still never bypasses a lease.

Known limit: the relay deploy's rig-busy guard reads the SNV strih/stream (`STRIH_HOST`/
`STREAM_HOST` defaults) also for an away-venue (`--venue pp`) relay, so a broadcast at SNV refuses a
PP relay deploy (named FAILED, the CI venue step goes red). Set those two env vars for a manual PP
run.

## Scope: camboxes only

The handheld SBC (arm64, registered via `register:<id>`) is not a cambox: it is not in any
`CAMERA_SET` and stays out of `deploy-fleet`. Its relay is `bkshading-deploy-relay.sh --host <sbc>
--arch arm64` (`bkshading-sbc.md`).

## The readiness row (rig-dev-handover-check item 15, shading)

`probe_shading` prints a SECOND line per box: `camN (ip): camera-box=X relay=Y ->
verdict=RELAY-…` from `bkshading_relay_version_verdict` (graded against THAT box's camera-box):
- `RELAY-STALE` (versions differ) and `RELAY-MISSING` (unit without binary) are FORGOT, even while
  the relay is off in TEST mode: an old relay must show before an event;
- `RELAY-CURRENT`, `RELAY-NONE`, `RELAY-UNKNOWN` are neutral, so a TEST-mode all-off fleet stays
  UNKNOWN (never a false OK).
The `shading` item's `forgot_token_msgs` names the fix per forgot token (deploy-fleet for a stale
relay, the issue-1309 restart for SHADING-DEAD). Chosen over the version-integrity gate: that gate
grades the OBS boxes, while this probe already ssh's every active cambox for its relay.

Tests: `tests/python/test_deploy_fleet_relay_1452.py` (deploy-fleet run whole on the issue-1407 fake
box with a fake `sshpass`; plant an old relay at `fs/usr/local/bin/bkshading-relay`, an active one
via `state/active-bkshading-relay.service`; a foreign / own lease via `RIG_LEASE_DIR`).
