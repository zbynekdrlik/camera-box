---
paths:
  - "scripts/camera_test_settings.py"
  - "scripts/lib/camera-test-settings.sh"
  - "scripts/camera-test-baseline.json"
  - "tests/python/test_camera_test_settings_1371.py"
  - "tests/python/test_camera_prod_exposure_restore_1371.py"
  - "tests/python/test_rig_dev_handover_exposure_1371.py"
  - "scripts/rig-mode.sh"
  - "scripts/rig-dev-handover-check.sh"
  - "scripts/rig_dev_handover_decision.py"
---

# E2E `[0/8]` test-camera shutter/ISO ENFORCE over the bkshading USB path (issue 1371)

Owner, 25.9.2026: the test camera's shutter and ISO were wrong, so the release E2E went red, and
"tie si mas uz ty vediet pri teste skontrolovat a nastavit". The E2E now READS the ONE test camera
(the BMPCC fed through the HDMI splitter into every cambox), SETS what differs from a checked-in
baseline, and READS IT BACK, before every run. Nothing is restored after a RUN: the baseline IS the
test state. What the owner had before the first set of a development period is snapshotted and put
back when the rig leaves development (the EVENT switch), see "The production exposure" below.

## Parts

| Part | Role |
|---|---|
| `scripts/camera_test_settings.py` | PURE decisions (pytest Tier-0): keys, baseline load/validate, parse the multi `--get-config` output, plan the sets, grade the read-back, the presence/ack/pinned matrix |
| `scripts/lib/camera-test-settings.sh` | THIN transport: sysfs presence probe, ssh + gphoto2 get/set, the issue-1271 guard before a set |
| `scripts/camera-test-baseline.json` | the ONE baseline, raw gphoto2 values |
| `scripts/recording-e2e.sh` | ONE bare call after the issue-808 relay pause + its temporary restore handler, before the reachability banner |
| `scripts/rig-mode.sh` (EVENT path) | the production-exposure restore before the relay start + the Discord note after the EVENT contract |
| `scripts/rig-dev-handover-check.sh` | the `exposure` item: `camera_test_settings.py snapshot-state` |

## Keys: the relay's own, never guessed

The key names are the bkshading relay's (`bkshading/relay/src/transport.rs` `CORE_CONFIG_KEYS`,
`bkshading/proto/src/read.rs` `plan_writes`). Two pytest tests pin that every key here is one the
relay reads and writes. If the relay renames a key, those tests go red, not the rig.

- **REQUIRED `iso` + `d002`.** `d002` is the shutter ANGLE x100 (18000 = 180 deg), the raw value the
  relay writes. It is NOT a 1/N denominator. The log prints the derived `1/N s at <fps> fps` using
  the relay's `convert_angle_or_denom` formula (`360*fps/angle`, round-half-up), as context only.
- **OPTIONAL `f-number` / `d004` (WB K) / `d005` (tint).** null = read + logged only; a value =
  enforced. Aperture is NOT pinned by default because the cam1 BMPCC silently drops aperture PTP
  writes (issue 1343). A pinned f-number would abort every run on MISMATCH until that is solved.
- **CONTEXT `d007` (project fps).** Read for the log line only, NEVER set. fps is the issue-809
  grab-mode coupling, not a test-exposure setting.
- A baseline value must be a plain token (`[A-Za-z0-9./_+-]`), because it becomes a word of a
  remote `gphoto2 --set-config key=value`. An int is normalized to its string. bool, an empty
  string, an unknown key and a missing key are all refused, with a named ERROR and exit 1.

## The decision matrix + the UNVERIFIED -> ENFORCE transition

`decide(present, acked, pinned)`. pinned = `iso` AND `d002` have values. acked = `testcam` named
in `CAMBOX_OFFLINE_ACK` / `rig-fleet.txt` (the existing `cambox-offline-ack.sh` mechanism).

| camera on USB | acked | baseline | result |
|---|---|---|---|
| yes | yes | any | ABORT: stale ack (the ack contract: remove it, the camera is back) |
| yes | no | null | ABORT: refuses, and PRINTS the camera's current `iso`/`d002` as a ready baseline JSON |
| yes | no | pinned | ENFORCE: read -> set what differs -> read back -> abort on any MISMATCH |
| no | yes | any | loud report-only UNVERIFIED + the ack NOTE |
| no | no | null | loud report-only UNVERIFIED (`::warning` annotation + log block) |
| no | no | pinned | ABORT: named, lists the boxes checked + how to ack |

**Pinned 26.9.2026**: `iso=8000`, `d002=2160` (1/1000 s at 60 fps), owner-confirmed with the BMPCC on
cam1 USB. So every run ENFORCES them, and an absent camera ABORTS unless acked `testcam:<reason>`.
(Until then the shipped state was `null` everywhere, a loud report-only UNVERIFIED.)

**Pinning (a SUPERVISOR step, once the owner plugs the BMPCC USB-C into the source cambox and
confirms the camera is set right):**
1. Run the E2E (or just the lib) once. With the camera present and the baseline null, the step
   aborts and prints `{"schema": 1, "values": {"iso": "...", "d002": "...", ...}}` with the
   camera's current values.
2. Put those `iso` + `d002` values into `scripts/camera-test-baseline.json` and commit.
3. From then on, an ABSENT camera ABORTS the run. That is the intended flip: once there is a
   baseline to vouch for, "camera not reachable" is a failure, not a note. For a planned absence,
   ack it: `testcam:<reason>` in `rig-fleet.txt`.

## Transport rules

- **Presence = sysfs `idVendor == 1edb` (Blackmagic design), no PTP session.** Candidates = the
  two relay-PAUSED boxes (`$CAMERA_NAME=$CAM1_IP`, `cam2=$PAINTER_IP`), first hit wins. The camera
  is only enforced where the issue-808 pause guarantees exactly one gphoto2 user. A camera cabled
  to any OTHER box counts as absent: move it, or extend the candidate list together with the pause.
  An ssh-unreachable candidate is named `UNREADABLE` and treated as absent (fail-closed once pinned).
- **At most 3 USB-PTP sessions per run** (read, set, read-back), each ONE gphoto2 process with
  every key: the relay's issue-1229 coalesced-read doctrine. Each gphoto2 call is bounded by a
  remote `timeout 20` inside a local `timeout 40` ssh. A camera already at the baseline costs ONE
  session and no guard.
- **The issue-1271 rig-busy guard runs INSIDE the lib, immediately before `--set-config`, and only
  when a set is needed.** A read is not a mutation. The static 1271 test's `muts` list does not
  include this site, because its guard is in the lib, not the e2e text.
  `test_a_live_broadcast_blocks_the_set` pins the order.
- **The call sits AFTER the temporary relay-restore handler**, so any abort here still restores the
  paused relays. Keep it there if the region is reordered.
- **"Exactly one gphoto2 user" is CHECKED on the box, not assumed.** The issue-808 pause is
  best-effort: it times out after 8 s, is `|| true`, and never confirms the unit stopped. So every
  gphoto2 session (`camera_test_settings_gphoto2_cmd`) starts with three checks in the SAME remote
  command:
  - `bkshading-relay.service` must be truly stopped. The check reads the printed
    `systemctl is-active` state, NOT `--quiet`, because `--quiet` is false for `activating`
    (auto-restart), `deactivating` and `reloading`. Only `inactive` / `failed` / `unknown` pass;
    anything else exits 97, the abort "relay still active on <box>";
  - `pgrep` must exist, else exit 96. A missing pgrep would otherwise silently skip the next check;
  - no leftover `gphoto2` process (`pgrep -x`), else exit 98, the abort "another gphoto2 process".
  The set path aborts on 96/97/98 too, so a relay that comes back between read and set is caught.
  Without these checks, a relay that kept polling would show up as a misleading "read failed" or
  MISMATCH.
- **The tests RUN the generated remote text.** The fake `sshpass` executes every gphoto2 command
  under bash, with PATH = stub `systemctl` / `pgrep` / `gphoto2` + the real `timeout`. So a
  changed exit code, an inverted condition or a check moved after the gphoto2 call goes red. Only
  the sysfs presence probe is emulated.
- **Transport and decision exit codes are kept apart.** The ssh/gphoto2 rc is named in the abort:
  124 timed out, 127 not found, 255 ssh. A python `plan`/`grade` rc is reported as "decision
  rc", so an unreadable output is never confused with a dead transport.
- Set failure (`gphoto2 --set-config` rc != 0, other than 97/98) is only a WARNING. The read-back
  grades the result, so a partially applied set is caught as a MISMATCH abort, never trusted.
- **Pinning `f-number` needs the camera's exact label spelling.** The relay treats `f/4` and
  `f/4.0` as the same aperture (read.rs), but this step compares strings, so pin the exact
  `Current:` text the camera prints. `suggest` only proposes `iso` + `d002`, and flags either one
  when the baseline would refuse it (`UNPINNABLE`). An f-number is pinned by hand.
- Presence matches any Blackmagic USB device (vendor only, no interface-class check). The
  interface class the BMPCC's PTP function uses has not been verified live (no camera on USB,
  issue 1350). A non-camera Blackmagic device would still abort loudly as a failed gphoto2 read on
  that named box, never a silent pass.
- The UNVERIFIED reaches only the run log + a `::warning` annotation, not the per-run Discord
  report. That is the same as the issue-1324 marker UNVERIFIED: the report has no channel for
  preflight notes yet.

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
  `d002`), `d007` as log context, the box and the UTC time. A key the camera reports no value for is
  left out. `d007` is never restored.
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
  "napíš Claudovi"); the commands are in the run log. The prepend is written to a temp file and
  moved over only when complete, and a failed read or write falls back to appending, so the
  contract text itself is never lost. Nothing is added when no snapshot was waiting. The note can
  never fail the caller.
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

## Tier-0 verification

`python3 -m pytest tests/python/test_camera_test_settings_1371.py` covers the whole step with no
camera and no rig. The bash lib is driven end to end under the caller's `set -euo pipefail` with
a fake `sshpass` on PATH (it emulates the cambox sysfs + gphoto2 get/set, with a camera that can
IGNORE a key) and a fake `obs_phase2.py` (the guard). The wiring test pins that the call is ONE
bare column-0 statement between the temporary trap close and the reachability banner.

The harness runs with a temp `HOME` (and `CAMERA_PROD_EXPOSURE_SNAPSHOT` unset), so no test ever
reads or writes the real dev1 snapshot. The fake `systemctl` also answers `stop` (the restore's
relay stop), and the fake `gphoto2` records whether the snapshot was already on disk at the moment
of the set. The snapshot/restore tests live in `tests/python/test_camera_prod_exposure_restore_1371.py`
(they import this file's harness; keep each file under ~1000 lines). They drive
`camera_test_settings_restore` under the caller's
`set -euo pipefail` and assert the script continues after every failure. `rig-mode.sh` itself is
checked statically (the call sits before the relay start, the note between the EVENT contract and
the Discord send) and by sourcing it (the helpers are defined above its source-guard). The EVENT
path cannot be run in a test; after editing it, run the old-vs-new literal-count anchor sweep over
every test that reads `rig-mode.sh`.
