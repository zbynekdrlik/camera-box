---
paths:
  - "scripts/bundle_state_audio_rx.py"
  - "scripts/lib/program-audio-rx.sh"
  - "scripts/audio-mixer-alert-watchdog.sh"
  - "scripts/audio_mixer_decision.py"
  - "tests/python/test_program_audio_rx_1460.py"
  - "tests/python/test_audio_mix_late_1460.py"
---

# Program-audio RECEIVE loss pager (issue 1460, design 6757370365)

The program audio of every Linux OBS box enters OBS through ONE receiver: the obs-vban SOURCE
(stream-pp `Zvuk z mixu`, strih-pp `ASIO zvuk`, `.claude/rules/obs-vban-receive.md`) or the Dante
`dante_input` source (strih-snv, stream-snv, `.claude/rules/strih-dante-input.md`). Both log their
loss counters every 10 s. Before this pager only verify item 9 and the E2E window gates read them, so
a loss during a production was invisible until someone heard it (owner: zero audio loss, like video).

## Gather: the nested `audio_rx` facet (`scripts/bundle_state_audio_rx.py`)

- **Nested, like `genlock_lock`:** `_parse_log_facets` returns `(facets, nested)`; the server attaches
  every non-None nested object after `build_bundle_state` (the flat every-value-a-string contract of
  the version-integrity gate stays). One entry per receiver, keyed `obs-vban:<input>` / `dante`:
  `kind`, `name`, `events` (gated delta sum + the Dante state faults), `deltas` (gated + named), `pairs`,
  `silent_pairs` (Dante pairs whose `delivered_frames` did not move), `rebases`, `gap_max_s`,
  `age_s` (newest status line behind the log head). Omitted when the tail has no
  receiver status line.
- **Stateless window delta, the obs-vban pacer's 660 s:** a delta is counted only between two status
  lines of ONE receiver that are BOTH inside the window. The obs-vban status line is written per
  received packet, so a sender off for longer than the window (the console overnight) never reads
  as one loss at resume; a pause inside the window pages (its starve second is counted `lost`, plus
  one re-anchor) and shows as `gap_max_s`. A pause of p seconds keeps its pair inside the window for
  about 650 - p s, so a pass is GUARANTEED to see it only up to ~230 s (650 - p >= the 420 s longest
  pass gap); a longer pause pages only sometimes, and one longer than the window never. Stated
  choice: a pause is program audio gone. The obs-vban PACER arm keeps its 2-pass confirm over the same 660 s window and has the same
  latent one-pass-only case (not changed in this lane).
- **Rebase, never a loss:** a counter that went DOWN, or between the two lines an `obs-vban receive
  anchored:` / `obs-vban receive worker: ... stopped` line of THAT source, or a `dante-input start:`
  line. A re-created source can already count past the old values in 10 s, so the re-init line is
  the signal, not only the backwards step.
- **GATED** (= a loss): obs-vban `lost` (concealed slots included, ROZHODNUTÉ 6359382787),
  `re_anchors` (the held second of a re-anchor is never in `lost`), `emit_missed`; Dante
  `DANTE_RX_GATED`, pinned equal to `scripts/dante_input_continuity.py` `GATED`, plus the Dante
  STATE faults the E2E gate also fails on, counted per line inside the window: `clock_faults` (a
  status line with `clock=` not `media` or `holdover=` not 0) and `errors` (a `dante-input error:`
  line). The obs-vban difference from the E2E gate (`late`, `stale` not gated) is deliberate and
  pinned by a test. A fault while the device STARTS (after a `dante-input start:`, before its first
  `clock=media holdover=0` status line, within `DANTE_RX_START_GRACE_S` = 180 s: the in-process PTP
  follower may need a few lines to lock, dsync's own cold lock is ~95 s) is never counted, so an OBS
  restart or deploy never pages; a start that never reaches the media clock counts after the grace.
  The start-up line shape is NOT read live yet (the supervisor reads the first status lines after an
  OBS start on strih-snv once deployed). **NAMED** only:
  obs-vban late / stale / reordered / concealed / silence_ms (a late packet's slot is already in
  `lost`), Dante rx_late / overruns / late_blocks. A counter absent on one side (an older release) is
  simply not counted.
- **A new module goes into BOTH file lists** (`scripts/lib/bundle-state-files.txt` + `.sh`) and the
  facade's `__all__`; `test_bundle_state_files_1386.py` pins it.

## Decision + watchdog arm

- `audio_mixer_decision.classify_program_rx(entry, rx_key, reachable, log_head_age_s)`: SKIP / NONE
  (the facts name no receiver: resolume) / UNKNOWN (no entry for that key) / STALE (log head frozen >
  180 s -- decided BEFORE the loss, or a dead OBS's last window pages forever) / LOSS / SILENT (the
  program stopped while the OBS log is live: the newest status line older than 180 s with the log
  head <= 60 s old, or every counted Dante pair without delivered frames) / STALE (the status line is
  old with no proof the log is live) / HEALTHY. A receiver that never wrote a status line (only an
  error line) is never HEALTHY: LOSS while the error is in the window, then SILENT / UNKNOWN. Nor is
  a receiver whose every pair in the window was a rebase (a device or source re-created over and
  over: `pairs == 0`, `rebases > 0`), review round 3. Residual (review round 4): a slower restart
  loop that still writes two or more status lines per start, with audio flowing and no fault outside
  the start grace, reads HEALTHY; its `rebases=` count is on every machine-channel `program_rx` line. `analyze(..., rx_key=)`, CLI `--rx-key`, prints `rx_*`.
- **SILENT pages only during a production (owner ruling 12.11.2026, design 6836485361):** a dead sender,
  a dropped NIC or a down leg reads SILENT, and so does a console switched off overnight. So on SILENT
  `handle_program_rx_silent` reads the venue's STREAM box (`program_audio_rx_stream_box`: the
  stream-role box with the same `STRIH_VENUE`, from the facts; dialled at its row in the pass's roster)
  through the read-only `stream_output_stuck_probe.py --window-s 0.5`: both scene reads `PRO` and
  `active0=1` = production -> with `AUDIO_MIXER_PROGRAM_RX_PAGE=1` the `program-rx` arm pages after a
  2-pass confirm. An absent OBS (probe rc 1) = no, hung / unreadable / no stream box in the roster =
  unknown: both stay machine-channel only and reset the arm's confirm. This is NOT the A/V alarms' rule
  (`scripts/lib/avsync-production.sh`: rig EVENT mode, unknown counts as a production). Test seam
  `AUDIO_MIXER_PRODUCTION_CMD` (`tests/python/test_program_rx_silent_production_1460.py`).
- **The receiver comes from the box FACTS, never a literal:** `scripts/lib/program-audio-rx.sh`
  `program_audio_rx_key NAME` (facts in a subshell): `STRIH_PROGRAM_VBAN_STREAMS != none` ->
  `obs-vban:<STRIH_PROGRAM_AUDIO_INPUT>`; a declared Dante program device -> `dante`; no fact file ->
  nothing (NONE); a fact file that does not load -> rc 1 (the arm logs FACTS, never NONE). A bench
  source (`VBAN bench (issue 1460)`) is never the program key, so its scheduled losses never page.
- **The fifth arm of `audio-mixer-alert-watchdog.sh`** (`handle_program_rx`, prefix + key base
  `program-rx`): the obs-fleet `audio-mixer` roster, LOSS pages on its FIRST pass (`handle_arm`
  threshold 1, review round 1: the timer's `OnUnitActiveSec=5min` + `RandomizedDelaySec=2min` makes
  passes 300-420 s apart, so a loss pair (~650 s visible in the 660 s window) is seen by ONE pass
  in ~27 % of cases and a 2-pass confirm missed it; a counter delta is a fact, not a noisy reading),
  the arm + box throttle keeps one ongoing loss one incident, PRODUCTION-CRITICAL
  `watchdog_notify_key "program-rx-<box>"`, HEALTHY = one machine-channel RECOVERY line. A test pins
  the window above the timer's longest pass gap + one status period.
- **SHIPS DISABLED inside an ENABLED watchdog:** the audio-mixer timer already runs on dev1, so the
  arm grades and LOGS every pass but pages only with `AUDIO_MIXER_PROGRAM_RX_PAGE=1` (a unit drop-in,
  `systemd/audio-mixer-alert-watchdog.README.md`). Tests pin the default (a LOSS logged, notify never
  called).

## The MIX-LATE follow-ups (28.10 review residuals, comment 6480741240)

- **Session identity:** `audio_mix_session` = the `HH:MM:SS.mmm` stamp of the newest audio-reset
  `buffering type:` line (`audio_mix_session_from_log`: tail, else the head slice of a bounded read).
  Residual: a mid-session audio reset whose line has left the tail but is not in the 2 MB head reads
  the start-up stamp (time of day only), so that one case keeps the count-only compare.
  The watchdog stores it per box (`<box>_mix_late_session`); a CHANGED identity with a stored count
  stores 0 and re-runs the decision with `--prev-mix-late 0`, so a new session whose count is EQUAL
  or HIGHER is graded from zero (the confirm is kept). An absent identity on either side changes
  nothing (an older gather).
- **`handle_arm` returns** 0 when the verdict is confirmed this pass (paged, throttled or a dry-run
  WOULD alert), 1 while holding; `handle_mix_late` advances its stored count on that return and no
  longer reads the arm's confirm key. The other arms ignore the return (the watchdog runs without
  errexit).

## Tests

`tests/python/test_program_audio_rx_1460.py` (status lines rendered from the shipped
`vban-source.c` format string, the Dante line shape of the E2E tests, the REAL stream-snv 23.10 log
read as the E2E gate's +7 lost / 3 concealed, the truth table, the facts-derived receiver per box,
the real bash watchdog through `AUDIO_MIXER_FETCH_CMD`) and the follow-up tests appended to
`tests/python/test_audio_mix_late_1460.py`. The issue-1386 goldens carry both facets: the synthetic
`all_families.txt` gained one head `buffering type:` line and seven receiver lines at the 00:24:16 -
00:25:00 gap; the served JSON gained only `audio_mix_session` + `audio_rx` (key order kept); the one
input-caused value change is `bounded:concat` `audio_mixer_age_s` 645 -> 606 (the bounded tail cut
moved by the inserted bytes; that age is the whole tail span there).
