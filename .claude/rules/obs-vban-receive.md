---
paths:
  - "vendor/obs-vban/src/vban-source.c"
  - "vendor/obs-vban/src/vban-receive-timeline.h"
  - "src/vban_receive_timeline.rs"
  - "src/vban_receive_timeline_bench.rs"
  - "src/vban_receive_timeline_tests.rs"
  - "src/vban_receive_timeline_late_tests.rs"
  - "src/vban_receive_timeline_worker_model.rs"
  - "src/vban_receive_timeline_bench_wake.rs"
  - "src/vban_receive_timeline_bench_reorder.rs"
  - "src/vban_receive_timeline_reorder_tests.rs"
  - "src/vban_receive_timeline_conceal_tests.rs"
  - "src/vban_conceal.rs"
  - "src/vban_conceal_tests.rs"
  - "vendor/obs-vban/src/vban-conceal.h"
  - "vendor/obs-vban/src/vban-sample-io.h"
  - "tests/vban_conceal_parity_1439.rs"
  - "tests/vban_source_reorder_1439.rs"
  - "tests/vban_source_conceal_1439.rs"
  - "tests/vban_receive_timeline_parity_1439.rs"
  - "tests/support/vban_receive_scripts_1439.rs"
  - "tests/vban_source_loss_1439.rs"
  - "tests/vban_receive_wiring_1439.rs"
  - "tests/support/vban_c_1439.rs"
  - "tests/support/vban_source_driver_1439.c"
  - "tests/support/vban_source_harness_1439.rs"
  - "scripts/program_audio_vban.py"
  - "scripts/lib/strih-program-vban.sh"
  - "tests/python/test_stream_role_vban_1439.py"
  - "scripts/program_audio_continuity.py"
  - "scripts/lib/program-audio-continuity.sh"
  - "scripts/e2e_report_program_audio.py"
  - "tests/python/test_program_audio_continuity*.py"
  - "tests/python/test_e2e_discord_report_program_audio_1439.py"
  - "scripts/lib/strih-vban-leg.sh"
  - "tests/python/test_vban_leg_1439.py"
  - "tests/python/vban_leg_fakes_1439.py"
  - "scripts/strih-boxes/*.env"
---

# obs-vban receive side: the VBAN program straight into OBS (issue 1439)

Owner 22.10.2026, on the stream-pp pops: "Nesuhlasim s tym interkom hubom na stream nb - naco tam je
dalsia vrstva?". Design revision 6338127636: on the STREAM role the console's VBAN program (lv1's
`stream` to stream-pp UDP 6980) goes straight into the vendored obs-vban SOURCE. No intercom hub, no
PipeWire. The strih role keeps its hub-fed program until its own slice (addendum 6338457001: one
source per console stream on UDP 6981, the hub's `program_out` removed).

## The receive timeline (`vban-receive-timeline.h` <-> `src/vban_receive_timeline.rs`)

Stock 0.3.1 stamped every packet `arrival - length`, so the sender's network jitter reached an OBS
that mixes at 0 ms audio buffering: a late packet was re-anchored by OBS, and that was a click. A
`nuFrame` gap was padded with COPIES of the next packet, back-dated by the whole gap per copy (2+
lost packets landed on audio already delivered).

- **Anchor:** the first packet's stamp = arrival - length + R. R = the fixed "Receive Buffer (ms)"
  setting (`receive_buffer_ms`, default 40, clamped 10..500), read at create/update only, NEVER adapted
  at runtime (the owner's rule). A different R re-anchors once (counted, `reason=config`).
- **The latency is set ONCE per anchor, then held (ROZHODNUTÉ 6343922049 + 6345255813; the owner:
  OBS never changes its latency after sync, no exceptions):**
  - the anchor is the EARLIEST arrival of the first second: a packet whose END position is within one
    second of the anchor packet's start and that arrived earlier against the line moves the line to
    the anchor it would give (carried back by the line's own arithmetic, so a packet ON the line never
    moves it by a rounding ns). After that second the phase is fixed;
  - **the hold (round 4, option B):** that first second is never EMITTED. The anchor, every full
    re-anchor and every packet ending within the window are `HOLD` events (they refine and fold, no
    slot is filled); the first in-time packet past it starts the stretch's output, already on the final
    line, so libobs never sees a stamp step (it appends a step under 70 ms, `TS_SMOOTHING_THRESHOLD`,
    and its stamp-basis ASRC would walk the error out at <= 300 ppm, `ASRC_MAX_PPM`). Slots lost
    before it are not sent (libobs has nothing from the source yet). Cost: ~1 s of silence at OBS start
    and after each full re-anchor;
  - **the lead-in (review round 5):** a stretch's output always starts with an in-time packet (review
    round 6: a late packet before it is dropped and counted with nothing sent -- a fill there was a
    first output whose stamp had passed, and a hiccup ending a re-anchor's hold started the stretch
    6-36 ms late). Before its slot the worker sends `Due::LeadIn`: silence from the worker's own now
    + the 2 ms margin (review round 5: a mix tick between the worker's clock read and the handover
    must never find the lead-in behind its window -- with no encoder running that grows the whole
    mix's buffering) up to that slot's stamp, stamped to end exactly there (none when less than one
    frame is left). Why: a stretch's first chunk lands far from where the source's previous audio
    ended. Past 2 s (`MAX_TS_VAR`) libobs RESETS the source's timing (`reset_audio_timing`,
    `source_output_audio_data`) and puts that chunk at the instant it ARRIVED, whatever its stamp;
    under 2 s (a starve after a ~1 s outage or delay step: the gap is about the outage, the worker's
    second of silence covered the rest) the source was cleared and the chunk is placed at its stamp.
    Without a lead-in the new stretch once started one R early (bench: 10.0 ms realized against a
    40.4 ms stamp latency after a 1.5 s outage, then a 30.5 ms walk). With it the output lands on its
    stamp either way;
  - only the RATE is followed: the phase term joins the step only while `|stamp - (fit + R)|` is over
    R/4 (the dead band), then at the old 5 ppm per bucket until back inside;
  - before the fit the outlier reference is the anchored line - R (no longer the first packet's
    arrival, which may itself have come at the end of a gap);
  - effective R = max(R, one packet + 10 ms), fixed at the anchor; the source logs ONE `obs-vban
    receive buffer:` warning per (R, effective R) pair.
- **The playout deadline (round 5, ROZHODNUTÉ 6347872706, Option B):** the source emits on its OWN
  clock, by every slot's deadline, never on a packet's arrival.
  - The receive callback only decides (`vban_recv_packet`: HOLD / PACKET / LATE / STALE / a new anchor)
    and stores an in-time PACKET for its slot (`d.pos`). It never outputs; it only wakes an IDLE worker.
  - A playout WORKER thread per source (`vban-src-out`) calls `vban_recv_due(now, store head)` until it
    says NONE. Every slot is one packet's length; its deadline is `stamp - VBAN_RECV_DEADLINE_MARGIN_MS`
    (2 ms).
  - **The deadline is the DECISION point, never the handover time (fresh-context review of round 5,
    finding 1).** Handing every slot over exactly at its deadline made the 2 ms margin the worker's
    whole wake-jitter budget: a SCHED_OTHER thread (rtprio stays off on the OBS boxes) waking later
    handed the slot after its window was mixed, libobs marked the source pending and played everything
    after it one window (21.3 ms) later, uncounted. Now a slot whose packet is the store's head goes out
    at the worker's look, however early (`late_ns` negative); while the slot's packet is missing the
    worker looks again after at most `VBAN_RECV_POLL_MS` (5 ms), its deadline being the last look,
    where the slot goes out as silence of exactly one slot (counted `lost`). An in-time packet reaches
    libobs at most 5 ms after it arrived, about R before its stamp; only a lost slot's silence, and a
    packet arriving within one look of its deadline, depend on the worker waking within the margin.
  - The deadlines run on the stamp line, i.e. the fitted SENDER rate, so libobs gets one continuous
    stamp sequence: while the worker wakes within the margin it never underruns, never clears the
    source and never grows the mix's buffering (the window holding a stamp is mixed at its end).
  - `missed` (the status line's `emit_missed`) counts the slots handed to libobs after their own stamp:
    the worker woke later than the margin. Conservative: a slot 1 ms after its stamp usually still makes
    its window (libobs mixes it at its end). Verify item 9 FAILs on it. The known limit is pinned in
    the bench: a worker 25 ms late moves the realized latency 32 ms, every such slot counted.
  - The margin comes out of R's budget: a packet is in time with up to R - one packet - 2 ms of delay
    over the earliest arrival. It stays 2 ms: stream-pp's budget is R 40 - 5.3 - lv1's worst 29 ms gap
    = 5.7 ms, no room for more without raising its R. Size it from `emit_late_ms` / `emit_missed` read
    on the box under load (the OBS log's status lines). `emit_late_ms` is the window's latest slot
    against its deadline: negative while stored packets go out ahead (about -28 ms on a clean stream
    at R 40), a positive value is a lost slot's silence late by the worker's wake jitter.
  - A stretch STARVES once a slot's deadline is more than `PERSIST_S` (1 s) after the last in-time
    arrival: the worker emits nothing more, and the next packet re-anchors.
  - Another packet size re-anchors as `format` (a slot is one packet, and a lost slot's length and
    the slot positions come from that one size): a sender whose packet size VARIES is not supported.
    Its re-anchor line names the change (`frames 256 -> 128, rate .., ch ..`) and is written at most
    once per status period with the count it did not write (review round 5, finding 3).
  - A slot still to go out when the fit rebases the line at the next packet's start is read back along
    the line (`stamp_at` before `base_pos`: within ~0.2 us of the old line, far under a sample).
- **The worker in the source (`vban-source.c`):**
  - One `pthread_mutex` guards the timeline, the store (a `util/deque.h` of `struct vban_src_slot`
    headers + planes, at most 4096 packets: a packet that does not fit is dropped and logged once,
    `obs-vban receive store:`), the stream's sample format (the silence's) and the idle flag.
  - The store is in SLOT order (ROZHODNUTÉ 6352253147 (a)): an in-order packet is appended; a
    reordered one goes in by turning every stored packet once through the deque (`s->rot`, rare, at
    most ~R / one packet entries, no allocation once grown), so the worker's head is always the next
    stored slot and `vban_recv_due` keeps its `(has_head, head_pos)` API.
  - The lock is held only for a decision and a copy: the worker calls `obs_source_output_audio` after
    it, and the receive side logs from a snapshot after it.
  - The worker sleeps to its next look with `os_sleepto_ns` (absolute on the OBS clock: the earlier of
    the slot's deadline and 5 ms on, never more than 20 ms at a time so a stop is prompt). While idle
    (no stretch emitting) it waits on `worker_wake` (100 ms), which a stored packet signals. On Linux it
    sets its timer slack to 1 ns (`prctl(PR_SET_TIMERSLACK)`), so the kernel does not batch its looks by
    the default 50 us. Windows `os_sleepto_ns` spins the last 1-2 ms of every sleep: no Windows box runs
    the receive source; if one ever does, give the worker the sender's high-resolution waitable timer
    (the obs-vban pacing rule).
  - It is spawned at the END of `update()`, and stopped and JOINED at its start and in `destroy()`:
    after the UDP callback is removed, before the summary reads the timeline and before anything is
    freed. Each stop logs `obs-vban receive worker: source '<input>' stopped (update|destroy)`.
    `update()` runs whole under its own `update_mutex` (review round 5, finding 5): the UI thread and
    the obs-websocket thread can both call it, and two calls must never join one worker twice.
  - `VBAN_SRC_SPAWN_WORKER` / `VBAN_SRC_JOIN_WORKER` are the test seam: the lift-and-run driver runs
    `vban_src_worker_step` on simulated time, or the real thread with `thread`.
  - The real-thread driver run is clean under ThreadSanitizer (`cc -fsanitize=thread -pthread`, run
    under `setarch -R`).
- **Fit:** least squares of arrival over the packet's END sample position, in one-second buckets,
  64 s window, used from 10 buckets. A packet more than 10 ms late against the reference line is not
  folded (a gap's delayed packets never pull the line late).
- **Stamps:** `stamp(pos) = base + T(pos - base_pos)`, T scaled by `1 + step_ppb/1e9`. Per fitted
  bucket the step moves toward `fit_ppb` (+ `clamp(phase / 20 s, +-100 ppm)` only outside the dead band)
  by at most 5 ppm, clamped +-500 ppm, rebased on every change: emitted stamps never jump. The genlock
  ASRC (`basis=stamp`) resamples the sender's rate to the mix (pinned: the lifted
  `genlock_audio_asrc_placement` returns STAMP for the source's direct stamps; nothing in obs-vban
  touches asrc / genlock_fifo / monitoring).
- **Loss:** a slot whose packet is not stored at its deadline = one slot at that deadline, stamped on
  the same line (once the stretch emits), counted `lost`: CONCEALED or silence (below).
- **Reorder (ROZHODNUTÉ 6352253147 item (a), 23.10.2026).** Live on stream-snv (R 80) `lost=107
  stale=40` in 55 min: a router reorder delivered packet n right behind n + 1, the timeline dropped
  n as stale while its slot was ~80 ms ahead, and the slot went out as silence.
  - The timeline keeps `recv_mask`: bit j = the packet j indexes behind `last_nu` came (the
    anchor sets bit 0, every in-order packet shifts it by its index step and sets bit 0).
  - A packet 1..63 behind `last_nu` whose bit is clear, whose slot `next_pos - (back + 1) x frames`
    is still `>= emit_pos` (the worker has not handed it out), in an emitting stretch that has not
    starved (no packet in time for `PERSIST_S`), is `VBAN_RECV_REORDER` / `Event::Reorder`: stored
    for that slot and played in it, counted `reordered`.
  - A reorder never moves the line, the fit, `last_nu`, `next_pos` or `last_intime_ns`: it is never
    folded (its arrival is out of order; a fold before a bucket's first packet would also wrap the
    bucket's unsigned offsets). Pinned: a stream with reorders leaves the timeline bit-identical to
    the same stream without them.
  - STALE stays for a duplicate (its bit is set), a packet whose slot already went out, one exactly
    `STALE_PACKETS` (64) behind (beyond the mask), one in the hold (nothing emitted yet) and one in a
    starved stretch; never a re-anchor. Before round 1 one swapped pair restarted the timeline.
  - The bench (`..._bench_reorder.rs`): adjacent packets swapped every 97 on the jittered sender at
    -6.5 ppm. R 80: every overtaken packet played, 0 lost, 0 stale, the latency held. R 40: 2 of 575
    reorders stale -- the overtaken packet of the one that ENDS a 30 ms gap is past its own deadline
    when that one arrives (R 40 leaves ~2.7 ms there) -- each one lost slot, never a re-anchor.
- **Concealment (ROZHODNUTÉ 6352253147 item (b); the run limit raised from 2 to 3 on 23.10.2026 from a
  5-min wire capture on stream-snv: the network loses packets in bursts of 1-3).**
  - The decision (`vban_recv_due` / `Timeline::due`): a slot missing at its deadline is `CONCEAL`
    when the slot before it went out with audio (`prev_audio`: its packet, or a concealment of this
    same run) and the store's head -- the next stored packet; the store is in slot order and holds
    only slots at or after `emit_pos` -- is `next` slots on with `conceal_run + next <=
    VBAN_RECV_CONCEAL_MAX_SLOTS` (3); `toward_next` when it is the very next slot. Else `SILENCE`.
  - Decided from what is stored AT the deadline, never waiting for a later packet. With R 80 the next
    packet is normally stored ~75 ms ahead, so both fades are known. A run that starts as silence stays
    silence (`prev_audio` false), and the limit holds whatever head the caller hands in (the run so far
    is counted, not only the distance).
  - The math (`vban-conceal.h` <-> `src/vban_conceal.rs`, C-vs-Rust bit for bit in
    `tests/vban_conceal_parity_1439.rs`): the previous packet's samples, faded IN over 1 ms (at most a
    quarter slot, at least one frame) from the last sample handed to libobs on that channel (held), and
    -- `toward_next` -- OUT over the last 1 ms toward the next packet's first sample. Raised cosine
    `0.5 - 0.5 cos(pi (i + 1) / (n + 1))`, no libm (a Taylor cos on [0, pi/4] and sin on [pi/4, pi/2],
    8 terms each, 1.2e-15 off). A concealed run never steps more than the signal's own step plus the
    fade slope `pi / (2 (n + 1))` over the gap it bridges (pinned on sines of 100 Hz-6 kHz).
  - Timing is untouched: a concealed slot has the slot's frames and stamp; only its CONTENT differs.
  - The source: each stored packet caches its first frame per channel (`first[]` in the slot header);
    the worker keeps the last packet it handed out (`s->out` and `s->prev` swap, no copy) and its last
    frame per channel (`tail[]`), and builds the slot in that packet's OWN sample format through
    `vban-sample-io.h` (the five formats the callback hands over, rounded half away from zero and
    clamped) -- libobs resets the resampler and the genlock ASRC at every format change.
- **Late = lost (round 4; the round-3 late shift is withdrawn as a runtime latency change):** a fixed
  latency is a fixed jitter buffer. A packet whose slot already went out (as silence, at its deadline)
  is a `LATE` event: dropped and counted in `late`; before the stretch's first output, a packet past
  its deadline is. The stamps, the fit and the line stay where the anchor put them.
  - **The late fill of rounds 4-6 is gone.** The arrival-driven source sent a late or lost slot's
    silence when the next packet came, after libobs had underrun at 0 ms buffering. That cost +1 or +2
    mix windows (21.3 / 42.7 ms) for a 40-90 ms loss or hiccup, and with no encoder running a cleared
    source grew the whole mix's buffering by the outage (+455 / +861 ms for 500 / 900 ms).
  - With the deadline, every row holds the realized latency within 0.6 ms, with and without an encoder,
    and the buffering never grows (bench below).
- **Re-anchor (one counted event, then the hold):** a STARVE -- no packet in time for `PERSIST_S` (1 s)
  on OUR clock: any packet arriving more than that after the last in-time packet did (`last_intime_ns`;
  an outage, a sender pause, every packet late on a delay step), late or not, with nothing filled
  after the fact (`reason=starve`; review round 4: the first version counted the sender's positions
  and only on a late packet, so a 1.5 s outage filled 1.5 s of silence behind libobs' label and a
  pause cost a second of drops) -- an index further behind (a sender restart), an index jump > 2 s
  of audio while the packets keep arriving (a sender restart), another rate / channel count / R, or a
  fit that cannot hold (rate beyond +-500 ppm, stamps more than R off `fit + R`). The re-anchoring
  packet is the new anchor; the line restarts at the new earliest arrival + R.
- **In-time packets never re-anchor (round 4):** the 3-s fold-starve re-anchor is gone (it re-anchored
  audio still in time). Packets that stay in time but sit more than 10 ms above the fit (an on-time
  delay level step of ~10-30 ms at R 40) stop folding, so the fit holds its last rate and level
  instead of following; nothing walks and nothing re-anchors. The status line then says
  `fit=frozen` (nothing folded for `FROZEN_S` = 3 s while the fit is valid) and verify item 9 NOTEs it.
- **`lost` counts every slot whose packet was missing at its deadline** (round 5; ROZHODNUTÉ
  6352253147): whether it never came or came `late` afterwards, concealed or silent. `concealed` counts
  the concealed ones, `silence_ms` only the real silence. A slot lost inside the hold, or before the
  stretch's first output, is never sent and never counted. A late packet before the first output is
  counted in `late` (its `late:` line says nothing was filled).
- **The loss line is logged by the playout worker when the run ends** (ROZHODNUTÉ 6352253147 (a)):
  one `missing N packet(s) before frame F -- concealed: ...` / `-- silence for those slots` line per
  run of slots that went out without their packet, at the slot of the packet that ends it (or `before
  the stretch ended` when the stretch starves). Never at gap detection: a gap a reorder fills is no
  loss. A run is concealed or silent as a whole (the timeline decides it at its first slot).
  - The worker WRITES the lines only once `vban_recv_due` says nothing more is due at that look (up
    to 4 runs per look, then a count): the packet that ends a run is decided about one slot + the
    2 ms margin before its stamp, and a log write can stall (the OBS log's lock, a flush, journald).
    `emit_missed` is computed before any write, so it could not show such a stall (the fresh-context
    review of this change; pinned with the lift driver's `logs=` count on each `out` line).
  - Known limits (review): a reorder dropped on a FULL store (4096 packets: only a worker stalled
    for ~20 s) is still counted `reordered` -- its slot is counted `lost` at its deadline; and each
    reorder turns the whole store through the deque under the lock -- ~16 entries at R 80, up to the
    4096-packet cap with a stalled worker (a slot ring by position would make it O(1) if that ever
    matters).
- **Bench** (`src/vban_receive_timeline_bench.rs`): a two-clock sender at +-50 ppm, 5..7 ms bursts, gaps
  of 5..30 ms every ~400 ms, receive-thread wake jitter.
  - **What it runs:** the replay runs the source as shipped. The receive decision, then the playout
    worker of `src/vban_receive_timeline_worker_model.rs` -- ONE model shared with the unit tests' `Rig`
    (review round 5): it wakes only at the times it scheduled itself, or idle when a packet is stored,
    and the bench can make every wake late (`WakeLate`: a fixed part + a u^4 tail) -- into the libobs
    model `Obs`. The parity test keeps its own driver: it mirrors the C test driver line for line.
  - **What `Obs` models:** the stamp-domain 70 ms snap, the system-domain 70 ms append, the
    `MAX_TS_VAR` (2 s) reset that places a chunk at its arrival, the kept partial remainder of an
    underrun, the clear of a stopped source, the 300 ppm stamp-basis ASRC, and (round 5) the
    NO-ENCODER growth of the whole mix's buffering for a chunk behind the window (`replay_with(..,
    no_encoder)`).
  - **What it grades:** the stamp AND the REALIZED latency per anchor over the whole run after the
    hold, every anchor at its own stamp latency (+-1 ms), the buffering never grown, every missing
    slot's silence one slot.
  - Main runs: zero late, zero re-anchors at R = 40 ms over 10 min (stamp - arrival at least 3.8 ms, so
    every packet beats its deadline), the fitted rate within ~1.5 ppm, held within 0.87 ms at
    +-50 ppm, 0.29 ms at 0 ppm.
  - The round-2 reviewer's 25 ms first-gap case: realized 40.4-40.6 ms, held within 0.13 ms at 0 ppm
    and 0.72 ms at +-50 ppm (round 3 emitted the first second: 59.9 ms realized, then 19.3 ms of walk).
  - **The acceptance (ROZHODNUTÉ 6347872706), each with the encoder running and not:**
    - a loss of 40 / 60 / 80 / 90 / 500 / 900 ms: 0 re-anchors, one slot of silence per lost packet,
      realized held within 0.29 ms;
    - the 90 ms one-off hiccup: 11-12 late drops, 0 re-anchors, held within 0.52 ms;
    - the 90 ms persistent delay step: 170 late drops, exactly ONE re-anchor, the new stretch at the
      old latency + 90.0 ms and at its own stamp latency, held within 0.51 ms;
    - every run: the mix buffering stays 0.
  - A 90 ms hiccup that ends a re-anchor's hold (950 / 965 / 980 ms after it): the stretch realizes its
    stamp latency within 0.02 ms (it was 6-36 ms off before round 6).
  - A 1.5 s outage or sender pause: the worker keeps the line with ~181 silent slots (a second), then
    the starve; ONE re-anchor; the lead-in puts the new stretch on its stamp; held within 0.51 ms.
  - At R = 20 ms the same sender drops late packets (never a re-anchor): the test that the bench can
    fail.
  - **A late worker (`src/vban_receive_timeline_bench_wake.rs`, review round 5):** every acceptance row
    above (loss 40-900 ms, the hiccup, the step) and the 50 ppm sender with 30 ms gaps, with EVERY wake
    up to 1.9 ms late (inside the margin), encoder running and not: the same numbers, nothing missed.
    stream-snv's R 80 against the same sender with every wake up to 15 ms late: held within 0.81 ms,
    nothing missed (a worker that held each packet to its deadline put every one behind its window).
    The known limit: every wake 3 ms late on a 60 ms loss = 14 slots after their stamps (the 12 lost
    slots + 2 packets that arrived within one look of their own stamps), all counted, the latency
    held; 25 ms late = 464 counted, the realized latency moved 32 ms.
- **Known limit, pinned:** before the fit the line runs at the nominal rate, then slews to it at 5 ppm
  per bucket, so the held latency moves ~ the sender's ppm x 20 s: within 1 ms to ~+-55 ppm, 1.9-2.0 ms
  at +-100 ppm (`known_limit_a_100_ppm_sender_moves_the_held_stamp_latency_about_2_ms_1439`).
- **The pre-fit outlier reference is the anchored line - R (kept despite review round 3):** on a
  bufferbloat sawtooth (delay ramping 0 -> 30 ms every 5 s) it starves the fit more often than the old
  first-packet reference, but with the old reference the 25 ms first-gap case breaks the +-1 ms bound
  (the gap's packets fold into the first fit).

## The E2E's BLOCKING program-audio continuity gate (ROZHODNUTÉ 6359382787)

Owner 23.10.2026, verbatim: "A tiez tak ako pri obraze je poziadavka na zero loss tak aj pri zvuku je
poziadavka na zero flrame loss v e2e, cize hovorit ze veci su ok ked rozne zamaskujes audio vypadky je
uplne zle! Vobec by taky stav nemal dat zelenu e2e na rigu v snv!!!"

- **A concealed slot is a LOST slot.** Concealment (above) stays only as the on-air safety net so a
  loss is not a click; it never makes a run green, and every report names concealed slots as lost
  (verify item 9's counters note: `N slot(s) LOST: K concealed (a concealed slot is still lost ...)`).
- **The decision** is the pure `scripts/program_audio_continuity.py` (pytest
  `tests/python/test_program_audio_continuity_1439.py`, over a REAL stream-snv log excerpt in
  `tests/fixtures/program_audio_continuity_1439/` plus lines rendered from the shipped C format strings).
  It reads the stream box's OBS log between two LINE MARKS (the log's line count at the window start and
  stop):
  - the status-line DELTA of `lost`, `late`, `re_anchors`, `stale`, `emit_missed` (the last status line
    of the input at/before the start against the first one at least one status period, 10 s, after
    the stop -- every slot up to the stop is decided by then). `concealed` is named as a subset of
    `lost`. An older build is graded on the tokens it prints, the rest named `not_graded`;
  - a `buffering-guard: '<input>' ...` line inside the counter interval (any reason: not mixed,
    entered late, `MIXED_LATE`);
  - the `asrc: source '<input>'` `place_jumps` / `steps` delta (60 s lines, the last at/before the
    start against the first after the stop);
  - the per-source `audio-telemetry #800 '<input>': ... mixed_late=N` delta (60 s, same bracket).
    **The guard line alone is not enough (review round 1):** `genlock_mix_guard_log_due` logs a
    source's later events at most once a minute, so a MIXED_LATE storm that began just before the
    window logged its line THERE and its in-window events never reach a line in the window. The
    NOT_MIXED / entered-late reasons have no counter; their line stays the gate (an entry logs at
    once);
  - a re-init inside the window (`obs-vban receive worker: source '<input>' stopped`, `obs-vban
    receive anchored:` -- the first anchor only, a re-anchor logs `re-anchor:`) is UNKNOWN: every
    counter restarts, so a source younger than the window shows no backwards step;
  - the mix-wide `#800 mix: mixed_late=` delta is REPORTED only (it counts every mix member).
  Any delta or guard line = FAIL. No status line on either side, a counter that went backwards (OBS
  restarted, the source reconfigured), a token on one side only, a missing ASRC / telemetry line =
  UNKNOWN, which fails the run. `retryable` = "the closing line is not in the log yet" (never when
  the OPENING line is missing: waiting cannot bring it; `opening_missing` asks the gate to read once
  more from 10 x the lookback, never the whole log: tens of MB on a multi-day session, review round 2).
  - **Line numbering = newlines only** (`text.split("\n")`), like `wc -l` / `tail -n +N` on the box.
    `str.splitlines()` also splits on `\f`, a lone `\r`, `\x1c`.. -- a form feed in one OBS line
    moved the marks a line early and read a 4-slot loss as PASS (review round 1).
- **The E2E side** is `scripts/lib/program-audio-continuity.sh` (pytest
  `tests/python/test_program_audio_continuity_e2e_1439.py`): `program_audio_continuity_mark start|stop`
  right before the StartRecord banner and right after the StopRecord block (fail-open: a missing mark
  makes the gate UNKNOWN, never aborts the recording), `program_audio_continuity_gate` after the merge
  in both verdict paths, folded `if ! ...; then [ "$GATE" -ne 0 ] || GATE=1; fi` (never lowers a red
  gate). It reads the log from 2000 lines before the start mark through `stream_box_exec` (bash over
  plain ssh on Linux, PowerShell on Windows, `timeout`-bounded), waits up to 90 s for the closing lines,
  merges the block into the verdict JSON as `program_audio_continuity` and prints it. The input comes
  from the box facts (`stream_mbc_vban_expect`: stream-snv `mbc`); a Windows stream box (Dante `mbc`, no
  obs-vban source) is graded on the guard + ASRC + mixed_late checks with `obs_vban: n/a ...` in the
  block. A block the merge could not write into the verdict JSON is a loud `ERROR ... the E2E report
  will NOT show it` line (the run's exit is red either way).
- **The report** (`scripts/e2e_report_program_audio.py`): a FAIL or UNKNOWN block turns the summary
  and the full report RED even when the merge passed (`_run_passed`); see `e2e-discord-report.md`.
- **Live proof so far:** the transport was run read-only against stream-snv on 23.10.2026 (a 25 s
  window, PASS, `lost 242 -> 242`, the retry path waited for the ASRC line). The full SNV E2E still
  refuses earlier at the `[0/8]` version-integrity gate on the Linux stream box (issue 1462 comment
  6354889903), so the gate is unproven end to end until that run gets past `[0/8]`.
- **Follow-up candidate (review round 1):** the "newest log + line mark + read from a line" texts
  repeat what `strih_log_remote_cmd` (`strih-log-read.sh`) does for the strih box; a platform-
  parametrized `mark` op in one shared reader would also give the genlock-audit BEFORE/AFTER window
  its OBS-restart detection.
- **Known limits:** the window spans the two status lines around it (up to ~10 s before the start and
  10-20 s after the stop), so a loss there fails the run too -- conservative, never a miss. The STOP mark
  is taken after the strih genlock-audit read (a few seconds after StopRecord): the window only grows.

## The status lines (OBS log, prefixed `[obs-vban] ` by the plugin's blog macro)

```
obs-vban receive anchored: source '<input>' frame=N rate=48000 ch=2 receive_buffer_ms=R -- holding its first second
obs-vban receive emitting: source '<input>' frame=N margin_ms=X after the first second's hold -- every slot goes out by 2 ms before its stamp
obs-vban receive: received=N lost=N late=N re_anchors=N stale=N reordered=N concealed=N silence_ms=X max_late_ms=X sender_ppm=X phase_ms=X fit=locked|acquiring|frozen emit_late_ms=X emit_missed=N receive_buffer_ms=R rate=N ch=N port=P stream='S' source='<input>'
obs-vban receive late: source '<input>' frame=N late_ms=X -- dropped, its slot went out without it, on the anchored line (late=N)
obs-vban receive re-anchor: source '<input>' reason=starve|backward|jump|format|config|fit frame=N starve_ms=X re_anchors=N late=N -- holding its first second
obs-vban receive re-anchor: ... reason=format ... -- holding its first second (frames 256 -> 128, rate 48000 -> 48000, ch 2 -> 2: a varying packet size is not supported; N more not logged in the last period)
obs-vban receive buffer: source '<input>' receive_buffer_ms=R is not above one packet (X ms) + 10 ms: using X ms
obs-vban receive worker: source '<input>' stopped (update|destroy)
obs-vban receive store: source '<input>' frame=N -- full (4096 packets): the playout worker is not emitting, the packet is dropped
source '<input>': missing N packet(s) before frame F -- concealed: the previous packet repeated, crossfaded
source '<input>': missing N packet(s) before frame F -- silence for those slots
source '<input>': missing N packet(s) before the stretch ended -- silence for those slots
source '<input>': received N packets, lost N (N concealed), late N, reordered N, stale N, N re-anchor(s)
```

`obs-vban receive: ` stays on exactly ONE log line (the wiring test pins it): every other line uses
another prefix (`anchored:`, `emitting:`, `late:`, `re-anchor:`, `buffer:`). `late:` is logged once
per run of late packets (the counter counts each one); `reason=starve` on a re-anchor means no packet
came in time for a second, and `starve_ms` says how long (0 for every other reason). `fit=frozen` on the
status line: nothing folded for 3 s while the fit is valid. `max_late_ms` includes the dropped late
packets and the reorders. `reordered` counts the reorders played in their slot, `concealed` the lost
slots that went out concealed (a subset of `lost`; `silence_ms` is the silence only). `emit_late_ms` (round 5) is the window's latest slot against its deadline (negative while
stored packets go out ahead of it; positive = a lost slot's silence late by the worker's wake jitter,
which must stay under the 2 ms margin). `emit_missed` counts the slots handed after their own stamp
since the source started (verify item 9 FAILs on it). A `format` re-anchor line is written at most
once per status period.

The status line is written by the receive callback, so it stops when packets stop. Counters are
cumulative since the source was created (a port / stream / host change re-inits them). `max_late_ms`
is the window's largest arrival - stamp of an on-time packet: about -(R - one packet) + jitter when
healthy (the stamp is the packet's START, its arrival its END: -34.7 ms at R 40, 256 frames, 48 kHz).
- **R is lifted to one packet + 10 ms** (round 3; review round 2 found that the real margin is R minus
  the packet's length, so R no longer than a packet re-anchored every packet). The rig's 48 kHz 256-frame
  packets with R = 40 are unaffected; mbc's 239-frame packets (4.98 ms) too.
- **A hostname `ip_from`** is resolved asynchronously by `vban_udp_set_host`, after the update's
  last reset, so the new host's first packets can land on the old timeline. The stream role uses
  `ip_from ""` (any host); give a host as an IP if one is ever set.
- **Thread safety:** the callback runs on the per-port `vban-r-<port>` thread under `dev->mutex`; the
  timeline and the store are shared with the playout worker under `s->mutex` (round 5). The status line
  takes the stream name from the PACKET header
  (`%.*s`, 16 bytes), never `s->stream_name` (the UI thread's, which `update()` frees), and the port
  from an atomic copy.
- **A reconfiguration (another port / stream / host) asks for the new timeline BEFORE the new sender
  is let in** (review round 1; set after only, its first packet could land on the old timeline as lost
  slots), and again after. `tests/vban_source_loss_1439.rs` reads the pending reset at each UDP
  registration call.
- **A failed bind is final:** `vban-udp-thread.c` logs `[obs-vban] Failed to bind port P` and its
  thread exits, no retry. A hub still holding 6980 when OBS starts = no program audio until OBS
  restarts with the port free. verify item 9 names it.

## The stream role (setup / seed / verify)

- **Facts:** `STRIH_PROGRAM_VBAN_STREAMS` / `_PORT` / `_RESERVE_MS`: stream-pp `stream` / `6980` /
  `40`, stream-snv `stream` / `6980` / `80` (finding 6348583939: mbc's gaps reached 52 ms under the
  22.10.2026 production load, lv1's are 26-29 ms; R - one packet - the 2 ms deadline margin must sit
  above the sender's worst gap, pinned by the pytest); strih boxes `none` x 3 until their slice; the loader refuses a strih role with values
  and a stream role without exactly ONE stream). `none` is a sentinel, never a stream name.
  `STRIH_PROGRAM_AUDIO_INPUT` (issue 1462) names the OBS program-audio input per box: stream-pp
  `Zvuk z mixu`, stream-snv `mbc` (the SNV stream tooling keys on it), strih boxes `none`. The seed
  manifest, verify item 9 (`strih_program_vban_grade`'s default NAME), the stream-monitor row and the
  Development-scene guard read it; never a role constant.
- **stream-snv (issue 1462):** a Linux stream box like stream-pp since 22.10.2026. mbc.lan sends the SNV
  program as VBAN `stream`, 24-bit PCM (format byte 0x02), 48 kHz stereo, 239 frames per packet, to
  UDP 6980: the source decodes it (pinned by `tests/vban_source_loss_1439.rs`). Its OBS input `mbc`
  read as the obs-vban source on 23.10.2026 (`stream_name` `stream`, port 6980, R 80; the setup step 13
  hub removal and the launch seed's switch from the earlier hub-fed capture had run), and mbc.lan
  must keep sending to 6980. The collection port has no `stream-snv` preset
  (the box's collection already exists; the launch seed switches `mbc` with the 80 ms fact). An `mbc`
  that is ALREADY an obs-vban input keeps its operator R (verify item 9 NOTEs the difference): set
  `receive_buffer_ms` to 80 in its properties. rig-mode TEST's `#901 item 2` grades this source there
  (kind + the facts' stream and port, unmuted; `obs_phase2.py mbc-input-check --vban-stream --vban-port`,
  the decision `program_audio_vban.mbc_vban_transport_problem`), never the Dante device
  (`stream-box-linux.md`).
- **Seed:** the stream manifest carries `program_audio_vban`; `strih_scenes.py` switches `Zvuk z mixu`
  through `scripts/program_audio_vban.py` (installed next to it by setup step 6; the launch preflight
  imports it). A replace reads the operator state BEFORE `RemoveInput` and puts back scenes (hidden
  items stay hidden), volume, mute, monitoring, tracks, balance, sync offset and filters (index order,
  disabled ones stay disabled). An input already of the vban kind is never rewritten (a difference
  from the facts is only named). `GetInputKindList` without the vban kind (a bundle without the
  plugin) = nothing removed. An unreadable manifest block = the input untouched, never turned back
  into the pulse capture. Every create and re-create (`input_create_verified`) first waits, bounded
  (30 x 0.1 s), for the name to be free (a removed source can hold it for a moment and CreateInput
  then refuses), reads the result back from `GetInputList`, and tries once more (a re-create removes
  an old input still listed first). An input OBS never lists as the wanted kind is a loud
  `replace FAILED (was ...)` / `create FAILED` naming what OBS lists, with nothing put back onto it:
  restart OBS and the launch seed runs again. verify item 9 FAILs the kind too.
- **Collection port:** the `stream-pp` preset converts the program input to the same source
  (`--program-audio-vban PORT:STREAM:MS` / `none`); the collection keeps the mixer keys on the object.
  Every OTHER VBAN receive source is dropped with a DROP line on every preset (review round 2: the
  strih bundle loads obs-vban, so a dormant Windows one would come alive and bind a UDP port the hub
  or the program source needs); `--keep-kind net.nagater.obs-vban.source` keeps them.
- **No hub on the stream role:** `obs_box_step_applies 13` skips step 13 there and calls
  `strih_intercom_hub_remove` (stop + disable, then unit, drop-in dir, binary and `/etc/intercom-hub`
  MOVED to `/var/lib/camera-box/intercom-hub-removed/<UTC stamp>/<abs path>`, daemon-reload;
  idempotent). The hub's two drop-ins (NDI_CONFIG_DIR, local audio) are written by step 13's hub branch
  only, so the stream role gets none. Verify item 16 = `strih_intercom_hub_absent_grade`
  (FAIL while a hub file or a running unit is left), item 34 NOTEs the absent hub drop-in, item 9 =
  `strih_program_vban_grade` (the kind, the bind, port + stream vs the facts, `received` advancing over
  12 s, R vs the fact as a NOTE, loss counters as a NOTE, `emit_missed` > 0 as a FAIL naming the
  vban-src-out thread; a status line without the token, from an older build, is not graded on it; the
  counters note says how many of the lost slots were concealed and how many went out as silence, the
  live row names `reordered=`; an older line without those tokens names nothing for them);
  the hub-only rows of item 9 (the program
  rx, the hub's HDMI leg, the MiniFuse talkback) run only in its hub branch.
- **Windows (review round 1):** both Windows genlock builds compile the same `vban-source.c`, so a
  FULL deploy changes every Windows OBS's VBAN SOURCE too (R more latency, loss = silence). Read
  each Windows box's collections for `net.nagater.obs-vban.source` before that deploy (not read yet).
  The same for strih-snv / strih-pp (review round 2): their bundle loads `obs-vban.so` now, and a
  dormant VBAN source in a ported collection would come alive and could take the hub's UDP port
  (`grep -l net.nagater.obs-vban ~/.config/obs-studio/basic/scenes/*.json` on the box, read-only).
- **Rollout (supervisor):** the strih bundle with obs-vban (linux-genlock.yml strih variant stages
  `obs-vban.so`), then setup-strih on stream-pp with OBS stopped (the genlock deploy stops it), then
  start OBS: the seed switches `Zvuk z mixu`. A setup re-run with OBS RUNNING removes the hub under a
  live pulse input: restart OBS right after. lv1 must be sending `stream` to stream-pp:6980.
- **stream-snv's 80 ms (supervisor):** an `mbc` that is already an obs-vban input keeps its operator R:
  set `receive_buffer_ms` to 80 in its properties (one counted `reason=config` re-anchor, then the
  hold), read `receive_buffer_ms=80` back on the next status line, then re-measure the program A/V
  offset -- R 80 plays the program audio ~40 ms later than R 40 did. After the deploy read
  `emit_missed=0` and `emit_late_ms` on the status lines (verify item 9 grades the first).

## The VBAN-only leg: a second NIC or a tagged VLAN on the audio VLAN (ROZHODNUTÉ 6360417876)

Owner 23.10.2026 ~10:00, verbatim: "na stream-snv je druha sietovka usb-c 2.5gbits a ta je pripojena do
vlan 70 audio siete, vies nastavit aby na stream bola ta sietovka na 10.77.7.204 a islo cez nu iba ten
vbani nic ine aby cez nu ani nic ine neodchadazlo, vsetko ostatne streamy ndi video internet naech ide
ce tu sietovku klasicku"; ping allowed "aby sme mali aspon nejake info ze to bezi". mbc's VBAN used to
cross the venue router (VLAN 70 -> 89), which delivered it late and reordered (finding 6352601026: mbc
sent every frame, the ROUTED copy lost them). The owner then pointed mbc's `stream` at 10.77.7.204
himself: **the mbc retarget is owner-side, never provisioning.** At 10:31 the supervisor added the
same leg on strih-snv as VLAN 70 TAGGED on the rig NIC (foh2 `sfp-sfpplus2`: VLAN 89 untagged PVID,
VLAN 70 tagged, `frame-types=admit-all`; the switch side is the supervisor's, never provisioning).

- **Facts** (`scripts/lib/strih-box-facts.sh`): `STRIH_PROGRAM_VBAN_LEG` = `mac:<MAC>` (a second NIC;
  stream-snv `mac:00:e0:4c:68:01:85`, the USB-C r8152 `enx00e04c680185`) | `vlan:<id>` (a tagged VLAN
  on the rig NIC; strih-snv `vlan:70` = `enp7s0.70`) | `none`; `STRIH_PROGRAM_VBAN_LEG_ADDR` = the box's
  /32 on it (stream-snv .204, strih-snv .202); `STRIH_PROGRAM_VBAN_SENDER` = the console (stream-snv
  mbc.lan 10.77.7.232) or `none` = the leg passes only ping (strih-snv until the phase-2 senders exist).
  The loader refuses a malformed MAC / VLAN id (1..4094), an address that is not IPv4 or is STRIH_IP, a
  sender outside the address's /24, and a sender without `STRIH_PROGRAM_VBAN_PORT`. Every role.
- **Why a /32 + a policy route.** A /24 on the leg would put a connected 10.77.7.0/24 route in main,
  and anything to the audio VLAN (dantesync, mDNS, a ping from the box) would leave through it. The /32
  has no connected route; one route `10.77.7.0/24 table=70` + rule `priority 100 from <addr> table 70`
  sends only replies FROM the leg's address (the echo replies) back through it. No gateway,
  never-default, ignore auto routes/DNS, IPv6 off: the rig NIC keeps the only default route.
- **The filter** `table inet vban_dante70` (`/etc/nftables.d/vban-dante70.nft`, loaded by its own
  oneshot `vban-dante70-filter.service` before network-pre, never the distro nftables.service, never
  `flush ruleset`; the `table` / `delete table` atomic replace of `dscp-nft.sh`): in on the leg only the
  sender's VBAN to the port + echo requests, out only echo replies, everything else dropped and
  counted. ARP is not IP and still answers (the owner's ARP check from Ableton-FOH). With a sender, one
  more input rule only COUNTS packets to the VBAN port arriving on any OTHER interface (`iifname !=
  leg iifname != lo`): verify's proof that nothing comes through the router any more. dantesync stays
  on video-clock.lan (gm_allowlist) and never sees audio-VLAN PTP (the input drop); avahi is never
  touched (the output drop keeps mDNS off the leg; an avahi restart risks the NDI sender announcement).
- **ARP sysctls** `arp_ignore=1` / `arp_announce=2` in `/etc/sysctl.d/60-vban-dante70.conf`, in the
  SLASH form (`net/ipv4/conf/enp7s0.70/arp_ignore`): a VLAN name carries a dot, which the dotted form
  would read as a path separator. systemd's `99-systemd.rules` re-runs `systemd-sysctl
  --prefix=/net/ipv4/conf/$name` on every net-device add, so the file applies at boot, on a USB re-plug
  and when NM creates the VLAN.
- **The NM profile** `vban-dante70` (`ethernet` pinned by interface name, or `vlan` with `vlan.parent`
  = the rig NIC from `strih_lx_rig_nic` + `vlan.id`). A changed profile is `nmcli connection modify` +
  `nmcli device reapply` (in place, no link bounce); only `connection.id` / `connection.autoconnect`
  changes skip the reapply. The mac kind adopts the profile already on the NIC (a fresh install brings
  it up by DHCP with a SECOND default route) and deletes every other one on it; a refused reapply
  falls back to `connection up` (the leg is not the ssh link). The interface is resolved by the MAC of
  a PHYSICAL NIC (one with a `device` link), so a VLAN twin sharing the MAC is never taken for it.
- **HARD trap, the vlan kind (supervisor 23.10.2026): the Aquantia `atlantic` driver RESETS the rig
  link for ~8 s** (`link change old 10000 new 0`, carrier lost) the FIRST time a VLAN enters its HW
  filter (`8021q: adding VLAN 0 to HW filter`): NDI, PTP and the ssh session on a production box drop.
  So setup creates the VLAN connection ONLY when absent, prints a loud `WARNING ... ~8 s` first,
  creates it DOWN (`connection.autoconnect no`, so no autoconnect races a second activation), brings it
  up exactly once, then sets autoconnect yes. It NEVER deletes, re-creates, brings down or up again an
  existing VLAN profile: a refused reapply FAILS by name, an inactive existing VLAN profile FAILS ("up
  it in a maintenance window"), an identity change (`vlan.parent`, `vlan.id`, the interface name) is
  refused untouched, and a second ACTIVE VLAN profile is only named. The ACTIVE profile is always the
  kept one, whatever its name. Verify is read-only and never touches the link.
- **Verify** (`strih_vban_leg_grade`, item 9, after the role branch, every role): `(vban-leg)`
  interface (the MAC's NIC / the VLAN on the rig NIC exists), profile (every property, one profile),
  routes (exactly the /32, no default or main-table route through the leg, the rule, the table-70
  route), arp, filter (the live table = the file, unit enabled + active), then over 3 s
  (`STRIH_VBAN_LEG_WAIT_S`) the accept counter MUST grow ("is the console sending stream ... to
  <addr>:<port>?") and the off-leg counter must NOT; no sender = one NOTE, ping only. Run it as root
  (nft).
- **setup** (`strih_vban_leg_install`, step 13 after the role branch): filter, sysctl, then the
  profile, so the leg never holds its address unfiltered; then the live read-back (address, routes).
  An absent NIC fails by its MAC with nothing changed; an uncabled one is a WARN.
- **Tests:** `tests/python/test_vban_leg_1439.py` with the fakes of `vban_leg_fakes_1439.py`
  (nmcli keeps saved vs APPLIED profiles; `ip` derives from the applied one; `nft -f` lists the loaded
  file in the nft 1.0.9 shape, pinned byte for byte to a netns render and to the live strih-snv
  table). The fixtures under `tests/fixtures/vban_leg_1439/` are a throwaway-netns render and the two
  live tables read on 23.10.2026.
- **Live state when codified (23.10.2026):** both boxes match the profile; their tables and sysctls are
  NOT persistent until setup-strih runs (no `/etc/nftables.d`, no drop-in); stream-snv's live table
  lacks the off-leg counter, so verify FAILs `(vban-leg) filter` there until then. On stream-snv the
  run touches no NM state; on strih-snv neither (the VLAN exists): no link reset on either.

## Verify locally (Tier-0, no cargo)

- `rustc --test --edition 2021 src/vban_receive_timeline.rs` (unit tests + the bench) and
  `clippy-driver --edition 2021 --test -D warnings` on the same file. Compile WITHOUT `-O`: CI runs
  the debug profile, where an unsigned underflow panics; an `-O` build wraps it silently (round 3
  shipped two GREEN commits that only passed under `-O`). Where the C header wraps, the Rust twin
  uses `wrapping_sub`, and test arrivals are monotonic like the real receive thread.
- `CARGO_MANIFEST_DIR=<wt> rustc --test --edition 2021 tests/vban_receive_timeline_parity_1439.rs`
  (the C-vs-Rust trace, bit-for-bit doubles under `-ffp-contract=off`), the same for
  `tests/vban_receive_wiring_1439.rs` (the ASRC lift, the source wiring and the workflow pins; split
  out in review round 1, both share `tests/support/vban_c_1439.rs`) and `tests/vban_source_loss_1439.rs`
  (the SHIPPED `vban-source.c` lifted into a C driver with libobs stubs: loss = silence, contiguous
  stamps, the status line's fields).
- The plugin sources: `gcc -std=gnu11 -fsyntax-only -Wall -Wextra -Wformat=2 -Werror` with a generated
  `plugin-macros.generated.h` + a two-define `obsconfig.h` (the pacing rule's recipe).
- ROZHODNUTÉ 6352253147: `rustc --test --edition 2021 src/vban_conceal.rs` (the math), the same with
  `CARGO_MANIFEST_DIR` for `tests/vban_conceal_parity_1439.rs`, `tests/vban_source_reorder_1439.rs`
  and `tests/vban_source_conceal_1439.rs` (the shipped source through the lift driver, whose `out`
  line carries each output's `mid=` and `last=` sample and `logs=`, the plugin's log lines written
  before it, atomic: both threads log). The real-thread driver with a reorder and
  short losses in it runs clean under ThreadSanitizer.
- The mutation pass of the reorder + concealment change (29 hand mutants on a scratch copy, the
  decisions edited identically in C and Rust; the store's insert-at-front mutant came from the
  review): 28 killed. The survivor -- not updating `tail[]` after a
  concealed slot -- is equivalent for every packet of 2+ frames (such a slot ends on the repeat
  itself unless it fades toward the packet that ends the run); kept for 1-frame packets.
- `pytest tests/python/test_stream_role_vban_1439.py` (facts, seed, port, hub removal, item 9/16).

## Traps met building it

- **Rendering a log line in a pytest from the shipped C format string** (the continuity gate's
  `_c_format`): substitute `" PRIu64 "` / `" PRId64 "` on the WHOLE source before searching the
  head and the closing `",` -- a format that ENDS in `PRIu64,` (the per-source `#800` line) needs its
  own `"\s*PRI[ud]64\s*,` -> `d",` rule, or the search runs on into the next call. A head must not
  end in a quote that the substitution removes (`received=%"` becomes `received=%d`).
- **A log read against `wc -l` marks splits on `\n` only.** `str.splitlines()` also splits on `\f`,
  a lone `\r`, `\x1c`-`\x1e`, `\x85`, ` /9` and moves every later index.
- A C driver that `#include`s the shipped `.c` must stub `bmemdup` too (libobs `bstrdup` is an inline
  over it), and generate `plugin-macros.generated.h` from the `.h.in`.
- The lift harness's log lines carry the `[obs-vban] ` macro prefix: match it.
- A 32+ character spelled-out character set (`ABC...xyz0123456789._-`) is blocked as a secret at
  `git add`: build it from `string.ascii_letters + string.digits` (python) or short bash locals.
- Parity coverage: random event scripts that re-anchor too often never reach a valid fit; a
  boundary-hit counter test (17 counters, every one >= 1) keeps the gate from going blind.
- The source driver (`tests/support/vban_source_driver_1439.c`) is baked into the Rust test binary at
  COMPILE time (`include_str!`): after editing it, recompile the test binary, or the old driver runs.
  The shipped `vban-source.c` and the header are read at RUN time, so a RED can be checked against
  the committed twins with `git stash push -- <twin files>`, the same compiled binary, `git stash pop`.
- Driver tokens (review round 5): `NU:NOW:VALUE` a packet, `NU:NOW:VALUE:FRAMES` one of another size,
  `stall` = the worker takes no look before the next packet (every slot it owes goes out at that
  arrival), `port=` / `name=` / `host=` / `buffer=` an update, `thread` (2nd argument) the real thread
  on the real clock. Simulated time wakes the worker ONLY at its own scheduled times or on the idle
  signal, like the shipped thread: a driver that ran the worker at every arrival would hand stored
  packets "on arrival" and hide a regression.
- Never `println!` a bench `Run` with `--nocapture`: its Debug prints every emitted record (MBs per
  run). For numbers, copy `src/vban_receive_timeline*.rs` to a scratch dir, append a test that prints
  the compact fields, and run that copy.
- `rustfmt src/vban_receive_timeline.rs` formats every `#[path]` child too (the bench, its `wake`
  child, the tests, the worker model).
