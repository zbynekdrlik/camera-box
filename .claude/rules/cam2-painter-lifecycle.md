---
paths:
  - "scripts/rig-mode.sh"
  - "scripts/lib/cam2-painter-handoff.sh"
  - "scripts/lib/cam2-painter-ro-persist.sh"
  - "scripts/lib/cam2-painter-restore-verify.sh"
  - "scripts/lib/cam2-painter-restore-retry.sh"
  - "scripts/lib/cam2-painter-restore-recheck.sh"
  - "scripts/lib/cam2-painter-deadman.sh"
  - "systemd/cam2-painter.service"
  - "tests/harness_cam2_painter_steady_state_handoff.rs"
  - "tests/harness_cam2_painter_ro_persist_1175.rs"
  - "tests/python/test_cam2_painter_ro_verify_1405.py"
  - "tests/harness_cam2_painter_coordination.rs"
  - "tests/harness_cam2_painter_restore_recheck_1126.rs"
  - "scripts/lib/rig-test-ledger.sh"
  - "tests/harness_rig_test_ledger_723.rs"
  - "tests/python/test_rig_test_ledger_register_1382.py"
---

# cam2 painter lifecycle — WHO paints /dev/fb0 (+ emits the QPSK marker) in each state (#1008/#937)

Two painters exist on cam2, and getting "who is painting now" wrong is how the rig goes silently
dark. Both are `frame-probe --paint-only --dual-qr` writing `/dev/fb0` (KMS/DRM in practice) and,
since #984, emitting the QPSK marker default-ON.

- **PERMANENT `cam2-painter.service`** (#863) — the DURABLE steady-state painter. `Restart=always`,
  `WantedBy=multi-user.target`; when `enabled` it survives reboot; `--marker-log
  /run/rig-qpsk-markers.csv` in its ExecStart (#1008) so it writes the SAME growing marker CSV the
  offline verdict + the "must-stay-alive" liveness check read. **This is what the standing
  rig-test-mode-must-stay-alive rule requires** — supervised, self-healing, boot-persistent.
- **TRANSIENT painter** launched by `rig-mode.sh test`'s `painter_launch_remote` — a `nohup
  frame-probe --duration-secs N` used ONLY for the at-mode-set CHAIN VERIFICATION window
  (freshly-resolved marker device #725, marker-log growth #431, optical non-black #901). It is
  unsupervised and MUST NOT be left as steady state (it was — the #1008/#937 bug: a 2h nohup that
  expired silently).

## The lifecycle (do not break this ordering)

- **`rig-mode.sh test`**: (1) `painter_launch_remote` STOPS the permanent unit first (#440 — two
  painters racing fb0 make the displayed QR alternate run_ids, desyncing the marker), launches the
  transient painter, verifies the whole chain. (2) At the END, `do_test` calls
  `cam2_painter_steady_state_handoff_cmds` (`scripts/lib/cam2-painter-handoff.sh`): disarm the
  dead-man, stop the transient via its pidfile, `systemctl enable cam2-painter.service` inside the
  remount-rw window,
  verify the root reads read-only again, `systemctl start` it (issue 1405, below), FAIL LOUD unless
  it is active + genuinely painting (presenter-aware #464) + marker CSV growing. **Steady state ends
  on the PERMANENT unit, never the nohup.**
- **`rig-mode.sh event`**: `painter_stop_remote` STOPS **and DISABLES** the permanent unit (#892 —
  EVENT must never leave a QR that can return via a restart or a reboot onto the LIVE broadcast).
  So `test` must ENABLE it (not just `start`) to re-arm it after any prior EVENT cycle.
- **`recording-e2e.sh` measurement**: STOPS the permanent unit (arms the #872 on-box dead-man so a
  SIGKILLed run self-heals), runs its OWN measurement painter, and `cleanup()` restarts +
  `cam2_painter_restore_verify_cmds` + disarms the dead-man. This is now the ONLY time the unit
  yields fb0. The handoff above composes with it cleanly.
- **dev1-side `optical-chain-alert-watchdog.sh`** (#860) pages when a painter is EXPECTED (pidfile
  present OR `cam2-painter.service` enabled) but strih's program reads black — so an enabled unit
  makes `painter_expected` correctly true in TEST mode.

## Gotchas

- The handoff builder embeds `$(audio_marker_emission_check_cmds ...)` inside its heredoc — mind
  the #744/#746 trailing-newline-strip rule (keep a literal line after the `$(...)`).
- Adding logic to `do_test` uses the #675 sourced-lib pattern (a new `_cmds` builder + one
  `cam_ssh "$(...)"` line) so no `tests/rig_mode.rs` static anchor is touched. Always re-run the
  FULL `cargo test` suite after any `rig-mode.sh` edit (anchor-collision class).
- `--marker-log` may be added to the base unit ExecStart WITHOUT tripping the provisioning test's
  `!out.contains("--audio-marker")` assertion (marker-log ≠ audio-marker; the marker stays
  default-ON, flag-free).
- **`--wall-clock` on BOTH painters (#1312).** Both the permanent unit's ExecStart (setup-device.sh)
  and the transient `painter_launch_remote` (rig-mode.sh) pass `--wall-clock`, so the QPSK marker
  CSV's `emit_ts_ns` is stamped on `CLOCK_REALTIME` (the DanteSync wall clock) not the painter's
  monotonic `start.elapsed()`. This is what lets the dev1 `avlatency` handover check
  (`measurement-chain-latency.sh`) pair the markers against dev1's wall-clock `mbc` onsets instead
  of reading the monotonic-emit UNKNOWN forever. Place it AFTER `--paint-fps`/`--duration-secs` so
  the pinned contiguous `--paint-only --dual-qr --qr-size N --duration-secs N` vernier anchor
  (`tests/rig_mode.rs::test_mode_launches_pinned_painter`) stays intact. It is a SAFE no-op for the
  A/V verdict path (`av_sync_recording.rs` pairs by fid, ignores `emit_ts`) — the E2E burn painter
  in `recording-e2e.sh` already carries it. Both changes take effect only after a cam2 re-provision
  (or a remount-rw unit edit + `daemon-reload` + painter restart) — a supervisor rig step.

## The enable-state window must end READ-ONLY before the painter starts (issue 1405)

cam2's root is read-only (setup-device STEP 18). Changing the unit's persistent enable-state needs a
`mount -o remount,rw /` window. That window lives in ONE emitter,
`cam2_painter_persist_state_cmds` (`scripts/lib/cam2-painter-ro-persist.sh`, issue 1175), used by
the TEST handoff (`enable-now`) and the EVENT disable (`disable`).

- **Never start the painter inside the window.** The old `enable-now` ran `systemctl enable --now`
  there. The following ro remount failed EBUSY, its `|| true` hid that, and cam2 ran on a
  read-WRITE root until the next reboot. Live 4.10.2026: the last `r/w` remount at 10:36:54 had no
  `ro` after it, and the painter became active that same second. Only `systemctl enable` /
  `disable` runs in the window now.
  - That the START opened the blocking writer is INFERRED from that timing, not proven:
    `cam2-painter.service` writes only `/run`, and the one writer seen live (a second
    systemd-journald) is not explained by this unit. The live put-back names the real holder.
- **The root mode decides, never the remount's exit code.** After the ro remount the emitter reads
  `findmnt -no OPTIONS /` (with the `/proc/mounts` fallback) through the shared ro-root canon's
  `ro_root_mount_mode`. Its definition is emitted INTO the remote text (`declare -f`, lazy-sourced
  from `scripts/lib/ro-root.sh`).
  - `rw` or `unknown` FAILS LOUD (exit 1), and the emitter never starts the painter. `unknown` is
    never assumed `ro`. The message names the findmnt reading, the mount error and the current
    `is-enabled` and `is-active` states (it reads whether the painter runs, never asserts it: an
    earlier dead-man fire can have started it).
  - **It names the HOLDERS, two ways.** First, the `fuser -vm /` lines whose ACCESS field carries
    `F` (a file open for writing). fuser lists PID 1 and the kernel threads first, so a `head -n
    40` cut a high-PID writer off (review round 1). Second, the deleted-but-open files from the
    repo's ONE probe for that other EBUSY cause, `bkshading_deploy_ro_holder_probe_cmd` (issue 808,
    `lsof +L1`, else the /proc fd scan). It is emitted into the failure branch, the same two-part
    naming as `bkshading-deploy-relay.sh` `remount_ro_checked`.
- **Order inside the emitter:** remount rw, change, remount ro, ro verify, the change's rc check, the
  `is-enabled` read-back, and (enable-now only) `systemctl start`. A failed start after a good
  enable is its own named `[#1405]` failure.
  - There is no retry loop: a writer that keeps `/` busy keeps it busy on every retry.
  - The disable mode never names a start. The `tests/rig_mode.rs` #892 check forbids
    `systemctl start cam2-painter.service` anywhere in the EVENT text.
- **EVENT disarms the dead-man FIRST.** `rig-mode.sh test` arms the cam2-painter dead-man as a
  standing net, and its action starts the painter whenever no frame-probe runs, even for a disabled
  unit. `painter_stop_remote` runs under `set -e`, so a disable step that fails loud ends the
  cam-side EVENT script.
  - With the old order (disable at 2.5, disarm at 2.6), a root stuck rw left the dead-man armed,
    and the QR came back on air within ~5 min (review round 1, 🔴).
  - Now the disarm is step 2.5 and the stop+disable step 2.6. No failure in the disable step can
    leave a resurrection timer behind, and the timer can no longer fire between the stop and the
    disarm. The issue-1351 frame-probe swap uses the same order.
  - The 1075 harness pins the order: the disarm comes before the stop and before the disable call.
  - A failed disable still ends the script before the camera-box restart AND before the issue-1176
    fb0 blank (step 5), so the cam2 screen keeps whatever fb0 last held until someone blanks it.
    That is the same place every other #1175 failure stops it, and do_event still runs the
    burn-clear and exits non-zero (#868). So a cam2 that is stuck read-write must be put back to ro
    BEFORE the next `rig-mode.sh event`. Never reboot a cambox remotely for it.
- **The TEST handoff disarms the dead-man too (step H1b), before the transient painter stop.**
  `rig-mode.sh test` arms it only after a GOOD handoff, and only EVENT disarms it otherwise. On a
  second TEST whose window cannot close read-only, the first TEST's dead-man would start the
  painter on the writable root within ~5 min, after the FAIL line said the handoff does not start
  it (review round 2). It could also fire between the transient stop and the start. A failed
  handoff therefore leaves cam2 disarmed, normally dark, and its FAIL line reads `is-active`
  rather than asserting it (a fire just before the disarm can still have started the painter);
  the next good `rig-mode.sh test` re-arms it.
- **Every emitted statement ends with `;`.** The callers embed the text through `$(...)`, which
  strips its trailing newline (the CLAUDE.md #744/#746 gotcha).
- **Test by running the text.** `tests/python/test_cam2_painter_ro_verify_1405.py` runs it with
  stateful fakes on a stub-only PATH:
  - `mount`, `systemctl`, `findmnt`, `fuser` and `lsof` share one fake root and log each call with
    the root mode at that moment;
  - a start on a rw root plants a writer that fails the next ro remount (the MODELLED 4.10
    mechanism, inferred, see the first bullet);
  - the fake fuser prints PID 1 and 48 kernel threads before the real writer, the way a box does;
  - it covers the real TEST handoff (the dead-man stop before the window), the EVENT disable cut
    out of rig-mode.sh, and the WHOLE `painter_stop_remote` text (the dead-man stop before the
    failing verify);
  - a fuser that prints no listing must read as such, never as "no writer".
  - The unreadable-root test swaps in a failing `awk`, so the `/proc/mounts` fallback never reads
    the test machine's own root.
  - The Rust `tests/harness_cam2_painter_ro_persist_1175.rs` fakes `findmnt`/`fuser` too: without
    them the real findmnt reads the CI runner's own rw root.
  - An order anchor must find the CALL (`systemctl enable cam2-painter.service ||`), not the bare
    command text: the rw-remount FAIL message names the same command earlier.
- **Live put-back** after a box was left rw: `findmnt -no OPTIONS /` on cam2. If it reads rw, run
  `fuser -vm /` (and `ls -l /proc/*/fd 2>/dev/null | grep deleted` for a deleted-but-open file).
  Stop the holder that is not supposed to hold `/` (4.10: a second systemd-journald), then
  `mount -o remount,ro /` until findmnt reads `ro`. Then confirm with the next `rig-mode.sh test`
  that it still reads `ro`. This is a supervisor rig step, never a lane worker's.
- **Same defect elsewhere, not covered here:** `scripts/deploy-fleet.sh` (an `enable --now
  cam2-painter.service` then a swallowed ro remount), `scripts/lib/bkshading-relay-mode.sh`,
  `scripts/dantesync-fleet-upgrade.sh` and `scripts/lib/dantesync-rollback.sh` close the window
  with `mount -o remount,ro / ... || true` and never read the mount state. A shared "close ro,
  verify, name the holders" emitter would serve them all. It is cross-cutting (5 sites, 4
  subsystems), so the lane reported it to the supervisor as a `followup_candidates` entry in its
  issue-1405 LANE-RETURN; the ticket number goes here once the supervisor files it.
  It should also absorb this emitter's writer filter: `bkshading-deploy-relay.sh`
  `remount_ro_checked` still prints `fuser -vm / | head -n 40`, the cut-off fixed here. (The
  filter cannot live in `ro-root.sh`, which must stay free of grep/awk/sed.)

## The TEST painter's rig-test LEDGER entry — quoting the remote PID (issue 1382)

`painter_launch_remote` registers the transient painter in the ledger
(`scripts/lib/rig-test-ledger.sh` `rig_test_ledger_register_remote_cmds`) with a PID that exists
only on cam2, in the same remote script that just launched it. So the PID argument is a REMOTE
variable reference, and it has to reach the box in a form that expands THERE:

- **Pass the bare `$PAINTER_PID` text:** `'$PAINTER_PID'`, single quotes inside the heredoc's
  `$(...)` (the same idiom as the `'kill "$PAINTER_PID" ...'` argument of
  `audio_marker_check_cmds`). Never `'\$PAINTER_PID'`: that delivers a backslash too.
- **The builder puts PID_OR_UNIT in remote DOUBLE quotes as a printf `%s` argument**, so the
  variable expands on the box. A literal PID resolved on dev1 (recording-e2e.sh's pgrep) or a unit
  name passes through unchanged: backslash, double quote and backtick are escaped, `$` is left live.
- **WHAT / BOX / STARTED_BY / MAX_DURATION are data:** JSON-escaped, then single-quoted, so a quote,
  backslash, `%`, `$` or backtick is written verbatim.
- **One row format:** the local `rig_test_ledger_entry_json` and the remote registration share the
  printf format `RIG_TEST_LEDGER_ROW_FORMAT`; every value is an argument, never spliced into it.
- **The failure this fixed (27.9.2026, found at the EVENT switch):** the old builder spliced every
  value into a SINGLE-quoted printf format. With `'\$PAINTER_PID'`, cam2 wrote the literal
  `"\$PAINTER_PID"`, an invalid JSON escape. `event_mode_ledger_cleanup`'s jq read then came back
  empty and it printed `skipping malformed ledger line`. The painter was never terminated through
  the ledger; EVENT still stopped it through its own pidfile path, so only the safety net was lost.
- **Test by EXECUTING the text, never by matching it.** `tests/python/test_rig_test_ledger_register_1382.py`:
  - runs the builder's output, and the real registration block cut out of `painter_launch_remote`,
    in a local bash with `PAINTER_PID=4242` and a temp ledger;
  - parses the row as JSON and reads it back with jq, the way the EVENT reader does.
  - A text match passes a single-quoted `$PAINTER_PID` too.
- **A backtick in a COMMENT inside an unquoted `<<REMOTE` heredoc is a command substitution run on
  dev1.** Step (5)'s comment spelled a fuser command in backticks, so every `rig-mode.sh test` ran
  fuser locally while building the cam2 script and sent the comment with the text missing. Write
  commands in these comments with plain quotes. The same test file puts a stub `fuser` first on PATH
  and asserts building the script never calls it.

## Recovering a dead standing painter after a run (#1072)

Three composed recovery seams keep the standing painter from staying dark after an E2E run — the
combination unifies worst-case recovery to ~5 min on BOTH the clean-cleanup and SIGKILL paths:

- **cleanup restore is a bounded RETRY, not one-shot** (`scripts/lib/cam2-painter-restore-retry.sh`,
  `cam2_painter_restore_retry_cmds`). It runs AFTER the anchored `systemctl start cam2-painter
  2>/dev/null || true` + adjacent `$(cam2_painter_restore_verify_cmds)` (those two lines are pinned
  by #863/#872/#312/#713/coordination — never edit them; the retry is a #675 sourced-lib appended
  via ONE `$(...)` line). It sets the remote var `_cprr_ok`; cleanup wraps
  `$(cam2_painter_deadman_disarm_cmds)` in `if [ -n "$_cprr_ok" ]` so a FAILED restore LEAVES the
  dead-man armed. **Budget gotcha:** the whole cam2 cleanup ssh is wrapped in
  `timeout "$CLEANUP_SSH_TIMEOUT"` (30s) — keep the retry bounded (CAM2_PAINTER_RESTORE_RETRIES=3,
  ~3s poll each; the common already-active case adds ~0s). If a pure-failure retry does overrun the
  timeout, the SIGKILL is a SAFE fail: the `if [ -n "$_cprr_ok" ]` never runs, so the dead-man stays
  armed and self-heals — never "fix" this by extending CLEANUP_SSH_TIMEOUT.
- **the #872 dead-man is now PERIODIC** (`--on-active` + `--on-unit-active`, window 5 min, not a
  one-shot 90 min). Mid-run safety is the `pgrep -x frame-probe` guard (every fire during a live run
  is a no-op — frame-probe is armed+launched within ~25s in the SAME `_cam2_prep` ssh command and
  runs the whole run via `--duration-secs`), NOT a long delay. A short one-shot would be WRONG here
  (fires once, cannot recover a run killed after it fired) — periodic is what makes the short window
  safe. Residual: `--av-sync` mode's brief kill→relaunch frame-probe gap is a low-probability
  measurement artifact only, never a dark rig.
- **the [0/8] optical-chain preflight self-heals ONCE** (`scripts/lib/optical-chain-preflight.sh`):
  when a standing painter is EXPECTED but DEAD after the grace re-probe, it does EXACTLY ONE
  `systemctl start $service` over ssh + re-probe before the existing `exit 1` fail-closed backstop.
  No retry loop — the periodic on-box dead-man is the net; this is one fast recovery so a
  previous run's leftover-dead painter does not waste the whole gate run.

## cleanup() final restore re-check — a PRUNE decision may fire only on a POSITIVE paint signal (#1126)

`scripts/lib/cam2-painter-restore-recheck.sh` (`cam2_painter_restore_final_recheck`) runs in
cleanup() BETWEEN `cambox_parallel_wait_and_report` and `cambox_parallel_surface_painter_failure`.
The cam2/painter restore does a lot of serial work inside ONE `CLEANUP_SSH_TIMEOUT`(=30s) ssh; on a
slow restart `timeout` SIGKILLs it a hair (~50ms live, run 1104689227) BEFORE cam2-painter.service
reports active — the restore SUCCEEDED, only the verify window lost the race — and since the #715
retry never prunes a painter, a false `::error::` reds a GREEN-verdict run. The re-check is ONE
separate short bounded ssh (its own `CAM2_PAINTER_RECHECK_TIMEOUT`=25s < 30s, so it never widens the
tight parallel-restore budget / cancellation grace) that prunes cam2/painter from
`CAMBOX_PARALLEL_FAILED_LABELS` (+ lockstep `_FAILED_IPS`) only when genuinely painting NOW.

**Gotcha — a check whose exit code drives a PRUNE must NOT reuse the WARN-only "unit not installed →
no-op" convention.** `cam2_painter_restore_verify_cmds` treats "unit not installed" as a harmless
`[#863] nothing to verify` (it changes no gating decision). But `cam2_painter_genuine_paint_check_cmd`'s
exit code REMOVES a recorded restore failure + suppresses the #860 `::error::`, so it must EXIT 1
(not 0) on not-installed / a `list-unit-files` hiccup: a prune may fire ONLY on a POSITIVE paint
signal (KMS device held + `vblank-locked`, OR `/dev/fb0` held), never on absence-of-painter — that
absence IS the #863 black-monitor case a prune must never mask. Same discipline for any future
prune/gating reuse of a WARN-only signal.

**Consolidated (#1148):** the presenter-aware paint SIGNAL itself — the KMS-line parse + `fuser`
device-held check + `vblank-locked` confirmation + `/dev/fb0` fallback — is now the SINGLE
`_cb_paint_signal` (emitted by `cam2_paint_signal_remote_fn` in `scripts/lib/cam2-paint-signal.sh`).
The five builders (`cam2_painter_restore_verify_cmds` / `cam2_painter_steady_state_handoff_cmds` /
`painter_liveness_check_cmds` / `mv_reverify_painter_up_cmds` / `cam2_painter_genuine_paint_check_cmd`)
each lazy-source it and pipe their own log source into it; they keep only their own poll counts +
exit semantics (WARN-only / FAIL-LOUD / exit-0-1 prune / PAINTER_UP / the file-reading granular
messages). A future correction to the signal (an OBS presenter-log rename, a new presenter backend)
now lands in ONE place — `tests/harness_cam2_paint_signal_1148.rs` is where it is tested — instead
of risking five divergent copies that false-pass a black monitor. (`scripts/verify-device.sh` keeps
its own REDUCED dev1-side variant — journal-only, no `fuser`/fb0 because it runs the check from
dev1, not on the box — deliberately out of scope.)

**Extending it (the non-obvious mechanics, so you don't re-derive them):**
- `_cb_paint_signal` is a REMOTE bash FUNCTION (not a dev1-side one) because the predicate must run
  `fuser` ON the cam box. The shared thing is therefore emitted TEXT, and each site emits it by
  calling `cam2_paint_signal_remote_fn` **OUTSIDE its own `cat <<…` heredoc** (in the builder
  function body, before the heredoc). That is what makes it embed identically into a single-quoted
  heredoc (`<<'VERIFY'`, `<<'PAINTCHK'` — which cannot do `$(…)`) AND a `\$`-escaped one
  (`<<HANDOFF`, `<<CMDS`, `<<PCHECK`). Never try to `$(cam2_paint_signal_remote_fn)` inside a site's
  own heredoc.
- It uses `return`, never `exit` — safe both inside a `set -e` remote (handoff) and inside
  cleanup()'s WARN-only EXIT trap (a bare `exit` there aborts the whole trap).
- It echoes a REASON TOKEN (`KMS_OK <dev>` / `KMS_NODRM <dev>` / `KMS_NOVBLANK <dev>` / `FBDEV_OK` /
  `FBDEV_DEAD`) so a site that needs granular operator messages (presenter-liveness) maps the token,
  while the four boolean sites just `_cb_paint_signal >/dev/null`.
- If you change the signal STRINGS, an anchor test that used to read a lib SOURCE for the literal
  (`fs::read_to_string(lib).contains("presenter: using DRM/KMS page-flip")`) must instead assert the
  EMITTED builder output (run the builder, assert its stdout) — the literal now lives only in the
  shared lib, so a source-read of the individual site is 1→0 (the #1148 recheck-anchor migration).
