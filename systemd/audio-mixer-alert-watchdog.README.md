# audio-mixer-alert-watchdog — install note (issue 1381)

The dev1-side alert watchdog (`scripts/audio-mixer-alert-watchdog.sh`) pages when an OBS **audio
mixer leaves real time** or an **obs-vban sender loses audio**. On 27.9.2026 the resolume cg OBS
mixer fell behind from 06:00 local and both VBAN outputs to FOH dropped audio for over an hour;
nothing paged and FOH heard it at 06:56. Both signals were already in the OBS log.

It reads two facet groups `bundle_state_gather` exposes on each box's `:8899/bundle-state.json`
(from the SAME bounded head+tail log read — no second scan):

- **`audio_mixer_*`** — the newest complete `audio-stall #1367` dump: `ticks`, `ticks_over`,
  `tick_ms`, `window_ms` (context), `age_s`. The dump is written once per 60 s on the audio
  thread's own clock, so `ticks` IS the per-minute rate (2812.5 = real time at 48 kHz).
  - **BEHIND** — `|ticks − 2812.5| > 5` (a surplus pages too: the mixer catching up in bursts).
  - **OVERLOADED** — `ticks_over > 30` (late ticks, gap > 1.5 ticks).
  - **STALLED** (issue 1385) — the newest dump is > 180 s behind the log head AND the log head is at
    most 60 s old on the box's own clock (`obs_log_head_age_s`): the audio thread stopped while OBS
    keeps logging — silence on air on a box without VBAN outputs.
- **`vban_pacer_*`** — per destination, how much the obs-vban pacer's loss counters grew inside the
  last 660 s of the log (two passes plus slack): underflows / overflows / trims on the shipped
  pacer, discontinuities / repays / resyncs + silence_ms / discarded_ms on the fixed-timeline
  pacer. `late_sends` is not a loss. A `dest=` line is keyed on destination + stream and, while
  it is one sender, graded by a plain per-line delta (two senders sharing it fall back to the
  method below). The shipped line has no destination and the two outputs print identical
  lines, so a loss is a counter tuple never seen before that dominates one seen earlier (a clean
  or restarted output never produces one). That under-counts only, except the one residual
  stated in `.claude/rules/audio-mixer-watchdog.md`.
  - **VBAN_LOSS** — any loss counter moved.

Roster: the obs-fleet **`audio-mixer`** facet (`strih-lx stream resolume`). resolume travels, so it
is polled only while home (`obs_fleet_poll_now`); a box without VBAN outputs reads the VBAN arm
UNKNOWN.

## It never false-pages

- A normal OBS start has one partial dump (`ticks=1`) → **UNKNOWN**, never BEHIND.
- The count is graded as dumped, never rescaled by the wall-clock interval between log lines: the
  dantesync nightly date step (02:00 UTC) stretches that interval (60.193 s on 27.9) while the
  mixer is in real time.
- `:8899` not fetchable → **SKIP** (the bundle-state / network-reach watchdogs own that page).
- **STALE** (an old dump with no proof the log is live NOW — OBS down or hung, or a gather without
  `obs_log_head_age_s`; or the pacer line stopped) is logged on the machine channel only, never
  paged. obs-liveness / bundle-state own a dead OBS.
- **CLOCK (once, stable key):** the OBS log advances between passes, yet its head reads old -- the
  OBS log stamps and the gather clock disagree (time zone), so the mixer arm is blind on that box.
  One `⚠️` page per incident (`audio-mixer-clock-<box>`), recovery machine-channel only.
- 2 confirmations before a page; a HEALTHY pass clears the arm, and on the mixer arm a STALE pass
  resets the pending confirmation (issue 1385: the date-less log of a dead OBS reads live ~70 s once
  a day, and a held confirm would pair two such days into a false STALLED). A frozen log (head
  older than 180 s) reads STALE and never grades its last counts.

Replay proof (`tests/python/test_audio_mixer_replay_1381.py`, real 27.9 resolume log excerpts):
03:00–04:15, a normal OBS start and the clean window after the 716 deploy never grade a paging
verdict at any pass phase; the 06:00 onset pages VBAN_LOSS by the second pass (≤ 06:10) and the
mixer arm by 06:16.

## PRODUCTION-CRITICAL — time-bucketed re-ping (issue 1308)

Both arms carry a **time-bucketed** `--dedup-key` (`watchdog_notify_key "audio-mixer-<box>"` /
`"vban-loss-<box>"`) — it is on the `_PRODUCTION_CRITICAL_TIME_BUCKETED` allowlist in
`tests/python/test_notify_dedup_key_sweep_1206.py`. The throttle re-fires once per ~1 h
(`AUDIO_MIXER_ALERT_THROTTLE_PASSES`, 12 passes) while the same box + arm keeps firing.

## DETECTION ONLY — ships DISABLED

The cure (an OBS restart, the dock / pacer fixes of issue 1381) is a supervisor/owner call, so the
watchdog is alert-only and recovery is a machine-channel log line. The units are committed but
**not installed and not enabled** — the SUPERVISOR does that after the bundle-state server carrying
the new facets is deployed to the boxes.

## Supervisor install + live-verify procedure

```bash
# 0. Deploy the bundle-state server files that carry the new facets to each box (the usual
#    bundle-state-server.py + bundle_state_gather.py redeploy + BundleStateServer task / unit
#    restart), then confirm the facets are served:
curl -s http://resolume.lan:8899/bundle-state.json | python3 -m json.tool | grep -E 'audio_mixer|vban_pacer|obs_log_head'
curl -s http://10.77.9.204:8899/bundle-state.json | python3 -m json.tool | grep -E 'audio_mixer|obs_log_head'
curl -s http://10.77.9.202:8899/bundle-state.json | python3 -m json.tool | grep -E 'audio_mixer|obs_log_head'
#    issue 1385 HARD ACCEPTANCE before step 2: `obs_log_head_age_s` must read 0-15 s on EACH box
#    (the STALLED proof). Near a whole number of hours = OBS and the gather disagree on the time
#    zone: fix the service environment first (the watchdog would page CLOCK and stay blind).

# 1. Dry-run against the LIVE rig (read-only; no page):
scripts/audio-mixer-alert-watchdog.sh --dry-run
#    Expect mixer=HEALTHY on every box, vban=HEALTHY on resolume (UNKNOWN on strih-lx / stream),
#    and SKIP / "away" for a dark box.

# 2. Install the units and enable the timer:
mkdir -p ~/.config/systemd/user
cp systemd/audio-mixer-alert-watchdog.service systemd/audio-mixer-alert-watchdog.timer \
   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now audio-mixer-alert-watchdog.timer

# 3. Confirm it fires + reads the facets:
systemctl --user list-timers | grep audio-mixer
journalctl --user -u audio-mixer-alert-watchdog.service -n 40 --no-pager
```

Offline replay of a captured body: `AUDIO_MIXER_FETCH_CMD=<script printing the JSON>
AUDIO_MIXER_ALERT_STATE_FILE=<scratch> scripts/audio-mixer-alert-watchdog.sh --dry-run`.
