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
| C-vs-Rust parity gate | `tests/genlock_lock_state_parity.rs` | Lifts the C block, `cc`-compiles it, compares `(state, reason)` over all 2^7 flag combos × 10 input/locked pairs. |
| Per-source stats API | `obs.h` (`struct obs_genlock_stats`, `obs_source_get_genlock_stats`) + `obs-source.c` (`genlock_fill_stats`) | The `genlock-fifo audit` log line and the API BOTH route through `genlock_fill_stats` — they can never disagree. Additive + versioned (`OBS_GENLOCK_STATS_VERSION`). |
| Per-output stats API | `obs.h` (`struct obs_genlock_output_stats`, `obs_output_set_genlock_wall_stamping`, `obs_output_get_genlock_stats`) + `obs-output.c` + `obs-internal.h` (two bool fields, bzalloc-zeroed) | DistroAV's `ndi-output.cpp` sets `wall_stamping=true` at `begin_data_capture` success, `false` at stop. |
| The widget | `OBSBasicStatusBar.{hpp,cpp}` (`UpdateGenlockLabel`, `PollGenlockClock`) | A permanent `QLabel` + an ALWAYS-ON 1 Hz `QTimer` (NOT the stream-only `refreshTimer`). |
| Vendored-source guards | `tests/genlock_lock_indicator_guards.rs` | std-only, runnable via `rustc --test`; the Linux-CI twin of the pwsh gates. |
| pwsh source-anchor gates | `windows-genlock.yml` + `windows-genlock-fast.yml` (`Assert in-OBS genlock LOCK indicator present (#1298)`) | 3-copy lock-step per `obs-titlebar-build-id.md`. |

## State decision (the contract)

`genlock_decide_lock_state(facets, &reason)` — UNLOCKED (red) takes precedence clock > output >
no-input-locked; then DEGRADED (amber) precedence some-input-unlocked > recent-event > ntp-failed
> qpc-drift; else LOCKED (green).

- **UNLOCKED:** clock absent/not-locked (`no clock discipline` / `clock not locked`); OR a genlock
  NDI output is present but not stamping wall time (`output not stamping`); OR inputs exist but
  none locked (`no input locked`) / none configured (`no genlock inputs`).
- **DEGRADED:** some (not all) inputs unlocked (names the input, e.g. `NDI cam7`); OR a
  relock/underrun/late-hold/backward-step in the last 60 s (`recent relock/underrun`); OR clock
  `ntp_failed`; OR `wall_qpc_drift` beyond `GENLOCK_QPC_DRIFT_BOUND_MS` (100 ms).
- **LOCKED:** `GENLOCK ● LOCKED n/m @ L ms` (n=locked, m=genlock inputs, L=min latency or a range).

## Gotchas

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
- **`recent_event` is derived by the WIDGET, not libobs** — it tracks the aggregate cumulative
  counter (underruns+relocks+late_holds+backward_steps) across its 1 Hz samples and stamps "now"
  on any increase; a decrease (reconnect reset the counters) re-baselines with no event. This
  keeps libobs free of a new hot-path remembered-state field.
- **`genlock-lock:` is a new OBS-log family** (emitted on state/reason CHANGE, not every tick) —
  mutually non-substring with `genlock-fifo audit '`, `genlock-ndi-output audit '`,
  `genlock-ndi-filter audit '`, `genlock-relock`, `genlock-acquire-bracket '%s':` (guarded by
  `tests/genlock_lock_indicator_guards.rs`, per `jitter-audit-parser.md`).
- **This is a vendored FRONTEND change ⇒ FULL-BUNDLE deploy** (not a fast obs.dll hot-swap), per
  `vendored-obs-frontend-crash-safety.md`. CI is the first place the C/Qt compiles — locally only
  `cargo fmt --all --check` + the pure Rust module (`rustc --test`) + the parity/guard tests
  (standalone) verify; the blog refactor's format/arg types were lift-compiled under `gcc
  -Wformat=2 -Werror`.
