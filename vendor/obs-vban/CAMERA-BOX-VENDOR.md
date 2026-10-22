# obs-vban, vendored for camera-box (issue 1372)

## Upstream

- Repository: https://github.com/norihiro/obs-vban (GPL-2.0-or-later, see `LICENSE`).
- Version: tag `0.3.1`. The annotated tag object is `08c077330416bf8c43661e21517c92fe0c64015a`, and it
  points at commit `58edc8ab449dbbc45367846126bc7b4921d06f1b`.
- Imported as plain vendored files, like `vendor/av-sync-dock`. The import commit carries the tree
  byte-identical to that tag. Every camera-box change is in later commits, so
  `git diff <import commit> HEAD -- vendor/obs-vban` shows exactly our diff.

## Why it is vendored

The cg OBS on RESOLUME-SNV sends the program audio to FOH (fohabl) and lv1 over VBAN through this
plugin. In 0.3.1 the send thread sent at most ONE packet per wake. It woke on the audio callback,
or after a timeout truncated to 4 ms. On resolume the OBS audio callback takes 14–28 ms of each
21.3 ms tick and sometimes stalls (55.3 ms). So the packets left in bursts and gaps, and the FOH
receiver likely underran. The rate itself was correct: over 20 min the stream was on the PTP
grandmaster within 0.2 ppm (finding in issue 1372, comment 5845239583).

## Our diff

| file | change |
|---|---|
| `src/vban-pacing.h` | NEW. The pure pacing decision (see below). It has no OBS dependency, and `tests/vban_pacing_parity_1372.rs` compiles it and pins it to `src/vban_pacing.rs`. |
| `src/vban-output-thread.c` | `vban_out_loop` is paced (details below). `send_packet` and `packet_samples_for` are the 0.3.1 packet code, moved into helpers without changing it. `send_silence` sends a zero-filled packet of the same size (issue 1381). `pacing_sleep_until` waits on a Windows high-resolution timer and spins only the last 0.2 ms. The unused `buf_ts_ns` field is removed. |
| `src/vban-output.c` | Adds the `pacing_target_ms` setting: the "Send Buffer" property (20–200 ms), with a default of 64. |
| `src/vban-output-internal.h` | Adds `pacing_target_ms` to `struct vban_out_s`. |
| `data/locale/en-US.ini` | Adds `VBAN.out.prop.pacing_target_ms="Send Buffer"` and (issue 1439) `VBAN.src.prop.receive_buffer_ms="Receive Buffer (ms)"`. |
| `src/vban-receive-timeline.h` | NEW (issue 1439). The pure receive-timeline decision of the VBAN SOURCE (see below). No OBS dependency; `tests/vban_receive_timeline_parity_1439.rs` compiles it and pins it to `src/vban_receive_timeline.rs`. |
| `src/vban-source.c` | Issue 1439. The receive callback stamps every packet from the receive timeline instead of `arrival − length`, outputs a lost packet as silence for exactly its slot (0.3.1 padded it with copies of the next packet), adds the `receive_buffer_ms` setting ("Receive Buffer (ms)", 10–500, default 40, read at create/update only) and logs the 10 s `obs-vban receive:` status line. |

How the paced `vban_out_loop` works:

- It converts every queued mix block into the jitter buffer as soon as the block arrives.
- It calls `vban_pacing_step()` on each wake, to learn what to drop, how many audio packets and
  how many silence packets to send.
- A drop is always whole packets; the thread advances `nuFrame` by the dropped packets.
- It sleeps to the next deadline with `pacing_sleep_until()`, or, while a slot waits for its
  audio, waits on the audio event (`vban_pacing_wait_ms()`: at most until the grace ends).
- It logs the 10 s status line and a line whenever the config changes.

### The pacing (`vban-pacing.h`)

- **Buffer.** A jitter buffer with a target depth, 64 ms by default and clamped to 20–200 ms. A
  value of 0 or a negative value means the default. So a Lua script that never sets the value
  gets 64 ms.
- **Schedule (a fixed timeline, issue 1381).** Packet slot `n` leaves at
  `t0 + n × packet_samples / rate` on the disciplined `os_gettime_ns()`, the media clock from issue
  1372 part A. `t0` = the moment a full packet is first buffered + the target, set once; lateness
  never moves it.
- **Send.** Every due packet goes out in the same wake, never one per wake.
- **Late audio.** A due slot whose audio is not buffered yet waits for it up to 100 ms (the
  grace). It then leaves late but complete (`late_sends`). The catch-up after it is capped at twice
  real time.
- **Silence.** A slot still without audio at the grace end is a zero-filled silence packet. So is
  every following slot, on schedule, until the buffer holds the target again. One episode is one
  counted `discontinuities`.
- **Stale repay.** Each silence packet is a debt. When the pacer is on schedule and the buffer holds
  target + debt (the backlog arrived), the debt is dropped once, in whole packets. The stalled
  audio plays late until then, so the drop is a forward skip, counted as `repays`. A buffering
  hole has no backlog: nothing is dropped, and its debt is forgiven at the next episode.
- **Ceilings.** More than 2 s buffered, or the next slot more than 2 s overdue, is one counted
  `resyncs`: back to the target, on the grid.
- **Retarget.** A new target while running moves the schedule later (up) or drops the difference
  at the next wake, never below the new target (down, a counted discontinuity). The counters
  carry on.
- **No trim, no overflow drop** (both removed by issue 1381).
- **Packets.** The stream is cut into the same packets as in 0.3.1: 256 samples, or 239 for 24-bit
  stereo. The payload bytes of an audio packet are the same. `nuFrame` counts every packet, audio
  or silence, and skips the dropped packets so the receiver's own loss counter sees a drop.

### Log lines (OBS log, prefixed `[obs-vban]` by the plugin macro)

```
obs-vban pacing-config: target_ms=64 grace_ms=100 packet_samples=239 rate=48000 counters=reset stream='cg'
obs-vban pacing: depth_ms=… late_sends=… discontinuities=… repays=… silence_ms=… discarded_ms=… resyncs=… late_max_ms=… target_ms=64 dest=10.77.x.x:6980 stream='cg'
```

The second line comes every 10 s. The counters are cumulative since the thread started
(`counters=reset`); a Send Buffer change logs `counters=kept`. `late_max_ms` is the largest
lateness of an audio packet in the window. `dest=` is the resolved receiver address and port.

### The receive timeline (`vban-receive-timeline.h`, issue 1439)

The Linux OBS boxes take program audio as a VBAN stream into this plugin's SOURCE. 0.3.1 stamped
every packet at its arrival minus its length, so the sender's network jitter reached OBS (which
mixes at 0 ms audio buffering) and a late packet was a click. The timeline instead:

- **Anchor.** The first packet is stamped at arrival − length + R. R is the fixed Receive Buffer
  (default 40 ms, clamped 10–500), read at create/update only, never adapted at runtime. R is
  measured from the packet's start, so the line uses max(R, one packet + 10 ms), fixed at the anchor
  (one `obs-vban receive buffer:` warning names both when it lifts R). Within the first second of the
  sender's audio a packet that arrived earlier against the line moves the line to it: the anchor is
  the EARLIEST arrival of that second (ROZHODNUTÉ 6343922049), and after it the latency is held.
- **Fit.** A least-squares line of arrival over the sender's sample position, over the last 64
  one-second buckets, used from 10 buckets. A packet more than 10 ms late against the line is not
  folded, so a gap's delayed packets never pull the line late.
- **Stamps.** Slewed toward the fitted RATE (at most 5 ppm per update, clamped ±500 ppm), rebased on
  every change, so they never jump. A phase term toward `fit + R` joins only while the stamps are more
  than R/4 off it (a real sender-clock event), until they are back inside that dead band; inside it the
  latency is constant to the sample. The genlock ASRC (`basis=stamp`, libobs
  `genlock_audio_asrc_placement`) resamples the sender's rate to the mix.
- **Loss.** A `nuFrame` gap is silence for exactly the lost slots, stamped on the same line.
- **Stale.** A duplicate, or a `nuFrame` at most 64 packets behind the last one (a network
  reorder), is dropped and counted (`stale=`): its slot already went out as silence.
- **Reconfiguration.** Another port, stream name or host starts a new timeline. `update()` asks for
  the reset BEFORE it lets the new sender in (`vban_src_string_changes`, the predicate
  `update_string()` uses), and again after.
- **Late shift (one counted re-anchor).** A packet after its stamp while the fit is valid shifts the
  line once to the stamp it would anchor (`fit + R` + its excess over the fit), the fit kept, the slots
  lost before it silence on the new line; the excess is held (`shift_ms`), so the next fit never
  re-anchors again (one network event = one re-anchor).
- **Re-anchor (one counted event).** A packet after its stamp before the fit is valid, a `nuFrame`
  further behind (a sender restart), a jump of more than 2 s of audio, another rate/channel count/R,
  or a fit that cannot hold (rate beyond ±500 ppm, stamps more than R off `fit + R + shift`, nothing
  folded for 3 s).

**Every Windows OBS gets it too.** Both Windows genlock builds compile this `vban-source.c`, and a
FULL bundle deploy replaces `obs-vban.dll` on every Windows OBS box (the pacing section's note). A
Windows VBAN SOURCE then stamps on the timeline: R (40 ms by default) more latency, a lost packet is
silence, the status line is logged. Before the first FULL deploy that carries it, read each Windows
box's scene collections for `net.nagater.obs-vban.source` (read-only); no box was read for this.
The same holds for the Linux strih-role boxes (strih-snv, strih-pp): their strih bundle loads
`obs-vban.so` from now on, and a VBAN source dormant in a collection ported from Windows would come
alive and could take the intercom hub's UDP port. Read their collections the same way first.

Log lines:

```
obs-vban receive anchored: source 'Zvuk z mixu' frame=… rate=48000 ch=2 receive_buffer_ms=40
obs-vban receive: received=… lost=… late=… re_anchors=… stale=… silence_ms=… max_late_ms=… sender_ppm=… phase_ms=… shift_ms=… fit=… receive_buffer_ms=40 rate=48000 ch=2 port=6980 stream='stream' source='Zvuk z mixu'
obs-vban receive re-anchor: source '…' reason=late|backward|jump|format|config|fit frame=… late_ms=… re_anchors=… late=… fit=kept|restarted shift_ms=…
obs-vban receive buffer: source '…' receive_buffer_ms=10 is not above one packet (5.33 ms) + 10 ms: using 15.33 ms
```

The status line comes every 10 s; its counters are cumulative since the source was created.
`max_late_ms` is the largest arrival − stamp of an on-time packet in that 10 s window, so it is
negative while packets arrive with margin (about −(R − one packet) plus jitter: the stamp is the
packet's start, its arrival its end, −34.7 ms at R 40 / 256 frames / 48 kHz); a packet past its
stamp shifts the line (or re-anchors before the fit) and is counted in `late`. An R no longer than
one packet + 10 ms is lifted to that floor (ROZHODNUTÉ 6343922049), so the margin is never below
10 ms.

## Re-basing onto a newer upstream

1. Import the new tag verbatim in its own commit, and update the pin in `vendor/README.md`.
2. Re-apply the files in the table above.
3. `tests/vban_pacing_parity_1372.rs` and the pwsh assert step in both `windows-genlock*.yml`
   files fail if the pacing is lost. `tests/vban_receive_timeline_parity_1439.rs`,
   `tests/vban_receive_wiring_1439.rs`, `tests/vban_source_loss_1439.rs` and the same pwsh step fail if
   the receive timeline is lost.
