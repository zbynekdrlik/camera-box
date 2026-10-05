---
paths:
  - scripts/lib/ro-window.sh
  - scripts/lib/ro-root.sh
  - scripts/deploy-fleet.sh
  - scripts/dantesync-fleet-upgrade.sh
  - scripts/lib/dantesync-rollback.sh
  - scripts/lib/bkshading-relay-mode.sh
  - scripts/bkshading-deploy-relay.sh
  - scripts/lib/bkshading-deploy-runtime.sh
  - scripts/lib/cam2-painter-ro-persist.sh
  - scripts/lib/ndi-discovery.sh
  - tests/python/ro_window_fakes_1407.py
  - tests/python/test_ro_window_1407.py
  - tests/python/test_ro_window_sites_1407.py
---

# A read-only-root rw window closes through ONE verified emitter, and nothing starts before it (issue 1407)

A cambox (setup-device STEP 18) and a handheld SBC run on a READ-ONLY root. A tool that changes a
file there opens a window: `mount -o remount,rw /`, the change, the ro remount. Issue 1405 found the
failure shape on cam2: a service START inside the window opened a writer on `/`, the ro remount
failed EBUSY, a `2>/dev/null || true` swallowed it, and the box ran on a writable root until its
next reboot. A cambox is never remote-rebooted, so that state persists, and it is the stick-wear
state the read-only appliance exists to prevent. The same hand-written close sat at five more sites.

## The doctrine (every site)

- **Inside the window: only file writes, `systemctl stop`, and `systemctl enable`/`disable`.**
  Never a `start`, `restart`, `enable --now` or a dead-man re-arm (its action starts a unit).
- **Close it with `ro_window_close_cmds`** (`scripts/lib/ro-window.sh`). Never a hand-written
  `mount -o remount,ro / ...`.
- **Start, restart or re-arm only AFTER the close returned.** A close that fails ends the step by
  name and starts NOTHING. A writable root is already the failure; a start would only add writers.
- **No retry loop.** A writer that keeps `/` busy keeps it busy on every retry, so a retry only
  delays the same failure (the design's rejected Approach 2). A `systemctl stop` returns after the
  process exited, so a just-stopped service holds no file.
- **A later write reopens its own window and closes it the same way** (the dantesync self-heal's
  `.bak` copy and the master's date-state delete, below).

## The emitter (`scripts/lib/ro-window.sh`, source-only)

`ro_window_close_cmds TAG BOX CONSEQUENCE HINT` emits remote bash that:
1. defines `ro_root_mount_mode` (`declare -f`, the ONE first-token reading of `scripts/lib/ro-root.sh`);
2. runs `sync -f /` and `mount -o remount,ro /`, keeping each rc and error (never swallowed);
3. READS the root mode (`findmnt -no OPTIONS /`, else `/proc/mounts`). Only `ro` passes: the mount
   exit code is never trusted on its own (a zero-exit remount can leave rw), and `unknown`
   (unreadable) is a failure;
4. on anything but `ro`: FAIL lines on stderr, then `exit 1`:
   - `FAIL: [TAG] BOX's root is NOT read-only ... (findmnt, the ro remount rc + error, sync rc). CONSEQUENCE`
   - the WRITERS: `fuser -vm /` filtered to ACCESS `F` (`ro_window_writers_cmd`). fuser lists PID 1
     and the kernel threads first, so a `| head -n 40` cut hides a high-PID writer. No listing at
     all is named as such, never as "no writer";
   - the deleted-but-open holders: `ro_window_holder_probe_cmd` (`lsof +L1`, else a `/proc`
     exe/maps/fd scan; issue 808, moved here from `bkshading-deploy-runtime.sh`);
   - `FAIL: [TAG] HINT` LAST. `bkshading_relay_mode_apply` relays the last `^FAIL` line, so the HINT
     says what to do.

After a good close, `$_row_opts` holds the options read (the persist success line prints it).

Contract for the arguments: TAG, BOX, CONSEQUENCE and HINT land verbatim in a double-quoted remote
`echo`. No double quote, no backtick. A `$` expands ON THE BOX: the persist lib and the dantesync
prologue read the unit state that way, the relay mode and the ndi apply name the box with
`$(hostname ...)`.

The emitted text, by construction:
- ends every statement with `;` (the `$(...)` trailing-newline gotcha), so it embeds inline, in an
  `if`, or inside a function body;
- names `remount,ro /` exactly ONCE (the command). The bkshading deploy-relay test counts close
  calls by that text, and the FAIL line says "the ro remount rc", never the command;
- never names `systemctl` or `is-active`. Two reasons: the relay-mode rule "every systemctl line
  ends `|| true`", and the issue-1405 rule that EVENT's first `is-active cam2-painter` must be the
  step-5 check. A CONSEQUENCE/HINT that reads unit state uses `systemctl show -p ActiveState --value`.

Two dev1-side pure parsers: `ro_window_holders CLOSE_OUTPUT` (one line naming the writers
`cmd[pid]` and the deleted holders `cmd[pid] path`, read section by section from a failed close's
output) and `ro_window_deleted_holders LSOF_TEXT`.

## The sites

| Site | Inside the window | After the verified close |
|---|---|---|
| `deploy-fleet.sh` camera-box swap | `systemctl stop camera-box`, scp | `systemctl start camera-box` |
| `deploy-fleet.sh` cam2 frame-probe swap (`painter_restore`) | dead-man + painter stop, scp + rename, `systemctl enable` (enable-now) | painter start, dead-man re-arm (#1351 prior state) |
| dantesync upgrade (`dantesync_linux_upgrade_cmd`) | backup, `systemctl stop dantesync`, install | `systemctl restart dantesync` |
| dantesync rollback | stop, `.bak` restore, the master's date-state delete | restart |
| `bkshading-relay-mode.sh` stop / start | `disable` / `enable` | (start only) `systemctl start` |
| `bkshading-deploy-relay.sh` (`remount_ro_checked`) | relay stop, scp + rename | the relay restore (`start` if it ran) |
| `cam2-painter-ro-persist.sh` (issue 1405) | `enable` / `disable` | (enable-now) `systemctl start` |
| `ndi-discovery.sh --cambox-apply` | the config/drop-in removal | `systemctl daemon-reload` |

- **deploy-fleet:** `close_ro_or_fail IP BOX LABEL CONSEQUENCE` runs the close over ssh and records
  `LABEL(root-rw: <holders>)` in FAILED (`LABEL(root-unverified: ssh rc N)` on a transport
  failure), so the final `FLEET NOT FULLY ALIGNED` line names the writer. Every terminal path
  closes the window: a failed stop, a failed scp (the old binary is restarted only on a verified
  ro root), and the normal path.
- **dantesync:** both Linux programs open their window through ONE prologue,
  `_dantesync_linux_rw_window_sh` (`dantesync-rollback.sh`). It reads the root mode, remounts rw on
  a read-only root, and defines `_dantesync_remount_ro` (the close, at most once per open window,
  `_ds_rw_open`) and `_dantesync_reopen_rw` (reopen for a later write; it NEVER returns non-zero, so
  the still-armed ERR trap cannot roll a good upgrade back over a refused remount).
  - The EXIT trap calls the close too.
  - The self-heal (ERR trap) reopens, restores the `.bak`, closes, restarts.
  - The master date-state block carries its own reopen + close, so a downgrade deletes after the
    good start in a second verified window. The "plain program + ONE delete block" invariant
    (test_dantesync_date_state_1372.py) holds, because the block itself carries the window calls.
  - A node whose root is read-write (strih-lx, dev1) opens no window at all.
- **bkshading-deploy-relay:** the close runs in ONE ssh call. rc 1 = the box said the root is not ro
  (its FAIL lines are printed, then one summary line from `ro_window_holders`); any other rc is the
  transport (ssh 255, sshpass 5/6). Either way `finish_box` starts nothing: the relay is left
  STOPPED, and it says so.

Audit, left as they are:
- `rt-kernel-plan.sh` only PRINTS supervisor commands for a reboot-class kernel step.
- `setup-device.sh` `restore_root_mode` and `bkshading-provision-sbc.sh` are provisioning-time and
  already fail loud.

## Tests (Tier-0, no cargo, no rig)

- `tests/python/ro_window_fakes_1407.py`: a stub-only PATH box whose `mount`, `findmnt`, `systemctl`,
  `fuser`, `lsof`, `sync`, `systemd-run` share ONE root state and log `<tool> <args> root=<mode>`.
  A start on an rw root plants a writer (the modelled 1405 mechanism). `starts_on_rw(log) == []` IS
  the invariant.
- `test_ro_window_1407.py`:
  - the emitter itself: clean / busy / lying remount / unreadable / no fuser / no sync / embedding;
  - a static window pin per text site: the text between the rw open and the close (inline, or the
    standalone `_dantesync_remount_ro` / `_ndi_restore_ro` call) holds no start, with the bodies of
    the functions defined inside the window stripped first;
  - **the sweep:** no `remount,ro` followed by `2>/dev/null` / `|| true` / `; true` anywhere under
    `scripts/` outside the lib. The pattern is anchored right after the command (past
    redirections), so a FAIL message that reads unit state with `2>/dev/null || true` later on the
    same line is no hit. Comment lines are skipped.
- `test_ro_window_sites_1407.py`: every site run WHOLE on the fake box. deploy-fleet runs with a fake
  `sshpass` that executes each remote command on the box, with `/usr/local/bin/` mapped into the
  box's own fs, so the real `sha256sum` and the artifact's `--version` work.
- **A harness that RUNS the close needs a `findmnt` stub.** Without one, the real findmnt reads the
  CI runner's own rw root and the close fails. Three Rust deploy-fleet harnesses and the relay-mode
  python test got a stub (`ro,relatime`) for that reason.
- **A fake ssh that pattern-matches commands must RUN the close.** The bkshading deploy-relay fakes
  answered by `case "$cmd" in *findmnt*)` first, and the close text contains `findmnt`, so a busy
  box read as a clean close. The close arm now comes first and runs the text on fake box tools
  (`_box_stubs` in `test_bkshading_relay_gaps_808.py`).
- **A stateful fake root must live in a FILE.** The ndi apply test's stubs are bash functions; the
  close reads `findmnt` and runs `mount` inside `$(...)` subshells, so a variable-held state
  silently never changes.
