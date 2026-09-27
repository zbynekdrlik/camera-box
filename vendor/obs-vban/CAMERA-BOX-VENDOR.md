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
| `data/locale/en-US.ini` | Adds `VBAN.out.prop.pacing_target_ms="Send Buffer"`. |

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

## Re-basing onto a newer upstream

1. Import the new tag verbatim in its own commit, and update the pin in `vendor/README.md`.
2. Re-apply the files in the table above.
3. `tests/vban_pacing_parity_1372.rs` and the pwsh assert step in both `windows-genlock*.yml`
   files fail if the pacing is lost.
