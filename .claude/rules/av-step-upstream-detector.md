---
paths:
  - "scripts/av_step_decision.py"
  - "scripts/av-step-alert-watchdog.sh"
  - "systemd/av-step-alert-watchdog.*"
  - "tests/python/test_av_step_*.py"
  - "tests/fixtures/av-step-*/**"
---

# Upstream-audio-latency STEP detector (#1267) — the av-sync dock measured-offset early warning

The dev1 report-only watchdog that flags an UPSTREAM audio-chain latency STEP on the stream box (the
2026-09-01 A/V-gate failure class: the mastered Dante feed into the stream box's DVS `mbc` source
stepped ~-60..-90 ms ~3 h before the E2E A/V gate failed). Box-side parser
`bundle_state_gather.av_offset_series_from_log` (facets on `:8899`), pure decision
`scripts/av_step_decision.py`, dev1 watchdog `scripts/av-step-alert-watchdog.sh` reusing
`scripts/lib/obs-watchdog-decision.sh`. Distinct from `.claude/rules/audio-lag-watchdog.md` (#1226,
the OBS-internal `ts_lag` HEALTH axis) — a run AFTER a stream-OBS restart can have a FLAT ~85 ms
ts_lag band yet still measure a −111 ms A/V residual, because the upstream step is independent of the
OBS ts_lag flap.

## The signal — the dock's monitor-only SUGGESTED line carries the pin INLINE

Stream OBS runs the dock monitor-only (DockLockCorrector is monitor-only by default, #942), so it
logs the Suggest branch (`vendor/av-sync-dock/src/sync-test-output.cpp:~1485`, verified LIVE
2026-09-02, ~2/min):

```
HH:MM:SS.mmm: [obs-audio-video-sync-dock] av-sync-dock: LOCK-CORRECT SUGGESTED genlock_latency_ms_src <pin> -> <new>ms (measured offset=<X>ms) [monitor-only ...]
```

Both the CURRENT genlock pin (`<pin>`) and the measured A/V offset (`<X>`) are on the ONE line — no
separate WS read of `genlock_latency_ms_src` is needed. The regex matches BOTH `SUGGESTED` and
`requested` (a future actuation build); the OTHER `LOCK-CORRECT` variants (apply skipped / read-back
mismatch / pinned / unavailable) lack the `-> Nms (measured offset=` shape so never match. The plain
`LOCKED/UPDATED offset=` line is a DIFFERENT (raw per-tick) line WITHOUT the pin — do not use it for
the step signal. The dock offset is a STREAM-only signal (strih logs `ASRC section unavailable --
source 'mbc' not found`), so the watchdog watches stream only by default.

## GOTCHA — never subtract the pin as a covariate; gate on pin STABILITY instead

The obvious pin-covariate is `pin_adjusted = offset - pin` (or the printed SUGGESTED target, which is
exactly `pin - offset`). It is WRONG and was FALSIFIED against the live box: a pin jump 976→1024 (an
E2E test-latency write) left the RAW measured offset ~unchanged (~50 ms), so `offset - pin` reads a
−48 ms PHANTOM step — the exact false page the covariate is meant to prevent. The offset↔pin coupling
has a settling LAG the instantaneous subtraction ignores. The robust design (chosen): the box reports
a `pin_stable` flag over the analyzed span; a step is judged ONLY across a CONSTANT-pin window, and a
pin move → verdict REPIN (report-only, no page). No sign assumption, so a wrong sign can never
false-page. The real 2026-09-01 episode had a constant pin (926 held for hours), so this catches it.
**Lesson for any future av-sync/dock offset analysis: verify a covariate against LIVE data before
designing around it — the dock offset does not instantly track the genlock pin.**

## GOTCHA — a rolling-baseline step detector self-normalizes; freeze the baseline for RECOVERY

The baseline is a ROLLING 10-40 min window (bounded above by how far the #1222 bounded log TAIL
reaches, ~50 min on a long session). A PERSISTENT step therefore SELF-NORMALIZES: the recent median
flips ~5 min after onset and the rolling baseline absorbs the step ~25-40 min after onset, after
which the box reads HEALTHY-against-the-rolling-baseline though the offset never returned. Two
consequences, both handled:

- **Detection is a ~20-40 min ONSET window** (~4-8 passes) — inherent to reading a bounded log tail.
  That is fine for an early-warning (catch the onset hours before the E2E); once a shift is the steady
  state, the #856 controller + the E2E gate own the alignment.
- **Recovery MUST be judged against a FROZEN pre-step baseline, never the rolling one** — else a
  persistent step falsely self-reports "back to normal" and clears the alert. The watchdog freezes
  `alert_base_$box` at the first confirmed alert (the box's `base_med` is still the pre-step level
  then) and `av_step_decision.recovered_to_baseline` compares the CURRENT recent median against THAT.
  An absorbed step is HELD ("holding alert, no recovery"); recovery fires only on a genuine physical
  return. **This is a general pattern for ANY rolling-window step/anomaly detector with a recovery
  latch: the recovery reference must be frozen at alert time, not the same rolling window the
  detection uses.**

## Verdicts + the never-false-page invariant (mirrors #1226)

SKIP (fetch failed → defer #732/#1001) · STALE (dock stopped while the log advanced — `av_offset_age_s
> stale_threshold`, decided BEFORE the step check) · UNKNOWN (facet absent OR too few samples) ·
REPIN (pin moved in the span → report-only) · LOW_QUALITY (#1319 P3 — dock reading too noisy/thin or
non-finite, log-only, decided AFTER REPIN and BEFORE STEP/HEALTHY; see the P3 section below) ·
HEALTHY · STEP (`|recent_med - base_med| > AV_STEP_THRESHOLD_MS`, default 45 — normal medians wander
±30, the real step was −60..−90; page after 2-pass confirm). The ONLY page condition is a fetched
POSITIVE step reading AT A TRUSTWORTHY QUALITY, so a dev1-side outage → SKIP → no page (no
reference-anchor needed). Report-only ⚠️ with a stable `--dedup-key av-step-$box` (#1206); recovery
is machine-channel only. Ships DISABLED (units committed, not enabled).

## Tier-0 verify (no cargo)

`python3 -m pytest tests/python/test_av_step_*.py tests/python/test_notify_dedup_key_sweep_1206.py` +
the #1226/#1231/server siblings; `bash -n` + `shellcheck -S warning` on the watchdog. The shell state
machine (STEP → confirm → alert → absorbed-hold → recovery) has no Rust test under Tier-0 — drive it
with a stubbed `curl` on `$PATH` serving a different crafted bundle-state body per pass (the #836
executable-fixture pattern) + a persistent `AV_STEP_ALERT_STATE_FILE`, and assert the log lines +
the `alerted_/alert_base_` state transitions across passes.

## #1319 — the absolute-BAND arm (owner-facing alarm the STEP term is blind to)

The STEP term above measures the CHANGE-rate of the 10-min median vs a ROLLING baseline, so it is
structurally blind to a SLOW absolute drift: the owner's 15.9.2026 wander (+13 → +47 ms over 80 min
at a constant pin) never crossed a 45 ms adjacent-median delta and the rolling baseline absorbed it,
so no notification fired. The SAME watchdog file (`av-step-alert-watchdog.sh`) now carries a SECOND
arm, run alongside the step arm per box:

- **Signal + facet.** `classify_av_band`/`analyze_band` (in `av_step_decision.py`, pure, the
  `analyze-band` CLI subcommand) page **OUT_OF_BAND** when the RECENT median offset (≥ `min_samples`,
  at a CONSTANT pin) leaves ±`AV_BAND_MS` (default 30) of a **FIXED E2E-aligned reference**. The
  reference is judged against the aligned RESIDUAL, **never the genlock pin** — `resolve_band_reference`
  reads (in order) `AV_BAND_REFERENCE_MS` → `residual_median_ms` of
  `~/.camera-box/av-sync-residual-last.json` (the `[4i/8]` align residual `av_sync_apply_guard`
  persists) → a 0 ms fallback STATED in the log. A fixed anchor means recovery is a plain return into
  band, no frozen-baseline latch (unlike the STEP arm).
- **The dead-band freshness facet.** The dock only logs its SUGGESTED offset line while `|offset|` is
  OUTSIDE its suggestion dead band, so a quiet in-band window makes the SUGGESTED series go silent and
  the old `av_offset_age_s` read STALE even though the dock is LIVE (the 19:51 `age_s=427` false
  STALE). `bundle_state_gather.av_offset_dock_live_age_from_log` adds the `av_offset_dock_live_age_s`
  facet = age of the freshest `av-sync-dock: diag … locked=yes` heartbeat, so `classify_av_band`
  reads **IN_BAND_QUIET** (dock LIVE + offset in the dead band = healthy) instead of STALE. **STALE**
  fires only when that dock-live line ITSELF is stale.
- **DELIBERATELY not folded (ROZHODNUTÉ):** the raw per-tick `UPDATED/LOCKED offset=` lines are NOT
  folded into the offset series — it broke the #1267 exclusion test (`av_offset_series_from_log` stays
  byte-identical) AND was wrong (their transient lock-acquisition values pushed a quiet dead-band
  window OUT_OF_BAND). The freshness facet is the correct fix.
- **Production-critical re-ping.** A/V-sync measurement is production-critical (issue 1308), so the
  OUT_OF_BAND page uses a TIME-BUCKETED `--dedup-key` (`watchdog_notify_key "av-band-$box"`) — it
  re-pings "dokolečka" while the offset stays out of band; recovery is machine-channel only. The
  file is allowlisted in the #1206 dedup sweep; the STEP arm's own `av-step-$box` key stays a stable
  (non-bucketed) one. New band tunables: `AV_BAND_MS`, `AV_BAND_REFERENCE_MS`, `AV_BAND_REFERENCE_FILE`,
  `AV_BAND_CONFIRM_THRESHOLD`, `REPING_INTERVAL_S`.
- **Never a false page:** OUT_OF_BAND needs a FETCHED positive reading, so SKIP/STALE/UNKNOWN/REPIN/
  IN_BAND/IN_BAND_QUIET never page. Tier-0: `tests/python/test_av_band_1319.py` (incl. the realistic
  19:40-19:59 fixture) + the sweep + `bash -n`/`shellcheck`; live dry-run against stream `:8899`.

## #1319 Part 2 — the band arm FALSE-PAGED 78×; fix = measurement-quality gate + dock-native reference

The Part-1 band arm (above) paged the owner **78 times overnight 15./16.9.2026**. Root cause, two
defects in the alarm's INPUT (never the rig — the stream `NDI 2ME PGM` FIFO was calm throughout):

1. **No measurement-QUALITY gate.** The band judged the dock's live estimator (per-sample scatter
   MAD 9-31 ms) against a ±30 ms band — a band cannot be judged from an estimator that noisy.
2. **A reference-FRAME mismatch.** `resolve_band_reference` used `residual_median_ms` (the
   RECORDING-based E2E residual, −35.3 ms), while the dock's live estimator carried a NON-constant
   bias (≈ +85 ± 40 ms). `|85 − (−35.3)| = 120 ms ≫ 30`, re-pinged every 10 min by the
   production-critical bucket → 78 pages.

The fix (all under this watchdog + its facet source):

- **Quality facet.** `bundle_state_gather.av_offset_quality_from_log` parses the dock's OWN
  `LOCKED/UPDATED offset= … matched=M mad=Dms` lines (sync-test-output.cpp:1378) into
  `av_offset_recent_mad_ms` (recent-window MEDIAN MAD) + `av_offset_recent_matched_min` (recent-window
  MIN matched). It is a SEPARATE parser from `av_offset_series_from_log` — the offset SERIES stays
  byte-identical (the Part-1 "raw UPDATED/LOCKED lines are NOT folded into the series" decision holds;
  only their matched/mad feed quality, never their offset — a different sign/bias from the SUGGESTED
  series, #952).
- **`band_quality_ok` + the `LOW_QUALITY` verdict.** The OUT_OF_BAND page requires
  `recent_mad_ms <= 15 ms AND recent_matched_min >= 30`, else `LOW_QUALITY` (log-only, NEVER a page).
  Three-state, and the ABSENT-vs-CORRUPT distinction is load-bearing: an ABSENT quality facet (older
  box / no cluster line in the recent window) returns **None → PROCEED (page-capable)** so a genuine
  sustained offset with no recent `UPDATED` line still pages (never SWALLOW a real drift); a PRESENT
  but NON-FINITE (NaN/Inf) mad returns **False → LOW_QUALITY** (a corrupt reading is untrustworthy,
  never a page). `LOW_QUALITY` sits AFTER the REPIN check (a pin move still wins) and inside the
  enough-samples guard.
- **Dock-native reference (dock-to-dock).** After a green `[4i/8]` align, the E2E persist step
  (`av_sync_persist_dock_reference` in `scripts/lib/av-sync-apply-guard.sh`, ONE call line in
  `recording-e2e.sh` — the #675 sourced-helper pattern) fetches the stream box's `:8899` bundle-state,
  asks the pure `av_step_decision.dock_reference` whether the dock's RECENT window is trustworthy
  (reachable AND n>=min_samples AND pin_stable AND quality — all four, and a FINITE median), and ONLY
  then MERGES `dock_offset_median_ms`/`dock_offset_n`/`dock_offset_mad_ms` into
  `~/.camera-box/av-sync-residual-last.json` (read-modify-write, atomic). `resolve_band_reference`
  then PREFERS `dock_offset_median_ms` over `residual_median_ms` (and STATES which key it used), so
  the ~120 ms recording-vs-dock frame bias cancels. The recording residual stays the E2E GATE's
  truth; only the ALARM's reference changes.
- **Dock BIAS observability (vendored C++, CI-compile).** `sync-test-output.cpp` appends
  `lag_idx=%ld cands=%zu` to the UPDATED/LOCKED line (`lag_idx` = the display offset quantized into
  ±`CB_CLUSTER_TOL_MS` buckets, so a wrong-cluster pick jumps it; `cands` =
  `cb_offset_cluster.samples.size()`, the total pool the densest window was scored from, so
  `matched << cands` is a bimodal tell) and emits a `pin-change observed <old> -> <new>` marker (a
  `-1`-sentinel `cb_last_seen_pin_ms`, gated on `>= 0 && !=` so the first read never fires) — a pin
  move is exactly when the cluster can re-pick a wrong QPSK marker lag. Guarded by
  `tests/av_sync_dock_pin_observability_1319.rs` (source anchor) AND a pwsh presence check in BOTH
  `windows-genlock*.yml` (the two-language double coverage `av-sync-dock-anchor-refactor-safety.md`
  mandates). Root-causing the dock bias itself (the cluster lag pick after a pin write) is the
  remaining follow-up this observability enables.

**Tier-0:** `pytest tests/python/test_av_band_quality_1319.py` (+ the extended `test_av_band_1319.py`,
gather + step + notify-dedup siblings) + `bash -n`/`shellcheck` + a functional `curl`-stubbed run of
`av_sync_persist_dock_reference` (good quality merges the dock keys, bad/absent records nothing);
the C++ compiles at CI only. The band arm is held OFF via a dev1 `AV_BAND_MS=100000` drop-in until
this deploys; re-enable after.

## #1319 P3 — the STEP arm gets the SAME dock-quality gate (both arms quality-gated)

Part 2 quality-gated only the BAND arm. The STEP arm (`classify_av_step`/`analyze`, the path
`handle_box` calls) was still deciding purely on `|recent_med − base_med| > threshold` — so on
**16.9.2026** it fired **two owner pages** (13:44 `step 1069 ms`, 15:09 `step −92 ms`, plus the
hourly „still stepped" re-arm) on readings whose medians swung `recent_med −1391…+940 ms` /
`base_med −887…+625 ms` **within 5-minute passes** — impossible for a real upstream A/V shift. In the
SAME passes the BAND arm already logged `verdict=LOW_QUALITY … band not judged, no page`, because
`band_quality_ok()` (mad 9-31 ms wide, matched down to 10) rejected the reading. The two arms
disagreed because only one consulted quality.

The fix: **wire the STEP arm through the SAME `band_quality_ok()` predicate** (the P2 one, its module
defaults `DEFAULT_BAND_QUALITY_MAX_MAD_MS=15.0` / `DEFAULT_BAND_QUALITY_MIN_MATCHED=30` — single-sourced,
never a retyped 15/30).

- **`classify_av_step`** gains `recent_mad_ms` / `recent_matched_min` params and one check —
  `band_quality_ok(...) is False → LOW_QUALITY` — positioned AFTER REPIN and BEFORE the STEP/HEALTHY
  decision, byte-identical ordering to `classify_av_band`. The three-state ABSENT-vs-CORRUPT rule is
  the same and load-bearing: an ABSENT facet (older box) → `None` (NOT False) → PROCEED to the legacy
  step judgement, so a genuine step is never swallowed; a NON-FINITE mad → False → LOW_QUALITY.
- **`analyze`** reads `av_offset_recent_mad_ms` / `av_offset_recent_matched_min` from the parsed
  bundle dict via the same `_float_or_none`/`_int_or_none` path `analyze_band` uses and passes them
  through. `extract_av_step` (the 7-field public helper) is untouched.
- **`handle_box`** gains a `LOW_QUALITY` case: log-only with the BAND arm's exact wording
  (`… dock reading not trustworthy … -- step not judged, no page`), NO page, NO „still stepped"
  re-ping, and NO RECOVERY ping. It resets ONLY the confirm counter (a noisy pass must not count
  toward the 2-pass confirm — mirroring the BAND arm's LOW_QUALITY) and leaves the alert latch
  (`alerted_/alert_base_`) untouched; crucially it does NOT run the HEALTHY branch, so
  `recovered_to_baseline` never treats a LOW_QUALITY pass as a recovery.

Live proof (16.9., stream, threshold unset): `recent_med 343.5 / base 404.5` (`step_ms −61.0`, a
pre-fix STEP page since |−61| > 45) now classifies **LOW_QUALITY — step not judged, no page**,
matching the BAND arm in the same pass. Until this deploys, the dev1 timer's STEP arm is neutered
with an `AV_STEP_THRESHOLD_MS=100000` drop-in (remove after deploy, same as the P2 band drop-in).

**Tier-0:** `pytest tests/python/test_av_step_stepgate_1319.py` (classify/analyze/CLI + a sourced-bash
orchestrator test that overrides `fetch_bundle_json` and asserts the LOW_QUALITY branch is log-only,
no notify, confirm reset) + the existing `test_av_step_decision_1267.py` (unchanged, still green).
