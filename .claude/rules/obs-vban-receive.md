---
paths:
  - "vendor/obs-vban/src/vban-source.c"
  - "vendor/obs-vban/src/vban-receive-timeline.h"
  - "src/vban_receive_timeline.rs"
  - "src/vban_receive_timeline_bench.rs"
  - "src/vban_receive_timeline_tests.rs"
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
- **The latency is set ONCE per anchor, then held (ROZHODNUTÉ 6343922049, round 3):**
  - the anchor is the EARLIEST arrival of the first second: a packet whose END position is within one
    second of the anchor packet's start and that arrived earlier against the line moves the line to
    the anchor it would give (carried back by the line's own arithmetic, so a packet ON the line never
    moves it by a rounding ns). After that second the phase is fixed; stamps may step BACK inside the
    first second (libobs' 70 ms smoothing appends them, the stamp-basis ASRC reads the placement error);
  - only the RATE is followed: the phase term joins the step only while `|stamp - (fit + R + shift)|`
    is over R/4 (the dead band), then at the old 5 ppm per bucket until back inside;
  - before the fit the outlier reference is the anchored line - R (no longer the first packet's
    arrival, which may itself have come at the end of a gap);
  - effective R = max(R, one packet + 10 ms), fixed at the anchor; the source logs ONE `obs-vban
    receive buffer:` warning per (R, effective R) pair.
- **Fit:** least squares of arrival over the packet's END sample position, in one-second buckets,
  64 s window, used from 10 buckets. A packet more than 10 ms late against the reference line is not
  folded (a gap's delayed packets never pull the line late).
- **Stamps:** `stamp(pos) = base + T(pos - base_pos)`, T scaled by `1 + step_ppb/1e9`. Per fitted
  bucket the step moves toward `fit_ppb` (+ `clamp(phase / 20 s, +-100 ppm)` only outside the dead band)
  by at most 5 ppm, clamped +-500 ppm, rebased on every change: stamps never jump after the first second. The genlock ASRC (`basis=stamp`) resamples
  the sender's rate to the mix (pinned: the lifted `genlock_audio_asrc_placement` returns STAMP for
  the source's direct stamps; nothing in obs-vban touches asrc / genlock_fifo / monitoring).
- **Loss:** a `nuFrame` gap = silence for exactly the lost slots, stamped on the same line.
- **Stale (review round 1):** a duplicate, or an index at most `STALE_PACKETS` (64) behind the last
  one (a UDP reorder), is dropped and counted (`stale=` on the status line), never output: its slot
  already went out as silence. Before, one swapped pair restarted the timeline and read `lost=2`.
- **Late shift = one network event, one re-anchor (round 3):** a packet past its stamp while the fit
  is valid shifts the line ONCE to the stamp it would anchor (arrival - length + R = `fit + R` + its
  excess over the fit), rebased at the first lost slot (those slots are silence on the new line), the
  fit + buckets kept, counted as a `reason=late` re-anchor with `fit=kept`. The excess is the held
  `shift_ns`: the band and the R hold are centred on `fit + R + shift`, so the shifted latency is HELD
  (a 90 ms hiccup at R 40 leaves the program ~130 ms late until the next full anchor -- an A/V offset
  the operator sees; the main's interpretation call, raised on the ticket) and no second re-anchor
  follows. Before the fit is valid a late packet still restarts the timeline.
- **Re-anchor (one counted event, `fit=restarted`):** a late packet before the fit, an index further
  behind (a sender restart), a jump > 2 s of audio (a sender restart), another rate / channel count /
  R, or a fit that cannot hold (rate beyond +-500 ppm, stamps more than R off `fit + R + shift`,
  nothing folded for 3 s of audio). The re-anchoring packet is the new anchor. Known limit: a
  PERSISTENT delay step of more than 10 ms over the fit stops the folding, so the starve rule restarts
  3 s after the late shift (two re-anchors for one step).
- **Bench** (`src/vban_receive_timeline_bench.rs`, a two-clock sender at +-50 ppm, 5..7 ms bursts,
  gaps of 5..30 ms every ~400 ms, receive-thread wake jitter): zero late slots and zero re-anchors at
  R = 40 ms over 10 min (min margin 3.8-6.0 ms since the earliest-arrival anchor), the fitted rate
  within ~1.5 ppm, the latency graded over the WHOLE run after the first second: within 0.87 ms of its
  anchored value at +-50 ppm (the 10 s at the nominal rate before the fit + the 5 ppm/bucket slew to
  it), 0.29 ms at 0 ppm. The round-2 reviewer's case (the first packet at the end of a 25 ms gap)
  anchors at 40.4-40.6 ms (65.4 ms before round 3, then walked 21.8 ms) and holds within 0.72 ms. A 90
  ms hiccup is one late shift, the shifted latency held within 0.05 ms. At R = 20 ms the same sender
  goes late (the test that the bench can fail).
- **The bench grades STAMP latency, not what OBS realizes (review round 3).** libobs appends a stamp
  that steps by less than 70 ms (`TS_SMOOTHING_THRESHOLD`) and the stamp-basis ASRC then pulls the
  placement onto the RAW stamps at most `ASRC_MAX_PPM` = 300 ppm (`media-io/asrc-compensator.h`; the
  error is measured before the smoothing, obs-source.c `genlock_intended_ns`). So a first-second
  refinement of X ms (a first packet that came X ms late) or a late shift under 70 ms reaches OBS's
  real latency over ~X / 300 ppm (25 ms: ~83 s). Read it on the rig as the input's `asrc:` placement
  error right after an anchor. Raised to the main as a design question (hold the output for the first
  second, or accept the ASRC's walk).
- **Known limit, pinned:** before the fit the line runs at the nominal rate, then slews to it at 5 ppm
  per bucket, so the held stamp latency moves ~ the sender's ppm x 20 s: within 1 ms to ~+-55 ppm,
  1.9-2.0 ms at +-100 ppm (`known_limit_a_100_ppm_sender_moves_the_held_stamp_latency_about_2_ms_1439`).
- **The pre-fit outlier reference is the anchored line - R (kept despite review round 3):** on a
  bufferbloat sawtooth (delay ramping 0 -> 30 ms every 5 s) it starves the fit more often than the old
  first-packet reference (24 vs 12 fit re-anchors in 120 s; both are unusable at R 40), but with the old
  reference the 25 ms first-gap case breaks the +-1 ms bound (the gap's packets fold into the first fit).

## The status lines (OBS log, prefixed `[obs-vban] ` by the plugin's blog macro)

```
obs-vban receive anchored: source '<input>' frame=N rate=48000 ch=2 receive_buffer_ms=R
obs-vban receive: received=N lost=N late=N re_anchors=N stale=N silence_ms=X max_late_ms=X sender_ppm=X phase_ms=X shift_ms=X fit=locked|acquiring receive_buffer_ms=R rate=N ch=N port=P stream='S' source='<input>'
obs-vban receive re-anchor: source '<input>' reason=late|backward|jump|format|config|fit frame=N late_ms=X re_anchors=N late=N fit=kept|restarted shift_ms=X
obs-vban receive buffer: source '<input>' receive_buffer_ms=R is not above one packet (X ms) + 10 ms: using X ms
```

`obs-vban receive: ` stays on exactly ONE log line (the wiring test pins it): every other line uses
another prefix (`anchored:`, `re-anchor:`, `buffer:`).

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
