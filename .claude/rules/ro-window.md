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
  - scripts/lib/rt-kernel-plan.sh
  - scripts/rt-kernel-upgrade.sh
  - tests/python/ro_window_fakes_1407.py
  - tests/python/test_rig_mode_relay_rc_1407.py
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
`echo`. A `$` expands ON THE BOX: the persist lib and the dantesync prologue read the unit state
that way, the relay mode and the ndi apply name the box with `$(hostname ...)`. A double quote or a
backtick would break or inject into that echo (deploy-relay passes the operator's `--host`), so the
emitter STRIPS those characters (one WARNING on the caller's stderr) and still emits the full
verified close. Review round 2: refusing the close instead left the root writable, and the dev1 side
misread that ("none named", "self-healed").

The emitted text, by construction:
- ends every statement with `;` (the `$(...)` trailing-newline gotcha), so it embeds inline, in an
  `if`, or inside a function body;
- names `remount,ro /` exactly ONCE in its shared part (the command); the FAIL line says "the ro
  remount rc", never the command. A caller's HINT may name it again (the persist hint does). The
  bkshading deploy-relay test counts close CALLS by that text, so the deploy-relay HINT must not;
- never names `systemctl` or `is-active`. Two reasons: the relay-mode rule "every systemctl line
  ends `|| true`", and the issue-1405 rule that EVENT's first `is-active cam2-painter` must be the
  step-5 check. A CONSEQUENCE/HINT that reads unit state uses `systemctl show -p ActiveState --value`.

The writer filter finds the ACCESS field by its own shape (5 characters of `.rcefFm`, the PID
right before it). fuser prints an unresolvable USER as a number, so "the field after the first
number" read `1000 4242 F.... cmd` as PID 1000 and reported "(none: ...)" with a writer present
(review round 1).

Three dev1-side pure parsers:
- `ro_window_holders CLOSE_OUTPUT`: one line naming the writers `cmd[pid]` and the deleted holders
  `cmd[pid] path`, read section by section from a failed close's output;
- `ro_window_deleted_holders LSOF_TEXT`;
- `ro_window_close_failed TEXT`: 0 when a remote program's output holds the close's
  "root is NOT read-only" FAIL line, i.e. the program stopped there and started nothing after it.

## The sites

| Site | Inside the window | After the verified close |
|---|---|---|
| `deploy-fleet.sh` camera-box swap | `systemctl stop camera-box`, scp to the `camera-box.new` sidecar, the sidecar's byte-verify, `chmod` + `mv -f` rename | `systemctl start camera-box` |
| `deploy-fleet.sh` cam2 frame-probe swap (`painter_restore`) | dead-man + painter stop, scp + rename, `systemctl enable` (enable-now) | painter start, dead-man re-arm (#1351 prior state) |
| dantesync upgrade (`dantesync_linux_upgrade_cmd`) | backup, `systemctl stop dantesync`, install | `systemctl restart dantesync` |
| dantesync rollback | stop, `.bak` restore, the master's date-state delete | restart |
| `bkshading-relay-mode.sh` stop / start | `disable` / `enable` | (start only) `systemctl start` |
| `bkshading-deploy-relay.sh` (`remount_ro_checked`) | relay stop, scp + rename | the relay restore (`start` if it ran) |
| `cam2-painter-ro-persist.sh` (issue 1405) | `enable` / `disable` | (enable-now) `systemctl start` |
| `ndi-discovery.sh --cambox-apply` | the config/drop-in removal | `systemctl daemon-reload` |
| `rt-kernel-plan.sh` printed runbook (print-only) | the step's apt / grub work, its rc kept | (nothing started) the work's own FAIL / OK line |

- **deploy-fleet:** `close_ro_or_fail IP BOX LABEL CONSEQUENCE` runs the close over ssh and records
  `LABEL(root-rw: <holders>)` in FAILED (`LABEL(root-unverified: ssh rc N)` on a transport
  failure), so the final `FLEET NOT FULLY ALIGNED` line names the writer. Every terminal path
  closes the window: a failed stop, a failed swap, and the normal path.
  - **The camera-box binary is swapped through a SIDECAR** (design addendum item 2, the frame-probe
    swap's #1351 shape): scp to `/usr/local/bin/camera-box.new`, byte-verify THAT file against the
    artifact (`BINARY_SHA`, hashed once per run), then `chmod 0755 && mv -f .new camera-box && sync`,
    all inside the window. scp writes its target in place, so the old direct copy could leave half a
    binary at the live path when a transfer died. Now a failed copy, a sidecar that does not
    byte-match, or a failed rename removes the sidecar while the root is still writable, closes the
    window the verified way and starts camera-box again on the live binary (a whole build: the old
    one, or the new one if the rename landed before the step failed), with the box FAILED
    (`scp-failed` / `sidecar-sha-mismatch` / `swap-failed`). The final-path byte-verify after the
    start stays (`sha-mismatch`): it proves the rename landed.
  - The frame-probe swap keeps its own, weaker shape for now (rename `|| true`, no sidecar verify):
    its issue-1351 test pins "byte-verify reads the final path, never the sidecar", and it is bound
    to the #892 painter restore. Giving it the same sidecar verify is a Design-question to the main
    (issue 1407 comment 5996455127), not a silent change.
  - **The window spans several separate ssh calls from dev1** (rw + stop, scp, close). Two markers
    cover it (review rounds 1-2):
    - `OPEN_WINDOW` (`ip|box|label`) is set just BEFORE the rw remount. `close_ro_or_fail` clears it
      only once the BOX answered the close (rc 0 or 1). An interrupted or failed close ssh keeps it.
    - `PENDING_START` (`label|box`) lives from the window open until the service start was
      attempted (or the box's flow ended). The painter gets one only for an enable-now restore:
      the advice "start it by hand" must never reach a deliberately dark (#892 EVENT) painter.
    - The EXIT trap is also reached on INT/TERM (`exit 130` / `exit 143`, e.g. a CI cancel). An open
      window is closed once more, verified, under `setsid -w`, so a second Ctrl-C cannot reach it
      (sshpass forwards SIGINT to ssh). Nothing is started there: the swap may be unfinished (the
      live binary is the old or the new whole build; a partial copy can only be the `.new` sidecar,
      which the next deploy overwrites).
    - A pending start is named: "the service on <box> may be STOPPED". It says "may", because the
      signal can land before the stop ran.
    - Before this, a cancel mid-swap left the cambox writable with camera-box stopped, silently.
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
  - **The self-heal says whether it restored anything.** It copies the `.bak` back inside its own
    window; when that copy fails it prints `SELF-HEAL FAILED ... restore by hand`, never
    "restored".
  - **The orchestrator never calls a failed close a self-heal.** `upgrade_node` checks REMOTE_OUT
    with `ro_window_close_failed`. If the program stopped at a failed close, it reports
    "NOT self-healed; dantesync may be LEFT STOPPED", because nothing was started after the close.
  - The ERR self-heal stays ARMED through the final `dantesync --version` on purpose: a binary that
    cannot print its version is rolled back, and that is what the orchestrator then reports.
    `trap - ERR` follows right after it (review round 2), so the master's date-state delete that
    comes next can never roll the master back to its `.bak` after its date file is gone, the state
    the issue-1372 delete-last order exists to avoid.
  - A `SELF-HEAL FAILED` program is reported as NOT self-healed too.
- **bkshading-deploy-relay:** the close runs in ONE ssh call. rc 1 = the box said the root is not ro
  (its FAIL lines are printed, then one summary line from `ro_window_holders`); any other rc is the
  transport (ssh 255, sshpass 5/6). Either way `finish_box` starts nothing: the relay is left
  STOPPED, and it says so.

**Why the cambox-only sites force the root back read-only (review round 1, a decision, not a
gap).** deploy-fleet and the relay mode never read the root mode before they open the window; the
dantesync programs, deploy-relay and the ndi apply do, because they also reach boxes whose root is
read-write by design (strih-lx, dev1, an SBC before its first read-only reboot). deploy-fleet and
the relay mode reach only camboxes, which run read-only (setup-device STEP 18), and they always
remounted ro after the window.
- A cambox found writable is the stuck state this ticket exists to surface. If a writer keeps it
  writable, the step now fails loud naming that writer and starts nothing. It used to start the
  service and leave the box writable, silently.
- The main's design names this trade-off ("a site that used to 'succeed' on a rw root now fails
  loudly. That is the point").
- On `rig-mode.sh event` this means a stuck-writable relay box gets no shading relay until the
  supervisor's live put-back. That put-back: `fuser -vm /`, stop the writer, `mount -o remount,ro /`
  until `findmnt` reads ro, then re-run.
- `bkshading_relay_mode_apply` returns non-zero when any box failed (issue 1311 chose that; its
  header used to say "always returns 0").

### A failed relay step never stops a rig-mode switch half-way (design addendum item 1, the issue-868 pattern)

Before issue 1407 a failed relay-box ro close was swallowed, so nothing depended on the apply's exit
code. Once 1407 made it fail loud, `rig-mode.sh` still called the apply bare under
`set -euo pipefail`. EVENT then aborted BEFORE `toggle_burn event`: the measurement burn stayed ON
going into a production (the issue-868 class). TEST aborted before the painter steps.

- **Both modes record it:** `local _relay_rc=0` + `bkshading_relay_mode_apply <mode> ... || _relay_rc=$?`
  (the call line itself is unchanged up to the `||`, so the 1311/1371 anchors still find it), then
  `bkshading_relay_mode_warn_continue <mode> "$_relay_rc"` prints ONE loud named WARNING, and every
  remaining step runs.
- **It is folded into the exit:**
  - EVENT's PASS branch needs `_relay_rc` 0 too. `bkshading_relay_mode_result event` prints the
    relay RESULT line before the 868 / contract lines (the contract line is now an `elif` on
    `EVENT_ASSERT_PASS`, so a relay-only failure never reads as a contract failure), then `exit 1`.
  - TEST checks it after the ACHIEVED lines: `bkshading_relay_mode_result test` + `exit 1`, and the
    "WHOLE CHAIN verified" RESULT is never printed for that run.
- **The boxes are named at the end:** the apply sets `BKSHADING_RELAY_MODE_FAILED`
  (`label (ip) [writers: ...]`, `, `-joined) and `BKSHADING_RELAY_MODE_FAILED_BOXES` (`label (ip)`)
  in the CALLER's shell (it is never run in a subshell). The WARNING and RESULT lines read the first.
- **The owner's EVENT Discord confirmation says it:** `bkshading_relay_mode_discord_note` puts one
  plain-Slovak ⚠️ line ON TOP of the issue-724 message, naming the boxes only, never process names,
  and claiming nothing the contract below it decides. Without it the phone would read a clean
  confirmation while the run exits 1.
- **ONE writer for every EVENT Discord note:** `event_mode_discord_note_add MSG LINE top|end` in
  `scripts/lib/event-mode-discord-confirm.sh` (top = a temp file moved over only when complete, else
  an append; a missing file adds nothing; always 0). The issue-1371 exposure-restore note and the
  relay note both call it, and both libs source that lib at load time. A new EVENT note calls it
  too, never a third copy of the prepend (review round 1 found the relay note copying it). The
  1371 `Rig` harness copies a fixed list of sibling libs into a scratch tree, so a new top-level
  `.` of a sibling in `camera-test-settings.sh` must join that list.
- **Say only what is not confirmed:** a failed box can still run its relay (the start runs after a
  failed enable) or still be armed, so the RESULT reads "may not be running / not armed for a
  reboot", never "is NOT running".
- All four helpers are reports: nothing for rc 0, always return 0.
- **Open edge (Design-question to the main, comment 5996455127):** when the failed relay box IS cam2
  with its root stuck read-write, TEST still runs the painter steps as decided; the issue-1405
  handoff then fails on the same writer and cam2 ends dark with no dead-man. The recommended
  alternative stops TEST before the painter launch in that one case. A test pins the decided
  behaviour (`test_test_failed_relay_on_the_painter_box_*`).
- **Any NEW step in a rig-mode switch whose failure must not strand the rig gets the same shape:**
  record the rc, warn by name, continue, fold at the end. A bare call is right only for a step that
  must stop the switch (a hard precondition before any mutation, like the #789 TEST-entry gate).
- **Test it by RUNNING the mode bodies**, never only by text: `tests/python/test_rig_mode_relay_rc_1407.py`
  sources the flow section of rig-mode.sh (it sits after the source guard, so a plain source never
  defines `do_event` / `do_test`), stubs every other function to a step logger, and keeps the whole
  relay chain real against fake read-only-root boxes behind a fake `sshpass` (TEST-NET addresses).
  It asserts the step order after a failed relay step, the exit code, the named WARNING/RESULT and
  the Discord top line, plus the healthy controls. Its `_KEEP` pattern lists the functions that stay
  REAL: when kept code starts calling another lib's helper (the shared `event_mode_discord_note_add`),
  that helper must join `_KEEP`, or the stub silently swallows its effect and the test reads
  "nothing written". A test can run one more override through `_run(..., extra=...)` (the combo
  test fails only the first `cam_ssh`, the issue-868 cam-side restore).

Audit, left as they are:
- `setup-device.sh` `restore_root_mode` and `bkshading-provision-sbc.sh` are provisioning-time and
  already fail loud.

### rt-kernel-plan.sh prints the shared close (design addendum item 3)

`rt_kernel_step_command` only PRINTS the supervisor's commands for the reboot-class kernel step,
but those commands ended `&& mount -o remount,ro /`: never verified, and skipped once an earlier
`&&` step failed. Every mutating step (`install-lowlatency`, `grub-pin:saved`, `safe-grub-regen`,
both `purge-superseded-generic` forms, `blocked:no-rt-candidate`) now prints ONE program from
`_rt_window_program STEP WORK [WHY]`:
- `bash -s <<'RT_KERNEL_STEP'` ... `RT_KERNEL_STEP`, so the close's `exit 1` ends that child shell,
  never the root session the supervisor pasted it into;
- WORK as a function `_rt_work() { WORK; }`, called `_rt_work </dev/null`. **That heredoc is the
  child shell's STDIN**: a WORK that reads stdin (a dpkg conffile prompt, a debconf question) ate
  the rest of the program, the verified close included, and left the root writable with exit 0
  (review round 1, reproduced on the fake box). The purge steps also carry
  `DEBIAN_FRONTEND=noninteractive`. Any future printed `bash -s` program must keep its commands off
  the heredoc's stdin the same way;
- a WORK holding an unreplaced `<...>` placeholder (`<OLD_VER>`, `<Advanced...>`) gets a guard first:
  `declare -f _rt_work | grep -q '<[A-Za-z][^>]*>'` refuses (exit 2) before the root is touched. It
  reads the function as the supervisor edited it, so a filled-in step runs;
- `if mount -o remount,rw /; then _rt_work </dev/null || _rt_rc=$?; else _rt_rc=$?; fi`;
- the shared `ro_window_close_cmds` text (tag `issue 899`, the box names itself via `$(hostname)`),
  run whatever WORK did;
- then WORK's own `FAIL: [issue 899] the <step> step failed (rc=N)` + `exit N`, else an OK line.
A note token keeps its `# SUPERVISOR:` / `# BLOCKED:` first line and the program follows it (the
Rust `starts_with('#')` pin). A placeholder the supervisor edits (`<OLD_VER>`, `<Advanced...>`)
sits inside the program. The driver (`rt-kernel-upgrade.sh --commands`) prints a multi-line step
BELOW its token, never `token  bash -s ...` on one line (copying that line would run
`install-lowlatency bash -s`); a one-line step keeps the `%-28s` token column. `tests/python/test_ro_window_sites_1407.py` runs every printed step on the
fake box (with logged apt-get / update-grub / grub-set-default / mkdir stubs): rw open, work on rw,
verified close, root ro (the placeholder steps filled in first); a failing work still closes; a
stdin-reading work still closes; an unfilled placeholder refuses before any mount; a busy root fails
loud naming the writer; a pasted step never ends the session; the driver prints each step below its
token.

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
  - **the sweep:** no ro remount anywhere under `scripts/` outside the lib whose failure is
    discarded. It is structural, per ro remount CALL (`remount,ro` / `ro,remount`, quoted, with
    `,opts`, with or without ` /`). Its redirections must not send the error to `/dev/null`. What
    follows must not just go on: `|| true`, `|| :`, `|| echo/printf/warn/log/info/err/logger`,
    `|| return 0`, `|| exit 0`, `|| continue`, `|| break`, `; true`, `; :`, a retry loop's
    `&& break`, and a `|| { ... }` group that does not end the step with the failure. The group is
    read to its MATCHING brace, across lines, quote-, comment- and `${...}`-aware (a comment's
    text is not code: "fail" in it makes no group loud) (an unclosed one is no
    hit). It is loud only with a non-zero literal or a named variable as the exit/return code
    (`exit "$rc"` after `rc=$?`), `fail`/`die`, or the mount's own `$?` as its FIRST command.
    Inside a group a bare `return`/`exit`, or `$?` after another command, hands back THAT
    command's status, so it goes on. A loud `|| fail ...` /
    `|| { ...; exit 1; }` is fine, and so are `|| return` / `|| return 1` (they pass the failure on).
    So is a FAIL message that reads unit state with `2>/dev/null || true` later on the same line
    (the check is anchored right after the command).
    Comment lines are skipped; a continued line is one statement.
- `test_ro_window_sites_1407.py`: every site run WHOLE on the fake box. deploy-fleet runs with a fake
  `sshpass` that executes each remote command on the box, with `/usr/local/bin/` mapped into the
  box's own fs, so the real `sha256sum` and the artifact's `--version` work. Its `mv`, `rm`, `chmod`
  and `sha256sum` are LOGGED wrappers around the real tools (the fs prefix un-mapped in the log), and
  the write tools refuse a write under the box fs while the root reads ro, so the sidecar swap's
  order (scp `.new`, verify `.new`, `mv -f`, close, start) is read from the log. The fake scp has
  `FAKE_SCP_PARTIAL` (half the bytes, exit 1) and `FAKE_SCP_CORRUPT` (wrong bytes, exit 0).
- **The Rust deploy-fleet harness runs the remote text on the CI HOST itself**
  (`tests/harness_deploy_fleet.rs`), so it stubs `chmod` / `mv` / `rm` to a no-op for any
  `/usr/local/bin/` argument (pass-through otherwise): the sidecar rename must never touch the
  host's own `/usr/local/bin`.
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
