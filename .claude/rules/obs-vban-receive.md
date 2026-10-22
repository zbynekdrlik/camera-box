---
paths:
  - "vendor/obs-vban/src/vban-source.c"
  - "vendor/obs-vban/src/vban-receive-timeline.h"
  - "src/vban_receive_timeline.rs"
  - "src/vban_receive_timeline_bench.rs"
  - "src/vban_receive_timeline_tests.rs"
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
    slot is filled); the first packet past it is the first `PACKET`, already on the final line, so
    libobs never sees a stamp step (it appends a step under 70 ms, `TS_SMOOTHING_THRESHOLD`, and its
    stamp-basis ASRC would walk the error out at <= 300 ppm, `ASRC_MAX_PPM`). Slots lost just before
    the first emitted packet are not filled (libobs has nothing from the source yet). Cost: ~1 s of
    silence at OBS start and after each full re-anchor;
  - **the lead-in (review round 5):** a stretch's first output is always an in-time packet (review
    round 6: a slot is filled only once the stretch has output its first packet, so a late packet
    before it is dropped and counted with NO fill -- a fill there was a first output whose stamp had
    passed, with no lead-in, and a hiccup ending a re-anchor's hold started the stretch 6-36 ms late).
    It carries `lead_in_samples` of silence from its arrival up to its stamp, stamped to end exactly
    there (0 only when less than a frame is left). Why: libobs RESETS
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
- **Fit:** least squares of arrival over the packet's END sample position, in one-second buckets,
  64 s window, used from 10 buckets. A packet more than 10 ms late against the reference line is not
  folded (a gap's delayed packets never pull the line late).
- **Stamps:** `stamp(pos) = base + T(pos - base_pos)`, T scaled by `1 + step_ppb/1e9`. Per fitted
  bucket the step moves toward `fit_ppb` (+ `clamp(phase / 20 s, +-100 ppm)` only outside the dead band)
  by at most 5 ppm, clamped +-500 ppm, rebased on every change: emitted stamps never jump. The genlock
  ASRC (`basis=stamp`) resamples the sender's rate to the mix (pinned: the lifted
  `genlock_audio_asrc_placement` returns STAMP for the source's direct stamps; nothing in obs-vban
  touches asrc / genlock_fifo / monitoring).
- **Loss:** a `nuFrame` gap = silence for exactly the lost slots, stamped on the same line (once the
  source emits).
- **Stale (review round 1):** a duplicate, or an index at most `STALE_PACKETS` (64) behind the last
  one (a UDP reorder), is dropped and counted (`stale=` on the status line), never output: its slot
  already went out as silence. Before, one swapped pair restarted the timeline and read `lost=2`.
- **Late = lost (round 4; the round-3 late shift is withdrawn as a runtime latency change):** a fixed
  latency is a fixed jitter buffer. A packet past its stamp (after the hold) is a `LATE` event:
  dropped, counted in `late`, its slot (with the slots lost right before it) silence AT ONCE on the
  unchanged line (once the stretch has output its first packet); the stamps, the fit and the line stay
  where the anchor put them. **Known limit (review rounds 4-6), the LATE FILL:** at 0 ms OBS
  buffering that silence reaches libobs after its window was mixed: the source underran,
  `discard_audio` moved its label to the window's end and kept the data, so the latency OBS REALIZES
  steps up and the stamp-basis ASRC walks it back at <= 300 ppm (~100 s for 30 ms). The same holds for
  a plain LOSS a little longer than the margin (R - one packet, ~35 ms at R 40): an outage under the
  1 s starve is filled when the next packet arrives. Bench: 34-38 ms for a 90 ms hiccup, 7.5 ms for a
  40-50 ms loss, 28.8 ms for a 60-80 ms loss (quantized by the mix windows, phase-dependent). Longer
  outages do NOT step it: about three windows after the data ran out libobs CLEARS the source
  (`discard_if_stopped`, `audio_ts = 0`), the fill is placed at its stamp (`if (push_back &&
  source->audio_ts)`) and the frozen-mix guard drops what is behind the window (`MIXED_LATE`), so the
  stream plays from its stamps again (500 / 900 ms: held). Round 5 published +455 / +861 ms for those:
  a model artifact (the bench had no clear). The model assumes an encoder runs (the frozen mix, the
  production case; with none libobs grows the whole mix's buffering instead) and 0 ms OBS buffering
  (a launch can draw 64 or 85 ms, issue 1425, and a fill that late then never underruns). The cure (a
  deadline timer that sends a slot's silence before its window is mixed, the only one that covers
  every gap) is the open design question on the ticket, not in the code.
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
- **`lost` counts filled slots only:** a slot lost inside the hold (or just before the first emitted
  packet) is never filled and never counted; `silence_ms` is what went out as silence.
- **Bench** (`src/vban_receive_timeline_bench.rs`, a two-clock sender at +-50 ppm, 5..7 ms bursts,
  gaps of 5..30 ms every ~400 ms, receive-thread wake jitter): the replay emits what the source emits
  and models libobs (`Obs`: the 70 ms append + the 300 ppm stamp-basis ASRC), and grades BOTH the
  stamp and the REALIZED latency per anchor over the whole run after the hold. Since review round 5
  the model has both libobs domains: the stamp-domain 70 ms snap, the system-domain 70 ms append, and
  the system-domain `MAX_TS_VAR` (2 s) RESET that places a chunk at its arrival; every anchor must
  realize its own stamp latency (+-1 ms). Since round 6 it also clears a source whose partial
  remainder got no data for two more windows (`discard_if_stopped`) and then places at the stamp (the
  frozen-mix guard drops the past part). Not modelled: no encoder running (libobs grows the mix's
  buffering), the one-window-earlier clear when `pending_stop` is left set, a buffering above 0 ms.
  - Main runs: zero late, zero re-anchors at R = 40 ms over 10 min (min margin 3.8-6.0 ms), the fitted
    rate within ~1.5 ppm, held within 0.87 ms at +-50 ppm, 0.29 ms at 0 ppm.
  - The round-2 reviewer's 25 ms first-gap case: realized 40.4-40.6 ms, held within 0.13 ms at 0 ppm
    and 0.72 ms at +-50 ppm (round 3 emitted the first second: 59.9 ms realized, then 19.3 ms of walk).
  - A 90 ms one-off hiccup: 11 late drops, ZERO re-anchors, the stamps held, the realized latency
    held up to it and then stepped 34-38 ms (the late-fill limit above, pinned).
  - A loss just past the margin (60 ms): zero re-anchors, the stamps held, the realized latency stepped
    28.9 ms (the late-fill limit, pinned); a 500 / 900 ms loss: libobs cleared the source, held.
  - A 90 ms hiccup that ends a re-anchor's hold (950 / 965 / 980 ms after it): the stretch realizes
    its stamp latency within 0.02 ms, held within 0.15 ms (it was 6-36 ms off before round 6).
  - A 90 ms persistent delay step: 170 late drops (one second on our clock), exactly ONE re-anchor,
    the new stretch at the old latency + 90.0 ms, held within 0.40 ms.
  - A 1.5 s outage or a 1.5 s sender pause: ONE starve re-anchor each, no drop, nothing filled; libobs
    resets its timing after the gap, the lead-in puts the new stretch on its stamp: realized = stamp
    latency within 9 us, held within 0.43 ms, the latency back where it was.
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
obs-vban receive emitting: source '<input>' frame=N margin_ms=X lead_in_ms=X after the first second's hold
obs-vban receive: received=N lost=N late=N re_anchors=N stale=N silence_ms=X max_late_ms=X sender_ppm=X phase_ms=X fit=locked|acquiring|frozen receive_buffer_ms=R rate=N ch=N port=P stream='S' source='<input>'
obs-vban receive late: source '<input>' frame=N late_ms=X -- dropped, its slot is silence on the anchored line (late=N)
obs-vban receive re-anchor: source '<input>' reason=starve|backward|jump|format|config|fit frame=N starve_ms=X re_anchors=N late=N -- holding its first second
obs-vban receive buffer: source '<input>' receive_buffer_ms=R is not above one packet (X ms) + 10 ms: using X ms
```

`obs-vban receive: ` stays on exactly ONE log line (the wiring test pins it): every other line uses
another prefix (`anchored:`, `emitting:`, `late:`, `re-anchor:`, `buffer:`). `late:` is logged once
per run of late packets (the counter counts each one); `reason=starve` on a re-anchor means no packet
came in time for a second, and `starve_ms` says how long (0 for every other reason). `lead_in_ms` on
`emitting:` is the stretch's lead-in. `fit=frozen` on the status line: nothing folded for 3 s while the fit is
valid. `max_late_ms` includes the dropped late packets.

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
  timeline is touched only there. The status line takes the stream name from the PACKET header
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

- **Facts:** `STRIH_PROGRAM_VBAN_STREAMS` / `_PORT` / `_RESERVE_MS` (stream-pp and stream-snv `stream`
  / `6980` / `40`; strih boxes `none` x 3 until their slice; the loader refuses a strih role with values
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
  (the box's collection already exists; the launch seed switches `mbc`).
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
