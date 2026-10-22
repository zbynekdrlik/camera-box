---
paths:
  - "vendor/obs-vban/src/vban-source.c"
  - "vendor/obs-vban/src/vban-receive-timeline.h"
  - "src/vban_receive_timeline.rs"
  - "src/vban_receive_timeline_bench.rs"
  - "src/vban_receive_timeline_tests.rs"
  - "src/vban_receive_timeline_late_tests.rs"
  - "tests/vban_receive_timeline_parity_1439.rs"
  - "tests/support/vban_receive_scripts_1439.rs"
  - "tests/vban_source_loss_1439.rs"
  - "tests/vban_receive_wiring_1439.rs"
  - "tests/support/vban_c_1439.rs"
  - "tests/support/vban_source_driver_1439.c"
  - "scripts/program_audio_vban.py"
  - "scripts/lib/strih-program-vban.sh"
  - "tests/python/test_stream_role_vban_1439.py"
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
    up to that slot's stamp, stamped to end exactly there (none when less than a frame is left). Why:
    libobs RESETS
    a source's timing when a chunk lands more than 2 s (`MAX_TS_VAR`) off where its previous one ended
    in the system domain (`reset_audio_timing`, `source_output_audio_data`) and puts that chunk at
    the instant it ARRIVED, whatever its stamp. A starve + its 1 s hold always leaves such a gap, so
    the new stretch started one R early (bench: 10.0 ms realized against a 40.4 ms stamp latency
    after a 1.5 s outage, then a 30.5 ms walk). The lead-in starts at the arrival, so the output lands
    on its stamp, reset or not;
  - only the RATE is followed: the phase term joins the step only while `|stamp - (fit + R)|` is over
    R/4 (the dead band), then at the old 5 ppm per bucket until back inside;
  - before the fit the outlier reference is the anchored line - R (no longer the first packet's
    arrival, which may itself have come at the end of a gap);
  - effective R = max(R, one packet + 10 ms), fixed at the anchor; the source logs ONE `obs-vban
    receive buffer:` warning per (R, effective R) pair.
- **The playout deadline (round 5, ROZHODNUTÉ 6347872706, Option B):** the source emits on its OWN
  deadline, never on a packet's arrival.
  - The receive callback only decides (`vban_recv_packet`: HOLD / PACKET / LATE / STALE / a new anchor)
    and stores an in-time PACKET for its slot (`d.pos`).
  - A playout WORKER thread per source (`vban-src-out`) calls `vban_recv_due(now, store head)` until it
    says NONE. Every slot -- one packet's length -- goes to libobs at its deadline,
    `stamp - VBAN_RECV_DEADLINE_MARGIN_MS` (2 ms): the stored packet when its position is the store's
    head, else silence of exactly that slot (counted `lost`).
  - The deadlines run on the stamp line, i.e. the fitted SENDER rate, so libobs gets one continuous
    stamp sequence ahead of the mix window that takes it: it never underruns, never clears the source,
    and never grows the mix's buffering. The window holding a stamp is mixed at its end, after the
    stamp; the margin covers the worker's wake jitter.
  - The margin comes out of R's budget: a packet is in time with up to R - one packet - 2 ms of delay
    over the earliest arrival. `emit_late_ms` on the status line is the window's latest slot past its
    deadline: read it on the box before blaming the network.
  - A stretch STARVES once a slot's deadline is more than `PERSIST_S` (1 s) after the last in-time
    arrival: the worker emits nothing more, and the next packet re-anchors.
  - Another packet size re-anchors as `format` (a slot is one packet).
  - A slot still to go out when the fit rebases the line at the next packet's start is read back along
    the line (`stamp_at` before `base_pos`: within ~0.2 us of the old line, far under a sample).
- **The worker in the source (`vban-source.c`):**
  - One `pthread_mutex` guards the timeline, the store (a `util/deque.h` of `struct vban_src_slot`
    headers + planes, at most 4096 packets: a packet that does not fit is dropped and logged once,
    `obs-vban receive store:`), the stream's sample format (the silence's) and the idle flag.
  - The lock is held only for a decision and a copy: the worker calls `obs_source_output_audio` after
    it, and the receive side logs from a snapshot after it.
  - The worker sleeps to its next deadline with `os_sleepto_ns` (absolute on the OBS clock, at most
    20 ms at a time so a stop is prompt). While idle it waits on `worker_wake` (100 ms), which a packet
    that gives it work signals.
  - It is spawned at the END of `update()`, and stopped and JOINED at its start and in `destroy()`:
    after the UDP callback is removed, before the summary reads the timeline and before anything is
    freed. Each stop logs `obs-vban receive worker: source '<input>' stopped (update|destroy)`.
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
- **Loss:** a slot whose packet is not stored at its deadline = one slot of silence at that deadline,
  stamped on the same line (once the stretch emits).
- **Stale (review round 1):** a duplicate, or an index at most `STALE_PACKETS` (64) behind the last
  one (a UDP reorder), is dropped and counted (`stale=` on the status line), never output: its slot
  goes out as silence (even when the reordered packet came before its deadline: a follow-up could
  store it). Before, one swapped pair restarted the timeline and read `lost=2`.
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
- **`lost` counts the slots handed to libobs as silence** (round 5): a packet missing at its deadline,
  whether it never came or came `late` afterwards; `silence_ms` is that silence. A slot lost inside
  the hold, or before the stretch's first output, is never sent and never counted, and no `missing N
  packet(s)` line is logged for it. A late packet before the first output is counted in `late` (its
  `late:` line says nothing was filled).
- **Bench** (`src/vban_receive_timeline_bench.rs`): a two-clock sender at +-50 ppm, 5..7 ms bursts, gaps
  of 5..30 ms every ~400 ms, receive-thread wake jitter.
  - **What it runs:** the replay runs the source as shipped. The receive decision, then the playout
    worker at every slot's deadline between arrivals (`Replay::worker`), into the libobs model `Obs`.
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
- **Known limit, pinned:** before the fit the line runs at the nominal rate, then slews to it at 5 ppm
  per bucket, so the held latency moves ~ the sender's ppm x 20 s: within 1 ms to ~+-55 ppm, 1.9-2.0 ms
  at +-100 ppm (`known_limit_a_100_ppm_sender_moves_the_held_stamp_latency_about_2_ms_1439`).
- **The pre-fit outlier reference is the anchored line - R (kept despite review round 3):** on a
  bufferbloat sawtooth (delay ramping 0 -> 30 ms every 5 s) it starves the fit more often than the old
  first-packet reference, but with the old reference the 25 ms first-gap case breaks the +-1 ms bound
  (the gap's packets fold into the first fit).

## The status lines (OBS log, prefixed `[obs-vban] ` by the plugin's blog macro)

```
obs-vban receive anchored: source '<input>' frame=N rate=48000 ch=2 receive_buffer_ms=R -- holding its first second
obs-vban receive emitting: source '<input>' frame=N margin_ms=X after the first second's hold -- every slot goes out 2 ms before its stamp
obs-vban receive: received=N lost=N late=N re_anchors=N stale=N silence_ms=X max_late_ms=X sender_ppm=X phase_ms=X fit=locked|acquiring|frozen emit_late_ms=X receive_buffer_ms=R rate=N ch=N port=P stream='S' source='<input>'
obs-vban receive late: source '<input>' frame=N late_ms=X -- dropped, its slot is silence on the anchored line (late=N)
obs-vban receive re-anchor: source '<input>' reason=starve|backward|jump|format|config|fit frame=N starve_ms=X re_anchors=N late=N -- holding its first second
obs-vban receive buffer: source '<input>' receive_buffer_ms=R is not above one packet (X ms) + 10 ms: using X ms
obs-vban receive worker: source '<input>' stopped (update|destroy)
obs-vban receive store: source '<input>' frame=N -- full (4096 packets): the playout worker is not emitting, the packet is dropped
```

`obs-vban receive: ` stays on exactly ONE log line (the wiring test pins it): every other line uses
another prefix (`anchored:`, `emitting:`, `late:`, `re-anchor:`, `buffer:`). `late:` is logged once
per run of late packets (the counter counts each one); `reason=starve` on a re-anchor means no packet
came in time for a second, and `starve_ms` says how long (0 for every other reason). `fit=frozen` on the
status line: nothing folded for 3 s while the fit is valid. `max_late_ms` includes the dropped late
packets. `emit_late_ms` (round 5) is the window's latest slot handed to libobs past its deadline: the
worker's wake jitter on the box, which must stay well under the 2 ms margin.

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
  UDP 6980: the source decodes it (pinned by `tests/vban_source_loss_1439.rs`). Its OBS input `mbc` is
  today a hub-fed capture: the stream role's setup step 13 moves the hub aside, and mbc must keep
  sending to 6980 (OBS binds it once the hub is gone). The collection port has no `stream-snv` preset
  (the box's collection already exists; the launch seed switches `mbc` with the 80 ms fact). An `mbc`
  that is ALREADY an obs-vban input keeps its operator R (verify item 9 NOTEs the difference): set
  `receive_buffer_ms` to 80 in its properties.
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
  12 s, R vs the fact as a NOTE, loss counters as a NOTE); the hub-only rows of item 9 (the program
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
- `pytest tests/python/test_stream_role_vban_1439.py` (facts, seed, port, hub removal, item 9/16).

## Traps met building it

- A C driver that `#include`s the shipped `.c` must stub `bmemdup` too (libobs `bstrdup` is an inline
  over it), and generate `plugin-macros.generated.h` from the `.h.in`.
- The lift harness's log lines carry the `[obs-vban] ` macro prefix: match it.
- A 32+ character spelled-out character set (`ABC...xyz0123456789._-`) is blocked as a secret at
  `git add`: build it from `string.ascii_letters + string.digits` (python) or short bash locals.
- Parity coverage: random event scripts that re-anchor too often never reach a valid fit; a
  boundary-hit counter test (17 counters, every one >= 1) keeps the gate from going blind.
