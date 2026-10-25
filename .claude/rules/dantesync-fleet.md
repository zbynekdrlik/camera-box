---
paths:
  - "scripts/lib/dantesync-fleet.sh"
  - "scripts/dantesync_fleet.py"
  - "scripts/dantesync-canonical-config.json"
  - "scripts/dantesync-config-drift.sh"
  - "scripts/dantesync-version-gate.sh"
  - "scripts/dantesync-fleet-upgrade.sh"
  - "scripts/dantesync-clock-alert-watchdog.sh"
  - "scripts/rig-dev-handover-check.sh"
  - "tests/python/test_dantesync_fleet*_1372.py"
  - "tests/python/test_dantesync_clock_fleet_1372.py"
  - "tests/python/test_dantesync_fleet_pp_1412.py"
  - "scripts/strih-boxes/*.env"
  - "tests/python/test_clock_discipline_1372.py"
  - "tests/fixtures/dantesync_clock_discipline_1372.tsv"
---

# DANTESYNC_FLEET — the ONE declared list of every dantesync node (issue 1372 part B)

`scripts/lib/dantesync-fleet.sh` answers "which machines run dantesync". Before it, every consumer
answered that separately from the two VIDEO lists (camera-set.sh + obs-fleet.sh) plus literals, and
the audio-VLAN PCs **mbc** (10.77.7.232) and **fohabl** (10.77.7.30) were never version-gated,
upgraded, clock-watched or config-checked. The owner found them stale / differently configured by
hand (25.9.2026). Adding a node is now ONE row; every consumer picks it up.

## The list

- **Cameras (issue 1413: per VENUE):** the camera_resolve walk (cam1, cam2, … to the first unknown
  name; never a literal range), keeping each camera a strih fact file (`scripts/strih-boxes/*.env`,
  PARSED) lists in its `STRIH_CAMERAS`. Not `CAMERA_ACTIVE_SET`: a camera retired from measurement
  still runs dantesync.
  - A camera of the HOME venue (`DANTESYNC_FLEET_HOME_VENUE`, `snv`; strih-snv.env `2 3 5`) is
    `camN|<camera_resolve ip>|linux|video|always|root|`, the clock watchdog's CAM arm
    (`dantesync_fleet_camera_names` = the `always` camera rows only).
  - A camera of another venue (Poprad, strih-pp.env `1 4 6 7`) is
    `camN|<dev1 address>|linux|video-pp|reach|root|`: locked to the PP grandmaster, skipped while
    ssh :22 does not answer, and watched through the REACH arm (never twice).
  - The address comes from the venue's `STRIH_CAMERA_DEV1_NET` fact: `direct` = camera_resolve's
    address; `A.B.C` = that /24 + the camera's camera_resolve last octet (PP: `10.76.9`, cam7 =
    10.76.9.67 through companion-pp); `none` on a box that lists no camera (stream-pp).
  - A camera listed by two venue files, a bad venue name or a bad dev1 network prints no row for that
    camera: `dantesync_fleet_rows` prints the error and returns 1, python raises RuntimeError. The
    consumers (`dantesync_fleet_spec` / `_field` / `_names`) read the rows `2>/dev/null || true`, so
    there that camera is just absent; CI pins the committed fact files clean
    (`test_every_camera_row_follows_its_venue` asserts rc 0). A camera no venue lists is in no row.
  - **The camera-box BINARY rides the same rows (issue 1413, second part).** Two public filters over
    `_dantesync_fleet_camera_rows`: `dantesync_fleet_venue_camera_rows <venue>` (`camN|addr|homegate`;
    rc 1 + the named camera on a fact-file error) and `dantesync_fleet_away_venues` (every non-home
    venue that lists cameras, today `pp`). Never compute a dev1 camera address anywhere else.
    - `deploy-fleet.sh --venue <name>` deploys the venue's cameras at those addresses through the
      unchanged per-box swap (issue 1407 sidecar + verified ro close). An acked-offline camera (the
      SAME acks as the home deploy: explicit `CAMBOX_OFFLINE_ACK`, else `rig-fleet.txt`) and a `reach`
      camera that `dantesync_fleet_present` says is away are SKIPPED by name, never dialled (exit 0);
      a reachable one that fails, including one that answers the probe but refuses ssh, is FAILED
      (exit 1). It refuses the home venue (that set is `CAMERA_ACTIVE_SET` minus the acks),
      `CAMERA_SET` (read BEFORE camera-set.sh fills its default) and `--frame-probe` (the painter is
      the home rig's cam2); a fact-file error refuses the whole run before any box is touched. The
      default (SNV) path is unchanged.
    - The ci.yml `deploy-fleet` job's second step loops over `dantesync_fleet_away_venues` (no venue
      literal, so a third venue is a fact-file edit; a malformed `STRIH_VENUE` is left out and named),
      runs under `if: ${{ !cancelled() }}` (a failed SNV step does not stop it) and reds the job when
      `deploy-fleet.sh --venue` exits non-zero, naming the venue and the exit code.
    - `camera-box-version-gate.sh --other-venues` prints report-only rows (OK / PIN-DRIFT / UNKNOWN
      against main's pin, READ under `--no-main-pin`, `EXCLUDED` when acked, `SKIPPED (unreachable`
      when away) and never changes the exit; the E2E `[0/8]` call passes it, so a PP camera on an old
      build is named in every E2E log. Each read is bounded by `timeout`
      (`CAMERA_BOX_VERSION_GATE_VENUE_READ_TIMEOUT`, default 20 s): `ConnectTimeout` covers only the
      connect, and a PP box that answers :22 but hangs (a dying stick, fork exhaustion) would
      otherwise stall the home E2E while it holds the rig lease. Each away camera costs up to ~4 s
      reach probe plus the read, serially. The frame-probe align stays home-rig only.
    - Test: `tests/python/test_cambox_deploy_venues_1413.py` (the issue-1407 fake box behind a
      host-logging sshpass, `OBS_FLEET_HOME` deciding reach; run it under `unshare -n`).
  - **The glob must survive a caller's `set -f`:** the version gate builds its `--fleet` specs under
    noglob, and the first cut of issue 1413 silently dropped every camera from the gated set. Both
    fact-file readers run `local -; set +f` (bash restores the caller's option on return), pinned by
    `test_the_camera_rows_survive_a_callers_noglob`.
  - Moving a camera between venues is one edit in each of the two fact files (plus camera-set.sh's
    `CAMERA_ACTIVE_SET` default when the home venue changes; `harness_camera_set.rs` pins it equal to
    strih-snv.env's `STRIH_CAMERAS`).
- **The table `DANTESYNC_FLEET`** (env-overridable): `name|addr|os|role|homegate|user|credvar`.
  - `addr`: a literal, `obs:<name>` (resolved through `obs_fleet_host` — a `retired` obs-fleet box
    drops out, so imag returns with its obs-fleet flip), or `local` (dev1).
  - `role`: `video` | `audio` | `ntp-master` | a venue role `video-<venue>` / `ntp-master-<venue>`
    (issue 1412) — picks the canonical config template AND the grandmaster the node must lock to
    (`dantesync_fleet_role_gm_host`).
  - `homegate`: `always` | `obsfleet` (traveling resolume: read only while `obs_fleet_is_home`) |
    `reach` (issue 1412: read only while ssh :22 answers) | `local`.
  - `user`: `-` = the consumer's default login; fohabl is `master`.
  - `credvar`: the NAME of the env var holding the node's own ssh password (fohabl:
    `DANTESYNC_FOHABL_SSH_PASS`). NEVER a value in a committed file. The value comes from the env or
    the dev1-local `~/.config/camera-box/dantesync-fleet.env` (0600, `KEY=VALUE`, PARSED not sourced,
    only row-named keys read). A missing value = that node reads UNKNOWN / is skipped — never dialled
    with an empty or a foreign password.
- **The Poprad (PP) venue rows (issue 1412).** `strih-pp|obs:strih-pp|linux|ntp-master-pp|reach|-|`
  (the PP NTP master), `stream-pp|obs:stream-pp|linux|video-pp|reach|-|`,
  `resolume-pp|resolume-pp.lan|windows|video-pp|reach|-|` (no obs-fleet row; it runs no managed
  OBS). Owner (11.10.2026): "a tiez tam nie je dantesync tam nie je updatnuty cize potrebujes zaradit
  tento pc do updatovania". dev1 reaches the PP LAN (10.77.8.0/23) as 10.76.8.x through the
  companion-pp subnet router, so every PP row dials a NAME.
  - **GM per venue.** A venue role `video-<venue>` / `ntp-master-<venue>` (venue = lower-case ASCII
    letters + digits) locks to its venue's PTP grandmaster: `STRIH_DANTE_GM` of the ONE
    `scripts/strih-boxes/*.env` with `STRIH_VENUE=<venue>` + `STRIH_DANTESYNC_ROLE=server` (PP:
    strih-pp.env, 10.77.8.110), PARSED never sourced (`dantesync_fleet_venue_gm_host` /
    `dantesync_fleet.venue_gm_host`). None, two, or one without the GM fails LOUD naming the venue.
    Never a second literal, never moved by `RIG_GRANDMASTER_IP` (the home rig's override). The plain
    roles stay the home venue (SNV, video-clock.lan). The canonical config has `video-pp` /
    `ntp-master-pp` templates; `@video_gm` resolves to the role's venue grandmaster. A `video-pp`
    client's `ntp_server` is `$optional` `strih-pp.lan` (the stream-pp fact upstream).
  - **The `reach` homegate.** Present iff the address resolves AND ssh (`DANTESYNC_FLEET_REACH_PORT`,
    22) answers. The PP boxes are reachable only while the PP link is up, and ssh (not OBS-WS) is the
    box-up signal: a box with OBS off is still version-gated and rolled, and a dead dantesync on a
    reachable box still reads UNKNOWN / NO_DANTESYNC. An unreachable reach node is SKIPPED by the
    version gate (`SKIPPED (unreachable`), the upgrader (`dantesync_skip_away_traveling`; the fleet
    gate wins over strih-pp's obs-fleet `traveling` row), the drift check and the watchdog
    (`away (unreachable`): never a page, never a failed gate. `OBS_FLEET_HOME` (the one
    traveling-box force list) is authoritative for a reach node too, so a test that pins it never
    touches the network. A probe costs up to ~6 s per unreachable node (resolve 2 s + connect 4 s).
  - **Every venue NTP master is master-aware in the upgrader** (`dantesync_fleet_master_names` =
    every `ntp-master*` role; `NTP_MASTER` is a space-separated list), so strih-pp is never verified
    against the slave bound (the 29.9.2026 rollback class), and a forced downgrade below 1.15.0
    deletes its `date-offset.json` like strih-snv's.
  - **Watchdog tests must empty `DANTE_CLOCK_REACH_NODES`** (or pin `OBS_FLEET_HOME`) when they pin
    the roster: on dev1 the PP link is up, so a reach probe is live I/O there and absent on CI (the
    live-rig gotcha, `ci-testing-gotchas.md`). Run the PP tests in `unshare -n` too.
  - **The PP camboxes are in the list since issue 1413** (owner 12.10.2026: "chcem aby si sa staral
    ... aj o dantesync upgrady v pp sieti"), as `reach` camera rows (above). **Not in the list yet:**
    companion-pp (dantesync 1.8.10, client 10.77.8.2 in the recon; not part of this decision).
- **alex-nb (10.77.7.109, issue 1468)** is the Alex notebook on the audio VLAN (Dante Virtual
  Soundcard): `alex-nb|10.77.7.109|windows|audio|reach|alexnb|`, locked to the audio grandmaster,
  login `alexnb` with the fleet default password. A notebook is often off, so it is a `reach` node:
  skipped by every consumer while ssh :22 does not answer, never a page or a failed gate. Its W32Time
  service + the two Time Synchronization tasks are disabled (dantesync is the only clock authority).
- **lv1 (10.77.7.100) is deliberately absent** (owner ROZHODNUTÉ 25.9.2026: an operational PC, on the
  network temporarily).
- **The audio grandmaster** is `DANTESYNC_AUDIO_GM_HOST` (default 10.77.7.104 since the leader moved
  on 25.9.2026 ~21:07, Audinate 00:1d:c1:08:02:14/15; earlier 10.77.7.106, 00:1d:c1:1a:44:30 — the same clock as video-clock.lan, #1367 comment 5832526338). No DNS name exists
  for it yet; the address lives in this ONE variable.
- **Python twin:** `scripts/dantesync_fleet.py rows` prints byte-identical rows (parity pinned).
- **Clock-discipline twin (issue 1372):** `dantesync_fleet.classify_clock_discipline(status)`,
  `clock_discipline_unlocked(status)`, `date_master_micro_capable(status)`,
  `date_master_verdict(status, margin_us, micro_bound_ms=None)` (none/ok/out/paused/unknown; a
  dantesync 1.11.0 master is graded on its micro-corrections, a 1.12.0 daily-mode master on its
  nightly schedule + `DANTESYNC_DATE_DAILY_BOUND_MS`) and
  `journal_date_grade(step_bound_us, margin_us, status, micro_bound_ms=None)` (the journal path's
  date grade from the node's own /status, incl. `daily:<us>` / `daily-out` / `daily-unknown`) are
  the python twins of `scripts/lib/dantesync-clock-discipline.sh`'s `clock_discipline_class` /
  `clock_discipline_unlocked` / `date_master_verdict` / `journal_date_grade_from_step`, with
  `date_master_mode_class` / `_date_master_daily_verdict` twinning the bash daily functions of the
  same names. A python consumer grades the date at "now" = `date_now_s()`
  (`DANTESYNC_DATE_NOW_S` or the wall clock), never its own clock read. Pinned by ONE table
  `tests/fixtures/dantesync_clock_discipline_1372.tsv` (see `dantesync-clock-offset-gate.md`). A python
  consumer (a watchdog, a report) grades the 1.9.0 discipline through them, never by re-reading
  `phase_slew_enabled`.

## Consumers (each keeps its env override / explicit arms byte-compatible)

| consumer | how it derives |
|---|---|
| `dantesync-version-gate.sh --fleet` | adds every node not named explicitly; `--present` skips an away traveling box and an unreachable `reach` node (named `SKIPPED`, issue 1412); per-node credential by name (`_dv_ssh_pass` / `_dv_cred_missing`) |
| `dantesync-fleet-upgrade.sh --fleet` | same set + credential; every fleet `ntp-master*` node (strih-snv, strih-pp) is a master-aware verify unless `NTP_MASTER` is given; an audio or venue node is verified against its role's grandmaster (`dantesync_gate_env_for`); a node missing its credential, or an unreachable `reach` node, is SKIPPED |
| `dantesync-clock-alert-watchdog.sh` | `DANTE_CLOCK_{CAM,OBS,LOCAL}_NODES` defaults + `DANTE_CLOCK_FIXED_NODES` (mbc, fohabl; box-up = ssh :22, no mgmt axis) + `DANTE_CLOCK_REACH_NODES` (issue 1412: the PP boxes, skipped while unreachable, otherwise like a fixed node); roster lines carry a 4th ROLE field, and an audio or venue node is graded against its role's grandmaster (each venue grandmaster resolved once per pass) |
| `rig-dev-handover-check.sh` item 12 | `dantesync-version-gate.sh --fleet` |
| `deploy-fleet.sh --venue` + the ci.yml away-venue step (issue 1413) | `dantesync_fleet_venue_camera_rows` + `dantesync_fleet_away_venues`; reach = `dantesync_fleet_present` |
| `camera-box-version-gate.sh --other-venues` (issue 1413, report-only) | same two filters + `dantesync_fleet_present` |

The E2E `[0/8]` version gate is NOT switched to `--fleet`: it gates the run's ACTIVE set, and a
powered-off FOH PC would refuse every E2E. It stays per-run (camera_active_excluding + imag ack).

## Config drift (report-only)

`scripts/dantesync-config-drift.sh` scp's each node's config.json BYTES (Linux
`/etc/dantesync/config.json`, Windows `C:/ProgramData/DanteSync/config.json`) and the pure
`dantesync_fleet.drift()` compares it with its role template in `scripts/dantesync-canonical-config.json`:
every template leaf must match (a missing dict names each missing LEAF), a node-only key is an
`extra key`, a UTF-8/UTF-16 byte-order mark is DRIFT on its own (dantesync ignores such a file — the
PowerShell-write trap). Tokens: `@video_gm` / `@audio_gm`; rules `{"$optional": V}` (cams carry
`ntp_server` on the unit command line), `{"$any": true}` (the master's upstream) and
`{"$ignore": true}` (a retired policy key: present with any value or absent, never graded; any
other argument raises, so `{"$ignore": false}` can never silently mean "not graded"). Exit 0 /
20 DRIFT / 11 UNKNOWN. NOTHING is ever written to a box; a policy flip is a template edit plus a
deliberate roll-out.

**Clock policy since dantesync 1.9.0 (issue 1372):** every role asserts
`system.clock_discipline: {"$optional": "ptp_phase_lock"}` — absent (1.9.0's default) or
`ptp_phase_lock` passes, `legacy` (or any other value) is DRIFT. `system.phase_slew` is
`{"$ignore": true}`: the live nodes still carry `phase_slew.enabled=true` from the pre-1.9.0 rollout,
and ptp_phase_lock ignores it, so dropping the key without the rule would read "extra key" DRIFT on
all 13 nodes. The provisioning writers (`setup-device.sh`, `setup-imag.sh`) write
`"clock_discipline": "ptp_phase_lock"` and no phase_slew; `scripts/dantesync_config_patch.py` writes
the same by default and keeps the old phase_slew flip only behind `--legacy-phase-slew` (a
pre-1.9.0 node).

## Tests

`pytest tests/python/test_dantesync_fleet_1372.py tests/python/test_dantesync_fleet_consumers_1372.py
tests/python/test_dantesync_clock_fleet_1372.py tests/python/test_clock_discipline_1372.py
tests/python/test_dantesync_fleet_pp_1412.py tests/python/test_dantesync_fleet_pp_cams_1413.py` (live-captured configs under
`tests/python/fixtures/dantesync_config_1372/`; PATH `sshpass`/`dantesync` stubs record which password
each target was dialled with). A worktree worker drives bash through pytest subprocesses (the
isolation guard refuses a bare `bash -c`).
