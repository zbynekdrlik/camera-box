---
paths:
  - "intercom/**"
  - "intercom/*.toml"
  - "vendor/obs-vban/**"
---

# The audio clock-domain + latency map (issue 1461)

Owner, 22.10.2026: the genlock rules for samples and frames hold for EVERY audio path, and the
intercom and the strih OBS must run at low, fixed latency, or the cans or the vestibule hear the
sound twice. The rules for every audio path:

- A clock-domain crossing is done by a RESAMPLER (the genlock ASRC). It is never done by inserting
  or dropping samples.
- Every latency is fixed from the start and never adapted at runtime.
- Wherever two paths reach the same ears, the difference is stated and budgeted.

**Before any audio change, write its clocks, its crossings and its latencies into this map, in
the same PR.** "Measured" means a live read (named, dated). "Computed" means derived from the code
constants named here. Never present a computed number as measured.

## The clocks

| Clock | What runs on it |
|---|---|
| FLEET | `CLOCK_MONOTONIC` rate-disciplined by dantesync (PTP via the Dante grandmaster). The hub's mix grid, both OBS audio paths, the camboxes' own disciplined clocks. |
| DANTE | The console / Dante network sample clock (mbc, lv1, fohabl's Dante side). On the PTP grandmaster the fleet follows. Its VBAN *packets* still leave on the sender PC's own timing. |
| CAMBOX ADC | A cambox headset's USB audio crystal (cam1 measured ~+540 ppm, `capture 48026 samp/s`, 19.9.2026). |
| CARD | The cutters' USB card crystal: the MiniFuse 4 (strih-snv), the Yamaha MG-XU (strih-pp). |
| HDMI | The HDMI audio clock of the strih-pp vestibule output (the GPU's HDA codec). |
| BROWSER | A phone's WebRTC / Opus clock (Janus mixes on the box's own timer). |

## The hub's crossings (`intercom/hub`, the mix runs on FLEET at 256 frames / 5.33 ms)

| Crossing | What does it | Status |
|---|---|---|
| VBAN ingress (fohabl, lv1, mbc, cam1-7) | `asrc` resampler in `vban_jitter::NetworkFill`: 32 taps x 128 phases, ratio = the 64 s least-squares rate, +-1000 ppm (cam1's ADC +540 ppm is inside). | DONE (lane 1). Replaced the issue-1401 single-frame servo. |
| Card CAPTURE (the cutters' mics) | The same resampler leg (`JitterBuffer::card_capture`). | DONE (lane 1). Before: no rate handling at all, only a cap trim. |
| Card PLAYBACK (the cutters' cans, aplay) | `asrc` egress resampler in `pipe_fill::PipeFillControl` + `EgressQueue`: each write renders the hub blocks to the card-clock frame count; the ratio = the inverse 64 s least-squares slope of aplay's consumed frames (written minus the measured fill), +-1000 ppm. | DONE (lane 2). Replaced the issue-1401 single-frame `pipe_drift_servo`. |
| HDMI PLAYBACK (strih-pp vestibule, aplay) | The same egress resampler. | DONE (lane 2). |
| `pw-cat` PLAYBACK (strih-pp `program_out` to OBS) | The same egress resampler (pw-cat reads 1024-frame graph quanta). The PipeWire graph is driven by the card or a dummy driver. | DONE (lane 2). |
| Phones (Janus) egress | `janus_pacing::PacedRing` = a `NetworkFill` resampler leg (pushes timed before the lock, one 960-frame frame rendered per 20 ms tick). | DONE (lane 2). Replaced the 1 ms splices. |
| Phones (Janus) ingress | `JitterBuffer::janus_capture`: the same resampler leg as a VBAN leg, the room mix stamped before the jitter lock. | DONE (lane 2). Replaced the trim-only `local_capture` ring. |
| VBAN egress to the camboxes | No crossing at the hub: one packet per hub block. The cambox's own receive buffer (`src/intercom.rs`) crosses into its headset ADC clock. | Not mapped by this ticket (cambox side). |

**The resampler leg's fixed latencies** (the hub's ingress, computed):

| Leg | Target | Cap | Source |
|---|---|---|---|
| cambox (cam1-7) | 768 frames = **16.0 ms** | 9 blocks | TOML `target_ms = 16.0` |
| program feed (fohabl, lv1, mbc) | 1792 frames = **37.3 ms** | 13 blocks | TOML `target_ms = 37.33` |
| card capture (cutters) | 2048 frames = **42.7 ms** | 32 blocks | TOML `target_ms = 42.67` |
| phones (Janus room mix) | 2048 frames = **42.7 ms** | 32 blocks | default (TOML `target_ms` accepted) |

**The resampler egress's fixed latencies** (computed; the setpoint is the pipe's 1 s
time-weighted fill, plus the queue's 16-frame look-ahead, 0.33 ms):

| Egress | Setpoint | + reader buffer | Dead band | Source |
|---|---|---|---|---|
| aplay card / HDMI pipe | 640 frames = **13.3 ms** | ALSA buffer kept full 1024 (21.3 ms) + the period aplay holds 256 (5.3 ms) | +-480 | `pipe_setpoint_for(256, 256)`, `alsa_card::ALSA_CARD_PERIODS`, `hdmi_audio::ALSA_HDMI_*` |
| pw-cat pipe (strih-pp `program_out`) | 1792 frames = **37.3 ms** | one graph quantum 1024 (21.3 ms) | +-448 | `pipe_setpoint(256)` |
| Janus ring (phones egress) | 1920 frames = **40 ms** | the Janus audiobridge's own buffer | +-480 | `janus_pacing::RING_TARGET_FRAMES` |

- A spawn keeps the depth its first read's phase and the walk before its first fit gave it
  (benches: within 133 frames of the setpoint for pw-cat, 183 for aplay) for its whole run, inside
  the dead band; nothing adapts it. A refill (a starved pw-cat) re-primes to the same depth. The
  dead band is a quarter of the leg's fixed latency (setpoint + reader buffer), not of the pipe
  alone: for aplay the design's quarter of 640 (160) sat inside the +-128 its period reads ride
  plus a spawn's +-128 phase, and the phase term chased the read phase (a deviation flagged for the
  main on issue 1461).
- Benches (computed on the models, dev1 24.10.2026): pw-cat depth within 133 of 1792, aplay within
  183 of 640, the ratio within 0.5 / 4.6 ppm of the reader's clock, no refill / trim / starved read
  at 0 / +-50 / +-200 ppm.
- CPU, measured on dev1 (24.10.2026, `-C opt-level=3`, load ~7): the 6-channel card playback 107 us
  per 5.33 ms block (2.0 % of one core, on its `alsa-play` thread), a 2-channel aplay / pw-cat pipe
  37 us (0.7 %), the Janus ring 12 us per 20 ms pop.

- The target is the pre-pop depth ahead of the read point: the time from a frame's arrival to its
  turn in the mix.
- The resampler's 16-frame look-ahead comes out of the underrun margin, not on top of the latency.
- Read at start only. An underrun re-primes to the SAME target. Above the cap one counted resync.
- A lost hub tick is read past.
- The card capture's 2048 frames are the issue-1345 ring's two `pw-cat` bursts, kept by lane 1. The
  deployed `alsa` adapter's `arecord` delivers 256-frame periods, so 3-4 blocks is likely enough
  there: lower `target_ms` once the live capture jitter is read. That is 21-27 ms less on the
  cutters -> cameramen path (42.7 ms down to 21.3 or 16.0 ms).
- CPU, measured on dev1 (24.10.2026, `-C opt-level=3`): all 11 strih-snv legs playing, 17 rendered
  channels (a fanned mono cambox renders once), take 154 us of the 5.33 ms block = 2.9 % of one
  core. That runs on the hub-mix thread under the jitter lock; every push stamps its arrival
  before it takes the lock.

## strih-snv (the SNV cutting box, `STRIH_ROLE=strih`)

No sound server on this box (issue 1440): every path below is ALSA hw PCMs, VBAN or obs-vban; nothing
between the hub, the cutters' card and OBS can add a buffer or a crossing (`.claude/rules/strih-pipewire-purge.md`).

| Path | Source clock -> crossings -> listener | Fixed latency at the listener (computed) |
|---|---|---|
| Program into the strih OBS | FOH console `strih-obs` VBAN (port 6981, VLAN 70) -> obs-vban receive timeline (sender rate fitted, fixed R) -> OBS mix (FLEET) | R = **30 ms** (`STRIH_PROGRAM_VBAN_RESERVE_MS`) + the OBS genlock audio path (`.claude/rules/obs-vban-receive.md`) |
| Program in the cutters' cans | `fohabl-strih` (96 kHz, decimated) and `lv1-strih` VBAN -> hub program legs (resampler) -> mix -> card playback pipe (egress resampler) -> MiniFuse | 37.3 ms + the aplay pipe 640 frames (13.3 ms) + the look-ahead 0.3 ms + the ALSA buffer kept full 1024 (21.3 ms) + the period aplay holds 256 (5.3 ms) = **~77.5 ms** |
| Cameramen -> cutters' cans | cambox ADC VBAN -> hub cambox leg (resampler) -> mix -> card playback | 16 ms + ~40 ms = **~56 ms** (+ the cambox's own capture buffering, not mapped) |
| Cutters' mics -> cameramen | MiniFuse capture (arecord, period 256) -> hub card leg (resampler) -> mix -> VBAN `camN` -> the cambox's receive buffer | 5.3 ms + 42.7 ms + the cambox side (not mapped) |
| Phones <-> everyone | Janus (BROWSER) -> hub Janus leg (resampler, 2048 frames, 42.7 ms) -> mix; mix -> Janus ring (resampler, 1920 frames, 40 ms) -> Janus -> WebRTC jitter buffer | the hub's part: **42.7 ms** in, **40 ms** out (+ Janus's and the browser's buffers, not mapped) |
| YouTube / recordings | Not on strih-snv: the stream box below. The strih OBS records its own program (the path above). | as above |

## strih-pp (the PP cutting box, `STRIH_ROLE=strih`)

| Path | Source clock -> crossings -> listener | Fixed latency (computed) |
|---|---|---|
| Program into the strih OBS | lv1 VBAN `strih` -> hub program leg (resampler) -> `program_out` -> `pw-cat` pipe (egress resampler) -> PipeWire graph (quantum 1024) -> OBS pulse capture | 37.3 ms + pipe 1792 frames (37.3 ms) + look-ahead 0.3 ms + one graph quantum (21.3 ms) = **~96 ms** |
| Vestibule TV (HDMI-1 -> SDI) | lv1 -> hub program leg -> mix -> aplay HDMI pipe (egress resampler) | 37.3 ms + 13.3 ms + 0.3 ms + 21.3 ms + 5.3 ms + `hdmi_delay_ms` 0 = **~77.5 ms** + the Blackmagic converter and the TV |
| Program in the cutters' cans | lv1 -> hub program leg -> mix -> MG-XU playback (aplay, egress resampler) | ~77.5 ms (the same chain as the SNV cans) |
| Cameramen (cam1/4/6/7) <-> cutters, phones | as on strih-snv | as on strih-snv |

## stream-snv / stream-pp (`STRIH_ROLE=stream`: YouTube + the recordings)

| Path | Source clock -> crossings -> listener | Fixed latency (computed) |
|---|---|---|
| Program into the stream OBS (stream-snv) | mbc.lan VBAN `stream` (24-bit, 239-frame packets, USB-C NIC on VLAN 70) -> obs-vban receive timeline -> OBS mix (FLEET) -> YouTube + the recording | R = **80 ms** (`STRIH_PROGRAM_VBAN_RESERVE_MS`). No hub (setup-strih step 13 moves a stream-role hub aside). |
| Program into the stream OBS (stream-pp) | lv1 VBAN `stream` -> obs-vban -> OBS -> YouTube + the recording | R = **40 ms** |
| Headphone monitor (stream role) | the restreamer's RTMP -> ffmpeg -> relay -> PipeWire `stream-operator-headphones` | not a fixed path (monitoring only; `.claude/rules/stream-headphone-monitor.md`) |

The picture reaches YouTube over NDI from the strih box. The A/V alignment there is the genlock's
job (`OBS_GENLOCK_LATENCY_MS`, the av-sync dock), never a hub setting.

## Where two paths meet one listener (the double-sound budget)

| Listener | The two paths | Difference today (computed) | Allowed |
|---|---|---|---|
| Vestibule (strih-pp) | the hall's live sound vs the TV's HDMI audio | the TV is ~77.5 ms + converter + TV behind the console | Owner HARD rule (14.10.2026): the vestibule audio is never delayed further, and A/V there is fixed by speeding the picture up. The chain is now fixed (lane 2); lowering it is a smaller program target or ALSA buffer, an owner decision. |
| Cutters' cans (both boxes) | the program in the cans vs the room / foldback sound the cutter also hears | ~77.5 ms | Not ruled by the owner yet. Rule of thumb: under ~10 ms reads as one sound (comb filtering), over ~30 ms as an echo. The owner asked for "low/zero latency" there. Open: a smaller cans path (program target, ALSA buffer), an owner decision. |
| Cameramen | the cutter's voice in the headset vs the cutter shouting in the hall | ~48 ms + the cambox side | Not ruled. Same rule of thumb. |
| Stream program vs picture | obs-vban R vs the NDI picture's genlock pin | aligned by the av-sync dock / the genlock pins | The E2E A/V gate (`AV_OFFSET_GATE_TOLERANCE_MS`). |

## Measured vs computed

Every number in the tables above is **computed** from the constants named in the cells
(`vban_jitter`, `pipe_fill::pipe_setpoint_for`, `hdmi_audio::ALSA_HDMI_*`,
`alsa_card::ALSA_CARD_PERIODS`, the box facts in `scripts/strih-boxes/*.env`, the hub TOMLs).

The live reads so far:

- strih-snv hub 1.7.0-dev.822, 24.10.2026: `fohabl` `servo_drops: 908` (the single-frame
  crossing lane 1 removes), target 1536.
- strih-snv hub 1.7.0-dev.826, 24.10.2026: `cutters` `local_audio` `pipe_servo_drops: 71,
  pipe_servo_repeats: 516`, `pipe_depth_frames` 617 vs `pipe_setpoint_frames` 640 (the
  single-frame egress crossing lane 2 removes); `phones` ingress `overruns: 10` (the local-capture
  trims lane 2 removes).
- The FOH sender's largest gap: 27.6 ms (4.10.2026).
- mbc's gaps up to 52 ms under production load (`.claude/rules/obs-vban-receive.md`).

After a hub deploy with lane 1, read on `/api/state` each leg's `rate_ppm` and its
`asrc_phase_frames`, plus `underruns` / `resyncs` flat. The status line shows the same per leg as
`asrc=<leg>:rate_ppm=..:asrc_phase_frames=..:underruns=..:resyncs=..`.

After a hub deploy with lane 2, read each egress leg's `local_audio` facet: `pipe_rate_ppm`
settled at its reader's clock within a minute (the MiniFuse cans a few ppm to tens of ppm,
negative = the card runs fast), `pipe_rate_fit_ppm` next to it, `pipe_setpoint_frames` 640 (aplay)
or 1792 (pw-cat), `pipe_depth_frames` inside the dead band and not moving over the run,
`pipe_refills` / `pipe_trims` flat, and no `pipe_servo_*` key left. The phones: the `phones`
participant now carries a `jitter` facet (its ingress resampler leg) and an `asrc=phones:..`
status-line group; the janus facet's `tx_underflows` / `tx_overflow_trims` stay flat.
