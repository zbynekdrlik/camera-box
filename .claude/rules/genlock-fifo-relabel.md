---
paths:
  - "src/genlock_fifo_relabel.rs"
  - "src/genlock_fifo_relabel_tests.rs"
  - "src/genlock_fifo_relabel_bench.rs"
  - "vendor/obs-studio/libobs/obs-genlock-fifo-relabel.h"
  - "tests/genlock_fifo_relabel_parity_1372.rs"
  - "tests/genlock_fifo_relabel_wiring_1372.rs"
---

# The receive-FIFO date-step relabel (issue 1372 part B)

## Why

dantesync 1.12+ steps the fleet DATE once a night (02:00 UTC) by the whole day's error, +1543 ms on
6.10.2026 and +1549 ms on 7.10.2026, and 1.16.0 rounds it to 200 ms. Every wall clock steps by the
same S at the same instant; the monotonic/media clock never steps. The render tick re-grids in one
tick (`genlock-wall-step.md`), but the frames already in a genlock FIFO, and the ones a sender
stamped just before its own step, keep the OLD label and read S off the release target. Live:
stream `NDI 2ME PGM` `late_holds` +1 / `dropped_due` +1, the cambox `Zaloha kamera` `late_holds`
+28 / +29, 5 repeats + 4 skips on the stream program recording. Decision: design 6026394143,
Approach 1 (dantesync quantizes, the FIFO relabels its old-epoch frames by +S).

## The pieces

| Piece | Rust (`src/genlock_fifo_relabel.rs`) | C (`obs-genlock-fifo-relabel.h`) | Call site (`obs-source.c`) |
|---|---|---|---|
| booking | `Booking::observe` (runs `WallStepState::observe`) | `genlock_fifo_relabel_book` | `genlock_fifo_relabel_tick`, file-static `genlock_relabel_booking` |
| queued frames | `plan` + `RelabelState::apply` | `genlock_fifo_relabel_plan` / `_apply` (queue via two callbacks) | `ready_async_frame`, right after the one bracketed `wall_now` read |
| arrivals | `arrival_add` + `RelabelState::receive` | `genlock_fifo_relabel_arrival_add` / `_receive` | producer push, before `genlock_stamp_track_observe` |
| predicates | `dev_ns`, `continuous`, `delta_carries_step`, `step_relabels`, `jump_recorded`, `sender_stepped_before`, `window_ns` | same names, `genlock_fifo_relabel_` prefix | — |
| state | `RelabelState` | `struct genlock_fifo_relabel_state` | `obs_source.genlock_relabel` (obs-internal.h includes the header) |
| audit | — | — | `relabelled=` right after `n2_early=`; `src/jitter_audit.rs` `relabelled` / `delta_relabelled` |

Log lines: `genlock-fifo-relabel: the wall clock stepped +S ms -- booked step N ...` once per box,
`genlock-fifo-relabel '<src>': step +S ms -- relabelled A of B queued frame(s), boundary_moved=0|1
arrivals=judged|closed window_ms=W` once per source per booking.

## The rules (do not undo)

- **The booking runs at the RELEASE, not the render tick's end-of-tick detector.** The render tick
  observes the wall in `video_sleep`, after the tick's release; a step booked there reaches the
  FIFO one release late. The release takes its one `wall_now` between two monotonic reads and
  runs the same `genlock_wall_step_observe` (same 2 ms threshold, same 100 µs bracket trust) on
  it. Bench: booked one release late costs 1 repeat + 1 skip, the same as no relabel. Two detector
  instances (render tick + booking) see the same physical step; neither consumes the other's.
- **Queued frames are split by FIFO ORDER**, not by "older than the step instant": the old epoch
  is the prefix before the first adjacent stamp jump that carries S within one frame. With no
  such jump, the queue shares the epoch of the last presented frame — old, unless a remembered
  raw jump that carries S arrived within one window before the booking (the sender stepped first
  and its new frames were already presented, only possible for S < 0). For a negative step with a
  queue deeper than |S| the two epochs OVERLAP in stamp value; only the order separates them.
- **Arrivals:** relabelled while `stamp + S` continues the relabelled timeline within one frame and
  the raw stamp does not (= "S − one queue depth or more behind the release target" + "S matches
  within one frame"). A raw stamp that continues it = the sender stepped, the window closes. Neither
  = a real jump (sender restart, song change): never relabelled, the window closes.
- **The window is one latency window on the SENDER's stamp timeline:** a relabelled stamp past
  `booking wall + max(pin, presented age)` closes it. Relabel stamps by the EXACT booked S.
- **Only |S| >= 100 ms** (`MIN_STEP_NS`, half the 200 ms quantum): below two canvas frames a
  continuous delta and a stepped one cannot be told apart within one frame. A 50 ms step keeps the
  one-tick re-grid behaviour (≤ 5 visible events, the wall-step bench).
- **"One frame" = the canvas interval; the source step = the stamp tracker's learned min delta**
  (`genlock_rx_min_delta_ns`), the canvas interval when none is learned yet.
- **A booking reaches only a source that was releasing at the step** (review round 1, a 🔴): apply
  relabels only when the source's previous release is at most `APPLY_MAX_GAP_NS` (1 s) before this
  one; a new source, one silent across the step (an acked-offline camera, a cambox unplugged after
  an event, the away resolume feed) takes the booking as is. Every release records itself, applied
  or not. Without it a source created after a booking relabelled its post-step queue by +S, and a
  stale locked boundary opened a window of HOURS that relabelled every arrival (the input froze 1.6 s
  in the future). The bench `a_source_that_starts_after_the_step_is_never_relabelled_1372` bites on
  the pre-fix module (deep input: 48 skips + a relock).
- **The booking re-seeds after a silent stretch:** a trusted read more than `BOOK_MAX_GAP_NS` (1 s)
  after the previous trusted one resets the detector instead of booking (it is fed only by genlock
  releases; an untrusted read never refreshes the reference).
- **The window takes at most a 2 s presented age** (`WINDOW_MAX_AGE_NS`; the pin is never cut).
- **The flush closes an open window, forgets a remembered jump and the last release**; the booking
  stays applied (seq kept), so a booking is never re-applied to a new timeline.
- **Before every reader:** the release relabels before `present_ts`, the due scan, the backward-step
  guard, the source-multiple measure and the N>=2 select; the producer relabels before the stamp
  tracker and the arrival lag.

## Measured (the two-clock bench, `src/genlock_fifo_relabel_bench.rs`)

Content id advance per tick (one source frame on N==1, two on N>=2), step run minus the identical
no-step run; sender stepping -30 / -15 / 0 / +15 / +30 ms against the receiver:

| input | +1600 ms relabel | +1543.16 ms relabel | today (either step) |
|---|---|---|---|
| deep N==1 2ME PGM (pin 1026) | 0 / 0 | 0 / 0 | 1 / 1 (`dropped_due` +1) |
| shallow cg feed (pin 3) | 0 / 0 | 0 / 0 | 1 / 1 |
| N>=2 60->30 camera (pin 3) | 0 / 0 | 1 / 1 (2 / 2 at +30 ms) | 0..2 / 0..2 |

The unquantized camera cost is the 0.59-frame phase move the quantum removes (the N>=2 grid release
picks by stamp; an exact-S relabel lands 0.59 slot off the grid).

## The cambox stamp lag (Design-question 6032724613, open)

A cambox stamps through a mono→real offset re-sampled every 100 captured frames
(`genlock_stamp::OFFSET_RESAMPLE_INTERVAL_FRAMES`), so its stamps switch epoch up to ~1.67 s after
its own wall stepped (924 ms in the capture-phase bench). The whole-slot re-anchor is clean
(`a_whole_slot_date_step_re_anchors_by_whole_slots_with_no_crossing_1372`: 97 / -95 / 13 slots, 0
off-slot intervals, 0 crossings; the unquantized step exactly 1, so the measure is not vacuous). But the receiver's one-latency window (~67 ms on a strih-lx camera
input) closes long before: lag 400 / 800 / 1200 / 1600 ms costs 4 / 6 / 9 / 11 repeats + skips
(today one more each); a 2 s window gives 0 at every lag. The fork (cambox re-samples its offset on
a step vs a ~2 s receiver window) waits for the main.

## Known limits (review round 1 nits, accepted)

- A preempted bracket (over 100 µs) on the step tick leaves that one release unbooked against the
  post-step wall: the one-release-late cost (1 repeat + 1 skip) for that source. The `wall_now` read
  is a pinned anchor, so it is not re-read.
- The window age assumes the presented frame is old-epoch; when it is already new-epoch (a negative
  step with the sender first) it reads |S| too long, bounded by the 2 s cap.
- Only the LAST raw stamp jump is remembered; a later non-step jump of 50 ms or more before the
  booking hides the sender's step from `sender_stepped_before` (only decisive for S < 0).
- A frame received after the receiver's step but before the booking leaves
  `genlock_rx_arrival_lag_ns` off by S until the next push (the bench port models it, no cost).

## Verifying (Tier-0, no cargo)

- Unit tests: a scratch `lib.rs` with `genlock_wall_step` + `genlock_fifo_relabel` as `#[path]`
  mods, `rustc --edition 2021 --test -D warnings`, then `clippy-driver --test -D warnings`.
- Benches: symlink `src/genlock_*.rs` into `<scratch>/src`, link `<scratch>/tests` to the repo's
  `tests/` (the n2 grid tests `include_str!` a fixture), a stub `ndi.rs` (the const + `fn
  floor_boundary_100ns` awk'd out; the function is `pub(crate)`), `rustc --test -O`.
- Parity: `extern crate self as camera_box;` + the two modules + the test file as `#[path]` mods,
  `CARGO_MANIFEST_DIR` and `CARGO_TARGET_TMPDIR` set at COMPILE time. Mutation: point
  `CARGO_MANIFEST_DIR` at a scratch repo holding the mutated header AND a copy of
  `obs-genlock-wall-step.h`; 25/25 C mutants RED after review round 1 (the window end `>` and the
  equidistant tie needed their own vectors; the stale-booking bounds have seven).
  The generated C harness zero-fills the header's structs with `memset`, never a positional
  `{0, 0, …}` initializer: a field added to the header later fails the harness compile under
  `-Werror=missing-field-initializers` (review round 1 hit it on `last_mono_ns`).
- Wiring: std-only, `CARGO_MANIFEST_DIR=<wt> rustc --test tests/genlock_fifo_relabel_wiring_1372.rs`.
- A new bench case proves itself against the PRE-fix module: copy the replica, replace only
  `genlock_fifo_relabel.rs` with the RED commit's version (`show <red-sha>:src/…`), run the one test.
- Capture-phase bench: the `capture-phase-tracker.md` replica (its `ndi.rs` stub needs the
  `UNITS_PER_SECOND` const).

## Live acceptance (supervisor, FULL bundle on every genlock OBS box)

At the next 02:00 UTC step (quantized after the dantesync 1.16.0 roll): one
`genlock-fifo-relabel:` booking line per box at the `genlock-regrid:` second; one per-source line per
genlock input with `relabelled A of B` (A ≈ the queue depth, `arrivals=judged` on a receiver-first
input); the audit `relabelled=` steps once; `late_holds` / `dropped_due` / `relocks` flat across the
step on strih-lx and stream; the stream program recording shows 0 repeats / 0 skips at 02:00
(camera inputs: subject to the open cambox-lag question above).
