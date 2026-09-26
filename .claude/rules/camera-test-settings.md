---
paths:
  - "scripts/camera_test_settings.py"
  - "scripts/lib/camera-test-settings.sh"
  - "scripts/camera-test-baseline.json"
  - "tests/python/test_camera_test_settings_1371.py"
---

# E2E `[0/8]` test-camera shutter/ISO ENFORCE over the bkshading USB path (issue 1371)

Owner, 25.9.2026: the test camera's shutter and ISO were wrong, so the release E2E went red, and
"tie si mas uz ty vediet pri teste skontrolovat a nastavit". The E2E now READS the ONE test camera
(the BMPCC fed through the HDMI splitter into every cambox), SETS what differs from a checked-in
baseline, and READS IT BACK, before every run. Nothing is restored afterwards: the baseline IS the
test state.

## Parts

| Part | Role |
|---|---|
| `scripts/camera_test_settings.py` | PURE decisions (pytest Tier-0): keys, baseline load/validate, parse the multi `--get-config` output, plan the sets, grade the read-back, the presence/ack/pinned matrix |
| `scripts/lib/camera-test-settings.sh` | THIN transport: sysfs presence probe, ssh + gphoto2 get/set, the issue-1271 guard before a set |
| `scripts/camera-test-baseline.json` | the ONE baseline, raw gphoto2 values |
| `scripts/recording-e2e.sh` | ONE bare call after the issue-808 relay pause + its temporary restore handler, before the reachability banner |

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

## Tier-0 verification

`python3 -m pytest tests/python/test_camera_test_settings_1371.py` covers the whole step with no
camera and no rig. The bash lib is driven end to end under the caller's `set -euo pipefail` with
a fake `sshpass` on PATH (it emulates the cambox sysfs + gphoto2 get/set, with a camera that can
IGNORE a key) and a fake `obs_phase2.py` (the guard). The wiring test pins that the call is ONE
bare column-0 statement between the temporary trap close and the reachability banner.
