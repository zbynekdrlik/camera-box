---
paths:
  - "vendor/obs-studio/frontend/widgets/OBSBasicStatusBar.*"
  - "vendor/obs-studio/frontend/widgets/GenlockLockState.hpp"
  - "src/genlock_lock_state.rs"
  - "tests/genlock_lock_state_parity.rs"
  - "tests/genlock_lock_indicator_guards.rs"
  - "vendor/obs-studio/frontend/widgets/GenlockRecentEvents.hpp"
  - "tests/genlock_phase_baseline_1302.rs"
  - "vendor/obs-studio/frontend/widgets/GenlockRecentEvents.cpp"
  - "tests/genlock_idle_class_1302.rs"
  - "src/genlock_lock_state_idle_tests.rs"
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
| Per-source stats API | `obs.h` (`struct obs_genlock_stats`, `obs_source_get_genlock_stats`) + `obs-source.c` (`genlock_fill_stats`) | The `genlock-fifo audit` log line and the API BOTH route through `genlock_fill_stats` — they can never disagree. Additive + versioned (`OBS_GENLOCK_STATS_VERSION`, 4 since issue 1302: `audio_hold_mode` / `audio_withheld` / `audio_place_err_ms` / `audio_place_err_seeded`, which the audit line now prints from the same snapshot; the frontend names the mode through the export `obs_genlock_audio_hold_token`, never a copy of the strings). |
| Per-input event baseline (issue 1302) | `src/genlock_lock_state.rs` (`PhaseEventSample`, `input_new_phase_events`) ↔ `GenlockLockState.hpp` (`genlock_input_new_phase_events`, right after `genlock_input_phase_events`); the widget's state + tick in their own plain-C++ unit `GenlockRecentEvents.{hpp,cpp}` (registered in `frontend/cmake/ui-widgets.cmake`; the header does not include `GenlockLockState.hpp`, so the status bar's other includers see only std structs) | `tests/genlock_phase_baseline_1302.rs`: C-vs-Rust parity of the rule (512 vectors) + a replay of the widget's tick on the shipped C++ bytes graded by the C decision. Guards: `genlock_lock_recent_event_baseline_present_1302` + the issue-1302 pwsh block in both ymls. |
| Fast first idle classification (issue 1302) | `src/genlock_lock_state.rs` (`InputIdleClass`, `input_idle_class`, the `GENLOCK_IDLE_*` constants; unit cases in the `#[path]` child `genlock_lock_state_idle_tests.rs`) ↔ `GenlockLockState.hpp` (`genlock_input_idle_class` + the constants as macros, right after `genlock_input_new_phase_events`); the ring tick `genlock_idle_classify_tick` in `GenlockRecentEvents.cpp` (state `GenlockIdleClassifier`, the status bar member `genlockIdle`) | `tests/genlock_idle_class_1302.rs`: C-vs-Rust parity of the rule and its five constants (312 vectors) + a replay of the shipped ring tick and recent-event tick on six scenarios graded by the C decision, every tick's classes checked against a reference ring on the Rust authority. Guards: `genlock_lock_idle_first_classification_present_1302` + the issue-1302 idle-class pwsh block in both ymls. |
| Per-output stats API | `obs.h` (`struct obs_genlock_output_stats`, `obs_output_set_genlock_wall_stamping`, `obs_output_get_genlock_stats`) + `obs-output.c` + `obs-internal.h` (two bool fields, bzalloc-zeroed) | DistroAV's `ndi-output.cpp` sets `wall_stamping=true` at `begin_data_capture` success, `false` at stop. |
| The widget | `OBSBasicStatusBar.{hpp,cpp}` (`UpdateGenlockLabel`, `PollGenlockClock`) | A permanent `QLabel` + an ALWAYS-ON 1 Hz `QTimer` (NOT the stream-only `refreshTimer`). |
| Vendored-source guards | `tests/genlock_lock_indicator_guards.rs` | std-only, runnable via `rustc --test`; the Linux-CI twin of the pwsh gates. |
| Media-clock term (issue 1372 part D) | `src/genlock_lock_state.rs` (`MediaClock`, `MediaDiscipline`, `media_clock_window` (µs step band + time-weighted rate), `media_clock_window_ready`, `media_clock_verdict`; python mirror cross-checked by the same parity test) ↔ `GenlockLockState.hpp` (the `genlock_media_*` block after the qpc lift) + libobs `os_gettime_discipline()` (`util/platform.h`, `platform-windows.c`) | Parity: the decision grid × the three media verdicts + a 5th lift (`c_media_clock_matches_the_rust_authority_1372_part_d`); the libobs getter is checked read by read in `tests/os_clock_discipline_parity_1372.rs`. Guards: `genlock_lock_media_clock_term_present_1372_part_d` + the part-D pwsh block in both ymls. |
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
  - **The rate leaves wall steps out by their size in µs** (`genlock_media_clock_window_drift_us`:
    each pair with `0 < dt ≤ 5000 ms` gives `change_us × 1e6 / dt_ms` ppb; their median — the mean of
    the two middle rates for an even count — is the centre rate; a pair whose change is more than
    `GENLOCK_MEDIA_CLOCK_BAND_US` = 100 µs off `centre × dt / 1e6` is a wall STEP and left out; the rate
    is the TIME-WEIGHTED rate of the kept pairs, `Σ change × 1e6 / Σ dt`, scaled to the window, `× 600 /
    1000` µs). The deviation of a pair that spans a step IS the step, whatever the interval. dantesync
    requests steps of ≥ 200 µs (server, when not PTP-locked) / ≥ 500 µs (client) / `1000 µs + 2 × ppm
    × 10 s` (a locked master with phase_slew off), 2.5 ms at the cap, so they never count while they
    touch fewer than half the pairs. On Windows dantesync computes the step target from the coarse
    `GetSystemTimeAsFileTime`, so a step lands up to one timer tick short and a small remnant can fall
    inside the band (the −146 µs seen on win-resolume is likely one): unbiased, ≤ 100 µs each, < 0.7 ms
    per window at 30 steps. A drift in only PART of the seconds moves a ~1 s pair by its rate in µs, so
    up to ~95 ppm with ±50 ms tick jitter (20 ppm on 5 s pairs) it is kept and counted at its true
    share; time weighting makes each pair count by its duration. Review rounds 1–5 went through a 33 ms exclusion, a rate-bounded per-sample exclusion on
    integer ms, the same on µs, a pure median (partial drift invisible, 600 µs quantisation) and a
    25 ppm band around the median rate (partial drift > 25 ppm invisible, then over-read); each left a
    gap. Known limit: a drift faster than 100 ppm present in fewer than half the seconds reads as steps
    (a raw-QPC fallback, the fast case, is the discipline outcome's).
  - The window is ready (`genlock_media_clock_window_ready`) only once the COUNTED pairs cover ≥ 90 %
    of it, so a UI thread that keeps stalling past 5 s never reads as a healthy OK. Until then the
    widget publishes `drift_us` = 0 (label, tooltip, JSON; `ready:false`): a single step pair while the
    window fills is its own centre and would read as a huge rate.
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
- **`recent_event` is derived by the WIDGET, not libobs** — `recent_event = (now − last) < 60 s`,
  where `last` is the last 1 Hz tick that saw a NEW phase event. This keeps libobs free of a new
  hot-path remembered-state field.
  - **#1299 Part 3: PHASE events of CONNECTED inputs only.** An input's phase total is
    `relocks + late_holds + backward_steps` via the pure `genlock_input_phase_events` (mirrored in
    `GenlockLockState.hpp`, parity-gated). UNDERRUNS are DROPPED from the lock verdict (a
    latency-budget miss owned by the `genlock-fifo audit` + cg-chain-verify, and bursty — counting
    them latched the 60 s window chronically), and an ABSENT or IDLE input contributes 0. Underruns
    stay in the per-input facet counters (report-only).
  - **Issue 1302: each input is counted against its OWN baseline.** The #1299 widget summed every
    contributing input's LIFETIME total into one aggregate and raised the event on any rise, so an
    input that reconnected or woke from idle added its whole total at once: the box read DEGRADED
    `recent_event` for 60 s after every reattach (the SongPlayer A/V gate found it). Now
    `genlock_recent_events_tick` (`GenlockRecentEvents.cpp`, over the state in
    `GenlockRecentEvents.hpp`) asks the parity-gated `genlock_input_new_phase_events` per input
    against what it remembered last tick: the rise only when the input contributed (connected and
    not idle) in BOTH samples; a reconnect, a wake, a first sight or a backward total re-baselines
    (0). An input that leaves the scan is forgotten, so its return is a first sight. The counters
    stay cumulative in libobs (the audit delta readers depend on that). Real events after the attach
    still DEGRADE for exactly 60 s.
  - **The DEGRADED/recent_event reason NAMES the offender** — since issue 1302 the input with the
    most NEW events in the 60 s window (its own window of `(tick, new events)`), ties to the first
    in scan order. It rides the `genlock-lock-json:` line as `recent_event_inputs:[{name, events}]`
    (`events` = those windowed new events since v8, the lifetime total before) and the human
    `genlock-lock:` line as `reason=recent_event:<name>`; `genlock_lock_decision.analyze` enriches
    the watchdog's reason to `recent_event:<name>` so a page is actionable.
  - **Tier-0 proof of the tick on the shipped bytes:** `tests/genlock_phase_baseline_1302.rs`
    compiles `GenlockRecentEvents.cpp` as shipped (keep it plain std C++; the test refuses an OBS/Qt
    include) with g++ and the real headers, replays scripted 1 Hz scenarios (a 40-relock reattach, a
    real event after it, a wake, a vanish, a backward reset, the offender window and its tie, an
    offender that leaves the scan inside the window, saturation of both sums) graded by the C
    decision, and checks every tick against a reference built on the Rust authority.
  - **A (re)connected input is UNCLASSIFIED until it proves a live rate (issue 1302, ROZHODNUTÉ
    6028553391, design 6028838843).** Before it, the #1341 ring classified an input only once it
    spanned 90 % of the window, so a reconnected keep-alive playlist input (and every keep-alive input
    after an OBS start) counted as live for ~54 s and each keep-alive relock in that time was a new
    event. Now each connected input's ring tick (`genlock_idle_classify_tick`) asks the parity-gated
    `genlock_input_idle_class(span_ms, delta_frames, prev_class)`:
    - a ring spanning the full window (54 s): the #1341 rule, IDLE below 60 frames, else LIVE;
    - the fast rule: a ring spanning >= 5 s with >= 60 frames in it (>= 12 fps) is LIVE, whatever
      the previous class. A keep-alive input can never meet it, so it also safely promotes an IDLE
      input that went live while the widget timer stalled (5 s after the stall, not 54 s; the
      `idle_wakes_in_stall` replay, review round 1);
    - otherwise the previous class holds on a short ring: LIVE is never demoted when a stalled
      timer pruned the ring to one sample (the `widget_stall` replay), IDLE stays IDLE, UNCLASSIFIED
      stays UNCLASSIFIED. The fast stage never says IDLE, so a slow 1-5 fps source is LIVE once it
      has delivered 60 frames (5 fps at 12 s, 2 fps at 30 s) and a 1 fps one is left to the full
      window as before.

    A first sight, a received counter that goes backward (the ring and the class are cleared) or an
    input that left the scan (forgotten) restarts at UNCLASSIFIED. Only a LIVE input is graded: the
    widget gives UNCLASSIFIED and IDLE inputs the idle path (`r.idle`: out of `n_locked` /
    `n_connected`, 0 phase events, never blamed as unlocked), so the per-input event baseline is
    taken at the first LIVE tick and a keep-alive input never contributes. Consequences: a real new
    input's events are hidden for its first ~5 s; at an OBS start every input is UNCLASSIFIED for
    ~5 s, so the box reads HEALTHY-idle LOCKED (n_connected 0) until the live ones are proven; in the
    v8 JSON and the tooltip an UNCLASSIFIED input reads `"idle": true` / low-rate (no schema change).
    The window constants live in `GenlockLockState.hpp` now (`GENLOCK_IDLE_WINDOW_MS`,
    `GENLOCK_IDLE_INPUT_MIN_FRAMES`, `GENLOCK_IDLE_FAST_SPAN_MS`, `GENLOCK_IDLE_FAST_MIN_FRAMES`), never a
    second copy in the widget.
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
- **A coordinated dantesync fleet DATE step is BOOKED, not DEGRADED (issue 1372).** Before
  pushing this tick's sample, the widget asks the parity-gated `genlock_qpc_wall_step_rebase_ms`
  (GenlockLockState.hpp, right after the verdict ↔ `src/genlock_lock_state.rs::qpc_wall_step_rebase_ms`,
  parity-gated by `tests/genlock_qpc_wall_step_parity_1372.rs`) whether the jump against the previous
  sample is a date step: `33 < |jump| ≤ 66 ms` (`GENLOCK_QPC_WALL_STEP_BOOK_MAX_MS`, two frames:
  dantesync steps the date at a 50 ms error) and fewer than `GENLOCK_QPC_WALL_STEPS_PER_WINDOW` = 1
  booked in the 300 s window (`genlockQpcBookedSteps`). A booked jump re-bases every older history
  sample by it and logs ONE `genlock-wall-step:` line; the widget stays LOCKED. The verdict function
  is unchanged, so a bigger jump (> 66 ms: a clock set, an NTP-fallback step) or a step STORM (a 2nd
  step in the window) still DEGRADES `qpc_drift`. Why booking is right: the media clock never follows a step by design (issue 1372
  part A), and the render tick re-grids onto the stepped wall in one tick (`genlock-wall-step.md`),
  so a date step is no longer the genlock hazard this term was guarding. Anchors:
  `tests/genlock_lock_indicator_guards.rs::qpc_drift_books_a_fleet_date_step_1372` + the issue-1372
  pwsh block in both `windows-genlock*.yml`.
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
  `vendored-obs-frontend-crash-safety.md`. **A stats-version bump makes a fast deploy UNSAFE, not
  just incomplete (issue 1302 review):** the caller allocates `struct obs_genlock_stats` and passes no
  size, so a NEW obs.dll writes the bigger struct (memset + fill) past an OLD frontend's stack copy.
  The `version` field only protects the other pairing (an old libobs under a new frontend: the
  frontend reads the v4 fields only when `version >= 4`). For this bump that pairing never even
  reaches the struct on Windows: the new frontend imports `obs_genlock_audio_hold_token`, which an
  old obs.dll lacks, so obs64 does not start. On Linux (lazy binding) it starts and runs without
  the per-input audio keys, because the import is called only under `version >= 4`.
  Since the issue-1302 follow-up `deploy-genlock-fleet.sh --fast` refuses it mechanically: every
  full-bundle deploy records its frontend's `OBS_GENLOCK_STATS_VERSION` in `GENLOCK_STATS_ABI.txt`,
  and the FAST program refuses (exit 13) when that marker is missing or differs from the new
  obs.dll's version (`genlock-fleet-deploy.md`). A struct change MUST bump
  `OBS_GENLOCK_STATS_VERSION` (the obs.h comment's rule), or the gate cannot see it; the pytest pins
  the struct body to its version so a change without a bump fails CI. `struct
  obs_genlock_output_stats` (the per-output facet) is on the frontend's stack the same way; the gate
  does not compare its version yet, so its body is pinned at `OBS_GENLOCK_OUTPUT_STATS_VERSION 1`
  and a change fails CI until the gate covers it. CI is the first place the C/Qt compiles — locally only
  `cargo fmt --all --check` + the pure Rust module (`rustc --test`) + the parity/guard tests
  (standalone) verify; the blog refactor's format/arg types were lift-compiled under `gcc
  -Wformat=2 -Werror`.
