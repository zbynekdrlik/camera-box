---
paths:
  - "scripts/camera_test_settings.py"
  - "scripts/lib/camera-test-settings.sh"
  - "scripts/camera-test-baseline.json"
  - "tests/fixtures/shading_pinned_rest/CONTRACT.md"
  - "tests/python/test_camera_test_settings_1371.py"
  - "tests/python/test_camera_prod_exposure_restore_1371.py"
  - "tests/python/test_rig_dev_handover_exposure_1371.py"
  - "scripts/rig-mode.sh"
  - "scripts/rig-dev-handover-check.sh"
  - "scripts/rig_dev_handover_decision.py"
---

# E2E `[0/8]` test-camera shutter/ISO ENFORCE over the camera's REST API (issue 1371, REST since issue 1452)

Owner, 25.9.2026: the test camera's shutter and ISO were wrong, so the release E2E went red, and
"tie si mas uz ty vediet pri teste skontrolovat a nastavit". The E2E now READS the ONE test camera
(the BMPCC fed through the HDMI splitter into every cambox), SETS what differs from a checked-in
baseline, and READS IT BACK, before every run. Nothing is restored after a RUN: the baseline IS the
test state. What the owner had before the first set of a development period is snapshotted and put
back when the rig leaves development (the EVENT switch), see "The production exposure" below.

## Parts

| Part | Role |
|---|---|
| `scripts/camera_test_settings.py` | PURE decisions (pytest): endpoints + fields, baseline load/validate, parse the `CTS_GET` lines, plan the PUTs, grade the read-back, the presence/ack/pinned matrix |
| `scripts/lib/camera-test-settings.sh` | THIN transport: sysfs presence probe, ssh + `curl` REST get/put on the box, the issue-1271 guard before a set |
| `scripts/camera-test-baseline.json` | the ONE baseline (schema 2), the REST field values |
| `scripts/recording-e2e.sh` | ONE bare call after the issue-808 relay pause + its temporary restore handler, before the reachability banner |
| `scripts/rig-mode.sh` (EVENT path) | the production-exposure restore before the relay start + the Discord note after the EVENT contract |
| `scripts/rig-dev-handover-check.sh` | the `exposure` item: `camera_test_settings.py snapshot-state` |

## The REST transport (issue 1452, shading#6, owner 9.11.2026)

The step drives the camera over the Blackmagic REST API on the camera's own USB-C network link
(CDC-NCM), exactly as the shading v0.2.x relay does; gphoto2/PTP is gone. `curl` runs ON the cambox
(the link is the box's), against `http://<camera>/control/api/v1/<endpoint>`.

- **Endpoints + fields are the shading REST contract's** (`tests/fixtures/shading_pinned_rest/CONTRACT.md`):
  `video/iso` `iso`; `video/shutter` `shutterAngle` (degrees x100) OR `shutterSpeed` (1/N s);
  `video/whiteBalance`; `video/whiteBalanceTint`; `lens/iris` `apertureStop` (the only field that moves
  the iris); `system/format` `frameRate` (context only, never set). A pytest pins every one to a
  contract row. An optional endpoint the firmware does not serve (404/501, the Pocket 6K at fw 8.6) reads
  None; `video/iso` + `video/shutter` must answer 200 or the read is unreadable.
- **The shutter is enforced in the unit the camera reports** (its shutter-display setting). The
  baseline (schema 2) pins BOTH units of the one owner-confirmed exposure (ISO 8000, 1/1000 s at 60 fps
  = angle 2160), so either camera setting is enforced. A camera reporting a unit the baseline leaves null
  is a named refusal (exit 8), never a guessed conversion. Pinned = `iso` + at least one shutter unit.
- **Finding the camera on the box** (the relay's way): the `/sys/class/net` link whose driver is
  `cdc_ncm` on a vendor-`1edb` USB device, the networkd lease's `SERVER_ADDRESS` (else `ROUTER`), else
  the other host of the link's /30. No such link / address = exit 95, named: the camera link file
  (verify-device (bb)), Web media manager ON, fw >= 8.6 (6K) / 9.8 (4K). Seams:
  `CAMERA_TEST_SETTINGS_NET_SYSFS`, `CAMERA_TEST_SETTINGS_LEASES_DIR`.
- **A PUT body is ONE numeric field** (`{"iso":8000}`), checked by `camera_test_settings_put_ok`
  before the remote text is built, on one of the five enforceable endpoints: no body can carry shell
  syntax. A PUT answered with anything but 2xx is a WARNING; the read-back decides.
- **A gphoto2-era snapshot (schema 1) still restores**: `d002` IS the REST `shutterAngle`, `d004`/`d005`
  = K / tint, `f-number` `f/4.0` = `apertureStop` 4.0, the fps from `context.d006` (x100) or the legacy
  whole-fps `context.d007` (issue 1402). A snapshot holding the shutter in a unit the camera no longer
  reports is a named restore failure (exit 8).

## The decision matrix + the UNVERIFIED -> ENFORCE transition

`decide(present, acked, pinned)`. pinned = `iso` AND a shutter unit (`shutterAngle` / `shutterSpeed`) have values. acked = `testcam` named
in `CAMBOX_OFFLINE_ACK` / `rig-fleet.txt` (the existing `cambox-offline-ack.sh` mechanism).

| camera on USB | acked | baseline | result |
|---|---|---|---|
| yes | yes | any | ABORT: stale ack (the ack contract: remove it, the camera is back) |
| yes | no | null | ABORT: refuses, and PRINTS the camera's current `iso` + shutter (its own unit) as a ready baseline JSON |
| yes | no | pinned | ENFORCE: read -> set what differs -> read back -> abort on any MISMATCH |
| no | yes | any | loud report-only UNVERIFIED + the ack NOTE |
| no | no | null | loud report-only UNVERIFIED (`::warning` annotation + log block) |
| no | no | pinned | ABORT: named, lists the boxes checked + how to ack |

**Pinned 26.9.2026**: ISO 8000, 1/1000 s at 60 fps (schema 2: `shutterAngle` 2160 = `shutterSpeed` 1000), owner-confirmed with the BMPCC on
cam1 USB. So every run ENFORCES them, and an absent camera ABORTS unless acked `testcam:<reason>`.
(Until then the shipped state was `null` everywhere, a loud report-only UNVERIFIED.)

**Pinning (a SUPERVISOR step, once the owner plugs the BMPCC USB-C into the source cambox and
confirms the camera is set right):**
1. Run the E2E (or just the lib) once. With the camera present and the baseline null, the step
   aborts and prints `{"schema": 2, "values": {"iso": ..., "shutterAngle"|"shutterSpeed": ..., ...}}` with the
   camera's current values.
2. Put those `iso` + shutter values (both units of the one exposure) into `scripts/camera-test-baseline.json` and commit.
3. From then on, an ABSENT camera ABORTS the run. That is the intended flip: once there is a
   baseline to vouch for, "camera not reachable" is a failure, not a note. For a planned absence,
   ack it: `testcam:<reason>` in `rig-fleet.txt`.

## Transport rules

- **Presence = sysfs `idVendor == 1edb` (Blackmagic design), no request to the camera.** Candidates =
  the two relay-PAUSED boxes (`$CAMERA_NAME=$CAM1_IP`, `cam2=$PAINTER_IP`), first hit wins. A camera
  cabled to any OTHER box counts as absent. An ssh-unreachable candidate is named `UNREADABLE` and
  treated as absent (fail-closed once pinned).
- **At most 3 REST sessions per run** (read, PUTs, read-back), each ONE ssh with every request, each
  request bounded by `curl -m 5`, the ssh by a local `timeout 40`. A camera already at the baseline
  costs ONE session and no guard.
- **The issue-1271 rig-busy guard runs INSIDE the lib, immediately before the PUTs, and only when a
  set is needed.** `test_a_live_broadcast_blocks_the_set` pins the order.
- **The call sits AFTER the temporary relay-restore handler**, so any abort here still restores the
  paused relays.
- **The relay must be really stopped, CHECKED on the box** in the same remote command as every session
  (the issue-808 pause is best-effort): the printed `systemctl is-active` state, only `inactive` /
  `failed` / `unknown` pass, else exit 97 "relay still active"; `curl` must exist (exit 96). A relay
  that comes back between read and set is caught by the PUT session's own check.
- **The tests RUN the generated remote text.** The fake `sshpass` executes every REST session under
  bash with PATH = stub `systemctl` + a fake camera `curl` + the real text tools, over a fake
  `/sys/class/net` tree and lease file; only the sysfs presence probe is emulated.
- **Transport and decision exit codes are kept apart.** The ssh rc is named in the abort (124 timed
  out, 127 not found, 255 ssh); a python rc is reported as "decision rc".
- The UNVERIFIED reaches only the run log + a `::warning` annotation, not the per-run Discord report.

## The production exposure: snapshot before the first set, restore at the EVENT switch

Owner, 26.9.2026: "ked vypina sa development tak ze aj vratis iso a uzavierku naspat". The E2E
changes the test camera to the TEST baseline, so leaving development must put the owner's own
production ISO + shutter back. Design comment 5847510146 (Approach 1). A pinned "production
exposure" in the repo was rejected (the owner picks it per event), and so was a restore after every
run (wrong timing; TEST mode must stay alive between runs).

**The snapshot (E2E side, `camera_test_settings_enforce`).**
- Written right AFTER the issue-1271 guard allows the set and right BEFORE the ONE `--set-config`,
  from the values of the same read session. It is written ONLY when no snapshot is waiting: the
  first set of a development period records the owner's values, and later runs print
  `SNAPSHOT kept` and never overwrite it.
- A waiting snapshot is LOADED, never just checked for existence. An unreadable one is no record,
  so the E2E aborts before the set (`SNAPSHOT kept` on a broken file would overwrite the owner's
  values with nothing restorable). A key the baseline pins mid-period is ADDED from the camera's
  current value (the test never set that key, so it is still the owner's) -- `SNAPSHOT extended`;
  a kept key is never changed.
- Writing a NEW snapshot drops a leftover restore-failed marker: a marker without a snapshot is
  stale, and it would otherwise flag the fresh period as failed.
- No set = no snapshot. A camera already at the baseline, or a set the rig-busy guard blocks,
  writes nothing.
- Path: `~/.camera-box/camera-prod-exposure.json` on the runner (dev1, where both the E2E and
  `rig-mode.sh` run), or `$CAMERA_PROD_EXPOSURE_SNAPSHOT`. The ONE resolver is
  `camera_test_settings.py snapshot-path`; the bash lib, the rig-mode restore and the handover
  probe all ask it, never re-derive it.
- Content: the camera's current value of every key the run ENFORCES (`pinned_keys`, today `iso` +
  the shutter in the camera's unit), the project fps (`context.fps`) as log context, the box and the UTC time. A key the camera
  reports no value for is left out. The fps context is never restored (`load_snapshot` refuses a
  `d006`/`d007` value key).
- **LEGACY `context.d007` (issue 1402).** A snapshot saved before the d006 switch recorded the
  project fps as `context.d007`, WHOLE fps (`60`). `context_fps100` reads `context.d006` whenever
  the key is present and falls back to the legacy `context.d007` (x100 = whole fps x 100) ONLY when
  the d006 key is absent, so a snapshot taken during a running development period still restores and
  logs the same 1/N s
  (`test_a_legacy_snapshot_with_only_the_whole_fps_d007_still_restores_and_logs_the_same_shutter`).
  A present d006 that is null or not a number is unreadable and never falls back (no 1/N, just the
  angle). A new snapshot carries `d006` whenever the camera reports it as a plain token, and never
  writes `d007`; without a readable d006 it has no fps context, so it logs the angle only. The
  legacy branch is kept on purpose: it is one cheap branch, and no removal is planned.
- **No record = no set.** A value that is not a plain token (it could not be written back through
  the word-split `--set-config`), a box label or time that is not one, or a snapshot that cannot be
  written (a full disk, a file where the directory should be), ABORTS the E2E before the camera
  changes. Losing the owner's value silently is worse than one aborted run. The python output is
  captured into a variable first, so the abort never depends on the caller's `pipefail`.
- Written atomically and exclusively: a temp file in the same folder, then `os.link` to the final
  name (fails when a snapshot appeared meanwhile). A half-written snapshot cannot exist.

**The restore (EVENT side, `camera_test_settings_restore`, called from `rig-mode.sh`'s EVENT path).**
- ONE bare-ish call `camera_test_settings_restore ... || true` placed BEFORE the issue-1311 relay
  start, so the relay is still stopped (TEST mode stopped + disabled it). The restore also stops the
  relay on the camera box before its read (a rig that ran an E2E outside TEST mode); the EVENT
  switch starts + enables it right after.
- Candidates are the same two relay boxes as the E2E (`${RIG_SOURCE_BOX}=$RIG_SOURCE_IP`,
  `cam2=$PAINTER_IP`); the guard reads `STRIH_IP` / `STREAM_IP`. rig-mode keeps the OBS WebSocket
  password in `OBS_WS_PASSWORD` while the shared guard reads `OBS_PASSWORD`, so the restore hands
  it over inside its subshell (an auth-enabled OBS would otherwise make the guard fail OPEN).
- Flow: presence probe, **the issue-1271 guard**, relay stop, ONE read, `restore-plan` (only the
  snapshot keys that differ), the guard AGAIN, ONE set, ONE read-back, `restore-grade`, then
  `consume` moves the file to `camera-prod-exposure.consumed-<UTC stamp>.json` (a `-N` suffix if
  that name is taken), so the next development period snapshots afresh. The first guard sits BEFORE
  the relay stop and the PTP session: both are rig mutations, and a re-run of `rig-mode.sh event`
  while strih/stream is live must touch nothing (review round 1 found the stop + read ran first).
  A camera already at the production values costs one read and one guard; the snapshot is consumed.
- The set + read-back is ONE shared helper, `_cts_set_and_readback`, used by the E2E enforce and
  the restore, so every transport code (96/97/98 refusals, the read-back abort) is handled the same.
- **Never fatal to the EVENT switch.** Every camera step runs in a SUBSHELL (the sanctioned
  "own exit contract" caller shape of `stray-session-check.sh`), so the shared transport/guard
  helpers' `exit 1` ends only the subshell. An absent camera, a MISMATCH, a live broadcast, a relay
  that comes back before the set, a transport timeout, an invalid snapshot or a failed consume is a
  LOUD `WARNING: issue 1371 ... NOT restored` line + a `::warning` annotation, the snapshot STAYS
  for a retry (run `rig-mode.sh event` again with the camera on USB and the rig not on air), and the
  function returns 1. It also writes `camera-prod-exposure.restore-failed.json` next to the
  snapshot (`restore-failed` subcommand); `consume` removes it.
- **"Set the camera by hand" is never enough on its own.** A snapshot left in place is kept by the
  next development period's E2E (`SNAPSHOT kept`) and written back at the next EVENT switch, over
  whatever the owner set. So every failure text names the move-aside:
  `python3 scripts/camera_test_settings.py consume`.
- A restore that read back fine but could not move the snapshot aside is `restored-unconsumed`
  (subshell exit 22), never "NOT restored": the camera IS right, only the file needs moving.
- The outcome reaches the owner's phone: `camera_test_settings_restore_discord_note` adds one
  Slovak line to the EVENT Discord confirmation file right after the EVENT contract, before the
  send. A failure (⚠️ sa NEVRÁTILA) goes ON TOP of the message, so it is not buried under a green
  contract; a success (✅ vrátená / ✅ už sedela / ✅ vrátená, ale snímku treba odložiť) goes at the
  end. The phone line never names a command (the owner cannot run one from the phone, it says
  "napíš Claudovi"); the commands are in the run log. The writing is the ONE shared
  `event_mode_discord_note_add` (`scripts/lib/event-mode-discord-confirm.sh`, issue 1407; the relay
  note uses it too): the prepend is written to a temp file and moved over only when complete, and a
  failed read or write falls back to appending, so the contract text itself is never lost. Nothing
  is added when no snapshot was waiting. The note can never fail the caller.
- The outcome does NOT change the EVENT exit status: that verdict is the rig-cleanliness contract,
  and the camera exposure is a separate, loudly reported fact.

**The handover check** reads the state with `camera_test_settings.py snapshot-state` (item
`exposure`, see `.claude/rules/rig-dev-handover-check.md`), together with the rig-mode capture:
none / restored = OK; pending in TEST = OK (the normal state between an E2E and its EVENT switch);
pending in EVENT = SUPERVISOR (the handover moment: the EVENT switch never restored it -- it may
have aborted before the restore step, which leaves no marker); pending with an unreadable mode =
UNKNOWN; pending WITH the restore-failed marker, or an unreadable snapshot = SUPERVISOR.

**Supervisor live acceptance (never a worker step):** run the enforce with no snapshot and the
camera already at the baseline (no set, no snapshot); then put a hand-made snapshot on dev1 and run
`rig-mode.sh event`: the camera must read the snapshot values back and the file must move aside;
`rig-mode.sh test` + the next E2E re-apply the baseline and take a fresh snapshot.

## Verification

`python3 -m pytest tests/python/test_camera_test_settings_1371.py` covers the whole step with no
camera and no rig. The bash lib is driven end to end under the caller's `set -euo pipefail` with
a fake `sshpass` on PATH (it emulates the cambox sysfs presence + runs the REST sessions against a fake camera `curl`, with a camera that can
IGNORE a key) and a fake `obs_phase2.py` (the guard). The wiring test pins that the call is ONE
bare column-0 statement between the temporary trap close and the reachability banner.

The harness runs with a temp `HOME` (and `CAMERA_PROD_EXPOSURE_SNAPSHOT` unset), so no test ever
reads or writes the real dev1 snapshot. The fake `systemctl` also answers `stop` (the restore's
relay stop), and the fake `curl` records whether the snapshot was already on disk at the moment
of the set. The snapshot/restore tests live in `tests/python/test_camera_prod_exposure_restore_1371.py`
(they import this file's harness; keep each file under ~1000 lines). They drive
`camera_test_settings_restore` under the caller's `set -euo pipefail` and assert the script continues after every failure. `rig-mode.sh` itself is
checked statically (the call sits before the relay start, the note between the EVENT contract and
the Discord send) and by sourcing it (the helpers are defined above its source-guard). The EVENT
path cannot be run in a test; after editing it, run the old-vs-new literal-count anchor sweep over
every test that reads `rig-mode.sh`.
