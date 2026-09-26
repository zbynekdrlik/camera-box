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
| `src/vban-output-thread.c` | `vban_out_loop` is paced (details below). `send_packet` and `packet_samples_for` are the 0.3.1 packet code, moved into helpers without changing it. |
| `src/vban-output.c` | Adds the `pacing_target_ms` setting: the "Send Buffer" property (20–200 ms), with a default of 64. |
| `src/vban-output-internal.h` | Adds `pacing_target_ms` to `struct vban_out_s`. |
| `data/locale/en-US.ini` | Adds `VBAN.out.prop.pacing_target_ms="Send Buffer"`. |

How the paced `vban_out_loop` works:

- It converts every queued mix block into the jitter buffer as soon as the block arrives.
- It calls `vban_pacing_step()` on each wake, to learn how many packets to send.
- It sleeps to the next deadline with `os_sleepto_ns()`.
- It logs the 10 s status line and a line whenever the config changes.

### The pacing (`vban-pacing.h`)

- **Buffer.** A jitter buffer with a target depth, 64 ms by default and clamped to 20–200 ms. A
  value of 0 or a negative value means the default. So a Lua script that never sets the value
  gets 64 ms.
- **Schedule.** Packet `n` leaves at `t0 + n × packet_samples / rate` on the disciplined
  `os_gettime_ns()`, the media clock from issue 1372 part A. `t0` = the moment a full packet is
  first buffered + the target.
- **Send.** Every due packet goes out in the same wake, never one per wake.
- **Underflow.** When a packet is due and less than one packet is buffered, the thread sends
  nothing: no zero-fill, no fabricated samples. It counts `underflows`, and the next full
  packet re-anchors the schedule.
- **Overflow.** Above target + 200 ms, the oldest whole packets are dropped down to the target,
  and `overflows` counts it.
- **Unchanged.** The packet contents and the frame counter are unchanged. The stream is cut into
  the same packets as in 0.3.1: 256 samples, or 239 for 24-bit stereo. The payload bytes and
  `nuFrame` are the same.

### Log lines (OBS log, prefixed `[obs-vban]` by the plugin macro)

```
obs-vban pacing-config: target_ms=64 packet_samples=239 rate=48000 stream='cg'
obs-vban pacing: depth_ms=… underflows=… overflows=… late_max_ms=… target_ms=64 stream='cg'
```

The second line comes every 10 s. `underflows` and `overflows` are cumulative since the last
(re)configuration. `late_max_ms` is the largest send lateness in the window.

## Re-basing onto a newer upstream

1. Import the new tag verbatim in its own commit, and update the pin in `vendor/README.md`.
2. Re-apply the files in the table above.
3. `tests/vban_pacing_parity_1372.rs` and the pwsh assert step in both `windows-genlock*.yml`
   files fail if the pacing is lost.
