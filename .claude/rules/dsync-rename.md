---
paths:
  - "scripts/lib/dsync-swap.sh"
  - "scripts/lib/dsync-release.sh"
  - "scripts/lib/dantesync-install.sh"
  - "scripts/lib/dantesync-rollback.sh"
  - "scripts/lib/dantesync-tray-upgrade.sh"
  - "scripts/dantesync-fleet-upgrade.sh"
  - "tests/python/test_dsync_swap_1487.py"
---

# dantesync became dsync: the fleet swap (issue 1487, dsync 1.25.0, owner: no PC carries a mix)

dsync (`zbynekdrlik/dsync`, the source of the dstack project's clock) renamed everything in 1.25.0:
binary `dsync`, unit `dsync.service`, Windows service `dsync`, `C:\Program Files\dsync`, pipe
`\\.\pipe\dsync`, state `/etc/dsync` (`C:\ProgramData\dsync`), tray `dsync-tray`, the assets
`dsync-*`. `/status` stays on :8898 and `config.json` keys are unchanged.

## What is called what

- **Renamed:** every name that exists on a box or in a release.
- **Kept, deliberately (churn, no value):** every `dantesync_*` function, `DANTESYNC_*` variable
  (`DANTESYNC_VERSION_PIN` is named in dsync's own rollout doc), `STRIH_DANTESYNC_*`, the
  `dantesync-*.sh` script names, the fleet credential file `dantesync-fleet.env`, camera-box's own
  `dantesync-dscp` unit and `dantesync_dscp` nft table, the `dantesync-clock-alert-watchdog` unit,
  and the av-restart-matrix kind `dantesync`.
- **The old names stay ONLY as the legacy side of the swap:** `scripts/lib/dsync-swap.sh`,
  `dantesync_install_retire_legacy`, `strih_session_apps_retire_legacy_tray`, the fallback version
  reads, and the drift check's legacy config path.

## The pin

`DANTESYNC_VERSION_PIN` in the version gate, plus the four asset shas in
`scripts/lib/dsync-release.sh`: bump them together (a test pins them equal). A release asset replaced
after the pin is refused even with a matching `.sha256`. setup-imag now installs the pin too (it
took `releases/latest`).

## The state dir: rename, never copy

dsync resolves it once at start: `/etc/dsync` when it exists, else `/etc/dantesync` IN PLACE, else it
creates `/etc/dsync`. So `mv` is the whole migration. A leftover `/etc/dsync` keeps what it has and
takes only what it lacks (a near-empty new dir would hide the old config). The date master's
`date-offset.json` has the same format since 1.15 and is restored when under a day old, so the swap's
restart does not step the fleet date: followers hold, then re-join the same seq.

## The swap on a live node (the upgrader, `dantesync_linux_upgrade_cmd`)

1. Verify the sha (dev1 against the pin, the box against the staged `.sha256`) before anything stops.
2. Inside the rw window: stop + disable dantesync, rename the state dir, derive `dsync.service` and
   its drop-ins from the node's OWN unit (`sed`, so its arguments stay), install the binary, enable.
3. The verified `ro_window_close_cmds` close, daemon-reload, restart dsync, read the version.
4. The orchestrator verifies the node. Only then does `finalize_node` remove the old binary, unit and
   drop-ins in a second verified window. It also runs on a node already on the target, so an
   interrupted roll finishes on the next one, and a clean node opens no window.

Until step 4 the way back is whole: a failed start self-heals by the reverse swap, a failed verify
rolls back the same way (`dsync_linux_swap_back_sh`). The legacy state is read ON THE BOX:
`dantesync.service` present and `dsync.service` absent.

The derived unit is byte-identical to what `strih_dantesync_unit_text` renders (test-pinned), or the
next genlock deploy would read the unit as changed and restart the date master.

Windows mirrors it: the old service is stopped + Disabled, the state dir renamed (a rename that
fails is a `STATE-WARNING`, dsync then uses it in place), `dsync` created from the old service's own
binary path, and the finalize deletes the old service, install dir and autostart entries and points
the HKLM Run `DsyncTray` entry at the new tray. Failing the rollback path, a dsync tray stays
installed and shows no clock until the service is back on dsync (the pipe name changed).

## Provisioning

`dantesync_install_retire_legacy` (setup-strih through `dantesync_install`, setup-device, setup-imag):
the verified binary first, then the old service out and the state dir renamed, on a writable root.
setup-device stays enable-only, except that it starts dsync when it stopped a running dantesync
(`DSYNC_RETIRED_ACTIVE`): a live box must not lose its clock until a reboot. Step 16d removes the Linux
tray's old unit, binary and release record.

## Scope

`DSYNC_SELF_UPGRADED_NODES` (companion-snv mbc fohabl iem-snv alex-nb): dsync upgrades these itself.
They stay rows of the fleet table, so the clock watchdog and the drift check keep grading them on
:8898, but the upgrader never rolls them (named or not), and the version gate never pins them. They may
keep their state dir in place, so the drift check reads `/etc/dsync`, then `/etc/dantesync`.

**Their version is still READ and NAMED (9.11.2026, mbc on 1.27.3 vs the pin 1.28.0 silenced the
stream-snv program ~20 min with the version printed nowhere).** `dantesync-version-gate.sh --fleet`
reads each one with the same `dsync --version` reader and prints it in its own section
`-- nodes dsync upgrades itself (issue 1487, report-only, never blocks) --`, graded by the pure
`dantesync_self_upgraded_verdict` on `release_version_order_word` (semver, never text): below the pin
= `SELF-BEHIND (version=V pin=P; ...)` + a `!! DSYNC-SELF-UPGRADED WARNING` stderr line, at/above =
`OK`, unread = `SELF-UNKNOWN` + a NOTE. The gate exit NEVER moves (our gate must not follow dsync's
deploy cadence) and nothing rolls them. An unreachable `reach` one (alex-nb) is `SKIPPED (unreachable`
like a rolled node; one named on the command line is graded on the pin as before. The handover check
(item `dantesync`) turns the rows into a Slovak `pozor:` note on its line and the summary
(`rig-dev-handover-check.md`). Tests: `tests/python/test_dsync_self_upgraded_version_1487.py`.

**resolume-snv** moved on 2.11.2026 by dsync's own `install.ps1`, once. That is not a path: only
camera-box moves rig boxes. It reads SAME at the new location, so the upgrader only checks its tray
and runs a finalize with nothing to do, and the gates read `C:\Program Files\dsync`.

## The roll

Since 1.23 `is_locked` and LOCK/NANO need the phase within 50 us. A first start has no `drift.json`:
PROD -> LOCK 30-40 s, then 30-55 s to `[41]`, so ~95 s. `GATE_WAIT_TRIES` is 20 (120 s), measured from
dsync's rollout doc, not a loosened gate. A read-only cambox can never keep a `drift.json`
or `config.json.last-good`, so every restart there is a cold lock and logs the failed write.

The canary default skips an NTP master and the rest puts the masters last (dsync's
`docs/release-rollout.md`). A mixed fleet is safe during the roll: a 1.20 reader takes the 120-byte
DSYX reply in its 256-byte buffer, and both singleton locks mean a dantesync and a dsync never run
together.

`/status` 1.20 -> 1.27 is additive: `config_state`, `freq_seed_ppm`, `freq_seed_source`,
`ptp_interface`, `ptp_interface_configured`, `ptp_interface_ip`, `ptp_phase_boot_reseed_ppm`,
`ptp_phase_engaged_on_ppm`, `state_dir`. The date-master fields the gates grade are unchanged.
`/status.state_dir` shows which directory a node really runs on.

## Tests

`tests/python/test_dsync_swap_1487.py` RUNS the emitted Linux programs on a fake root (paths mapped,
stub `systemctl`/`mount`/`findmnt` logging the root mode), and pins the Windows text.
