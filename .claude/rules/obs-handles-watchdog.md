---
paths:
  - "scripts/obs-handles-alert-watchdog.sh"
  - "scripts/obs_handles_decision.py"
  - "scripts/bundle_state_host.py"
  - "scripts/bundle_state_windows.py"
  - "systemd/obs-handles-alert-watchdog.*"
  - "tests/python/test_obs_handles_*.py"
  - "tests/fixtures/obs_handles_1406/**"
---

# OBS handle-leak facet + dev1 alert watchdog (issue 1406)

On 5.10.2026 the stream obs64 held **4,066,772 handles** after ~22 h. The cause was the third-party
Audio Monitor plugin (`audio-monitor.dll`, exeldro, not vendored): its output 1 listed an absent
Focusrite endpoint (DeviceState NOTPRESENT). Every audio tick it opened that endpoint's
`MMDevices\...\Properties` registry key and never closed it: +46.875 handles/s (48000/1024),
168,750/h, kernel paged pool 1.7 GB. At that rate OBS reaches the 16,777,216 per-process cap ~100 h
after its start, i.e. during the next production. Nothing in camera-box read a handle count; the
leak was found by accident. The live fix (issuecomment-5992943363) removed the dead endpoint. This
facet + watchdog are the generic guard for the NEXT leaker, whatever plugin or device it is.

## The facet: `obs_handles`, `obs_handles_pid`, `obs_handles_start`, `obs_handles_run`, `obs_handles_limit`

- **Windows** (`bundle_state_windows.windows_obs_handles`): ONE
  `NtQuerySystemInformation(SystemProcessInformation)` call through ctypes. It returns every
  process's `HandleCount`, pid and create time, opens NO process and needs no pid lookup first.
  `GetProcessHandleCount` would need an open of the elevated obs64 from the non-elevated
  BundleStateServer task. Its `PROCESS_QUERY_LIMITED_INFORMATION` open is normally granted, but
  that is not verified live, and issue 1067 showed an open of obs64 denied in this task's context
  (`Get-Process .Path`, a heavier access). A CIM read would cost a PowerShell cold start on EVERY
  request, and a changing count cannot be cached (`bundle-state-gather-latency.md`). The call
  costs a few ms, no subprocess.
  - The PURE parser `bsg.system_processes_from_spi(raw, base_addr)` lives in `bundle_state_host.py`;
    only the ctypes call is Windows-side. Image-name pointers in the buffer are ABSOLUTE, so the
    parser takes the buffer's address.
  - `bsg.SPI_OFFSETS` are the x64 layout: `next` 0, `threads` 4, `create_time` 32, ImageName 56/64,
    `pid` 80, `handles` 96. `test_the_parser_offsets_match_the_documented_c_layout` rebuilds the C
    struct with explicit-width ctypes types (identical on LP64 Linux and Windows x64) and pins them.
    A 32-bit interpreter is refused (WARNING, facet omitted).
  - A short or malformed buffer is `None` (unreadable), never a partial list that could miss obs64.
  - A process with 0 threads has exited (a zombie the system still lists) and never counts.
- **Linux** (`bsg.linux_obs_handles(proc_root, clk_tck)`): the OBS process is comm `obs`
  (`strih-obs.service` runs `/usr/bin/obs`; `pgrep -x obs`). The count is `/proc/<pid>/fd`; the
  start epoch is `btime` + `/proc/<pid>/stat` field 22 (read after the LAST `)`); `obs_handles_limit`
  is the soft `Max open files` of `/proc/<pid>/limits` (`unlimited` = none). The server runs as the
  same user as OBS on strih-lx (both `--user` units of the desktop user), so the fd directory is
  listable; another user's process is skipped.
- **The Linux identity is `obs_handles_run` = `<boot id>:<start ticks>`, never the start epoch**
  (review round 1). The kernel's `btime` moves on every wall-clock step, and strih-lx steps its
  clock as the dantesync date master, so `btime + ticks` can shift by a second on a live process:
  the old identity re-baselined a leak mid-confirm and logged a false RECOVERY. The run token is
  the OBS-run identity `scripts/strih_browser_keeper.py` already uses; `proc_stat_start_ticks` is
  pinned to its `proc_start_ticks` by a parity test (the two live in separately deployed trees, so
  the parse is duplicated on purpose, never imported). Windows serves no run token: its create time
  is recorded once and never moves, so `<pid>@<start epoch>` is stable there.
- The OBS-shaped name test is the ONE shared `bsg.OBS_PROCESS_NAME_RE`. Two OBS processes: the one
  with the most handles wins.
- Omitted when no OBS process is readable: absent = UNKNOWN downstream, never a false 0. The server
  gathers it on BOTH platforms in `_obs_handles_facets()` (outside the Windows identity gate), timed
  as `obs_handles`; the keys are the LAST four of `BUNDLE_STATE_KEYS`.
- **No new deployed file**: everything lives in modules already in `scripts/lib/bundle-state-files.txt`.
  A deploy still means redeploying the WHOLE list (issue 1386).

## The decision (`scripts/obs_handles_decision.py`)

The server is a stateless reader; the watchdog keeps the reference sample (`ident`, handles, pass
epoch) per box in its state file and the pure decision grades one pass against it:

| verdict | when (in this order) | page |
|---|---|---|
| SKIP | `:8899` not fetchable | never (bundle-state / network-reach own a dark box) |
| UNKNOWN | facet absent or garbled (no pid = unusable: the restart detection needs it) | never |
| BASELINE | first reading, or a new identity (an OBS restart) | clears the alarm |
| REBASE | same process, interval <= 0 (dev1 clock stepped back, two runs in one second): the reference resets | confirm kept, no recovery |
| HOLD | same process, < 240 s since the reference (a manual run between passes): the older reference is KEPT, also over the ceiling | confirm kept |
| CEILING | handles >= `min(500,000, 0.8 x soft limit)` | after 2 passes |
| GROWING | >= `min(5,000, 0.5 x soft limit)` handles/h over this ONE interval | after 3 consecutive passes (~15 min) |
| HEALTHY | otherwise | clears the alarm |

Calibration (the 5.10 readings): healthy stream ~5,790 flat (+-16 in 150 s, ~384/h as a rate);
the leak 168,750/h.
- 5,000/h is 34x below the leak and ~13x above that healthy wobble's rate (~417 handles per 5-min
  pass).
- A one-off step (a scene loading sources, an NDI reconnect) is GROWING once, then HEALTHY. It never
  reaches 3 consecutive passes, which is why growth is graded PER INTERVAL with a 3-pass confirm and
  not over a sliding window (a step would sit inside a window for several passes).
- The ceiling 500,000 = 86x healthy, 3% of the Windows cap; the 5.10 leak crossed it ~3 h after start.
- On Linux the cap is the soft RLIMIT_NOFILE, so 80% of it is the ceiling there. `strih-obs.service`
  sets no `LimitNOFILE` and OBS raises nothing itself (no `RLIMIT_NOFILE` in `vendor/obs-studio`), so
  the soft limit is whatever the user manager hands out (systemd's usual default is 1024; NOT read
  live yet). **Read strih-lx's live fd count + soft limit BEFORE enabling**: a count already above
  80% pages CEILING from the first passes (a real headroom problem, cure = `LimitNOFILE` in
  `strih-obs.service`).
- The growth bound is scaled to that limit too (review round 1): at most half the soft limit per
  hour, so 512/h (~43 fds per pass) under 1024. A fixed 5,000/h is ~5x a 1024 limit per hour, and a
  Linux leak would hit EMFILE before paging. Honest limit under 1024: from a ~600-fd baseline the
  headroom is ~424 fds, so a leak faster than ~1,700/h reaches EMFILE within the ~15 min the 3-pass
  growth confirm (or the ceiling's crossing + 2 passes) takes. A large `LimitNOFILE` (524288 keeps
  the 5,000/h bound and Windows-scale headroom) is the durable cure -- a supervisor follow-up, not
  done here.
- Both inputs follow the quality-gated-input rule of `watchdog-notify-dedup.md`: the growth is a rate
  over one pass interval, never a since-start quantity; the ceiling is the current level.

## The watchdog (`scripts/obs-handles-alert-watchdog.sh`)

- The audio-mixer sibling shape: `fetch_bundle_json "$ip" OBS_HANDLES_FETCH_CMD`,
  `obs_watchdog_confirm` with a VERDICT-dependent threshold (GROWING 3, CEILING 2; the counter runs
  across GROWING -> CEILING), `obs_watchdog_alert_throttle` signed by the BOX (not the verdict),
  recovery via `recovery_latch_fires` on BASELINE/HEALTHY. HOLD / REBASE / SKIP / UNKNOWN leave the
  confirm alone (a REBASE is the same process, never a restart, so it never logs a recovery).
- PRODUCTION-CRITICAL class: `--dedup-key "$(watchdog_notify_key "obs-handles-<box>" "$now_epoch")"`,
  allowlisted in `tests/python/test_notify_dedup_key_sweep_1206.py`.
- Roster: obs-fleet facet `obs-handles` = strih-lx stream resolume; resolume via `obs_fleet_poll_now`.
  Timer on the `watchdog-roster.sh` handover roster (`:core`).
- Tier-0 seams: `OBS_HANDLES_FETCH_CMD` (the body), `OBS_HANDLES_NOW_EPOCH` (the pass time),
  `OBS_HANDLES_ALERT_STATE_FILE`, `AIRULESET_NOTIFY` (a fake notify for a real-pass test).
- `tests/python/test_obs_handles_watchdog_1406.py` replays the 4./5.10 readings
  (`tests/fixtures/obs_handles_1406/stream-5-10-readings.json`) pass by pass through the real script.

## Not done (optional in the design)

The dominant handle TYPE is not reported: it needs the system-wide handle table
(`SystemExtendedHandleInformation`), which held millions of entries on 5.10 -- not cheap per request.
When this watchdog pages, read the type on the box by hand (the 5.10 census: NtQueryInformationProcess
class 51 + NtQueryObject on duplicated handles).

## Rollout (supervisor; the lane never touched a box)

`systemd/obs-handles-alert-watchdog.README.md` holds the runbook: redeploy the whole server file
list to stream, strih-lx (and resolume while home), confirm the facet per box (stream ~5,790; strih-lx
equal to `ls /proc/$(pgrep -x obs)/fd | wc -l`), two dry-runs >= 4 min apart (BASELINE, then
HEALTHY), then enable the timer and read its first pass (never `status=203/EXEC`).
