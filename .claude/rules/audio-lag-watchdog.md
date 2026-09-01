---
paths:
  - "scripts/audio-lag-alert-watchdog.sh"
  - "scripts/audio_lag_decision.py"
  - "systemd/audio-lag-alert-watchdog.*"
  - "tests/python/test_audio_lag_*.py"
---

# dev1-side OBS audio-timeline-lag alert watchdog (#1226)

Closes the detection gap behind the **2026-08-30 nedeľná služba** incident: stream OBS's audio
subsystem fell ~24–27 s/min behind realtime from StartStream; `ts_lag_ms` grew to 1 672 741 ms
(27,9 min) and the YouTube stream's A/V desynced for a whole service, because the OBS log line that
screamed it the whole hour was read by nobody. A fifth dev1 alert-watchdog sibling
(network-reach #1001 → bundle-state #732 → cadence #794 → this).

## The signal + the exact log line — parse it PRECISELY

`vendor/obs-studio/libobs/obs-audio.c:698` emits, every 60 s per audio source:
`audio-telemetry #800 '<src>': ts_lag_ms=<int64> buffered_ms=<int> pending=<int> timing_adjust_ms=<int64>`
plus a summary line WITHOUT a quoted name: `audio-telemetry #800: total_buffering=... buffering_source=...`.

- `bundle_state_gather.audio_ts_lag_ms_from_log(text)` → `(max_lag_str, src)`: LAST reading PER
  source, then the **MAX** across sources (all sources lagging equally = a global audio-tick/mix
  pipeline behind realtime, the incident signature). The regex `'([^']*)': ts_lag_ms=(-?\d+)` anchors
  on the closing-quote-then-`: ts_lag_ms=`, so the summary line (no quoted name) never matches.
- **`ts_lag_ms=-1` means audio_ts==0 (source present, no audio timeline yet) — NOT a lag.** It is
  excluded from the max; a source whose newest reading is -1 does not contribute; only-negatives →
  `("","")` (facet omitted, never a fake 0).
- **Scan the TAIL window only.** The #1222 bounded read returns `head + LOG_BOUNDED_READ_SEPARATOR +
  tail`; the facet must reflect CURRENT state, so it `rsplit`s on the separator and scans only the
  tail — a stale HIGH value that survives only in the head (an old recovered episode from the startup
  region) is never reported. A small whole-file log (no separator) is scanned entirely. Reuse the
  SAME bounded `log_text` the other `_from_log` parsers use — never add a second log read.
- Wired into `bundle-state-server.gather_bundle_state` via `_parse_log_facets` (one `obs_log_parse`
  timing) + two `build_bundle_state` kwargs (`audio_ts_lag_ms`/`audio_ts_lag_src`, omit-when-empty).

## The watchdog is DETECTION-ONLY — no auto-action, alert-only

The observed cure was a **PC reboot of a live prod box** (a genuinely destructive owner-call,
`no-destructive-remote-actions.md`); the preventive pre-service reboot is an owner decision on the
ticket. So unlike bundle-state #732 (which auto-restarts the `:8899` task), this watchdog takes NO
action — it pages (throttled) and logs recovery machine-only. Pure decision core
`scripts/audio_lag_decision.py` (`extract_audio_lag` + `classify` + `analyze`, #1199 python-mirror
pattern, pytest Tier-0); orchestrator `scripts/audio-lag-alert-watchdog.sh` curls
`:8899/bundle-state.json` and reuses `scripts/lib/obs-watchdog-decision.sh`
(`obs_watchdog_confirm` 2-pass + `obs_watchdog_alert_throttle` ~1h) VERBATIM.

## Freshness / recency dimension (#1231)

The #1226 facet took the LAST reading PER source with NO age bound, leaving two adjacent gaps: (a) a
source removed/renamed while LAGGING kept its stale-high last line winning the MAX until the log
rotated (a false page for a gone condition), and (b) a telemetry tick that STOPPED while the OBS log
kept advancing read as healthy (no freshness signal). Both are closed by a purely **in-log relative
recency** — the `ndi_halving_decision.ts_to_seconds` + midnight-wrap precedent, MIRRORED locally in
`bundle_state_gather` (never imported — the gather runs on the box, that decision module is dev1-only):

- `bundle_state_gather.audio_telemetry_from_log(text)` → `(max_FRESH_lag_str, src, age_s_str)` in ONE
  pass over the SAME bounded tail. It ages each source's newest `#800` line against `log_newest_ts` —
  **the LAST parseable line in FILE ORDER, NEVER `max(seconds-of-day)`.** The OBS log is append-only,
  so file order IS time order; a max-of-seconds anchor picks a PRE-midnight line across a day boundary
  and reads a genuinely stale source as fresh (the review W1 🔴). `_recency_gap_s`'s single `+86400`
  wrap correction then makes the gap the TRUE elapsed time (mod 24h). No wall clock is injected, so it
  never mis-compares a date-less OBS timestamp against a foreign clock and stays a pure fixture-testable
  parser. `audio_ts_lag_ms_from_log` is now a 2-tuple wrapper over it.
- **(a)** A source silent `> AUDIO_TS_LAG_STALE_AFTER_S` (180 s ≈ 3× the 60 s emit period) behind the
  log head is EXCLUDED from the max → the removed lagging source no longer drives the reading, even
  when it went silent hours ago or across midnight.
- **(b)** `audio_ts_lag_age_s` = the in-log age (whole seconds) of the freshest `#800` line behind the
  log head — `_recency_gap_s` is in `[0,86400)` by construction, so there is **no upper clamp**: a >1h
  stall is a REAL fault reported honestly, never snapped to `"0"` (removing that snap-to-fresh clamp
  was the review W1 🔴 fix — it had caused fake-HEALTHY). `"0"` only when neither timestamp parses
  (a pathological prefix-less log). Present (`"0"` when fresh) whenever ANY `#800` line exists, `""`
  only when telemetry is fully absent. A large value → the dev1 `STALE` verdict.

## Classification + the NEVER-false-page invariant

`SKIP` (fetch failed — box/`:8899` down, defer to #732/#1001) · `STALE` (#1231: fetched, telemetry
PRESENT but `audio_ts_lag_age_s` > `AUDIO_LAG_STALE_THRESHOLD_S`, default 180 — the audio tick stopped
while the log advanced; surfaced DISTINCTLY on the machine channel, **never a phone page**) · `UNKNOWN`
(fetched but facet absent — no `#800` line in the tail yet) · `HEALTHY` (lag ≤ `AUDIO_LAG_THRESHOLD_MS`,
default 5000) · `LAGGING` (> threshold → page after 2-pass confirm). **The only page condition is a
successfully-fetched POSITIVE FRESH lag reading**, so a dev1-side path outage makes every fetch fail →
SKIP → no page — which is why this watchdog needs **no** reference-anchor/outage guard (bundle-state
#732 needs one because it restarts + pages on DOWN). `STALE` is decided BEFORE the lag checks (a stale
reading is never a false `LAGGING` page) and, like SKIP/UNKNOWN, HOLDS the confirm counter (an
unmeasured pass neither advances nor resets it) and fires no false HEALTHY recovery; HEALTHY resets it.
**Absence/staleness is never paged** — a box/`:8899`-down box is #732/#1001 territory.

**Residual (in-log recency is blind to a fully-STOPPED log):** the freshness signal detects telemetry
that stopped WHILE the log advanced (the ticket's scope). If OBS itself DIES while lagging (box +
`:8899` still up, so the log stops advancing entirely), the last `#800` reading persists with age ≈ 0
→ it reads as fresh, and a LAGGING reading could re-fire. That OBS-dead-box-alive class is
**obs-liveness (#391)** territory — NOT #732/#1001 (which watch box/`:8899` reachability) — and #391
already DETECTS a dead OBS, so no fault is missed (at most a duplicate alert). A server-layer
log-mtime supplement (same box clock, no foreign-clock comparison) would close the duplicate cleanly
later; it is deliberately out of this ticket's "while the log kept advancing" scope.

## #1206 notify discipline + ships DISABLED

ALERT carries a STABLE `--dedup-key "audio-lag-$box"` (no per-pass component — a growing lag EDITS
the one card instead of re-pinging); recovery is `log`-only / machine-channel, never a ✅ phone ping.
`tests/python/test_notify_dedup_key_sweep_1206.py` auto-discovers this script and enforces both.
Units are committed but NOT enabled — install/verify/enable per
`systemd/audio-lag-alert-watchdog.README.md` (the supervisor's step).

## Tier-0 verify (no cargo)

`python3 -m pytest tests/python/test_audio_lag_*.py` (the #1226 facet/decision/CLI + the #1231
`test_audio_lag_gather_1231.py`/`test_audio_lag_decision_1231.py` freshness/STALE tests) +
`test_bundle_state_server_log.py` (the wiring flows BOTH `audio_ts_lag_ms` and `audio_ts_lag_age_s`
through gather); `bash -n` + `shellcheck -S warning` on the watchdog. The shell verdict dispatch
(LAGGING/STALE/UNKNOWN/HEALTHY) is proven by a MANUAL stubbed-`curl` `--dry-run` recipe (a fake
`curl` on `$PATH` echoing a fixture body per case) — same manual convention as #1226 (no committed
driver; the STALE branch is a 6-line early return). CI runs nothing new for this (pure python/bash).

## #1265 — the per-REFERENCE-source ts_lag BAND arm (a SECOND, tens-of-ms dimension)

The #1226/#1231 arm is BLIND to the 2026-09-01 audio-timeline HEALTH degradation by 23×: the
A/V-gate reference source `mbc` went BIMODAL on the stream box (flat ~107 ms for 26 h, then flapping
107↔180 ms every 1–2 min, the high mode creeping to 180–217 through the day) — a
source-timestamp/mix-clock oscillation, NOT the buffering step-up (only ONE `adding … buffering`
line the whole log). Its peak ~217 ms is 23× under the 5000 ms lag threshold, AND the single
MAX-across-sources scalar facet does not even attribute it to `mbc` (a live read mid-flap can report
`ASIO Input Capture` as the max). A single instant's value cannot express a bimodal band, so the fix
is a SHAPE facet + a finer verdict.

**This band watch is an OBS audio-timeline HEALTH alarm — it does NOT by itself explain the A/V-gate
residuals (supervisor finding 2026-09-01).** The same-day A/V failures (residual −77/−111/−126 ms
past the ±90 gate) were a SEPARATE upstream-audio-latency STEP: after the stream-OBS restart the
`mbc` band went FLAT (~85 ms) yet a PR E2E still measured −111.5 ms across all 7 cameras, and the
av-sync dock showed the mastered Dante feed into the DVS `mbc` source physically shifting ~60 ms then
oscillating. So the band watch catches audio-timeline instability (a real health issue worth
paging), while the residual early-warning is the SEPARATE upstream-step detector filed as issue 1267
(the #856 guard below already HOLDs the apply on the residual case regardless of the band).

- **Box facet (`bundle_state_gather.audio_ref_band_from_log(text, ref_src)`):** for the named
  reference source (`AUDIO_REF_BAND_SRC`, default `mbc`; a box with no such source omits it — strih),
  from the SAME #1222 bounded log (ONE read), compute the band SHAPE: `audio_ref_lag_base_ms` (the
  flat-start baseline = median of the HEAD/startup region's readings — "the instance's own flat
  start"; `""` when there is no separator/head), `audio_ref_lag_high_ms`/`_low_ms` (p90/p10
  nearest-rank of the FRESH tail window, so a lone spike never widens the band), `audio_ref_lag_duty_pct`
  (% of the tail window above `baseline + AUDIO_REF_BAND_DUTY_MARGIN_MS`), `audio_ref_lag_n`. Same
  omit-when-empty rule; wired through `build_bundle_state` + `bundle-state-server._parse_log_facets`
  (still ONE `obs_log_parse` timing, no second read — the #1222 caching discipline).
- **dev1 decision (`audio_lag_decision.classify_band`/`analyze_band` + the `band` CLI):** DRIFTING
  iff `deviation = high − (base or low) > BAND_DEV_THRESHOLD_MS` (40) AND `duty_pct >= BAND_DUTY_MIN_PCT`
  (10) AND `n >= BAND_MIN_SAMPLES` (8). The flat-start baseline catches a WHOLE-band creep (both modes
  up together) that a within-window spread alone would miss; the duty term separates a genuine flap
  (~50%) from a single spike (~few %). SKIP (unreachable)/UNKNOWN (facet absent or too few samples)
  never page. All thresholds env-overridable.
- **Watchdog arm (`handle_box_band` in `audio-lag-alert-watchdog.sh`):** a self-contained SECOND arm
  (its OWN fetch + DISJOINT state keys, so the #1226 lag arm is byte-unchanged), 2-pass confirm,
  report-only ⚠️ alert with a STABLE `--dedup-key audio-band-$box` (distinct from the lag arm's 🚨
  `audio-lag-$box`), machine-channel recovery (no ✅ ping, #1206). Ships DISABLED like the lag arm
  (same committed-but-unenabled units).

## #1265 — the #856 A/V controller now HOLDs an apply computed from an unstable timeline

The #856 rig-wide A/V controller (`av_sync_combine_offsets.py` → `av_sync_calibrate.py --apply` in
`recording-e2e.sh cleanup()`) walked `NDI 2ME PGM` 926→976 after the flapping run, because its only
guards were <2-measured-cams / >100 ms-spread — a rig-wide-CONSISTENT (small-spread) but
timeline-corrupted run passes both. `scripts/av_sync_apply_guard.py` (pure, pytest Tier-0) +
`scripts/lib/av-sync-apply-guard.sh` (sourced I/O gather, #675) HOLD the apply on ANY of: (1) the
run's stream `mbc` band verdict is DRIFTING (gathered at `[8/8g]` into `AV_SYNC_BAND_VERDICT`) — a
supplementary "audio timeline UNSTABLE, defer tuning" hold, NOT a claim the flap explains the
residual (holds even when a step is sustained); (2) `|residual_median_ms|` exceeds a ±60 ms sanity
ceiling, checked REGARDLESS of the band (a flat/HEALTHY band still measured −111.5, a real upstream
step, so band-scoping was REJECTED) — **but ONLY while the step is not yet SUSTAINED**; or (3)
`|proposed − last_applied| > 90 ms` vs the dev1-persistent `~/.camera-box/av-sync-last.json`
(populated by COPYING the calibrate full-schema success file, preserving its `applied_latency_ms`
data contract) — an anti-oscillation/step guard, also SUSTAINED-gated.

**SUSTAINED two-run confirmation (supervisor 2026-09-02 — the #1265b 🔴 fix).** Conditions 2 and 3
alone would make a GENUINE sustained upstream step (2026-09-01: −77 → −126 → −111 across three runs,
agreeing within ~25 ms while the pin held 926 — the 926→976 step was CORRECT) UN-APPLIABLE FOREVER,
leaving the rig ~90 ms mis-aligned until a human hand-edits the pin — inverting the #856 "the gate
aligns, never the operator" contract. So the caller persists EVERY run's residual (held OR applied)
to `~/.camera-box/av-sync-residual-last.json` (`{run_id, ts, residual_median_ms, residual_spread_ms,
pin_at_measure}`), and the guard treats a step as SUSTAINED when the previous run's persisted residual
exists, is ≤ 24 h old, and agrees with this run within `SUSTAINED_TOL_MS` (default 33 = one 30 fps
frame). When SUSTAINED, conditions 2 and 3 STAND DOWN and the apply proceeds — the existing #856
±50 ms/run clamp bounds each step, so a two-run-confirmed step converges over a few runs instead of
never. A first anomalous run (or a prev that disagrees > tol, or a stale/missing prev) still HOLDs
("awaiting a 2nd consistent run" — outlier protection). Condition 1 is independent of SUSTAINED.

A HOLD clears `AV_SYNC_APPLY_OFFSET_MS` (skipping the byte-unchanged apply) with a loud log + a
per-run `av-sync-apply-hold-<run>.txt` AND a durable `~/.camera-box/av-sync-apply-hold-last.txt`
reason; when the guard says proceed, #856 is byte-identical. The residual EARLY-WARNING (before the
E2E even runs) is the separate upstream-step detector, issue 1267. See
`.claude/rules/avsync-monitoring.md` for the guard's placement in the cleanup composition.
