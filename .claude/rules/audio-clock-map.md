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
| Card PLAYBACK (the cutters' cans, aplay) | `pipe_fill` + `pipe_drift_servo`: single-frame drops / repeats. | **lane 2 pending.** Breaks the rule today. |
| HDMI PLAYBACK (strih-pp vestibule, aplay) | The same `pipe_drift_servo`. | **lane 2 pending.** Breaks the rule today. |
| `pw-cat` PLAYBACK (strih-pp `program_out` to OBS) | The same `pipe_drift_servo`. The PipeWire graph is driven by the card or a dummy driver. | **lane 2 pending.** |
| Phones (Janus) egress | `janus_pacing::PacedRing`: 1 ms splices at 50-pop runs. | **lane 2 pending.** |
| Phones (Janus) ingress | `JitterBuffer::local_capture` ring: no rate handling, only trims. | **lane 2 pending.** |
| VBAN egress to the camboxes | No crossing at the hub: one packet per hub block. The cambox's own receive buffer (`src/intercom.rs`) crosses into its headset ADC clock. | Not mapped by this ticket (cambox side). |

**The resampler leg's fixed latencies** (the hub's ingress, computed):

| Leg | Target | Cap | Source |
|---|---|---|---|
| cambox (cam1-7) | 768 frames = **16.0 ms** | 9 blocks | TOML `target_ms = 16.0` |
| program feed (fohabl, lv1, mbc) | 1792 frames = **37.3 ms** | 13 blocks | TOML `target_ms = 37.33` |
| card capture (cutters) | 2048 frames = **42.7 ms** | 32 blocks | TOML `target_ms = 42.67` |

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

| Path | Source clock -> crossings -> listener | Fixed latency at the listener (computed) |
|---|---|---|
| Program into the strih OBS | FOH console `strih-obs` VBAN (port 6981, VLAN 70) -> obs-vban receive timeline (sender rate fitted, fixed R) -> OBS mix (FLEET) | R = **30 ms** (`STRIH_PROGRAM_VBAN_RESERVE_MS`) + the OBS genlock audio path (`.claude/rules/obs-vban-receive.md`) |
| Program in the cutters' cans | `fohabl-strih` (96 kHz, decimated) and `lv1-strih` VBAN -> hub program legs (resampler) -> mix -> card playback pipe (servo, lane 2) -> MiniFuse | 37.3 ms + the aplay pipe 640 frames (13.3 ms) + the ALSA buffer kept full 1024 (21.3 ms) + the period aplay holds 256 (5.3 ms) = **~77 ms** |
| Cameramen -> cutters' cans | cambox ADC VBAN -> hub cambox leg (resampler) -> mix -> card playback | 16 ms + ~40 ms = **~56 ms** (+ the cambox's own capture buffering, not mapped) |
| Cutters' mics -> cameramen | MiniFuse capture (arecord, period 256) -> hub card leg (resampler) -> mix -> VBAN `camN` -> the cambox's receive buffer | 5.3 ms + 42.7 ms + the cambox side (not mapped) |
| Phones <-> everyone | Janus (BROWSER) -> local-capture ring 2048 frames (42.7 ms, no rate handling) -> mix; mix -> PacedRing 2 x 20 ms -> Janus -> WebRTC jitter buffer | lane 2 pending; the hub's part alone is ~40 ms each way |
| YouTube / recordings | Not on strih-snv: the stream box below. The strih OBS records its own program (the path above). | as above |

## strih-pp (the PP cutting box, `STRIH_ROLE=strih`)

| Path | Source clock -> crossings -> listener | Fixed latency (computed) |
|---|---|---|
| Program into the strih OBS | lv1 VBAN `strih` -> hub program leg (resampler) -> `program_out` -> `pw-cat` pipe (servo, lane 2) -> PipeWire graph (quantum 1024) -> OBS pulse capture | 37.3 ms + pipe 1792 frames (37.3 ms) + one graph quantum (21.3 ms) = **~96 ms** |
| Vestibule TV (HDMI-1 -> SDI) | lv1 -> hub program leg -> mix -> aplay HDMI pipe (servo, lane 2) | 37.3 ms + 13.3 ms + 21.3 ms + 5.3 ms + `hdmi_delay_ms` 0 = **~77 ms** + the Blackmagic converter and the TV |
| Program in the cutters' cans | lv1 -> hub program leg -> mix -> MG-XU playback (aplay, servo, lane 2) | ~77 ms (the same chain as the SNV cans) |
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
| Vestibule (strih-pp) | the hall's live sound vs the TV's HDMI audio | the TV is ~77 ms + converter + TV behind the console | Owner HARD rule (14.10.2026): the vestibule audio is never delayed further, and A/V there is fixed by speeding the picture up. Lowering this chain is lane 2 (resampler egress) + a smaller program target. |
| Cutters' cans (both boxes) | the program in the cans vs the room / foldback sound the cutter also hears | ~77 ms | Not ruled by the owner yet. Rule of thumb: under ~10 ms reads as one sound (comb filtering), over ~30 ms as an echo. The owner asked for "low/zero latency" there. Open: lane 2 + a smaller cans path. |
| Cameramen | the cutter's voice in the headset vs the cutter shouting in the hall | ~48 ms + the cambox side | Not ruled. Same rule of thumb. |
| Stream program vs picture | obs-vban R vs the NDI picture's genlock pin | aligned by the av-sync dock / the genlock pins | The E2E A/V gate (`AV_OFFSET_GATE_TOLERANCE_MS`). |

## Measured vs computed

Every number in the tables above is **computed** from the constants named in the cells
(`vban_jitter`, `pipe_fill::pipe_servo_setpoint_for`, `hdmi_audio::ALSA_HDMI_*`,
`alsa_card::ALSA_CARD_PERIODS`, the box facts in `scripts/strih-boxes/*.env`, the hub TOMLs).

The live reads so far:

- strih-snv hub 1.7.0-dev.822, 24.10.2026: `fohabl` `servo_drops: 908` (the single-frame
  crossing lane 1 removes), target 1536.
- The FOH sender's largest gap: 27.6 ms (4.10.2026).
- mbc's gaps up to 52 ms under production load (`.claude/rules/obs-vban-receive.md`).

After a hub deploy with lane 1, read on `/api/state` each leg's `rate_ppm` and its
`asrc_phase_frames`, plus `underruns` / `resyncs` flat. The status line shows the same per leg as
`asrc=<leg>:rate_ppm=..:asrc_phase_frames=..:underruns=..:resyncs=..`.
