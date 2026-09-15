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
| C-vs-Rust parity gate | `tests/genlock_lock_state_parity.rs` | Lifts the C block, `cc`-compiles it, compares `(state, reason)` over all 2^8 flag combos × a set of `(n_inputs, n_locked, n_absent)` triples (the 8th flag is #1303's `audio_unpaired`; the `n_absent` axis is #1299's — some-absent-but-all-connected-locked → LOCKED, some-absent-with-a-connected-unlocked → DEGRADED, all-absent → HEALTHY-idle, and the impossible `n_absent > n_inputs` both ports saturate to `n_connected=0`). |
| Per-source stats API | `obs.h` (`struct obs_genlock_stats`, `obs_source_get_genlock_stats`) + `obs-source.c` (`genlock_fill_stats`) | The `genlock-fifo audit` log line and the API BOTH route through `genlock_fill_stats` — they can never disagree. Additive + versioned (`OBS_GENLOCK_STATS_VERSION`). |
| Per-output stats API | `obs.h` (`struct obs_genlock_output_stats`, `obs_output_set_genlock_wall_stamping`, `obs_output_get_genlock_stats`) + `obs-output.c` + `obs-internal.h` (two bool fields, bzalloc-zeroed) | DistroAV's `ndi-output.cpp` sets `wall_stamping=true` at `begin_data_capture` success, `false` at stop. |
| The widget | `OBSBasicStatusBar.{hpp,cpp}` (`UpdateGenlockLabel`, `PollGenlockClock`) | A permanent `QLabel` + an ALWAYS-ON 1 Hz `QTimer` (NOT the stream-only `refreshTimer`). |
| Vendored-source guards | `tests/genlock_lock_indicator_guards.rs` | std-only, runnable via `rustc --test`; the Linux-CI twin of the pwsh gates. |
| pwsh source-anchor gates | `windows-genlock.yml` + `windows-genlock-fast.yml` (`Assert in-OBS genlock LOCK indicator present (#1298)`) | 3-copy lock-step per `obs-titlebar-build-id.md`. |

## State decision (the contract)

`genlock_decide_lock_state(facets, &reason)` — UNLOCKED (red) takes precedence clock > output >
no-input-locked; then DEGRADED (amber) precedence some-input-unlocked > recent-event > ntp-failed
> qpc-drift > **audio-pairing (#1303, lowest)**; else LOCKED (green).

- **UNLOCKED:** clock absent/not-locked (`no clock discipline` / `clock not locked`); OR a genlock
  NDI output is present but not stamping wall time (`output not stamping`); OR inputs exist but
  none locked (`no input locked`) / none configured (`no genlock inputs`).
- **DEGRADED:** some (not all) inputs unlocked (names the input, e.g. `NDI cam7`); OR a
  relock/late-hold/backward-step on a CONNECTED input in the last 60 s (#1299 Part 3: underruns are
  NO LONGER a recent-event class; the label names the offender, `recent event: cg`); OR clock
  `ntp_failed`; OR `wall_qpc_drift` beyond `GENLOCK_QPC_DRIFT_BOUND_MS` (100 ms); OR (#1303, lowest
  precedence) an audio-ENABLED genlock source whose `|audio_pairing_offset_ms|` breaches
  `GENLOCK_AUDIO_PAIRING_BOUND_MS` (33 ms / one 30 fps frame) — `audio unpaired: <src>`. The widget
  aggregates the per-source breach into `GenlockFacets.audio_unpaired` (the twin of how it reduces
  per-input qpc drift), surfacing the pairing-offset branch of
  `genlock_audio_pairing::decide_audio_health`. Audio disabled/absent NEVER degrades (the
  `audio_enabled` guard), so a camera input with `ndi_audio=false` is silent by design; the
  AudioDisabledOnProgram + AsrcSaturated branches (which need is-program-source / asrc-ppm data the
  v2 stats don't carry) are a deferred followup.
- **LOCKED:** `GENLOCK ● LOCKED n/m @ L ms` (n=locked, m=**connected** genlock inputs = `n_inputs - n_absent`, L=min latency or a range), plus ` (+K idle)` when `K = n_absent > 0` — #1299: a senderless input shows as idle, never as an unlocked shortfall.

## Gotchas

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
