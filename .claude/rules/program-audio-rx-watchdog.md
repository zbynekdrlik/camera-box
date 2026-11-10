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
  `kind`, `name`, `events` (gated delta sum), `deltas` (gated + named), `pairs`, `rebases`,
  `gap_max_s`, `age_s` (newest status line behind the log head). Omitted when the tail has no
  receiver status line.
- **Stateless window delta, the obs-vban pacer's 660 s:** a delta is counted only between two status
  lines of ONE receiver that are BOTH inside the window. The obs-vban status line is written per
  received packet, so a sender off for longer than the window (the console overnight) never reads
  as one loss at resume; a pause of 1-11 min inside the window DOES (its starve second is counted
  `lost`, plus one re-anchor) and shows as `gap_max_s`. Stated choice: that is program audio gone.
- **Rebase, never a loss:** a counter that went DOWN, or between the two lines an `obs-vban receive
  anchored:` / `obs-vban receive worker: ... stopped` line of THAT source, or a `dante-input start:`
  line. A re-created source can already count past the old values in 10 s, so the re-init line is
  the signal, not only the backwards step.
- **GATED** (= a loss): obs-vban `lost` (concealed slots included, ROZHODNUTÉ 6359382787),
  `re_anchors` (the held second of a re-anchor is never in `lost`), `emit_missed`; Dante
  `DANTE_RX_GATED`, pinned equal to `scripts/dante_input_continuity.py` `GATED`. **NAMED** only:
  obs-vban late / stale / reordered / concealed / silence_ms (a late packet's slot is already in
  `lost`), Dante rx_late / overruns / late_blocks. A counter absent on one side (an older release) is
  simply not counted.
- **A new module goes into BOTH file lists** (`scripts/lib/bundle-state-files.txt` + `.sh`) and the
  facade's `__all__`; `test_bundle_state_files_1386.py` pins it.

## Decision + watchdog arm

- `audio_mixer_decision.classify_program_rx(entry, rx_key, reachable, log_head_age_s)`: SKIP / NONE
  (the facts name no receiver: resolume) / UNKNOWN (no entry for that key) / STALE (log head frozen >
  180 s -- decided BEFORE the loss, or a dead OBS's last window pages forever -- or the newest status
  line older than 180 s: the sender stopped) / LOSS / HEALTHY. `analyze(..., rx_key=)`, CLI
  `--rx-key`, prints `rx_*`.
- **The receiver comes from the box FACTS, never a literal:** `scripts/lib/program-audio-rx.sh`
  `program_audio_rx_key NAME` (facts in a subshell): `STRIH_PROGRAM_VBAN_STREAMS != none` ->
  `obs-vban:<STRIH_PROGRAM_AUDIO_INPUT>`; a declared Dante program device -> `dante`; no fact file ->
  nothing (NONE); a fact file that does not load -> rc 1 (the arm logs FACTS, never NONE). A bench
  source (`VBAN bench (issue 1460)`) is never the program key, so its scheduled losses never page.
- **The fifth arm of `audio-mixer-alert-watchdog.sh`** (`handle_program_rx`, prefix + key base
  `program-rx`): the obs-fleet `audio-mixer` roster, 2-pass confirm (the 660 s window keeps one loss
  in two passes), arm + box throttle, PRODUCTION-CRITICAL `watchdog_notify_key "program-rx-<box>"`,
  STALE resets the confirm, HEALTHY = one machine-channel RECOVERY line.
- **SHIPS DISABLED inside an ENABLED watchdog:** the audio-mixer timer already runs on dev1, so the
  arm grades and LOGS every pass but pages only with `AUDIO_MIXER_PROGRAM_RX_PAGE=1` (a unit drop-in,
  `systemd/audio-mixer-alert-watchdog.README.md`). Tests pin the default (a LOSS logged, notify never
  called).

## The MIX-LATE follow-ups (28.10 review residuals, comment 6480741240)

- **Session identity:** `audio_mix_session` = the `HH:MM:SS.mmm` stamp of the newest audio-reset
  `buffering type:` line (`audio_mix_session_from_log`: tail, else the head slice of a bounded read).
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
