---
paths:
  - "vendor/obs-vban/src/vban-source.c"
  - "vendor/obs-vban/src/vban-receive-timeline.h"
  - "src/vban_receive_timeline.rs"
  - "src/vban_receive_timeline_bench.rs"
  - "tests/vban_receive_timeline_parity_1439.rs"
  - "tests/vban_source_loss_1439.rs"
  - "tests/vban_receive_wiring_1439.rs"
  - "tests/support/vban_c_1439.rs"
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
- **Fit:** least squares of arrival over the packet's END sample position, in one-second buckets,
  64 s window, used from 10 buckets. A packet more than 10 ms late against the reference line is not
  folded (a gap's delayed packets never pull the line late).
- **Stamps:** `stamp(pos) = base + T(pos - base_pos)`, T scaled by `1 + step_ppb/1e9`. Per fitted
  bucket the step moves toward `fit_ppb + clamp(phase / 20 s, +-100 ppm)` by at most 5 ppm, clamped
  +-500 ppm, rebased on every change: stamps never jump. The genlock ASRC (`basis=stamp`) resamples
  the sender's rate to the mix (pinned: the lifted `genlock_audio_asrc_placement` returns STAMP for
  the source's direct stamps; nothing in obs-vban touches asrc / genlock_fifo / monitoring).
- **Loss:** a `nuFrame` gap = silence for exactly the lost slots, stamped on the same line.
- **Stale (review round 1):** a duplicate, or an index at most `STALE_PACKETS` (64) behind the last
  one (a UDP reorder), is dropped and counted (`stale=` on the status line), never output: its slot
  already went out as silence. Before, one swapped pair restarted the timeline and read `lost=2`.
- **Re-anchor (one counted event):** a packet past its stamp (`late`), an index further behind (a
  sender restart), a jump > 2 s of audio (a sender restart), another rate / channel count / R, or a fit that cannot
  hold (rate beyond +-500 ppm, stamps more than R off `fit + R`, nothing folded for 3 s of audio).
  The re-anchoring packet is the new anchor. A late re-anchor anchors on a late packet: if its
  lateness exceeds R the phase hold re-anchors once more (~10 s later). Known, documented, not a bug.
- **Bench** (`src/vban_receive_timeline_bench.rs`, a two-clock sender at +-50 ppm, 5..7 ms bursts,
  gaps of 5..30 ms every ~400 ms, receive-thread wake jitter): zero late slots and zero re-anchors at
  R = 40 ms over 10 min (min margin 5.3-8.6 ms), the fitted rate within ~1.5 ppm, latency steady
  within ~100 us; at R = 20 ms the same sender goes late (the test that the bench can fail).

## The status lines (OBS log, prefixed `[obs-vban] ` by the plugin's blog macro)

```
obs-vban receive anchored: source '<input>' frame=N rate=48000 ch=2 receive_buffer_ms=R
obs-vban receive: received=N lost=N late=N re_anchors=N stale=N silence_ms=X max_late_ms=X sender_ppm=X phase_ms=X fit=locked|acquiring receive_buffer_ms=R rate=N ch=N port=P stream='S' source='<input>'
obs-vban receive re-anchor: source '<input>' reason=late|backward|jump|format|config|fit frame=N late_ms=X re_anchors=N late=N
```

The status line is written by the receive callback, so it stops when packets stop. Counters are
cumulative since the source was created (a port / stream / host change re-inits them). `max_late_ms`
is the window's largest arrival - stamp of an on-time packet: about -R + jitter when healthy.
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

- **Facts:** `STRIH_PROGRAM_VBAN_STREAMS` / `_PORT` / `_RESERVE_MS` (stream-pp `stream` / `6980` /
  `40`; strih boxes `none` x 3 until their slice; the loader refuses a strih role with values and a
  stream role without exactly ONE stream). `none` is a sentinel, never a stream name.
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
- **No hub on the stream role:** `obs_box_step_applies 13` skips step 13 there and calls
  `strih_intercom_hub_remove` (stop + disable, then unit, drop-in dir, binary and `/etc/intercom-hub`
  MOVED to `/var/lib/camera-box/intercom-hub-removed/<UTC stamp>/<abs path>`, daemon-reload;
  idempotent). The hub's two drop-ins (NDI_CONFIG_DIR, local audio) are written by step 13's hub branch
  only, so the stream role gets none. Verify item 16 = `strih_intercom_hub_absent_grade`
  (FAIL while a hub file or a running unit is left), item 34 NOTEs the absent hub drop-in, item 9 =
  `strih_program_vban_grade` (the kind, the bind, port + stream vs the facts, `received` advancing over
  12 s, R vs the fact as a NOTE, loss counters as a NOTE); the hub-only rows of item 9 (the program
  rx, the hub's HDMI leg, the MiniFuse talkback) run only in its hub branch.
- **Rollout (supervisor):** the strih bundle with obs-vban (linux-genlock.yml strih variant stages
  `obs-vban.so`), then setup-strih on stream-pp with OBS stopped (the genlock deploy stops it), then
  start OBS: the seed switches `Zvuk z mixu`. A setup re-run with OBS RUNNING removes the hub under a
  live pulse input: restart OBS right after. lv1 must be sending `stream` to stream-pp:6980.

## Verify locally (Tier-0, no cargo)

- `rustc --test --edition 2021 src/vban_receive_timeline.rs` (unit tests + the bench) and
  `clippy-driver --edition 2021 --test -D warnings` on the same file.
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
