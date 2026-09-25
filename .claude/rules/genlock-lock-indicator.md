---
paths:
  - "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.*"
  - "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp"
  - "src/genlock_lock_state.rs"
  - "tests/genlock_lock_state_parity.rs"
  - "tests/genlock_lock_indicator_guards.rs"
---

# In-OBS GENLOCK LOCK indicator (#1298)

The statusbar item every managed OBS (strih / stream / imag-nb / cg) shows so the operator sees
at a glance whether the box is LIVE-LOCKED to the fleet timing. Three facets → one decision → one
widget. Sibling tickets: #1294 (§7 three-state vocabulary contract), #1299 (the fleet bundle-state
facet + dev1 watchdog that CONSUMES the same structs over obs-websocket).

## Where each piece lives

| Piece | File | Notes |
|---|---|---|
| The DECISION (pure) | `src/genlock_lock_state.rs` (`decide`) | Tier-0 authority, crate-root, std-only. |
| The DECISION (C port) | `vendor/obs-studio/frontend/widgets/GenlockLockState.hpp` (`genlock_decide_lock_state`) | Byte-for-byte mirror; OBS/Qt-free so the parity gate lifts + `cc`-compiles it. Keep the two enums + struct + fn CONTIGUOUS (the lift slices from the first enum through the fn's closing brace). |
| C-vs-Rust parity gate | `tests/genlock_lock_state_parity.rs` | Lifts the C block, `cc`-compiles it, compares `(state, reason)` over all 2^9 flag combos × the three media-clock verdicts × a set of `(n_inputs, n_locked, n_absent, n_idle)` tuples (a TABLE + loop harness since issue 1372 part D: ~35k straight-line assignments took ~1 min at `-O1`, the table compiles in < 1 s — keep new axes in the table) (the 8th flag is #1303's `audio_unpaired`; the `n_absent` axis is #1299's — some-absent-but-all-connected-locked → LOCKED, some-absent-with-a-connected-unlocked → DEGRADED, all-absent → HEALTHY-idle, and the impossible `n_absent > n_inputs` both ports saturate to `n_connected=0`). |
| Per-source stats API | `obs.h` (`struct obs_genlock_stats`, `obs_source_get_genlock_stats`) + `obs-source.c` (`genlock_fill_stats`) | The `genlock-fifo audit` log line and the API BOTH route through `genlock_fill_stats` — they can never disagree. Additive + versioned (`OBS_GENLOCK_STATS_VERSION`). |
| Per-output stats API | `obs.h` (`struct obs_genlock_output_stats`, `obs_output_set_genlock_wall_stamping`, `obs_output_get_genlock_stats`) + `obs-output.c` + `obs-internal.h` (two bool fields, bzalloc-zeroed) | DistroAV's `ndi-output.cpp` sets `wall_stamping=true` at `begin_data_capture` success, `false` at stop. |
| The widget | `OBSBasicStatusBar.{hpp,cpp}` (`UpdateGenlockLabel`, `PollGenlockClock`) | A permanent `QLabel` + an ALWAYS-ON 1 Hz `QTimer` (NOT the stream-only `refreshTimer`). |
| Vendored-source guards | `tests/genlock_lock_indicator_guards.rs` | std-only, runnable via `rustc --test`; the Linux-CI twin of the pwsh gates. |
| Media-clock term (issue 1372 part D) | `src/genlock_lock_state.rs` (`MediaClock`, `MediaDiscipline`, `media_clock_window` (trimmed mean), `media_clock_window_ready`, `media_clock_verdict`; python mirror cross-checked by the same parity test) ↔ `GenlockLockState.hpp` (the `genlock_media_*` block after the qpc lift) + libobs `os_gettime_discipline()` (`util/platform.h`, `platform-windows.c`) | Parity: the decision grid × the three media verdicts + a 5th lift (`c_media_clock_matches_the_rust_authority_1372_part_d`); the libobs getter is checked read by read in `tests/os_clock_discipline_parity_1372.rs`. Guards: `genlock_lock_media_clock_term_present_1372_part_d` + the part-D pwsh block in both ymls. |
| pwsh source-anchor gates | `windows-genlock.yml` + `windows-genlock-fast.yml` (`Assert in-OBS genlock LOCK indicator present (#1298)`) | 3-copy lock-step per `obs-titlebar-build-id.md`. |

## State decision (the contract)

`genlock_decide_lock_state(facets, &reason)` — UNLOCKED (red) takes precedence clock > output >
no-input-locked; then DEGRADED (amber) precedence some-input-unlocked > recent-event > ntp-failed
> qpc-drift > **media-clock (issue 1372 part D)** > audio-pairing > **audio-unexpected (#1303,
lowest)**; else LOCKED (green).

- **The media-clock (audio clock) term — issue 1372 part D.** `os_gettime_ns()` paces the audio
  mixer, the video thread and every output. Since part A (`windows-disciplined-media-clock.md`) it
  runs at the dantesync-disciplined rate on Windows too (Linux's `CLOCK_MONOTONIC` always did), so
  the wall-vs-media offset must stay FLAT on every box apart from wall steps.
  - **The widget samples the offset ITSELF, in µs, every tick** (`genlock_wall_minus_media_us`: the
    wall clock — `std::chrono::system_clock`, which on MSVC is `GetSystemTimePreciseAsFileTime` and on
    libstdc++ `CLOCK_REALTIME`, the same clocks libobs' `genlock_wall_now_ns` reads — between two
    `os_gettime_ns()` reads, against their midpoint, retried while the bracket is > 50 µs; dev1
    measured ±1 µs sample-to-sample). NOT the libobs `wall_qpc_drift_ms`: that is integer ms truncated
    toward zero, and a 1–2 ms dantesync phase step reads like one second of a rate. It samples with or
    without genlock inputs, so the ring has no gaps.
  - **The rate is a TRIMMED MEAN of the per-pair rates** (`genlock_media_clock_window_drift_us`: each
    pair with `0 < dt ≤ 5000 ms` gives `change_us × 1e6 / dt_ms` ppb; their median — the mean of the
    two middle rates for an even count — is the centre; the result is the mean of the rates within
    `GENLOCK_MEDIA_CLOCK_BAND_PPB` = 25 ppm of it, scaled to the window, `mean × 600 / 1000` µs). A
    rate is in every pair; a wall step only in the pair that spans it, ≥ 146 000 ppb from the centre on
    a 1 s pair (dantesync steps from ~150 µs when not PTP-locked, ≥ 500 µs as a client,
    `1000 µs + 2 × ppm × 10 s` as a locked master with phase_slew off, 2.5 ms at the cap, the −146 µs
    seen on win-resolume), so steps of any size never count while they touch fewer than half the pairs.
    A drift present in only PART of the seconds (≤ 25 ppm) stays inside the band and is averaged in at
    its true share — a pure median dropped it below half coverage and quantised to 600 µs steps. Review
    rounds 1–4 went through a 33 ms exclusion, a rate-bounded per-sample exclusion on integer ms, the
    same on µs, and a pure median; each left a gap (sub-frame steps counted, a band of real steps
    counted, partial drift invisible). Known limit: a drift faster than 25 ppm present in fewer than
    half the seconds is not averaged in (a raw-QPC fallback, the fast case, is the discipline outcome's).
  - The window is ready (`genlock_media_clock_window_ready`) only once the COUNTED pairs cover ≥ 90 %
    of it, so a UI thread that keeps stalling past 5 s never reads as a healthy OK.
  - `genlock_media_clock_verdict(ready, drift_us, 2000, discipline, clock_present)`: `UNDISCIPLINED`
    when the Windows `os_gettime_discipline()` (libobs, `util/platform.h`) reports a raw-QPC fallback
    (disabled / read failed / API missing) while dantesync answers — at once, before any drift
    accrues; else `DRIFT` when the window is ready and `|drift| > 2 ms` per 10 min (3.3 ppm); else
    `OK`. Linux passes `NOT_APPLICABLE` (drift only).
  - Calibration: the undisciplined stream mixer drifted 67 ms / 83 min (≈ 8 ms per 10 min, green the
    whole time); after the part-A deploy 0 ms over 47 min; strih-lx 0 for hours. Those numbers are
    the ms-resolution libobs term; the µs signal was measured only on dev1 (±1 µs). **Supervisor step
    after the full-bundle deploy:** read `media_clock.drift_us` from each box's `genlock-lock-json:`
    line (bundle-state `genlock_lock.media_clock`) for ≥ 10 min — strih-lx, stream, resolume — and
    confirm it sits well inside ±2000 before relying on the DEGRADED term.
  - Known limit: `std::chrono::system_clock` is only as fine as the STL makes it. A coarse (15.6 ms)
    wall clock would give per-pair rates of 0 or ±15 ms/s and hide a real drift; no fleet box has one.
  - The widget reduces it in `ReduceGenlockMediaClock` (and the label text in
    `genlock_reason_text`), so `UpdateGenlockLabel` stays under ~300 lines. The media sub-kind is part
    of the `genlock-lock:` / JSON change key while the reason is `media_clock`, so a drift ↔
    undisciplined switch logs at once.
  - **Deploy: the FULL bundle, never the fast obs.dll alone or the frontend alone.** The widget
    imports the new obs.dll export `os_gettime_discipline`; a frontend without the matching obs.dll
    fails to load ("entry point not found").
  - Anything but OK DEGRADES with `LockReason::MediaClock` (= 11), label `audio clock drift N.N ms/10
    min` / `audio clock not disciplined (<outcome>)`, human line `reason=media_clock:<kind>`, JSON v7
    `media_clock:{state, drift_us, window_s, ready, discipline}`. It NEVER makes a box UNLOCKED.
  - This is NOT the rate term #1357 removed (that compared a windowed rate with one instantaneous
    dantesync `f_ptp + f_phase` sample — a different meaning per box). This one compares the rate
    with 0, which means the same thing on every box once part A is deployed. A Windows box still on
    a pre-part-A obs.dll will (correctly) DEGRADE `media_clock:drift` after ~10 min.

- **UNLOCKED:** clock absent/not-locked (`no clock discipline` / `clock not locked`); OR a genlock
  NDI output is present but not stamping wall time (`output not stamping`); OR inputs exist but
  none locked (`no input locked`) / none configured (`no genlock inputs`).
- **DEGRADED:** some (not all) inputs unlocked (names the input, e.g. `NDI cam7`); OR a
  relock/late-hold/backward-step on a CONNECTED input in the last 60 s (#1299 Part 3: underruns are
  NO LONGER a recent-event class; the label names the offender, `recent event: cg`); OR clock
  `ntp_failed`; OR (#1299 Part 4 + #1357) the wall clock STEPPED — a single-sample wall STEP exceeds
  `GENLOCK_QPC_STEP_BOUND_MS` (33 ms = one 30 fps frame, judged immediately), `clock step N ms`.
  That is the WHOLE `qpc_drift` verdict, identical on every box: neither the cumulative offset (grew
  unbounded on a dantesync-disciplined Windows box, false-paged the fleet after ~2 h) nor a rate
  (see the Gotchas bullet — 0 by construction on Linux, the free crystal on Windows) gates; OR (#1303, lowest
  precedence, part 3b) an audio-ENABLED genlock source whose `|audio_pairing_offset_ms|` breaches
  `GENLOCK_AUDIO_PAIRING_BOUND_MS` (33 ms / one 30 fps frame) — `audio unpaired: <src>`. The widget
  aggregates the per-source breach into `GenlockFacets.audio_unpaired` (the twin of how it reduces
  per-input qpc drift), surfacing the pairing-offset branch of
  `genlock_audio_pairing::decide_audio_health`; OR (#1303, **the lowest** DEGRADED axis)
  `GenlockFacets.audio_unexpected` — a source that is AUDIBLE (`ndi_audio=true`) when it is
  silent-by-contract per the certified per-box audio table, `audio unexpected: <src>` +
  `LockReason::AudioUnexpected` (=10). The SHIPPED subset is BOX-CLASS-AGNOSTIC: an audio-enabled
  CAMERA input (`genlock_name_is_camera` — the C mirror of `genlock_forced_table_audit::is_camera_input`,
  parity-gated), which is silent-by-contract on EVERY box, so it needs no box identity and can never
  false-DEGRADE a correctly-configured box (cameras are forced `ndi_audio=false`). Audio
  disabled/absent NEVER degrades (either term), so a camera input with `ndi_audio=false` is silent by
  design. **DEFERRED** (needs a robust deploy-written box-role marker — the widget does NOT know its
  box class today): the box-class-DEPENDENT audio cases — a NON-camera input audible on a Dante-fed
  box (strih/stream/imag), and a program source SILENT on the cg box (resolume) — which are already
  covered at DEPLOY time by the #1303 part-4 preflight (`genlock_forced_table_audit` +
  `deploy-genlock-fleet.sh`), so the live version is defense-in-depth. The
  AudioDisabledOnProgram + AsrcSaturated branches of `decide_audio_health` (which need
  is-program-source / asrc-ppm data the v2 stats don't carry) remain a deferred followup.
- **LOCKED:** `GENLOCK ● LOCKED n/m @ L ms` (n=locked, m=**connected-live** genlock inputs = `n_inputs - n_absent - n_idle`, L=min latency or a range), plus ` (+K idle)` when `K = n_absent + n_idle > 0` — #1299/#1341: a senderless OR a keep-alive-only input shows as idle, never as an unlocked shortfall.

## Gotchas

- **A CONNECTED-but-IDLE input (`n_idle`, #1341) is ALSO excluded from the DEGRADED gate — the
  keep-alive-sender class.** The cg OBS (RESOLUME-SNV) has 12 SongPlayer playlist inputs; the ~10
  idle ones keep a LIVE NDI connection (`connected == true`) but send one keep-alive frame every
  ~11 s, so their FIFO re-acquires a boundary (a relock) on each keep-alive frame — which fed
  `recent_event` and flapped the box `DEGRADED reason=recent_event` ~1 min/hour. The widget derives
  per-input `idle` from the `frames_received` DELTA over the same 60 s window `recent_event` uses:
  `< GENLOCK_IDLE_INPUT_MIN_FRAMES` (60 = 1 fps × 60 s; a live 23.98 fps source is ≥ 1400) is idle,
  classified only once the per-input sample ring spans ≥ 90 % of the window (the qpc `rate_ready`
  precedent, so a live source is never mislabelled idle at startup); a received-counter DECREASE
  re-baselines (reconnect). An idle input is excluded from `n_locked` by the widget scan, counted in
  `GenlockFacets.n_idle`, dropped from `n_connected = n_inputs - n_absent - n_idle` (saturating), and
  contributes 0 phase events via `InputEventCounts.idle` (the `connected == false` path) so it is
  never the `recent_event` offender. A box whose inputs are ALL idle/absent stays HEALTHY-idle
  LOCKED. `idle` is orthogonal to `absent`: absent = `connected == false` (sender not running); idle
  = connected but keep-alive-only. JSON schema v6 (additive): top-level `n_idle`, per-input `idle`.
- **An input with NO NDI receiver connection (`n_absent`, #1299) is EXCLUDED from the DEGRADED
  gate, not counted as unlocked.** The DEGRADED/no-input decisions judge only CONNECTED inputs
  (`n_connected = n_inputs - n_absent`): a genlock input whose sender is simply not running
  (stream's 'NDIA cg stream') is idle, not a fault, so it never pages — a dead/frozen sender is the
  reachability (#1001) / frozen-input (#1052) watchdogs' concern. Inputs-present-but-ALL-senderless
  (`n_connected == 0`, `n_inputs > 0`) is HEALTHY-idle → LOCKED, never UNLOCKED (the 3-state enum
  has no UNKNOWN, and UNKNOWN is a watchdog facet-absence concept, not a lock state); `n_inputs == 0`
  (no genlock configured at all) stays UNLOCKED/`no_genlock`. The producer chain: DistroAV's
  `ndi-source.cpp` receiver loop → `obs_source_set_genlock_connected` (runtime-resolved, from
  `recv_get_no_connections() > 0`) → `obs_source.genlock_connected` (default **true** at create, so
  an unreported source / an old build with the setter unresolved never masks a real degrade) →
  `obs_genlock_stats.connected` (v2→v3) → the widget's `n_absent` + per-input `connected` (JSON
  schema v1→v2). Both new JSON fields are additive: `bundle_state_gather` defaults `n_absent`→None
  and `connected`→True, so a v1 line from an older build reads exactly as pre-#1299.
- **The output facet is ABSENT on a pure receiver (imag) and must NOT force UNLOCKED.** The widget
  only penalizes `output_present && !output_stamping`; `output_present` requires an ACTIVE output
  whose `is_genlock_output` flag is set (only DistroAV's dedicated NDI output sets it). A
  filter-based sender (`ndi-filter`) is NOT an `obs_output`, so a box that publishes its program
  via a source FILTER reads the output facet as absent (safe). Extending to the filter path is a
  follow-up, not this ticket.
- **The clock poll must never block the UI.** `QNetworkAccessManager` + `setTransferTimeout(500)`
  (async on the event loop). `clock_present` = a successful `:8898/status` poll within the last 3 s
  — so killing dantesync flips the widget to UNLOCKED within ~3-5 s (the acceptance bar). JSON is
  parsed with OBS's own `obs_data_create_from_json` (no new dependency).
- **`recent_event` is derived by the WIDGET, not libobs** — it stamps "now" on any INCREASE of an
  aggregate cumulative counter across its 1 Hz samples (a decrease = reconnect reset → re-baseline,
  no event), and `recent_event = (now − last) < 60 s`. This keeps libobs free of a new hot-path
  remembered-state field.
  - **#1299 Part 3: that aggregate is CONNECTED inputs' PHASE events only.** The driver is
    `sum over connected inputs of (relocks + late_holds + backward_steps)` — computed post-scan via
    the pure `genlock_input_phase_events` (mirrored in `GenlockLockState.hpp`, parity-gated). UNDERRUNS
    are DROPPED from the lock verdict (a latency-budget miss owned by the `genlock-fifo audit` +
    cg-chain-verify / issue 1302, and bursty — counting them latched the 60 s window chronically),
    and an ABSENT input's #1096 rebind churn is excluded (`connected == false` contributes 0).
    Underruns stay in the per-input facet counters (report-only). The window itself is unchanged and
    correct — the fix was the FEED, not the window.
  - **The DEGRADED/recent_event reason NAMES the offender** — the connected input carrying the most
    phase events. It rides the `genlock-lock-json:` line as `recent_event_inputs:[{name, events}]`
    (schema v2→v3, additive, omit-when-absent) and the human `genlock-lock:` line as
    `reason=recent_event:<name>`; `genlock_lock_decision.analyze` enriches the watchdog's reason to
    `recent_event:<name>` so a page is actionable.
- **`qpc_drift` is the wall STEP only — never the cumulative offset (#1299 Part 4), never a rate
  (#1357 scope C).** The libobs producer `genlock_wall_qpc_drift_ms()` is a wall-vs-`os_gettime_ns`
  accumulator since OBS start. What it measures DIFFERS PER OS, which is why no rate/offset term may
  gate:
  - **Windows** (`os_gettime_ns` = QPC, the free crystal): the wall is slewed to the GM rate, so the
    accumulator grows ~13 ppm ≈ 47 ms/h BY DESIGN (stream 24.9.: 0 → 250 ms over 5.4 h). The old
    `> 100 ms` gate false-paged the fleet after ~2 h (#1299 Part 4).
  - **Linux** (`os_gettime_ns` = `CLOCK_MONOTONIC`, which the kernel frequency-disciplines together
    with `CLOCK_REALTIME`): the accumulator stays 0 by construction (strih-lx 24.9.: 0 on every
    sample for 5.2 h).
  - The #1299 Part 4 RATE branch compared the 300 s windowed rate with ONE instantaneous dantesync
    `f_ptp_ppm + f_phase_ppm` sample (-160..+171 ppm on the strih-lx NTP master). On Linux that
    re-reported a dantesync servo excursion (28 false DEGRADED samples, 24.9.); on Windows the
    instantaneous spikes tripped it against a steady 13-23 ppm rate (4 samples). #1357 removed it.
  - A rate is not a genlock hazard on any box: the render tick (`genlock_next_deadline`) re-derives
    every deadline from the wall clock per tick (2 ms/tick slew clamp), the release keys on the
    wall, and the ASRC servo runs against the mixer clock itself. The one clock hazard is a wall
    STEP: it moves every wall-keyed FIFO release / ts-align deadline by more than a frame at once
    (the render tick only slews through it) — same meaning on every box.
  - **What now notices a SECOND clock writer slewing the wall** (a w32time / systemd-timesyncd next
    to dantesync — the two-timesync-daemons incident class): dantesync's own lock/offset facet (the
    `clock` term + the dantesync clock watchdog), FIFO `recent_event` relocks/late-holds on the
    receivers, and the per-pass `qpc_drift_ppm` vs `qpc_expected_ppm` telemetry the
    genlock-lock watchdog logs. The removed rate term could only ever see it on Windows.

  The verdict is the pure `genlock_qpc_drift_beyond_bound(rate_ready, drift_delta_ms, elapsed_ms,
  max_step_ms, step_bound_ms, &measured_ppm)` (Rust authority in `src/genlock_lock_state.rs`, C
  mirror in `GenlockLockState.hpp`, python mirror in `scripts/genlock_lock_decision.py`,
  parity-gated by the 4th lift in `tests/genlock_lock_state_parity.rs`): `|max_step_ms| >
  GENLOCK_QPC_STEP_BOUND_MS` (33 ms), judged as soon as two samples exist. `measured_ppm` (once the
  `genlockQpcHistory` ring spans ≥ 90 % of `GENLOCK_QPC_WINDOW_S` = 300 s) and `qpc_expected_ppm`
  (`f_ptp + f_phase` from `:8898/status`) stay in the JSON as REPORT-ONLY telemetry, beside the raw
  cumulative `qpc_drift_ms`; the v5 JSON schema is unchanged. **Never re-introduce a rate or offset
  bound on the `qpc_drift` STEP verdict** — `tests/genlock_lock_json_guards.rs::genlock_lock_qpc_drift_is_the_step_only_1357`
  and both `windows-genlock*.yml` pwsh anchors forbid `GENLOCK_QPC_DRIFT_PPM_BOUND` in the widget. The
  separate media-clock term (issue 1372 part D, above) grades the drift GROWTH against 0, with its own
  constants and reason, and never touches the step verdict.
- **A `tests/genlock_lock_json_guards.rs` needle for a REAL C++ quote uses Rust `\"`, not the
  escaped-JSON `\\\"` (#1299 Part 4).** The guards `squish()` the source then `.contains(needle)`.
  A JSON KEY in the builder is an escaped-quote C string literal (`,\"qpc_drift_ppm\":`), so its
  needle is `"\\\"qpc_drift_ppm\\\":"` (→ literal `\"qpc_drift_ppm\":`). But a plain call like
  `obs_data_get_double(d, "f_ptp_ppm")` uses REAL C++ quotes, so its needle is
  `"obs_data_get_double(d, \"f_ptp_ppm\")"` (Rust `\"` → literal `"`). Using the `\\\"` form for a
  real-quote anchor fails the guard ("anchor GONE") even though the source is correct — distinguish
  the two when adding an anchor, and run `rustc --test tests/genlock_lock_json_guards.rs` (with
  `CARGO_MANIFEST_DIR` set) to confirm it actually matches before trusting green.
- **`genlock-lock:` is a new OBS-log family** (emitted on state/reason CHANGE, not every tick) —
  mutually non-substring with `genlock-fifo audit '`, `genlock-ndi-output audit '`,
  `genlock-ndi-filter audit '`, `genlock-relock`, `genlock-acquire-bracket '%s':` (guarded by
  `tests/genlock_lock_indicator_guards.rs`, per `jitter-audit-parser.md`).
- **The parity gate's lift anchor must NOT appear in the header's own doc comment (#1298 session
  trap).** `tests/genlock_lock_state_parity.rs` (and any future lift of a pure decision from a
  `.hpp`) slices from `typedef enum genlock_lock_state {` through the function's closing brace. The
  header's OWN explanatory comment originally quoted those literal strings (`typedef enum
  genlock_lock_state {`, `genlock_decide_lock_state(`), so `.find()` grabbed the COMMENT occurrence
  (earlier in the file) and the lifted "C" started mid-sentence → a stray-backtick compile error.
  Fix applied + the rule: anchor the function lift on the FULL DEFINITION signature (`static inline
  genlock_lock_state_t genlock_decide_lock_state(`), and keep the header comment from reproducing
  the enum/struct anchor literals verbatim. Same self-collision class the top-level CLAUDE.md +
  `vendored-libobs-change-safety.md` document for `recording-e2e.sh`/`obs-source.c` anchors.
- **This is a vendored FRONTEND change ⇒ FULL-BUNDLE deploy** (not a fast obs.dll hot-swap), per
  `vendored-obs-frontend-crash-safety.md`. CI is the first place the C/Qt compiles — locally only
  `cargo fmt --all --check` + the pure Rust module (`rustc --test`) + the parity/guard tests
  (standalone) verify; the blog refactor's format/arg types were lift-compiled under `gcc
  -Wformat=2 -Werror`.
