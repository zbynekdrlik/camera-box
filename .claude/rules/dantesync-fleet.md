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

- **Cameras:** the camera_resolve walk (cam1, cam2, … to the first unknown name — the ndi-discovery
  walk; never a literal range). ALL known cameras, not `CAMERA_ACTIVE_SET`: a camera retired from
  measurement still runs dantesync.
- **The table `DANTESYNC_FLEET`** (env-overridable): `name|addr|os|role|homegate|user|credvar`.
  - `addr`: a literal, `obs:<name>` (resolved through `obs_fleet_host` — a `retired` obs-fleet box
    drops out, so imag returns with its obs-fleet flip), or `local` (dev1).
  - `role`: `video` | `audio` | `ntp-master` — picks the canonical config template AND the
    grandmaster the node must lock to (`dantesync_fleet_role_gm_host`).
  - `homegate`: `always` | `obsfleet` (traveling resolume: read only while `obs_fleet_is_home`) |
    `local`.
  - `user`: `-` = the consumer's default login; fohabl is `master`.
  - `credvar`: the NAME of the env var holding the node's own ssh password (fohabl:
    `DANTESYNC_FOHABL_SSH_PASS`). NEVER a value in a committed file. The value comes from the env or
    the dev1-local `~/.config/camera-box/dantesync-fleet.env` (0600, `KEY=VALUE`, PARSED not sourced,
    only row-named keys read). A missing value = that node reads UNKNOWN / is skipped — never dialled
    with an empty or a foreign password.
- **lv1 (10.77.7.100) is deliberately absent** (owner ROZHODNUTÉ 25.9.2026: an operational PC, on the
  network temporarily).
- **The audio grandmaster** is `DANTESYNC_AUDIO_GM_HOST` (default 10.77.7.106, Audinate
  00:1d:c1:1a:44:30 — the same clock as video-clock.lan, #1367 comment 5832526338). No DNS name exists
  for it yet; the address lives in this ONE variable.
- **Python twin:** `scripts/dantesync_fleet.py rows` prints byte-identical rows (parity pinned).
- **Clock-discipline twin (issue 1372):** `dantesync_fleet.classify_clock_discipline(status)`,
  `clock_discipline_unlocked(status)`, `date_master_micro_capable(status)` and
  `date_master_verdict(status, margin_us, micro_bound_ms=None)` (none/ok/out/paused/unknown; a
  dantesync 1.11.0 master is graded on its micro-corrections) are the python
  twins of `scripts/lib/dantesync-clock-discipline.sh`'s `clock_discipline_class` /
  `clock_discipline_unlocked` / `date_master_verdict`, pinned by ONE table
  `tests/fixtures/dantesync_clock_discipline_1372.tsv` (see `dantesync-clock-offset-gate.md`). A python
  consumer (a watchdog, a report) grades the 1.9.0 discipline through them, never by re-reading
  `phase_slew_enabled`.

## Consumers (each keeps its env override / explicit arms byte-compatible)

| consumer | how it derives |
|---|---|
| `dantesync-version-gate.sh --fleet` | adds every node not named explicitly; `--present` skips an away traveling box (named `SKIPPED`); per-node credential by name (`_dv_ssh_pass` / `_dv_cred_missing`) |
| `dantesync-fleet-upgrade.sh --fleet` | same set + credential; the fleet `ntp-master` node is the master-aware verify unless `NTP_MASTER` is given; an audio node is verified against the audio grandmaster (`dantesync_gate_env_for`); a node missing its credential is SKIPPED |
| `dantesync-clock-alert-watchdog.sh` | `DANTE_CLOCK_{CAM,OBS,LOCAL}_NODES` defaults + the new `DANTE_CLOCK_FIXED_NODES` (mbc, fohabl; box-up = ssh :22, no mgmt axis); roster lines carry a 4th ROLE field, and an audio node is graded against the audio grandmaster |
| `rig-dev-handover-check.sh` item 12 | `dantesync-version-gate.sh --fleet` |

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

## Tier-0

`pytest tests/python/test_dantesync_fleet_1372.py tests/python/test_dantesync_fleet_consumers_1372.py
tests/python/test_dantesync_clock_fleet_1372.py tests/python/test_clock_discipline_1372.py` (live-captured configs under
`tests/python/fixtures/dantesync_config_1372/`; PATH `sshpass`/`dantesync` stubs record which password
each target was dialled with). A worktree worker drives bash through pytest subprocesses (the
isolation guard refuses a bare `bash -c`).
