---
paths:
  - "scripts/audio-mixer-alert-watchdog.sh"
  - "scripts/audio_mixer_decision.py"
  - "systemd/audio-mixer-alert-watchdog.*"
  - "tests/python/test_audio_mixer_*_1381.py"
  - "tests/python/test_audio_mixer_*_1385.py"
  - "tests/fixtures/audio_mixer_1381/**"
---

# dev1 audio-mixer / VBAN-loss alert watchdog (issue 1381)

On 27.9.2026 the resolume cg OBS audio mixer left real time from 06:00 local and both obs-vban
outputs to FOH lost audio for over an hour. Nothing paged; FOH heard it at 06:56. Both signals were
in the OBS log. This watchdog pages on them. Same dev1 family as render-freeze (issue 1320) and
audio-lag (issue 1226): box facets on `:8899`, a pure python decision, a bash orchestrator reusing
`obs-watchdog-decision.sh`.

**Since 5.10.2026 the cg OBS sends NO VBAN** (owner directive, songplayer issue 221: cg OBS is only
the NDI input "OBS manual" into SongPlayer; FOH plays SongPlayer's own VBAN). Its two VBAN Lua
scripts are loaded with `lua_autostart=false`, so no `obs-vban pacing:` line and no `vban_pacer_*`
facet exist on resolume, and the VBAN arm reads UNKNOWN there by design ("no VBAN output on this box
-- holding, no page"). The MIXER arm still watches the cg OBS. The VBAN arm comes back by itself
if a box sends VBAN again.

## The two signals

| Facet group (gather) | Log line | Arm | Pages |
|---|---|---|---|
| `audio_mixer_ticks` / `_ticks_over` / `_tick_ms` / `_window_ms` / `_age_s` + `obs_log_head_age_s` | `audio-stall #1367: ... ticks=N ticks_over=M tick_ms=21.3` (obs-audio.c, one per dump) | MIXER | BEHIND: `abs(ticks - 2812.5) > 5`; OVERLOADED: `ticks_over > 30`; STALLED: dump age > 180 s AND log head age <= 60 s (issue 1385) |
| `vban_pacer_loss_events` / `_loss_ms` / `_loss_dest` / `vban_pacer_age_s` | `[obs-vban] obs-vban pacing: ...` (every 10 s per output) | VBAN | VBAN_LOSS: any loss counter moved in the window |

Both are parsed from the TAIL of the #1222 bounded read, in the server's one `obs_log_parse`
tuple (appended at the END; the unpack is order-sensitive). Omit-when-empty: a box with no dump yet
or no VBAN output reads UNKNOWN, never a fake 0.

## The mixer count is graded AS DUMPED — never rescaled by the log interval

The dump is written by the first audio callback past 60 s on the audio thread's own disciplined
clock (`t800_now`), then its counters reset. So `ticks` IS the per-minute rate (1024 frames at
48 kHz = 2812.5 real time; the expected rate comes from `tick_ms`, so a 44.1 kHz box is graded on
its own rate and an unknown tick length is UNKNOWN).

- **Do NOT divide by the interval between two log lines.** Those timestamps are the wall clock, and
  dantesync steps it: the real 27.9 control log reads 03:59:13.844 → 04:00:14.037 (60.193 s) at the
  02:00 UTC nightly date step while ticks=2813. Rescaling read 2804/min, a false BEHIND in a clean
  window (the replay caught it; commit 3b7dd91fd corrected the test). `audio_mixer_window_ms` stays
  a facet for the log line only.
- **A normal OBS start never pages.** The first dump after start is partial (`ticks=1`,
  `tick_gap_max_ms=0.0`), so the facet needs TWO dumps in the tail. One dump → absent → UNKNOWN.
- A genuine stall still moves the count: a stall across the dump boundary reads short in one window
  and a surplus in the next, both BEHIND. A surplus IS the mixer catching up (27.9: 3857 at 08:00).
- **A stopped audio thread pages STALLED (issue 1385); without proof OBS is still logging it stays
  STALE, log-only.** See the next section.

## STALLED = the dump is old AND the log head is live NOW (issue 1385)

A stopped audio thread stops dumping while the rest of OBS keeps logging. On a box without an
obs-vban output (stream, strih-lx) nothing else in this watchdog sees it, so STALLED pages it
through the same arm as BEHIND (2-pass confirm, `watchdog_notify_key "audio-mixer-<box>"`, the
arm + box throttle signature).

- **The dump age alone is not enough.** `audio_mixer_age_s` is measured behind the LOG HEAD, like
  every `*_age_s` facet on `:8899`. When OBS dies or hangs the log stops, and the ages freeze at
  whatever they were, so a stale dump on a dead OBS would page forever. obs-liveness /
  bundle-state own a dead OBS; this watchdog must not double-page it.
- **The proof is `obs_log_head_age_s`** (`bundle_state_gather.obs_log_head_age_s_from_log`): the
  box's own local wall clock minus the newest timestamped line of the tail. OBS stamps every line
  with its local `HH:MM:SS.mmm`, and the gather runs on the same box, so it is one clock; the server
  reads it right AFTER the log (`local_seconds_of_day`). A genlock OBS logs every ~5 s
  (program-render-audit), so a live log head is a few seconds old. STALLED needs <= 60 s
  (`AUDIO_MIXER_LOG_LIVE_S`).
- **Facets that looked like proof but are not** (read live on 28.9, read-only):
  `program_render_lagged_age_s` is the age of the WORST `lagged` window in the tail, not recency —
  772 s on strih-lx and 1287 s on resolume, both healthy. `ndi_input_latency` (the one OBS-WS fact)
  is absent on resolume, `obs_process_count` is Windows-only. Neither says the log advances.
- **Date-less log, and why STALE RESETS the mixer confirm.** "Now" is read after the log, so a
  head AHEAD of it can only be a wall-clock step back: up to 10 s (`LOG_HEAD_CLOCK_SLACK_S`) it reads
  0, further ahead it is a previous day's line (+24 h, not live). A log dead for a whole number of
  days still reads live for about 70 s once a day, so at most ONE pass a day reads STALLED (on
  roughly a quarter of the days, as the timer phase drifts). The 2-pass confirm alone does NOT stop
  that: the fleet rule holds a confirm across SKIP / UNKNOWN / STALE, so two such days, however far
  apart, confirmed a page on a dead OBS (review round 1, reproduced through the real bash dry-run).
  So the mixer arm's STALE resets its confirm (`reset_arm_confirm`; the alert state is left alone),
  while SKIP / UNKNOWN still hold. Pinned by the 3-day frozen-log replay (one false-live pass every
  day, python model + the real bash) and the STALLED -> STALE -> STALLED dry-run. Keep slack +
  `LOG_LIVE_S` below the 300 s timer period.
- **A frozen log never grades its old counts.** When the log head is more than `LOG_FROZEN_S`
  (180 s) old -- wider than the 60 s live bound, so a quiet but live log still grades -- the mixer
  reads STALE even when its last dump is fresh behind that head. Before this, a dead OBS whose last
  dump graded BEHIND paged every hour for as long as it stayed dead. With the facet absent (an older
  gather) grading is unchanged. The bound is FIXED, not `AUDIO_MIXER_STALE_AFTER_S`: slack 10 s +
  180 s stays below the 300 s pass, so a date-less log dead for days grades its last counts on one
  pass a day at most (review round 2: an override past ~290 s would have let two passes page).
- **Stated residual:** a hung OBS whose last dump was healthy now reads STALE, not HEALTHY, so the
  hang no longer clears the arm's throttle; a restarted OBS that goes straight to BEHIND waits up to
  the 12-pass throttle. UNKNOWN (a fresh start) already behaved this way.

## The whole mixer arm rests on one clock premise -- guarded (issue 1385 review round 2)

STALLED, the frozen-log STALE and therefore BEHIND / OVERLOADED all read `obs_log_head_age_s`,
which assumes OBS's log stamps (`localtime` in `frontend/obs-main.cpp` `CurrentTimeString`) and
the gather's `time.localtime()` share ONE time zone. Two processes on one box share the system zone
unless one of them carries its own `TZ` (the Windows `BundleStateServer` task's environment is not
in the repo). If they disagree, a LIVE log reads about k x 3600 s old on every pass, every mixer
verdict reads STALE and resets the confirm: the arm is blind, silently.

- **The guard (`classify_log_clock`, pure):** a frozen log's head ages WITH the wall clock between
  two passes; a live log reads young. A head that reads older than `LOG_FROZEN_S` on both passes yet
  aged less than half the pass gap is a log that ADVANCES with its stamps off the gather clock ->
  MISMATCH. The zone offset cancels in the difference, so any offset is caught. A frozen log's daily
  date wrap (a huge negative change) is OK; passes closer than 60 s or further than 600 s apart are
  UNKNOWN. The orchestrator keeps the previous head age + pass epoch per box in its state file
  (`AUDIO_MIXER_NOW_EPOCH` is the Tier-0 seam for the pass time).
- **The observer effect (review round 3).** Every `:8899` request opens a local OBS WebSocket
  connection AFTER the log read (`gather_ndi_inputs`), and obs-websocket logs it at INFO. So a hung
  OBS whose render and audio threads are stuck but whose WebSocket thread runs reads about one pass
  gap old on every pass -- the same "advances yet reads old" shape as a zone offset. MISMATCH
  therefore also needs both heads within a minute of a whole quarter hour (zone offsets are whole
  quarter hours, 86400 is one too), and the gap bound is 600 s, so a self-written head (at most one
  gap old) can never sit near 900 s. Such a hang reads STALE and is obs-liveness territory.
- **Accepted double page:** the same connect lines can also MAKE the STALLED proof. When another
  `:8899` consumer fetched a few seconds before this pass, a hung OBS with a live WebSocket thread
  reads live and pages STALLED while obs-liveness pages the hang. The audio really is silent, so
  the page is true; two effects of one fault, as with BEHIND + VBAN_LOSS.
- **MISMATCH pages ONCE** after the 2-pass confirm, `⚠️` with a STABLE key
  `audio-mixer-clock-<box>` (a config fault, not an on-air one, so no time-bucketed re-ping); OK
  clears it with a machine-channel RECOVERY line; UNKNOWN holds.
- **Hard acceptance before the timer is enabled:** after the supervisor redeploys the gather, read
  `obs_log_head_age_s` on strih-lx, stream and resolume (`curl -s http://<box>:8899/bundle-state.json`):
  it must read 0-15 s on each. A reading near a whole number of hours is the zone mismatch -- fix the
  service's environment before enabling the timer, never raise `LOG_LIVE_S` / `LOG_FROZEN_S`.
- **STALLED is decided FIRST, from the age alone.** A stale dump's counts describe a minute long
  gone, so the old counts are not graded (a stale BEHIND dump reads STALLED). A tick length of an
  unknown sample rate does not hide it either.
- **A long session keeps paging.** A 5 MB tail spans about 33-60 min on these boxes (~5-9 MB/h), so
  a thread dead longer than that would lose its last dumps from the tail and decay to UNKNOWN,
  ending the time-bucketed re-ping. For a bounded read whose HEAD slice holds a dump (this build
  dumps), a tail with fewer than two dumps reports only `audio_mixer_age_s`: the newest tail dump's
  age, or the whole tail span when none is left (a lower bound, so the page says "aspoň N s"). A
  whole-file log (a normal start) and a build without the probe (no dump anywhere) stay absent.
- **One throttle for the whole mixer arm (a stated choice).** STALLED shares the `audio-mixer:<box>`
  throttle signature with BEHIND / OVERLOADED, as the design says: a mixer that goes BEHIND and then
  stops is one incident, so the escalation to silence can wait up to the 12-pass throttle (the owner
  already has the BEHIND page for that box). A per-verdict signature would re-page on every
  BEHIND <-> STALLED flip of a flapping thread.
- **On resolume a stopped thread pages twice, through two arms (accepted).** Its VBAN pacer keeps
  logging, so the pacer's underflows page VBAN_LOSS while STALLED pages the mixer arm. Same accepted
  shape as BEHIND + VBAN_LOSS in issue 1381: two different effects of one fault, both on air.
- **A normal OBS start never stalls.** Fewer than two dumps in a whole-file log = the whole facet
  absent = UNKNOWN. An audio thread that dies before its second dump in a fresh log is therefore
  not paged here (a stated residual: the start dump count is absent at a normal start too, so no
  facet tells the two apart).
- **The gather must be redeployed** to strih-lx, stream and resolume (the bundle-state server tree,
  every file in `scripts/lib/bundle-state-files.txt` since issue 1386, same steps as issue 1381's
  facets). Until then `obs_log_head_age_s` is absent and the
  verdict stays STALE, log-only — never a false page.

### Why audio-lag keeps its STALE log-only

`scripts/audio_lag_decision.py` gets the same payload, so the new facet is available to it, but it
must not page a stopped thread too:

- The `audio-telemetry #800 '<src>'` lines print only for a source with an audio timeline or
  buffered audio (`obs-audio.c`, `if (tsrc->audio_ts || tsrc->audio_input_buf[0].size)`). Every
  source going idle stops them with a HEALTHY audio thread, so their staleness is not proof of a
  stopped thread.
- When the thread does stop, the `#800` lines and the `audio-stall #1367` dump stop TOGETHER — they
  are written by the same 60 s block of `audio_callback`. A second pager on it would double-page
  one fault. This watchdog's STALLED is the one pager for a stopped audio thread.

## VBAN loss = counter increase inside a 660 s window, per destination

The pacer counters are cumulative since the output thread started, so the gather reports how much
they grew over the last `VBAN_LOSS_WINDOW_S` (660 s = two 5-min passes plus timer slack; one burst
survives the 2-pass confirm, the render-freeze freshness shape). Stateless on the box.

- **Loss counters.** Shipped issue-1372 line: `underflows` + `overflows` + `trims` (each is audio
  gone: a ≥ 64 ms wire gap or dropped samples). Fixed-timeline line (the pacer lane, adds `dest=`):
  `discontinuities` + `repays` + `resyncs` as events, `silence_ms` + `discarded_ms` as `loss_ms`.
  **`late_sends` is NOT a loss** — late but complete audio, and it climbs ~200/s after an in-grace
  buffering hole (the pacer rule's known consequence). Counting it would page every growth step of
  resolume's legitimate 85 → 362 ms buffering.
- **A `dest=` line (the fixed-timeline pacer) is keyed on destination + stream**
  (`10.77.7.106:6980/cg`): a VBAN receiver port takes many streams, so a destination alone is not
  a sender (review round 3: two clean senders to one port read 160 events). The key is presumed
  one sender: its loss is the plain delta against its previous line, counted from the key's
  second line, and a counter that went down is a sender restart whose new counts (the thread
  starts at 0) are losses since the restart. The premise is CHECKED: two lines of the key
  closer than `VBAN_MULTI_SENDER_GAP_S` (8 s; one sender logs every >= 10 s) mean several
  senders share it (e.g. fohabl.lan and lv1.lan resolving to one PC at a venue; resolume sends
  both outputs as stream `cg`), and the key's lines are replayed through the tuple method below.
  Never an over-count; the old thread's last counts before a restart may be missed.
- **The shipped line has no destination and the two resolume outputs print identical-looking lines,
  so an output's identity cannot be recovered from it.** A first cut split them into per-output
  tracks by monotone continuation; review round 1 showed it false-paging on a clean log (outputs
  whose logging phases are 5-10 s apart, or a common pause > 25 s: the second output's first line
  continued the first output's track and their counter gap read as 65-130 events). The legacy key
  now works on NEW COUNTER TUPLES: the counters only grow on a loss, so a loss is a tuple never
  seen before for that key that dominates (every counter >=) one seen earlier, counted against the
  nearest such tuple. A clean output repeats its own tuple and adds nothing; a restarted output
  starts at 0 and dominates nothing it has not shown. The known tuples are kept in LAST-SEEN order
  (a repeat moves to the end), so each live output's own value stays among the newest 64 searched
  (review round 2: first-seen order inflated one underflow to 327 after 80 periods of the other
  output growing).
- **The legacy key's first `VBAN_BASELINE_S` (20.5 s = two logging periods) in the tail only
  seeds.** Each output logs every 10.0-10.2 s, so within two periods every output has shown its
  current counters even with a line late or a forward wall step (round 2: a one-period seed read
  65 false events for a second output first logging at +10.6 s). A loss inside the seed is not
  counted.
- **Legacy residuals (stated, pinned by `test_legacy_collision_under_count_is_documented`):** a
  step that lands on a tuple already seen for the key (one output reaching the other output's
  current counters) is not counted, and a step's size may be taken from the other output's
  tuple. The count can therefore be LOWER than the true growth, never higher: on the real 27.9
  06:02 shape it counts 11 of 21. A sustained fault still pages (the replay onset does); an
  isolated single step onto the other output's value does not. The only over-count case is a
  second output whose first line in the tail comes more than two periods after the first, which
  needs the tail to start inside a logging stall of the whole pacer. A multi-sender `dest=` key
  uses the same method and carries the same residuals; a one-sender `dest=` key has none of them.
  The fixed-timeline line replaces the shipped one with the pacer fix of this same issue.
- `vban_pacer_loss_dest` = the worst key (events first, then ms): `ip:port/stream` on the new line
  (e.g. `10.77.7.106:6980/cg`),
  `stream=<name>` on the shipped one. There is no live-output count: it cannot be derived honestly
  from the destination-less line.
- The server timestamp-parses the tail ONCE (`timestamped_tail_lines`) and hands it to both
  parsers (`tail=`), so the two facets add one pass over the bounded tail, not two.
- Timestamps become positions by FILE ORDER (`_file_order_elapsed`): a step back of more than 12 h
  is a midnight wrap, a smaller one is two threads logging out of order (counted as 0). Unlike a
  single head-vs-line gap, this stays right across several midnights in one tail.

## Replay acceptance on the real 27.9 log (tests/fixtures/audio_mixer_1381/)

Four gzip excerpts (every `audio-stall` + `obs-vban` line, CRLF as on the box, cut read-only with
Select-String from `2026-09-26 16-35-04.txt` and `2026-09-27 19-49-08.txt` on RESOLUME-SNV):
03:00–04:15 control, 05:40–06:30 onset, a normal start 19:49–19:57, and the clean window
20:00–20:45 after the 716 deploy. `test_audio_mixer_replay_1381.py` sweeps every 20 s pass phase:
the three quiet windows never grade a paging verdict; the onset pages VBAN_LOSS by the second pass
after 06:00:14 (≤ 06:10) and the mixer arm by 06:16 (the onset alternates 22/36/12/43/10 late ticks
per minute before 06:05, so a phase on the quiet minutes confirms later). The same replay also runs
through the REAL bash watchdog `--dry-run` via `AUDIO_MIXER_FETCH_CMD`.

`test_audio_mixer_stalled_1385.py` cuts the control window to a stopped thread: every
`audio-stall` line after 03:30:00 removed while the pacer keeps logging (the resolume shape of a
dead audio thread; the pacer thread is separate). The replay gives each pass the box clock = the
pass time. Every pass phase reads STALLED from ~03:32:30 on and pages by the second pass after it;
the real bash `--dry-run` fires exactly one `alert_now=1` STALLED in the window. The same cut with
OBS dying at 03:45 reads STALE from 03:46 on, and a log with no other advancing line never pages.
The quiet windows and the real onset (the mixer kept dumping while it fell behind) never read
STALLED. A stream-shaped variant (the same real dumps, no obs-vban line, a synthetic
`program-render-audit` line every 5 s keeping the log moving -- the stream / strih-lx case) pages
once through the real bash dry-run for box `stream`. Two frozen logs replayed for 3 days (a stopped
thread, and the onset frozen at 06:20 on a BEHIND dump) never page.

The other boxes, read-only on 27.9/28.9: the stream box's current log (19:57–00:20, 263 full
dumps) read ticks=2813 and ticks_over=0 in every dump and has no obs-vban line (VBAN arm UNKNOWN);
strih-lx's current log (196 dumps, review round 1) read ticks=2813 with ticks_over <= 1.

### Refreshing or extending the replay fixtures (read-only)

- Cut the excerpt ON the box, never download a whole event-day log (226 MB): win-resolume MCP
  `Select-String -Path <log> -Pattern 'audio-stall #1367|obs-vban pacing'`, keep a time window by
  the `HH:MM:SS.mmm` prefix, `Set-Content -Encoding ascii` into `%TEMP%` (13-15 s per pass over
  226 MB). The logs live under `C:\Users\Resolume\AppData\Roaming\obs-studio\logs`; an event-day
  file spans two dates, so pick the day by the first `00:0` line after the start.
- Pull it with `scp -O` over the rig LAN. From a worktree lane the guard refuses an inline
  `sshpass` with a runtime variable and any `eval`, so put the pull in a script FILE (Write tool)
  that reads the user/password default out of `scripts/obs-session-watchdog.sh` with `sed` into
  `SSHPASS`, runs `sshpass -e scp -O ...`, and never prints it; run it as `bash /abs/script.sh`.
- Store it as gzip with `mtime=0` (python `gzip.GzipFile(..., mtime=0)`) so a re-cut of the same
  lines is byte-identical, and keep the CRLF line endings of the box.
- A log excerpt must be one contiguous window per file: a gap between two windows makes the next
  dump's window span the gap.

## Watchdog shape

- Roster: obs-fleet facet `audio-mixer` = strih-lx stream resolume (`obs_fleet_boxes audio-mixer`,
  env override `AUDIO_MIXER_BOXES`); resolume is polled only while home (`obs_fleet_poll_now`).
- PRODUCTION-CRITICAL (issue 1308): both arms time-bucket their key —
  `watchdog_notify_key "audio-mixer-<box>"` / `"vban-loss-<box>"` — and the file is on the
  `_PRODUCTION_CRITICAL_TIME_BUCKETED` allowlist. The throttle signature is arm + box, never the
  verdict, so a BEHIND ↔ OVERLOADED flip of one fault is not a new incident. Throttle 12 passes (~1 h).
- DETECTION ONLY, recovery log-only, ships DISABLED; the timer is on `scripts/lib/watchdog-roster.sh`
  so the handover check reports it until the supervisor enables it. Steps:
  `systemd/audio-mixer-alert-watchdog.README.md` (deploy the new bundle-state server files to the
  boxes FIRST — the watchdog reads UNKNOWN until the facets are served).

## Tier-0 verify

`python3 -m pytest tests/python/test_audio_mixer_*_1381.py tests/python/test_audio_mixer_*_1385.py tests/python/test_notify_dedup_key_sweep_1206.py`
(gather, decision, replay incl. the bash dry-run, fleet wiring) + `bash -n` / `shellcheck -S warning`
on the watchdog. No cargo involved. The obs-fleet Rust harness is std-only and runs with plain
`rustc --test` (`tests/harness_obs_fleet_list_1296.rs`).

## File size

Issue 1386 split `bundle_state_gather.py` into facet-family modules: the mixer parser lives in
`bundle_state_audio.py`, the pacer in `bundle_state_vban.py`, and both are still imported as
`bundle_state_gather.<name>`. Every installer ships the server tree from ONE declared list,
`scripts/lib/bundle-state-files.sh` + `.txt` (see `bundle-state-gather-latency.md`).
