---
paths:
  - "scripts/deploy-genlock-fleet.sh"
  - "scripts/lib/genlock-markers.sh"
  - "scripts/lib/genlock-fleet-boxes.sh"
  - "scripts/lib/strih-lx-deploy.sh"
---

# One canonical genlock deploy path across the whole rig (#789 bod 4 + bod 5)

`scripts/deploy-genlock-fleet.sh` is the SINGLE entry point for deploying the OBS genlock build to
strih + stream + imag from ONE anchor CI run id. Before it, each box had its own uncommitted path
(strih/stream = the prose runbook in `.claude/rules/rig-state-inspection.md §5`; imag =
`setup-imag.sh`) from THREE separate workflows/run-ids, so imag routinely lagged and refused the
Full-path E2E at the `genlock_parity` preflight (the recurring #923/#932 pain).

## The shape (a PLANNER + bounded ssh-executor, same as `launch-obs-genlock.sh`)

- Pure builder functions emit each box's program; unit-tested by sourcing (`tests/deploy_genlock_fleet.rs`).
- **Windows (strih/stream) is EMIT-ONLY** — a bash script cannot drive the `win-*` MCP
  (`win-ssh-vs-mcp` HARD rule), so it PRINTS the PowerShell program the agent pastes into the box's
  MCP Shell. **imag and strih-lx are Linux** (file copy + CLI = Context B), so execute mode actually
  ssh-deploys them (strih-lx: `scripts/lib/strih-lx-deploy.sh`, issue 1317 part 6 — below).
- `--plan --run-id <id> --sha <headSha> --stage <dir> [--full|--fast] [--boxes …]` prints the whole
  plan offline (no network) — this is the Tier-0-tested surface. Execute mode (`--run-id …`) resolves
  + downloads + deploys live (supervisor-driven; the download/scp/ssh glue is untestable offline).

## The strih is strih-lx; the Windows `strih` arm is RETIRED (issue 1317 part 3, M4 cut-over 20.9.2026)

The STRIH-SNV Windows PC is gone and 10.77.9.202 is the Linux strih-lx. So:

- **Default fleet = `strih-lx,stream`** (canonical order `strih-lx,stream,imag,resolume`). `--boxes
  strih` is REFUSED by name ("RETIRED … the strih is strih-lx"), and every per-box Windows function
  (`fleet_box_mcp` / `fleet_box_ip` / `fleet_box_ahk_script` / `fleet_box_ahk_prefer`) returns 2 for
  it. `build_windows_deploy_program BOX … HAS_AHK=1` for a box with no AHK identity is a loud rc 2 —
  it used to fall back silently to the retired strih's `D:\_APPS` path.
- **The per-box constant table lives in `scripts/lib/genlock-fleet-boxes.sh`**, sourced by this
  script AND `launch-obs-genlock.sh` + `obs-self-heal-install.sh` (three copies of the MCP/IP/AHK
  facts before). Addresses + the AHK fact come from `scripts/lib/obs-fleet.sh` (`obs_fleet_host`,
  `obs_fleet_has_ahk`); imag keeps its `imag` ssh alias. Registering a box = one lib edit + its fleet
  row. The move was byte-neutral for stream / resolume / imag (plan output diffed identical, full +
  fast); keep `tests/deploy_genlock_fleet.rs`'s `per_box_table_lives_in_the_shared_lib_1317` green
  (no inline redefinition, no `win-strih` / `.202` in the lib, deploy script < 1000 lines).
- **The strih-lx plan is `emit_strih_lx_plan`, NOT `emit_imag_plan`.** The old arm printed the imag
  program under a `box=imag` header — it restarts `imag-obs.service`, a unit strih-lx does not have,
  so it would have failed AFTER installing bytes. strih-lx installs the strih FULL artifact
  (`obs-genlock-linux-x86_64-strih`, also under `--fast` — fast is Windows-only) into `/usr` through
  `setup-strih.sh` (release-parity gate, runtime packages, /usr prefix, chrome-sandbox) and is
  supervised by `strih-obs.service`. **Every plan line is a `#` comment** (the issue-1295 saved-.ps1
  parse rule holds for a mixed plan).

## strih-lx EXECUTE arm (issue 1317 part 6) — `scripts/lib/strih-lx-deploy.sh`

`deploy-genlock-fleet.sh --run-id <id> --boxes strih-lx` deploys the production strih end to end;
it replaced the supervisor's ad-hoc scratch script, which twice failed half-silently (the box's
`/tmp` is a 7.5 GB tmpfs, each stage ~2.2 GB, rsync died `Disk quota exceeded` rc 11 and the old
build kept running with nothing refusing). The arm is a sourced lib (the deploy script has a
< 1000-line budget pinned by `per_box_table_lives_in_the_shared_lib_1317`), and the `--plan` arm
prints the SAME remote-command builders (`strih_lx_plan_steps`). Each failure =
`ERROR: [strih-lx <step>] failed (rc=N): …`; a failed strih-lx writes NO fleet-log line.

**Two phases — every requested box resolves BEFORE any box changes.** `strih_lx_prepare` (exit 3, box
untouched) runs before the Windows and imag resolution; `strih_lx_apply` (exit 4) runs after them and
before the imag ssh deploy. A default `strih-lx,stream` run with no Windows build at the SHA can
therefore never leave the production strih on a different commit than stream (genlock_parity).
The Windows paste-programs are printed only AFTER strih-lx VERIFIED (nobody pastes stream while the
strih apply can still fail), and the verified strih-lx gets its OWN fleet-log line at once (the rest
of the fleet is logged at the end), so a later imag failure never hides the strih deploy.

1. **prepare** — the `linux-genlock.yml` run at the anchor SHA; the artifact is REFUSED unless its
   `GENLOCK_BUILD_SHA.txt` is the canonical SHA and its `BUNDLE_MANIFEST.json` carries the
   `lib/x86_64-linux-gnu/libobs.so.30` sha256 (the read-back needs it). The dial host must be a dotted
   IPv4: the runner exports it as `STRIH_LX_IP`, which setup-strih.sh pins as the box's own static
   address (the janus `local_ip`), so a hostname override would recreate the 22.9. binding outage.
   The provisioning tree is the COMMITTED `scripts/ systemd/ intercom/` of the checkout (the archive
   of HEAD — uncommitted edits never ship) + a generated `run-setup.sh`; it is the checkout's tree,
   not the bundle's SHA (a scripts-only fix never triggers a genlock build; setup-strih.sh is box
   config — the same split as imag's `genlock-markers.sh`). Token: `STRIH_LX_GH_TOKEN` (read-only)
   else `gh auth token`.
2. **preflight** — refuse while a previous `setup-strih.sh` still runs (`pgrep -x`): the sweep would
   delete the stage it is reading and a second installer would race it.
3. **sweep** — `mkdir` + `touch` the stage FIRST (it is then the newest `genlock-stage-*`), then the
   EXISTING `obs-backup-retention.sh --local-sweep` decision with **`--stages-only`** (added here: the
   dated rollback backups are never touched), keep-runs 1 / keep-days 0, **as the operator** (the
   stage dirs are the operator's; no sudo, so no password line ever reaches a shell), fed the script
   from the committed tree, at the deploy's own host. The deploying stage is re-checked; any other
   stage dir still present is a loud WARNING (the rsync is the hard gate).
4. **stage** — `rsync -a --delete --timeout=120` bundle → `/tmp/genlock-stage-<sha>/bundle/`, tree →
   `…/repo/` (ONE per-sha dir the next sweep removes whole; the old fixed `/tmp/strih-lx-deploy-repo`
   is gone). **OBS is still running** — an rsync failure exits 4 BEFORE anything is stopped.
5. **stop** — delegated to `/usr/local/bin/strih-obs-stop.sh` (plain mode = `systemctl --user stop`,
   so the unit `ExecStop` runs and `Restart=on-failure` stays quiet) + a bounded wait for the unit AND
   every `obs`. The deploy itself sends no signal; the stop code's own SIGTERM → grace → SIGKILL ladder
   is the unit's contract. Not stopped in 30 s = exit 4, nothing installed, best-effort start.
6. **setup** — `sudo -k -S … bash <stage>/repo/run-setup.sh`, stdin = the sudo password line then
   `ghtoken:<token>`. The runner reads the token line (skipping a password line a NOPASSWD sudo left
   unread), exports `GH_TOKEN STRIH_LX_BUNDLE_SRC=<stage>/bundle STRIH_LX_IP`, and re-execs itself
   `setsid nohup` to run `setup-strih.sh` DIRECTLY and write `<stage>/setup-strih.rc`; dev1 polls it
   (default 45 min). An ssh drop cannot kill the install mid-apt; the token is never an argv or a
   file. **OBS is never started over a still-running installer**: a launch whose ssh returned
   non-zero while setup-strih.sh runs is THIS deploy's detached installer (the preflight saw none), so
   the deploy follows it through the rc poll (WARNING, not a failure — never the strih left dark); an
   rc timeout with the installer still running (or the box unreachable) is exit 4 "wait for
   setup-strih.rc" without a start; a genuine failure (rc != 0, or no installer running) gets the log
   tail + a best-effort start + exit 4; a failed launch with the box UNREACHABLE (installer state
   unknown) is exit 4 at once, no start, no 45-min poll. setup-strih's pending-reboot warning (rc 0)
   is printed as a NOTE — the grep is keyed on that warning's own text (`reboot strih-lx, then run
   verify-strih`, a test pins it in setup-strih.sh), never on "next boot", which the baseline prints on
   routine lines every run. A settle time the read-back budget (polls x secs) cannot outlast is
   refused in prepare (exit 3).
7. **start + verify** — touch `<stage>/obs-start.marker`, start the unit, then poll (default 24 x 10 s)
   and REFUSE unless the polls pass `strih_lx_deploy_verdict` (marker == canonical,
   installed `libobs.so.30` sha256 == the manifest's — the BYTES, not only the marker, since `:8899`
   serves the same marker file — unit `active`, `render tick ENABLED` in an OBS log newer than the
   start marker, `:8899 genlock_build_sha` == canonical) AND `strih_lx_stable_verdict` holds the SAME
   non-zero MainPID and NRestarts from the first good poll for `STRIH_LX_VERIFY_SETTLE_SECS` (default
   90 s; a bad poll restarts the window). A `Type=simple` unit reads `active` between crash-loop
   restarts, and the one crash loop seen on this box (the CEF trap) cycled every ~60 s, so neither a
   single `active` read nor two adjacent 10 s polls prove the new OBS runs. The tick grep is
   `LC_ALL=C grep -a` like every other render-tick reader. The dial host must be four octets 0..255
   (`strih_lx_is_ipv4`).

Every ssh is `sshpass -p … timeout ${STRIH_LX_SSH_TIMEOUT:-180} ssh …` (`timeout` INSIDE sshpass so
the password prompt still reaches sshpass's pty).

Tests: `tests/deploy_genlock_fleet_strih_lx_exec_1317.rs` runs the REAL script with `gh` / `sshpass` /
`ssh` / `rsync` / `curl` stubbed on PATH (the ssh stub dispatches on the remote command text — keep
the step commands' distinguishing substrings: `pgrep -x setup-strih.sh`, `--local-sweep`, `LEFTOVER`,
`setup-strih.rc`, `run-setup.sh`, `reboot strih-lx, then run verify-strih`, `setup-strih.log`, `strih-obs-stop.sh`,
`GENLOCK_BUILD_SHA.txt`, `--user start`), plus the generated runner (root check stripped for the test)
and `--stages-only` on real temp dirs. A test SHA must stay SHORT hex — a 40-hex literal trips the
secret-staging hook. Remaining supervisor steps after a green execute: `verify-strih.sh` on the box
and the WS filter-enum survival check (printed as ACCEPTANCE in the plan).

## Same-SHA cross-workflow resolution — the heart of "one canonical version"

Windows and Linux genlock artifacts come from SEPARATE workflows with SEPARATE run-ids
(`windows-genlock.yml` / `windows-genlock-fast.yml` vs `linux-genlock.yml`). So `--run-id` is only an
ANCHOR: its `headSha` is read, then `fleet_pick_run_at_sha` picks each sibling workflow's SUCCESSFUL
run AT THAT SAME SHA (`gh run list --json databaseId,headSha,conclusion | fleet_pick_run_at_sha SHA`).
If no sibling run exists at the SHA, it fails loud (build it first — e.g. the tag-dispatch recipe in
`rig-state-inspection.md`'s #923 note). This is what makes all three boxes converge to one commit.

## GOTCHA — a Windows deploy that stops AHK MUST restart it (verified) BEFORE handing off to `launch-obs-genlock.sh`

strih runs the `NL_STARTUP.ahk` (`AutoHotkey64`) watchdog that respawns obs64 via the `.lnk`. A deploy
must stop AHK for the robocopy (else it re-locks `data\`/`obs-plugins\` mid-copy → robocopy exit ≥ 8).
But **`launch-obs-genlock.sh` only restarts AHK it stopped ITSELF** (`if ($ahkStopped)`), and its #978
session gate then HARD-FAILS `exit 8` on `AutoHotkey64 count != 1`. So a deploy program that stops AHK
and defers the restart to `launch` leaves strih with NO watchdog AND makes the STEP-2 launch fail —
the two-step "one deploy path" breaks on the one box that has the watchdog. **Fix: the deploy program
restarts AHK itself, VERIFIED, before exiting** — reuse the ONE shared `scripts/lib/ahk-watchdog.sh`
`ahk_resolve_and_relaunch_ps` helper `launch` uses (never a fork; sets `$ahkRelaunchVerified`), fail
loud (`exit 9`) if it doesn't come back. This leaves AHK count==1 so `launch`'s gate passes.

## imag deploy = the WHOLE bundle, never a hand-picked subset (issue 1026)

The imag leg ships the ENTIRE linux bundle (`cp -a lib/x86_64-linux-gnu/.` incl EVERY
`obs-plugins/*.so` + the frontend + `share/obs/`), NOT the 4-file libobs/distroav/frontend/opengl set
that `setup-imag.sh` step 12 installs — a stale `obs-plugins/*.so` over a new `libobs.so.30` is a
latent SIGSEGV (`.claude/rules/rig-state-inspection.md §5b`, issue 1026). It sha256-verifies the
staged bytes vs `BUNDLE_MANIFEST.json` before install, checks BOTH the libobs and libobs-opengl SONAME,
and directs the mandatory post-restart WS filter-enum survival check
(`obs_burn_filter.py check` from dev1).

## Marker helper is ONE source of truth (bod 4)

`scripts/lib/genlock-markers.sh` (`genlock_write_markers` — atomic temp-then-rename of
`GENLOCK_BUILD_SHA.txt` + `DISTROAV_BUILD_SHA.txt` + `DEPLOYED_AT`) is shared. `setup-imag.sh` carries
a behaviorally-identical INLINE copy (it ships standalone to the box, so it cannot source the sibling
lib) locked by a byte-parity test. Retention (`genlock_retention_delete_plan`, keep newest N=3) is
PLAN-ONLY by default (`--yes` gates actual deletion; the deploy never silently `rm`s a backup).

## GOTCHA — DistroAV loads ONLY from ProgramData; the FULL deploy ships + byte-verifies it there (#1115)

OBS loads DistroAV EXCLUSIVELY from `C:\ProgramData\obs-studio\plugins\distroav\bin\64bit\
distroav.dll` — NEVER from `C:\Program Files\obs-studio\obs-plugins\64bit\distroav.dll` (that path is
absent on the boxes; a copy there is a shadow `drift-guard.sh` #124 flags). The FULL `copy_block`
therefore `/XF distroav.dll` on the Program-Files obs-plugins robocopy — that exclusion STAYS. What
`build_windows_deploy_program` FULL mode ALSO does (the `(6b)` block, #1115 / Option A): explicit
path-mapped `Copy-Item` of the staged bundle's `obs-plugins\64bit\distroav.dll` → the ProgramData
load path, a `distroav.dll.pre-789` backup alongside `obs.dll.pre-789`, and a fail-closed sha256
verify of the DEPLOYED ProgramData DLL against the manifest's distroav entry (mirrors the obs.dll
verify). FAST is obs.dll-only → the block is a no-op there (no distroav in the fast bundle).

- **DLL-scoped on purpose.** The `distroav\data\` tree is NOT mirrored: the byte-parity gate hashes
  only the DLL, and DistroAV data is stable across genlock rebuilds at the pinned 6.2.1. The
  bundle→ProgramData layout is NOT 1:1 (bundle `obs-plugins/64bit/distroav.dll` + `data/obs-plugins/
  distroav/`; on-box `plugins\distroav\bin\64bit\` + `plugins\distroav\data\`) — so it is an
  EXPLICIT one-DLL path map, never a bulk `robocopy` of the wrong tree.
- **The three-layer facet:** deploy writes the ProgramData DLL → `bundle-state-server.py` hashes the
  distroav FIRST-located copy (= ProgramData, since Program Files carries no shadow) into
  `distroav_dll_sha256` → `drift-guard.sh --compare` matches it to the manifest's `obs-plugins/64bit/
  distroav.dll` BY BASENAME. So Option A makes the gathered ProgramData sha equal the manifest sha.
- **Runtime ENABLE of the distroav compare is separate (issue 1100 / issue 1082 ENFORCE):** the gate
  auto-sources the FAST obs.dll-only manifest, so distroav is labelled SKIPPED at runtime until a
  distroav-bearing (FULL) manifest is auto-sourced — do NOT "fix" the FAST manifest to add distroav.

## imag post-swap restart HANDS OFF to imag-obs.service — never a raw setsid/nohup launch (#789 residual)

The imag leg's step (7) restarts OBS THROUGH the durable systemd USER unit, never a session-tied
raw launch. The first live fleet run (2026-08-18) `setsid nohup … imag-obs-start.sh &`'d OBS OUTSIDE
the `imag-obs.service` cgroup (no `Restart=on-failure`, `ExecStop=` bypassed, the launch tied to the
ssh session) — it died in ~21s and needed a hand `systemctl --user restart`. That is exactly the
`imag-obs-supervision.md` "never launch obs directly with setsid/nohup" trap, in the deploy leg. The
corrected shape (mirrors the strih AHK stop→verify→relaunch ordering):

- **The unit is a USER unit — reach its bus from the root deploy program via
  `sudo -u <user> env XDG_RUNTIME_DIR=/run/user/$(id -u <user>) systemctl --user …`.** Use `env` to
  set `XDG_RUNTIME_DIR`, NOT a bare `sudo -u u XDG_RUNTIME_DIR=… systemctl` — sudo's `env_reset`
  policy can strip a command-line `VAR=val`, and a stripped `XDG_RUNTIME_DIR` silently loses the
  user bus (`Failed to connect to bus`) and re-creates the unsupervised-launch failure. `env` sets
  it in the child, bypassing sudo's env filtering entirely (issue 998 is the ssh half of the same
  user-bus requirement).
- **Order: `systemctl --user stop` → `pkill -9 -x obs` (kill any stray from a prior raw-launch
  deploy; safe once the unit is stopped) → clear sentinels → `reset-failed` → `systemctl --user
  restart`.** The stop-then-pkill kills the OLD obs (old libobs still mapped, #912 stop-race) before
  the fresh supervised start; the idempotent guard in `imag-obs-start.sh` (`if pgrep -x obs; exit 0`)
  means a surviving stray would make the unit's ExecStart a no-op and leave it unsupervised, so the
  pkill is load-bearing, not decoration.
- **Verify three things, fail-loud `exit 4` on any miss:** `systemctl --user is-active` (bounded
  poll), the running obs pid actually sits inside the `imag-obs.service` cgroup
  (`grep imag-obs.service /proc/<pid>/cgroup` — the verify-imag.sh issue-1015 discriminator: systemd
  bookkeeping can say active while the real obs sits outside the cgroup), and the render-tick log
  (`/tmp/imag-obs-start.log` reaching `OK: OBS bezi`, the imag analogue of the Windows render-tick
  verify). Read the log only PAST the pre-restart `wc -l` line count so a stale `OK: OBS bezi` from a
  previous start cannot false-pass.
- The emitted-program assertions live in `tests/deploy_genlock_fleet.rs`
  (`imag_program_restarts_through_the_systemd_unit_not_a_raw_launch_789`) — a `!contains("setsid")`
  / `!contains("nohup")` negative anchor, so keep those words out of the emitted step's own comments.

## GOTCHA — the STREAM box also needs a keep-alive stop→verified-restart contract (scheduled tasks, not AHK) (#1140)

The AHK contract above (stop→verified-restart) covers strih's `NL_STARTUP.ahk` respawn watcher. The
STREAM box has the SAME hazard through a DIFFERENT mechanism: Task-Scheduler keep-alive job(s) that
respawn obs64 mid-copy → the deploy program stops obs64, a keep-alive task fires seconds later, the
running obs64 re-locks `bin\64bit`, and the robocopy dies on ERROR 32 sharing violations (deploy exit
4). Live incident 2026-08-19 (`build_windows_deploy_program` STREAM), worked around manually
(disable → deploy → enable). The fix mirrors the AHK contract for scheduled tasks:

- `fleet_box_keepalive_tasks BOX` (a per-box constant next to `fleet_box_has_ahk`) is the SINGLE
  configurable source of the OBS keep-alive task names to disable — CURATED, never all of a box's
  ~nine scheduled tasks. stream → `avsync-keepalive camera-box-obs-self-heal-stream`; other boxes → none.
- **Root cause precision (do NOT mis-attribute):** `avsync-keepalive` (~10 min, #812) does NOT
  respawn obs64 — its `Ensure-Running` only relaunches the two avsync monitor `.ps1` scripts
  (`watchdog.ps1`, `avsync-vlc-monitor.ps1`). The ACTUAL obs64 respawner is
  `camera-box-obs-self-heal-stream` (#411, `--interval-min 2`, relaunches OBS via
  `build_launch_program`), so BOTH are listed even though the incident named only avsync-keepalive.
- The emitted program (steps `# (1b)` disable / `# (8b)` verified restore) disables+restores ONLY a
  task that is PRESENT AND ENABLED (reads `Scheduled Task State:` from `schtasks /Query … /FO LIST /V`),
  so a deliberately-disabled task (obs-self-heal ships DISABLED) is never re-enabled. Runtime state
  (which tasks were disabled) lives in the PowerShell `$disabledKeepAlive` array, exactly as AHK's own
  restart tracks `$ahkRelaunchVerified`. Every fail path exits `10` (loud): a disable failure of an
  enabled task, an UNREADABLE task state (never fail-OPEN — that silently drops the protection = the
  exact incident), and a restore-miss. Disable also `schtasks /End`s any in-flight instance (parity
  with AHK's `Stop-Process`). The `Scheduled Task State:` parse is EN-locale-dependent, consistent
  with every other schtasks parse in the repo — the fail-loud-on-unreadable path surfaces it instead
  of hiding it.
- Assertions: `tests/deploy_genlock_fleet.rs` `windows_stream_disables_and_restores_obs_keepalive_tasks_1140`
  + `fleet_box_keepalive_tasks_lists_stream_obs_keepalives_1140`, driven purely by box name (the
  builder computes the list from `$box`, no new arg).

## GOTCHA — the static-anchor self-collision class ALSO bites `deploy-genlock-fleet.sh`'s own emitted-program tests (#1140)

The top-level CLAUDE.md documents the `.find()`/`!contains()` anchor-collision class for
`recording-e2e.sh` / `rig-mode.sh`. It applies IDENTICALLY to `build_windows_deploy_program`'s emitted
PowerShell: `tests/deploy_genlock_fleet.rs` asserts both POSITIVE (`p.contains("robocopy")`) and
NEGATIVE (`windows_fast_program_is_obs_dll_only`: fast stream `!p.contains("robocopy")`) anchors on the
emitted program TEXT — including comments. Adding a NEW comment to a keep-alive/AHK/copy block that
merely SPELLS a word an existing negative anchor forbids breaks that test even though the code is
correct. Live: a `# (1b)` comment saying "mid-robocopy" put the literal `robocopy` into the (comment-
carrying) fast-mode stream program and broke `!p.contains("robocopy")`. Before adding a comment near
any emitted block, grep the test file for `!p.contains(` / `!stream.contains(` and keep those words out
of the comment (reworded to "during the byte copy"). New `# (1b)`/`# (8b)`-style step markers must also
be UNIQUE in the emitted program (`grep -c` = 1) so an ordering `.find()` anchor can't latch the wrong one.

## GOTCHA — `cp -a "$SRC/." "$DST/"` into a SYSTEM dir stamps the source dir's perms/owner onto $DST (#1236)

The imag leg installs the whole bundle with `cp -a "$BUNDLE/lib/x86_64-linux-gnu/." "$LIBDIR/"`.
GNU `cp -a` with the `src/.` operand copies the CONTENTS **and** applies the SOURCE directory's own
mode+ownership onto the DESTINATION. `$BUNDLE` is an scp'd `mktemp -d` (0700, owned newlevel), so on
the 2026-08-31 fleet deploy of 3eb21b2ed `/usr/lib/x86_64-linux-gnu` itself became
`drwx------ newlevel:newlevel` and the installed libs landed `0600/0700 root:root` → user-run OBS
died with `cannot open shared object file: libobs.so.30`, `imag-obs.service` flapped, and the
supervised-restart verify correctly refused (exit 4). The 2026-08-26 deploys did NOT hit it — the
trigger is the staging dir's own perms, which vary with how CI/scp left them, so the fix must hold
REGARDLESS of them.

**The fix (keep `cp -a`, NORMALIZE after + fail-closed ASSERT — never a whole-libdir sweep):**
`cp -a` STAYS (the issue-1026 whole-bundle contract + its `tests/deploy_genlock_fleet.rs` anchor
`cp -a "$BUNDLE/lib/x86_64-linux-gnu/."`). After it: reset `$LIBDIR` root:root 0755, then walk the
BUNDLE source tree (`find . -mindepth 1 -printf '%P\0'`, NUL-safe) and per installed path set
`chown root:root` + dirs 0755 / files `a+rX` (a 0700 rwx lib → 0755, a 0600 data file → 0644, both
world-readable; `a+rX` never makes a non-exec file exec). The `share/obs` install (same `cp -a src/.`
shape) gets the same treatment. Then a fail-closed `(3b)` assert (`assert_installed_perms SRC DST`)
walks the same set and refuses the restart (`exit 4`) unless `$LIBDIR` is root:root 0755 and every
installed path is root:root with dirs o+rx / files o+r — same fail-loud spirit as the SONAME/manifest
guards. Scope to the just-installed set (walk the bundle tree), NOT `chown -R`/`chmod -R` over the
whole `/usr/lib/x86_64-linux-gnu` (thousands of unrelated distro files).

**`setup-imag.sh` step 12 does NOT share this defect** — its hot-swap uses per-file
`install -m 0644/0755 -o root -g root "$BUNDLE_LIBOBS" "$LIBOBS_REAL"` (deterministic mode+owner,
never stamps the containing dir); its only `cp -a` uses are single-FILE backup copies (no `src/.`
operand). Any FUTURE per-file `install` → `cp -a src/.` change there would reintroduce the class.

**Tier-0 note:** `tests/deploy_genlock_fleet.rs` asserts the EMITTED-program TEXT
(`build_imag_deploy_program`), so prove RED→GREEN locally by sourcing the script, calling the builder,
and grepping the emitted text (no cargo). The emitted program uses an UNQUOTED `cat <<EOS` heredoc —
escape every emitted `$` as `\$` and use NO backticks/`$(...)` in comments (they substitute at emit
time); `\0` in `-printf '%P\0'` passes through untouched. Verify the EMITTED program with `bash -n`.

## Report-only per-box forced-table AUDIO/yuv audit preflight (#1303 part 4)

`deploy-genlock-fleet.sh` emits a REPORT-ONLY audit step BEFORE STEP 0 of every box's plan
(`emit_forced_table_audit_preflight BOX HOST`, `#`-comment guidance like the resolume STEP -1
identity note). It exists because the fork's certified `GENLOCK_FORCED_SETTINGS` values were read
off a CAMERA input, so a genlock build onboarded onto a PROGRAM-audio box (cg OBS: `sp-*`/`cg`/music
inputs) silently shipped `ndi_audio=false` and only surfaced on air (2026-09-13 event morning,
#1295/#1303). The preflight directs the supervisor to enumerate the box's live NDI inputs over
OBS-WS (reusing `latency_pins_verify.py`'s `_conn` + `GetInputList` + `GetInputSettings`) as a
`name<TAB>ndi_audio<TAB>yuv_range<TAB>yuv_colorspace` TSV and pipe it into the classifier
`scripts/lib/genlock-forced-table-audit.sh`'s `genlock_forced_table_audit BOX`, then act on every
`MISMATCH-*` row (and any yuv NOTE) over OBS-WS BEFORE the swap.

- **The classifier is a byte-for-byte bash REPLICA of the canonical Rust table**
  `src/genlock_forced_table_audit.rs` (the `cg-chain-verify.sh` shell-replica-pinned-to-Rust idiom);
  `tests/genlock_forced_table_audit_1303.rs` pins the two together over an 88-vector parity set, so
  a change to the per-box-class expectations must land in BOTH (the parity test fails otherwise).
- **Report-only, never a gate.** The preflight prints and never writes; the audit function ALWAYS
  returns 0 even on a mismatch, so it can never block a deploy (the ticket's own hard constraint,
  and the "inform, don't force" / "latency is the operator's A/V-align domain" repo rules — an
  operator's legitimate per-source `ndi_audio`/`yuv` choice must not be overridden by the deploy).
- **The emit step + the `--plan` tail invariant are anchored** by
  `tests/deploy_genlock_fleet.rs`: `plan_emits_forced_table_audit_preflight_before_step0_1303`
  (one preflight per box, before STEP 0, references the classifier, "NEVER writes and NEVER gates")
  and — for the sibling issue-1295 planner nit — `plan_last_nonempty_line_is_exit_0_and_no_bare_log_record_1295`
  (a saved `--plan` .ps1 is parsed whole in file mode, so the plan ends on a clean `exit 0` and the
  fleet-log record is emitted as a `#`-comment, never a bare trailing tab record that breaks the
  parse).

## A dev push CANCELS every in-flight run on ancestor commits — dispatch the genlock builds LAST (16.9.2026)

The airuleset `post-push-ci-cleanup` hook cancels every run whose commit is an ancestor of the new dev HEAD, regardless of workflow — including the three manually dispatched genlock builds (`gh workflow run "Windows genlock build …"/"… FAST …"/"Linux genlock build …" --ref dev`) that a harness-only follow-up push does NOT re-trigger (the `paths:` filters skip it). 15.9.2026: ~45 min of bundle builds on cfb59a9cf died to a scripts-only push. Batch rule: integrate + push every ready lane FIRST, then dispatch the builds ONCE, then HOLD further dev pushes (cherry-pick returned lanes locally, push after the bundle is downloaded). A push to `dev` never cancels `main` runs (main's merge commits are not ancestors of dev).

After every fleet relaunch, re-run `scripts/ndi-portmap-audit.sh --check`: the sender-output pin (issue 1185) re-orders strih's NDI ports on relaunch, so the checked-in baseline goes stale and the dev1 port-map watchdog pages (14 pages 15./16.9.) — `--capture` + commit the baseline as part of the deploy sitting.

## GOTCHA — a FAST/full deploy's OBS stop DROPS live WS-applied values that OBS never saved (23.9.2026)

The Windows deploy program stops OBS hard to swap the bytes, so any input setting written over
OBS-WS since OBS last saved its scene collection is lost at the relaunch. Live: the `[8/8g]` apply
had set stream `NDI 2ME PGM genlock_latency_ms_src` 987 + `mbc` audio sync offset +2 (recorded in
`~/.camera-box/av-sync-last.json`), and after the next FAST deploy + relaunch the box came back at
976 / +29 (the values last saved). The #1265 swing guard then compares the next run against a pin
the box is not actually running. After EVERY stream deploy/relaunch: read the live pin + `mbc`
offset over WS and, if they differ from `av-sync-last.json`'s `applied_latency_ms` /
`audio_offset_ms`, re-apply those (read-back verified) before the next E2E.
