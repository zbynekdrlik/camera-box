---
paths:
  - "src/asrc_timecode_step_bench.rs"
  - "src/asrc_timecode_pending_bench.rs"
  - "src/asrc_timecode_bench.rs"
---

# The issue-1381 wall-step, relabel and pending two-clock benches

Split out of `asrc-bench-harness.md` (it had reached ~1000 lines, ROZHODNUTÉ 5903945145 point 4). The
compensator pieces these benches exercise (the placement re-seed, the beyond-cap backstop) stay there,
in its issue-1381 section; the receiver decisions they drive are in `genlock-audio-pairing.md`.

**The two-clock bench** `src/asrc_timecode_step_bench.rs` (a `#[path]` child of
`asrc_timecode_bench.rs`) replays the step with a sender that follows by a jump, a catch-up burst,
or never.

- Before (today's code): 682 ms followed at 3.3 s = 396 re-bookings and three −70 ms re-placements
  at 83/166/248 s, still 4.5 ms off at 20 min. As a burst: a −612 ms skip, then −70 ms every
  ~82 s, never settled in 20 min. 89.7 ms as a burst: 84.8 ms off, settled at +70 s.
- After: 682 ms followed = 0 events, |A/V| ≤ 0.5 ms. Burst = ONE placement at the 5 s release.
  89.7 ms = 0 events (followed) or one (burst). Never followed = one placement at the 10 s
  timeout. Steady state and a small step never hold.
- Review round 1: a sender that stepped FIRST, the receiver 2 s later (`sender_first_ns`), in
  both shapes, for 682, -682 and 89.7 ms. Before: a hold that never saw a follow, 683 ms off for
  10 s, then a placement at the bound. After: one zero-length release at the receiver's step, one
  placement, on the true landing at once. With 20 ms extra arrival jitter (`burst_jitter_ns`) a
  40 ms step with a jumping sender costs two events (an early age release, then the booked jump)
  and settles in about 41 s. 50/60/682 ms steps and a catch-up sender keep at most one event.
- Review round 2: sender first by 2 s at 35/50/65/-50/-65 ms (under OBS's 70 ms smoothing),
  placed at the smoothed timestamp: up to 63 ms off for about a minute. Placed at the raw-stamp
  landing: one ~2 ms event, |A/V| <= 0.8 ms, nothing booked. Sender first by +-2.5 s (over the
  2 s timestamp-jump limit): one placement at the receiver's step (was a 10 s hold when the reset
  re-seeded the nominal). A two-packet backlog at connect (`connect_backlog`): a receiver-first
  50 / 682 ms step behaves exactly as without it (was read as sender-first at 50 ms).
- Bench accounting: a backstop placement is one event, its discontinuity. `place_beyond_cap`
  also counts it as a placement jump, so the booking count subtracts it. The count's baseline is
  taken before the first measured packet.
- **The relabel (design 5900385541).** `Follow::Relabel` is the contract's sender: one block per
  boundary, stamped on its boundary. At the step, content k is stamped on boundary k + N,
  N = floor(S / slot), and is emitted when the stepped wall reaches that boundary, so the true
  landing moves r earlier. `Variant::NoRelabel` is the path before the relabel append. `Obs` now
  counts overwritten / dropped / zero-filled audio (a placement inside the buffer, a buffer reset,
  a placement past the end).
  - Production, at +260 / +682 / −1.5 s / +2.5 s, joint (lag 0) and split (lag 20 ms): 0 ms
    overwritten, dropped or zero-filled, no departure from the continuation, relabels = 1, and a
    split hold released `followed` once.
  - |A/V| never exceeds r. Slice 1 left a sub-band r to the level loop (15.8 ms back within
    2 ms only at +725 s). Since slice 2 (design 5901213031) every r is slewed at 1000 ppm and
    booked as NO placement jump: +260 ms back at +24.6 s, +682 ms at +13.8 s, asserted within
    r s + `REPAY_MARGIN_S` (1 s).
  - NoRelabel places r early (exactly r overwritten, a −r departure). At +2.5 s it drops
    165–175 ms of queued audio and zero-fills 96 ms.
  - A catch-up / pausing sender, one that never follows, and a steady feed give a BYTE-IDENTICAL
    per-packet trace (landing, append, servo input, release) with and without the relabel. That
    proves no false relabel on the model; the shipped C branch is pinned by the lift harness.
  - The rate estimate stays within ±5 ppm through a joint relabel's one short block (asserted;
    0.000 ppm measured).
- **Slice 2 (design 5901213031): the pending-relabel bench** `src/asrc_timecode_pending_bench.rs`
  (a `#[path]` child of the step bench). It uses the same `Obs` replay, which mirrors the ingest's
  pending start + timeline continuation, the relabel-pending count and the booking before the
  placement decision.
  - `StepCase.sender_first_ns = RECEIVER_NEVER_NS` means the receiver never steps. The relabel's
    true landing is then its whole N slots (the video follows the stamps through an unstepped
    wall).
  - `Follow::Pause` is a sender that stops for `wall_ns` and resumes on its own wall, with no wall
    step.
  - Losses, relabels and pendings count from the FIRST step (either box's). Timings, events and
    A/V count from the receiver's step (the sender's when the receiver never steps).
    `av_max_all_ms` spans the whole window.
  - Results (Production): +260 / +682 ms / -1.5 s / +2.5 s with the receiver stepping 0.5 s or
    3 s after the sender: 0 ms overwritten, dropped or zero-filled, no departure, one
    `RelabelPending` release at the receiver's step, 0 booked jumps, |A/V| <= r (15.8 ms,
    26.7 ms, 0, 0), back within 2 ms at 13.8 s / 24.7 s. Also with 10 ms of extra arrival jitter.
  - Never followed: one `Timeout` at 10.02 s and one placement (+682 ms: a 666.7 ms zero-filled
    gap, J applied once), on the relabelled landing after it.
  - A pause (100 ms / 500 ms / 3 s) and a catch-up sender that stepped first give a byte-identical
    trace to `NoRelabel` with 0 pendings.
  - `Variant::NoBook` (anti-tautology) puts the remainder back on the ASRC band: +682 ms joint or
    sender-first settles only at +725 s, and +260 ms books one placement jump.
  - The 1367 sender events never start a pending (`no_sender_event_starts_a_pending_relabel_1367`,
    the `Run.pendings` counter).
  - Review round 1: `Follow::RelabelPause` is the relabel sender plus a 500 ms pause 5 slots
    after its own step, inside the pending window. +682 ms and +260 ms, the receiver 3 s later:
    one `RelabelPending` release at the receiver's step, nothing overwritten or dropped, and a
    zero-filled gap no longer than the sender's own pause. Before the fix, the held offset
    absorbed the pause and the pending ran to the bound.
  - Review round 2: `Follow::JumpLate` is the raw-clock `Jump` sender whose step-carrying packet
    goes out 8 ms late (its stamp and its arrival). +682 / −682 / +90 ms, the receiver 0.5 s
    and 3 s later: one `RelabelPending` release, no loss, |A/V| back under 2 ms within 1 s of the receiver's step. With the
    round-1 fold rule the +8 ms stayed in the held offset: 8.8 ms off until +761 s.
  - Loss anti-tautology (review round 1): `Variant::NoStepHold` on the same sender-first relabels
    loses audio from the sender's step on (+260 / +682 ms: a 233 / 667 ms zero-filled gap, −1.5 s
    dropped, +2.5 s reset). `NoRelabel` is not such a check: it keeps the pure hold, and the
    pending start alone keeps a jump under 2 s appended there. Only the +2.5 s timeline reset
    loses.
- **Slice 3 (design 5902870861, ROZHODNUTÉ 5902983227): a one-slot sender-first step (N = +1).**
  - **The grid position matters.** The sender's stamps are floored to 100 ns
    (`stamp_wall / 100 * 100`), so an N = +1 relabel's stamp jump is one packet + 34 ns or one
    packet − 66 ns depending on the grid position of its first relabelled block (pinned by
    `a_one_slot_relabel_jumps_one_packet_minus_66_ns_on_one_grid_position_1381`). The bench's
    fixed 300 s step lands on position 0, where slice 2 already caught N = +1, so a case there
    cannot fail first.
  - **`StepCase.step_offset_ns`** moves both steps: `slot_ns(p)` puts the step on position p, and
    position 1 is the − 66 ns one. Every existing case uses 0.
  - **The two new senders have no wall step.** `Follow::SkipBlock` never sends the first block at
    or after the step time. `Follow::DupBlock` resends the block before it 3 ms later. Both use
    block-per-boundary stamps.
  - **Results (Production).** +35 / 40 / 50 / 60 / 66 ms on all three positions, with the receiver
    0.5 s or 3 s later: one pending, one `RelabelPending` release, 0 ms lost, r repaid within
    r s + 1 s. On slice 2, position 1 lost 2.1 / 4.6 … 33.2 / 35.7 ms, overwritten at the
    receiver's step.
  - **Never followed.** One `timeout` at the 10 s bound, J applied once, PLACED (a J-ms gap) on
    every grid position (ROZHODNUTÉ 5903945145 point 2; slice 3 first booked the − 66 ns one at
    1000 ppm through the timecode ASRC).
  - **Byte for byte.** A skipped block, a duplicated block, and N = −1 at −10 / −20 / −30 ms each
    give 0 pendings and a trace identical to `NoRelabel`.
  - **A late follow starts no pending** (review rounds 1-2): this box steps 50–66 / 260 ms first
    and the sender relabels 12 / 20 s later: one `timeout`, 0 pendings, no gap under 67 ms.
  - **A jittered late follow loses no audio from the follow on** (review round 2, ROZHODNUTÉ
    5903945145). S = ±34 / 36 / 38 ms, the sender 12 / 60 / 120 s late, `burst_jitter_ns` 3 / 5 ms:
    the jitter releases the skew hold early on the age test (the step is placed there -- the skew
    hold's known limit: |S| overwritten forward, a |S| gap backward, the audio |S| off its video
    until the follow), and the sender's follow is the remembered step's follow: 0 pendings and 0 ms
    overwritten, dropped or zero-filled from the follow on (`StepRun.follow_*_ms`, counted from
    `follow_at`). Before the remembered step, 17 of the 36 cases started a pending: 66.7 ms
    overwritten (backward) or a 33.3 ms gap (forward, 60 s or more late).
  - **The follow still costs A/V** (review round 3, pinned by the same test through
    `StepRun.follow_av_max_ms` / `follow_av_settle_s`). At the follow the error becomes the
    relabel's landing move, one slot forward or two backward, booked back at 1 ms per second:
    33.3 ms settled in 31.4 s, 66.7 ms in 64.7 s. The bounds are tight (1 ms / 3 s tighter fails).
    A scratch probe over the same 36 cases: the follow on the placement slew instead gives the same
    numbers; placing it zeroes the A/V but overwrites up to 66.7 ms or opens a 33.3 ms gap.
  - **A raw-clock sender's late step packet rings after any receiver-first hold** (`Follow::JumpLate`,
    probe only, review rounds 3-4): its +8 ms lateness is left to the level loop, ~7 ms off and back
    within 2 ms only after ~740 s. It happens on a clean in-hold `followed` release too (lag 5 s,
    jitter 0, any S and sign), never without a hold, and identically on the slice-3 base, under all
    three follow treatments. The receiver-first twin of the slice-2 round-2 JumpLate fix
    (`a_raw_clock_senders_late_step_packet_leaves_no_residual_1381`, sender-first only); a
    follow-up candidate.
  - The pin's A/V window starts at the sender's first followed BLOCK (its content slot at or after
    `follow_at`), not at the follow instant: an old-schedule block still in flight then reads |S|,
    over the forward bound (review round 4).
