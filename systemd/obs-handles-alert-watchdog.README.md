# obs-handles-alert-watchdog — install note (issue 1406)

The dev1-side alert watchdog (`scripts/obs-handles-alert-watchdog.sh`) pages when an **OBS process
leaks handles**. On 5.10.2026 the stream obs64 held **4,066,772 handles** after ~22 h. The
third-party Audio Monitor plugin's output listed an absent Focusrite endpoint, and every audio tick
it opened that endpoint's registry key without closing it: +46.875 handles/s, 168,750/h. Kernel
paged pool reached 1.7 GB. At that rate OBS reaches the 16,777,216 per-process cap ~100 h after its
start, during the next production. Nothing read a handle count; the leak was found by accident.

It reads the `obs_handles*` facet `bundle_state_gather` exposes on each box's
`:8899/bundle-state.json`:

- **`obs_handles`**: the OBS process's handle count. On Windows it comes from ONE
  `NtQuerySystemInformation(SystemProcessInformation)` snapshot, which opens no process and needs
  no pid lookup (an open of the elevated obs64 from the non-elevated BundleStateServer task is not
  verified live; issue 1067 saw one denied). On Linux (strih-lx) it is the count of
  `/proc/<obs pid>/fd`.
- **`obs_handles_pid` + `obs_handles_start`**: the pid and start epoch.
- **`obs_handles_run`** (Linux only): `<boot id>:<start ticks>`, the process identity. A wall-clock
  step moves the start epoch on Linux, never the run token. A new identity is an OBS restart and
  re-baselines the watchdog (Windows: pid + start epoch, which never moves).
- **`obs_handles_limit`** (Linux only): the soft open-files limit, the Linux cap.
- The keys are omitted when no OBS process is readable, never a false 0.

`scripts/obs_handles_decision.py` grades one pass's reading against the reference sample this
watchdog stored on its previous pass:

- **GROWING**: the count grew by ≥ **5,000 handles/h** over the pass interval (at most half a Linux
  box's soft open-files limit per hour: 512/h under 1024). That is 34× below the 5.10 leak rate and
  ~13× above the healthy wobble's rate. It pages after **3** consecutive GROWING passes (~15 min), so
  a one-off step (a scene loading sources, an NDI reconnect) reads GROWING once, HEALTHY next, and
  never pages.
- **CEILING**: the count is ≥ **500,000** (86× a healthy ~5,790, 3% of the Windows cap), or ≥ 80%
  of a Linux box's soft open-files limit when that is lower. It pages after **2** passes.
- **BASELINE**: the first reading of a process, or a restart (a new identity). The alarm clears
  (machine-channel recovery line).
- **REBASE**: the same process with an interval ≤ 0 (the dev1 clock stepped back). Only the
  reference resets; the confirm is kept and no recovery is logged.
- **HOLD**: under 240 s since the reference (a manual run between timer passes). The older reference
  and the confirm are kept, also over the ceiling, so a short interval never inflates the rate.
- **SKIP** (`:8899` not fetchable) and **UNKNOWN** (facet absent) never page. The bundle-state and
  network-reach watchdogs own a dark box.

Roster: the obs-fleet **`obs-handles`** facet (`strih-lx stream resolume`). resolume travels, so it
is polled only while home (`obs_fleet_poll_now`).

Replay proof (`tests/python/test_obs_handles_watchdog_1406.py`, the 4./5.10 readings in
`tests/fixtures/obs_handles_1406/`):

- the leak from a fresh OBS pages GROWING on the 4th pass (15 min after the baseline);
- the 5.10 census (4,066,772) pages CEILING on the 2nd pass;
- the post-fix readings (5,806 → 5,791) never page;
- a restart after a page logs a machine-channel RECOVERY;
- a manual run 60 s after a ceiling pass HOLDs and does not page early;
- a strih-lx fd leak under a 1024 limit pages through a one-second clock step of its start epoch.

## PRODUCTION-CRITICAL: time-bucketed re-ping (issue 1308)

The page carries a **time-bucketed** `--dedup-key` (`watchdog_notify_key "obs-handles-<box>"`). It
is on the `_PRODUCTION_CRITICAL_TIME_BUCKETED` allowlist in
`tests/python/test_notify_dedup_key_sweep_1206.py`. The throttle re-fires once per ~1 h
(`OBS_HANDLES_ALERT_THROTTLE_PASSES`, 12 passes) while the box keeps paging; the signature is the
box, so GROWING → CEILING of one leak is not a new incident.

## DETECTION ONLY — ships DISABLED

The cure is an owner call: find the leaking plugin or device, and plan an OBS restart (on 5.10:
remove the absent endpoint from the Audio Monitor output, then relaunch through the canonical
path). So the watchdog is alert-only and recovery is a machine-channel log line. The units are
committed but **not installed and not enabled**. The SUPERVISOR enables them after the
bundle-state server carrying the facet is deployed to the boxes.

## Supervisor install + live-verify procedure

```bash
# 0. Redeploy EVERY file in scripts/lib/bundle-state-files.txt to each box (issue 1386: the server
#    imports all of them), then restart the server:
#    - stream (+ resolume while home): the box fetches each file from GitHub at the merged sha into
#      C:\ProgramData\camera-box\ (the .claude/skills/genlock runbook), then the python child is
#      restarted (the run-bundle-state-server.ps1 loop restarts it in 5 s).
#    - strih-lx: re-run setup-strih.sh step 9's install loop (or the whole setup-strih.sh --box
#      strih-lx), then `systemctl --user restart strih-bundle-state-server.service` as newlevel.
#    Then confirm the facet is served on each box:
curl -s http://10.77.9.204:8899/bundle-state.json | python3 -m json.tool | grep obs_handles
curl -s http://10.77.9.202:8899/bundle-state.json | python3 -m json.tool | grep obs_handles
curl -s http://resolume.lan:8899/bundle-state.json | python3 -m json.tool | grep obs_handles
#    HARD ACCEPTANCE before step 2, per box:
#    - stream: obs_handles ~5,790 (the post-fix count), obs_handles_pid = the obs64 pid.
#    - strih-lx: obs_handles = `ls /proc/$(pgrep -x obs)/fd | wc -l` read on the box,
#      obs_handles_limit = the soft "Max open files" of /proc/<pid>/limits, and obs_handles_run =
#      `cat /proc/sys/kernel/random/boot_id`:<field 22 of /proc/<pid>/stat>. If the count is above
#      80% of that limit, raise LimitNOFILE in strih-obs.service first, or the watchdog pages CEILING
#      from its first passes. Under a 1024 limit a fast fd leak can still reach EMFILE before the
#      page lands (.claude/rules/obs-handles-watchdog.md); a large LimitNOFILE is the durable cure.
#    - An absent facet on Windows = the snapshot failed: read the server log
#      (C:\ProgramData\camera-box\bundle-state-server.log) for the obs_handles WARNING.

# 1. Dry-run against the LIVE rig (read-only; no page). Run it twice >= 4 min apart: the first pass
#    is BASELINE, the second must read HEALTHY on every box.
scripts/obs-handles-alert-watchdog.sh --dry-run

# 2. Install the units and enable the timer:
mkdir -p ~/.config/systemd/user
cp systemd/obs-handles-alert-watchdog.service systemd/obs-handles-alert-watchdog.timer \
   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now obs-handles-alert-watchdog.timer

# 3. Confirm it fires and reads the facet (never status=203/EXEC):
systemctl --user list-timers | grep obs-handles
journalctl --user -u obs-handles-alert-watchdog.service -n 40 --no-pager
```

Offline replay of a captured body: `OBS_HANDLES_FETCH_CMD=<script printing the JSON>
OBS_HANDLES_NOW_EPOCH=<epoch> OBS_HANDLES_ALERT_STATE_FILE=<scratch>
scripts/obs-handles-alert-watchdog.sh --dry-run`.
