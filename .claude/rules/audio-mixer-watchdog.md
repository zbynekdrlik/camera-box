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
| `vban_pacer_loss_events` / `_loss_ms` / `_loss_dest` / `vban_pacer_outputs` / `vban_pacer_age_s` | `[obs-vban] obs-vban pacing: ...` (every 10 s per output) | VBAN | VBAN_LOSS: any loss counter moved in the window |

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
  through VBAN_LOSS, because the pacer thread keeps logging. On a box without VBAN, STALE alone is
  not paged (a follow-up candidate, see below).

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
- **The shipped line has no destination and the two resolume outputs print identical-looking lines
  at the same millisecond.** `_vban_pick_track` splits them into tracks: a line continues the live
  track (fed within 25 s, not already fed within 5 s of this logging period) whose counters it
  continues monotonically, nearest by L1, ties to the older track. With no loss every line equals
  its own track exactly, so the delta is 0 BY CONSTRUCTION — the never-false-page property. A counter
  drop (output restart, `pacing-config ... counters=reset`) starts a new track whose first line is
  only a baseline. A mis-assignment under loss can only move increments between tracks of the same
  key; the key total stays right.
- `vban_pacer_loss_dest` = the worst key (events first, then ms): `ip:port` on the new line,
  `stream=<name>` on the shipped one. `vban_pacer_outputs` = tracks that logged within 25 s of the
  log head (context; a stopped output drops out).
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

## Open follow-ups (not done here)

- A stopped audio thread on a box WITHOUT VBAN outputs reads STALE (log-only) here and in audio-lag.
  Promoting it to a page is a separate decision.
- `bundle_state_gather.py` is past 1900 lines. A split needs every installer that ships the fixed
  three-file server tree (setup-imag, setup-strih, the Windows raw-fetch runbook) updated together.
