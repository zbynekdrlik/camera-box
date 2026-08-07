---
paths:
  - "vendor/obs-studio/libobs/obs-source.c"
  - "vendor/distroav/src/ndi-output.cpp"
  - "src/genlock_backlog.rs"
  - "scripts/av_sync_*.py"
  - "scripts/skew*"
---

# Genlock hold-collapse diagnosis (#1007/#1009, live 2026-08-07) — the −(latency) A/V signature

## The signature (recognize it in minutes, not hours)

A/V offset ≈ **−latency_ms** (e.g. −900 at a 894 ms knob) that appeared as a STEP and persists:
the deep hold on the receiver has collapsed to live-edge consumption. Confirm from the 5 s
`genlock-fifo audit '<input>'` line — all five together, one is not enough:

- `received − consumed` CONSTANT while both climb (consumption tracks arrival 1:1)
- `depth=0..1` with `latency_ms` still showing the configured value
- `underruns` climbing a few per window; `holds` and `late_holds` FROZEN
- `backward_steps > 0` (and possibly still incrementing)

## Log silence LIES — the once-per-event latch

The `genlock-fifo backward clock step ... re-anchoring` WARN logs once per event transition
(`genlock_in_backward_step` latch). Under a SUSTAINED condition the guard re-anchors every tick
with **no further log lines** — "last logged 05:09" proved nothing; the collapse ran silently for
3 more hours. Judge by the audit counters above, never by the absence of the WARN.

## Why it fires at a hair-trigger (until #1009 lands)

The sender (`ndi-output.cpp genlock_emit_timecode_100ns`) stamps the **next** frame boundary —
every stamp is 0..interval in the FUTURE at emit. The guard triggers on
`max(queued ts) > wall_now + interval` during any routine `due==0` hold tick. Margin between
normal operation and collapse ≈ network delay only (measured trigger excess: min 0.3 ms). A few
ms of sender-box-ahead clock skew (e.g. a dantesync NTP chase-step sawtooth — check
`[NTP] Stepped` lines on the RECEIVER box, and `:8898` HTTP status on both) tips it.

## Repair

`SetInputSettings` latency nudge re-inits the receiver but does NOT clear the collapse (latch +
condition persist — verified live). The repair is a **full OBS relaunch** via
`scripts/launch-obs-genlock.sh --box <box> --force` (win-* MCP Shell, never ssh). Side benefit:
resets the libobs audio-buffering ratchet — which is per-session state every calibration silently
bakes in (149 ms vs 0 ms shifted the measured baseline by ~+87 ms across the relaunch).

## The 15-minute independent arbiter (no dock, no gate)

With the permanent cam2 painter running (`cam2-painter.service` — its drop-in adds
`--marker-log /run/rig-qpsk-markers.csv`):

1. WS `StartRecord` → 150 s → `StopRecord` on stream.
2. `scp` the CSV from cam2 to `C:\camera-box\`, then on the stream box:
   `recording-verdict.exe --av-sync <rec.mp4> --av-marker-log <csv> --av-audio-track 0`.
3. Trust it over the dock: healthy read = mad ≤ ~15, matched ≥ ~15. The dock's `locked=yes` is
   NOT liveness (it held `locked=yes` on frozen counters for 25 min, #1008) and it carries the
   #1004 residual (+30..50 ms vs this arbiter).

## Windows-log remote-grep traps (cost three wrong conclusions in one night)

- A `|` inside a `Select-String -Pattern` regex dies in the cmd.exe layer over ssh ("X is not
  recognized...") — with stderr discarded it reads as ZERO MATCHES. Use `-SimpleMatch`, one
  pattern per ssh call, and treat empty output as suspect until a known-present pattern matches.
- `genlock-relock` lines are abnormal instants — hourly aggregates of them said "skew climbing
  906→1715" while the steady 5 s audit said stable ~900–1010. Aggregate the AUDIT, not the events.
- dantesync logs are UTC; OBS logs are local (+2 in DST). Align before correlating.
