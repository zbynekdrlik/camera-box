---
paths:
  - "scripts/dantesync-fleet-upgrade.sh"
  - "tests/dantesync_fleet_upgrade.rs"
  - "scripts/lib/dantesync-tray-upgrade.sh"
  - "tests/python/test_dantesync_fleet_upgrade_tray_1372.py"
---

# dantesync fleet-upgrade mechanism (#876)

`scripts/dantesync-fleet-upgrade.sh` is the REMEDIATION half of the dantesync version-drift
problem — `dantesync-version-gate.sh` (#862) is the DETECTION half. It is operator/agent-INVOKED,
never a scheduled task (a task that silently stops scheduling is as bad as no task — the exact
#876 root cause: the Windows `DanteSyncUpdate` task died at the DanteTimeSync→DanteSync rename and
sat `Enabled`/`Next=N/A`/`Result=0` for months). Structure mirrors the tested
`scripts/upgrade-fleet-ndi.sh`: pure fns above a `BASH_SOURCE != $0` source-guard (unit-tested by
sourcing), network/mutating flow below.

## Design invariants (do not regress)

- **Target the PIN, never `releases/latest`.** Default target = `DANTESYNC_VERSION_PIN`, sourced
  from `dantesync-version-gate.sh` (single source of truth). "latest" would chase docs-only
  version bumps and schedule pointless clock-master redeploys (the #876 follow-up comment).
- **Canary-first, one representative per OS CLASS present** (`dantesync_resolve_canary`). A green
  Linux canary must NEVER authorize touching a Windows box — the class here is the OS (the #452
  per-class insight from the NDI upgrader). Any canary failure ABORTS the whole roll (rest
  untouched, exit 10); a non-canary failure is recovered + recorded but the loop continues (exit 20).
- **Reuse, never reinvent:** the version PARSER (`dantesync_version_from_version_output`) + the PIN
  come from sourcing `dantesync-version-gate.sh`; canary VERIFY is `dantesync-gate.sh` (PTP-lock +
  fresh in-bound offset); offline exclusion is `scripts/lib/cambox-offline-ack.sh` + `rig-fleet.txt`.

## Three traps a review caught (2026-08-15) — keep them fixed

- **Self-heal the swap; NEVER blind-rollback on an upgrade-command failure.** The remote upgrade
  script downloads + sha256-verifies BEFORE stopping the service, backs up the current binary, then
  arms a restore trap (Linux `trap '_dantesync_restore' ERR` disarmed on success; Windows
  try/catch that restores the `.bak` + rethrows) — so a failure PAST the point of no return
  self-heals ON THE BOX. The orchestrator therefore only externally `rollback_node`s on the
  VERIFY-failure path (swap provably completed). Blind-rolling-back on ANY non-zero upgrade rc
  would, with a pre-existing `.bak`, stop a HEALTHY master and downgrade it (a failed *download*
  exits before the service is ever touched).
- **Windows path SENDS A `.ps1` (scp -O) and runs it with `-File`** — never a nested
  `powershell -Command "..."` over ssh, which fails SILENTLY (exit 0, no output) per
  `.claude/rules/rig-state-inspection.md` §2. `dantesync_windows_upgrade_ps`/`_rollback_ps` return
  the `.ps1` CONTENT; `dantesync_windows_run_ps_file_cmd` is the `-File` invocation.
- **Single-node `dantesync-gate.sh` verification — a SLAVE opts out (`--ntp-master ""`), the MASTER
  is graded master-aware (`--ntp-master <self>`) (#1077 refines the original blanket opt-out).** The
  gate defaults `GATE_NTP_MASTER_NAME=strih` and REFUSES (usage error) when `--win-http`/`--linux`
  is configured but the master name isn't among the configured nodes — so a non-master node MUST
  pass `--ntp-master ""` (the documented opt-out; it forgoes the #1041/#1055 master-step-chase
  widening — the settle poll covers a transient re-lock). But the MASTER node, verified alone, IS
  among its own one configured node, so pass `--ntp-master "<name>"` (`verify_node`'s `master_arg`):
  the gate then applies its #1014 master median+freshness grade (a master-only invocation is
  explicitly supported and pays no priming read) instead of the strict slave offset bound — which
  is what tolerates the master's OWN post-restart step-chase. Blanket `--ntp-master ""` on the
  master measured the restart-induced sawtooth and rolled back a HEALTHY swap (rc=20 twice, live
  v1.8.43).

## #1077 additions — non-root escalation, curl-less staging, master settle window

- **Non-root Linux nodes escalate; the script is run BY FILE, never inline.** The generated
  upgrade/rollback script does root-only ops (`mount -o remount,rw`, `install`, `systemctl`). A
  `root@` node (cam boxes) runs it directly (`dantesync_linux_run_script_cmd` → `bash "$path"`); a
  non-root node (imag-nb `newlevel@`, dev1 `--local`) runs it escalated — `sudo -n` where
  passwordless (dev1), else `printf '%s\n' '<pw>' | sudo -S -p '' bash "$path"`. The password is
  embedded only in the RUN COMMAND (the ssh/`bash -c` arg), NEVER written into the scp'd on-disk
  script FILE. This mirrors the `scripts/lib/imag-presented-frame-check.sh` `sudo -S` convention and
  the Windows `-File` delivery (no nested-quoting hazard). `dantesync_needs_sudo USER` is the
  root-vs-not decision (0 = needs sudo unless USER is `root`).
- **Binary fetch is dev1-staged first, then on-box curl→wget→fail-loud (curl-less boxes: cam3).**
  `ensure_linux_binary_staged` downloads + sha256-verifies the pinned binary ONCE on dev1 (memoized;
  the memo `STAGED_LOCAL_DIR` is published ONLY after the sha passes, so a failed fetch never
  poisons it), and `stage_linux_binary_to` scp's it to `DANTESYNC_LINUX_STAGED=/tmp/dantesync-staged`
  on each node (cp for `--local`). The generated script's fetch resolver is `[ -f staged ]` →
  `command -v curl` → `command -v wget` → `exit 1`, then re-sha-verifies whichever (guards a corrupt
  scp) BEFORE `systemctl stop`. cam3 (no curl, broken apt) upgrades from the pre-placed binary; the
  metered venue LAN pays ONE download, not eight.
- **The master node's settle window is LONGER + bounded** — `MASTER_GATE_WAIT_TRIES`/`_SECS`
  (default 20 × 15s ≈ 5 min) vs the slave `GATE_WAIT_TRIES`/`_SECS` (10 × 6s ≈ 60s). Still a bounded
  `for i in $(seq 1 "$tries")` loop with a clear final PASS/FAIL (`gate_rc`), never a `while true`
  sleep-and-hope. Because the master (strih) is the first Windows canary, waiting it to steady state
  also gates the REST loop — slaves are verified only after the fleet has re-converged. `NTP_MASTER`
  defaults from `DANTESYNC_NTP_MASTER_NAME` (strih), the gate's own single source of truth.
- **Read the ACTUAL root mount state (findmnt), NEVER a `touch` write probe** — the generated
  upgrade+rollback scripts detect a read-only root (cam boxes) with
  `findmnt -no OPTIONS / 2>/dev/null || awk '$2=="/"{print $4; exit}' /proc/mounts 2>/dev/null`
  matched `case "$opts" in ro | ro,*)`, adopted VERBATIM from `setup-device.sh`'s
  `ensure_root_writable()`/`root_mount_is_readonly()` (#599, also mirrored in `verify-device.sh`).
  A `touch` write probe conflates a read-only filesystem with a mere permission error — and once
  the script runs escalated (see the escalation bullet) it reads writable everywhere a real move
  is possible, so it can BOTH miss a genuine ro root AND misfire on a permission quirk. The
  `ro | ro,*` FIRST-comma-token match is why `errors=remount-ro` (present in every ext4 rw mount)
  never false-positives — a bare `contains("ro")`/`,ro,`-anywhere match would. Both the detection
  and the remount action key on `/` (never `-T "$bindir"`), so they cannot diverge. The initial
  #1077 cut used a findmnt-guarded read with a `touch`-probe FALLBACK — the review caught that the
  fallback reintroduced the exact conflation; the #599 `|| /proc/mounts` fallback reads REAL state
  on the findmnt-less path too. This is the FIRST place to reuse for any future generated-remote
  or on-box ro-root read in this repo.

## `--fleet` (issue 1372)

`--fleet` appends every node of `scripts/lib/dantesync-fleet.sh` not named by `--linux/--win/--local`
(every camera, dev1, the OBS boxes, the audio-VLAN PCs mbc + fohabl), so a roll can no longer forget
a box (mbc sat one release behind on 25.9.2026). A node whose fleet row names its own credential
(fohabl, user `master`, `DANTESYNC_FOHABL_SSH_PASS`) is dialled with that value via `node_pass`
(ssh AND scp); a missing value SKIPS the node, never dials it with `SSH_PASS`. Without an explicit
`NTP_MASTER`, the fleet's `ntp-master` row (strih-lx) gets the master-aware verify. An
audio-role node (mbc, fohabl) is verified with `RIG_GRANDMASTER_IP` = the audio grandmaster
(`dantesync_gate_env_for`), never the video one, so GM enforcement cannot fail its verify. Always
`--fleet --dry-run` first. A roll also refreshes `dantesync-tray.exe` on every Windows node, including
one whose service is already on the target (next section); the version gate's report-only tray
sha-pin names any tray that still lags.

## The tray rides the roll (issue 1372)

Before this, a roll swapped only the service, and the tray (the version the operator sees) lagged
until someone swapped it by hand (after the 1.11.0 and 1.11.1 rolls, 26.9.2026). The emitted `.ps1`
now carries a tray arm. Its text lives in `scripts/lib/dantesync-tray-upgrade.sh`, which keeps the
upgrade script under the ~1000-line budget.

- **Fetch before any stop.** `dantesync_windows_tray_fetch_ps VERSION` runs right after the service
  binary is verified. It reads the SAME pinned tag's `dantesync-tray-windows-amd64.exe.sha256`
  (`dantesync_release_url_windows_tray`, never latest).
  - If the installed tray already has that sha, it sets `$trayCurrent`: no download, no restart. A
    current tray is still checked for a running process in an interactive session; none running
    (a relaunch that found nobody logged on leaves exactly that) sets `$trayLaunch`, so step 6
    launches it and a re-run never reports a dead tray OK.
  - Otherwise it downloads the exe and verifies it.
  - A missing install or a failed download / sha is recorded in `$trayNotes` and never thrown, so the
    service upgrade still runs.
- **Swap after the service is back (step 5).** `dantesync_windows_tray_swap_ps` runs after the
  service's try/catch and the dead-task purge.
  - It stops `dantesync-tray` (exact name) and sets `$trayStopped`.
  - It backs the tray up to `dantesync-tray.exe.pre-<version>` only when that file does not exist, so
    a re-run never overwrites the original pre-roll tray.
  - It replaces the file and verifies the installed sha. A failed replace or a wrong sha restores the
    backup, and the restore is checked by hash: a failed restore is named (`the tray exe may be
    partial`), never claimed.
- **Relaunch whenever the tray was stopped or a current tray is not running (step 6), even after a
  failed swap**, so the operator is never left without a tray.
  - The temporary task (`DanteSyncTrayRelaunch-1372`) has the GROUP principal `BUILTIN\Users`, named
    by its well-known SID `S-1-5-32-545` because the account name is localized, with
    `-RunLevel Limited`. It starts the tray in the logged-on user's interactive session. The NAME
    form with default settings was proven live on mbc and fohabl, where the ssh account (`master`)
    differs from the desktop user (`Ableton-FOH`). The SID + settings form is the same principal by
    the documented API but is UNVERIFIED live until the next roll: check `TRAY OK` on all four
    Windows nodes then.
  - The task has explicit settings: `-AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
    -ExecutionTimeLimit 0`. Task Scheduler's defaults would refuse a FOH laptop on battery.
  - It is unregistered in a `finally`, and an unregister failure is a note.
  - The count is read again AFTER the unregister: exactly ONE `dantesync-tray` process with
    `SessionId >= 1` (session 0 is the ssh/service session). A box with nobody logged on reads as a
    warning, which is honest. The downloaded tray and its `.sha256` are removed.
- **Never a throw, never a rollback.** The block ends with ONE `TRAY OK: …` or `TRAY-WARNING: …`
  line.
  - `dantesync_tray_outcome` reads the last one; a missing line is a warning too.
  - `dantesync_tray_note` logs `[name] tray OK/WARNING`.
  - `dantesync_tray_report` prints `WARNING: dantesync-tray was NOT refreshed on N node(s)`, one
    reason per node, before EVERY exit after the roll started (canary abort, incomplete, complete).
  - The exit code stays the service's: the tray is UI, the service is the clock.
- **Re-running the roll repairs a tray.** A Windows node whose service is already on the target
  (`SAME`) is collected into `TRAY_ONLY`. After the roll, and on the all-current early exit,
  `refresh_same_node_trays` sends it `dantesync_windows_tray_only_ps` (fetch + swap + relaunch, no
  service step). A current tray costs one `.sha256` read. `--dry-run` only names these nodes. A
  canary abort does not touch them.
- **Anchors.** The Rust test `windows_upgrade_ps_waits_for_the_process_to_exit_between_stop_and_swap_1265`
  bans `dantesync-tray` only inside the daemon's stop -> swap window; the wildcard ban stays
  program-wide. The tray code uses its own variable names (`$trayExe`, `$trayTmp`, `$trayPre`), so the
  service anchors `Copy-Item -Force $exe $bak` / `$tmp $exe` / `$bak $exe` stay unique.
- **The service rollback does not touch the tray.** On a verify failure the service goes back to
  `.bak` while the tray stays on the target release. The node already counts as a failed node, and
  the version gate names the service drift. To restore the tray by hand, use
  `dantesync-tray.exe.pre-<version>`.

`--help` prints the whole extended header (`sed -n '2p;4,/^HERE=/…'`); it used to stop at a fixed
line 71, mid-paragraph. The header therefore documents `SSH_PASS` without its default value.

**Tier-0:** `pytest tests/python/test_dantesync_fleet_upgrade_tray_1372.py`.
- The emitted program is tested as text: order, SID + settings, `finally`, the step-6 guard, no throw
  after the last catch.
- The orchestrator runs end to end:
  - a stateful PATH `sshpass` stub plays the Windows node: scp saves the `.ps1`, `--version` answers
    from a state file, and `-File` flips it (or not, for the canary abort) and prints the stubbed
    TRAY line;
  - the gate's `DANTESYNC_GATE_WIN_HTTP_STREAM` seam is fed a fresh live `/status`;
  - the stub keys the version and the upload by host, so a mixed fleet is played too;
  - the tests cover warning, OK, canary abort, the all-current and the mixed-fleet SAME-node
    refresh, and dry-run.
- The Rust file runs locally with a plain `rustc --test`: it is std-only (`ci-testing-gotchas.md`).

## Testing (Tier-0)

Heavy `cargo test` is CI-only here (#477) — no `# airuleset:build-ok` bypass. Verify the bash logic
by SOURCING the script directly in bash (the same thing `tests/dantesync_fleet_upgrade.rs`'s
`run_sourced` does, minus cargo) and asserting the pure-fn outputs + generated command TEXT; plus
`shellcheck`, `bash -n`, and `rustfmt --check` on the test. The Rust binary compiles on CI.
