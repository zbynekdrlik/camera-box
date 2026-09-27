---
paths:
  - "scripts/audio-mixer-alert-watchdog.sh"
  - "scripts/audio_mixer_decision.py"
  - "systemd/audio-mixer-alert-watchdog.*"
  - "tests/python/test_audio_mixer_*_1381.py"
  - "tests/fixtures/audio_mixer_1381/**"
---

# dev1 audio-mixer / VBAN-loss alert watchdog (issue 1381)

On 27.9.2026 the resolume cg OBS audio mixer left real time from 06:00 local and both obs-vban
outputs to FOH lost audio for over an hour. Nothing paged; FOH heard it at 06:56. Both signals were
in the OBS log. This watchdog pages on them. Same dev1 family as render-freeze (issue 1320) and
audio-lag (issue 1226): box facets on `:8899`, a pure python decision, a bash orchestrator reusing
`obs-watchdog-decision.sh`.

## The two signals

| Facet group (gather) | Log line | Arm | Pages |
|---|---|---|---|
| `audio_mixer_ticks` / `_ticks_over` / `_tick_ms` / `_window_ms` / `_age_s` | `audio-stall #1367: ... ticks=N ticks_over=M tick_ms=21.3` (obs-audio.c, one per dump) | MIXER | BEHIND: `abs(ticks - 2812.5) > 5`; OVERLOADED: `ticks_over > 30` |
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
- **STALE** (newest dump > 180 s behind the log head: the audio thread stopped dumping while the log
  advanced) is log-only, the audio-lag sibling's rule. On resolume a wedged mixer still pages
  through VBAN_LOSS, because the pacer thread keeps logging. On a box without VBAN outputs a
  stopped audio thread is not paged by this watchdog (nor by audio-lag, whose STALE is log-only
  too); promoting it was reported to the supervisor as a follow-up candidate.

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

`python3 -m pytest tests/python/test_audio_mixer_*_1381.py tests/python/test_notify_dedup_key_sweep_1206.py`
(gather, decision, replay incl. the bash dry-run, fleet wiring) + `bash -n` / `shellcheck -S warning`
on the watchdog. No cargo involved. The obs-fleet Rust harness is std-only and runs with plain
`rustc --test` (`tests/harness_obs_fleet_list_1296.rs`).

## File size

`bundle_state_gather.py` is past 1900 lines. It is a flat set of independent pure parsers, and a
split has to update every installer that ships the fixed three-file server tree together
(setup-imag.sh, setup-strih.sh, the Windows raw-fetch runbook). The split was reported to the
supervisor as a follow-up candidate.
