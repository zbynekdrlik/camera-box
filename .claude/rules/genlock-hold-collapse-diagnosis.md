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

## Log silence LIES on PRE-#1009 builds — the once-per-event latch

On a build WITHOUT the #1009 fix, the `genlock-fifo backward clock step ... re-anchoring` WARN
logs once per event transition (`genlock_in_backward_step` latch). Under a SUSTAINED condition
the guard re-anchors every tick with **no further log lines** — "last logged 05:09" proved
nothing; the collapse ran silently for 3 more hours. Judge by the audit counters above, never by
the absence of the WARN. **Since #1009** a persistent regime re-warns on a bounded cadence
(`backward-step regime persists`, >2 s old, ≤1 per 5 s), logs `backward-step regime ENDED` on
exit, and every re-anchored tick increments `backward_regime_ticks=` on the 5 s audit line
(delta'd as `d_regimetick` by `genlock-jitter-report` / `delta_backward_regime_ticks` in its
JSON) — a healthy window's delta is 0.

## Why PRE-#1009 builds fire at a hair-trigger (fixed by #1009, both halves)

Pre-#1009 the sender (`ndi-output.cpp genlock_emit_timecode_100ns`) stamped the **next** (ceil)
frame boundary — every stamp 0..interval in the FUTURE at emit — and the guard triggered on
`max(queued ts) > wall_now + interval` on any single routine `due==0` hold tick. Margin between
normal operation and collapse ≈ network delay only (measured trigger excess: min 0.3 ms). A few
ms of sender-box-ahead clock skew (e.g. a dantesync NTP chase-step sawtooth — check
`[NTP] Stepped` lines on the RECEIVER box, and `:8898` HTTP status on both) tipped it.

**#1009 fixed both halves:** senders stamp the FLOOR boundary (at-or-before the emit instant —
`genlock_floor_boundary_100ns` / `floor_boundary_100ns`, never future-dated; NOTE: this shifts
every stamp exactly one interval earlier, so stamp-derived readings — `ts_head_skew_ms`, the
phase calibrator's reconstructed latency, effective A/V — move by ~one interval per hop on the
first post-#1009 deploy and must be re-baselined by measurement, not assumed unchanged), and the
receiver guard requires `max_ts > wall_now + max(3×interval, 250 ms)` SUSTAINED for 3
consecutive due==0 ticks (exit is qualified the same way; a due>0 tick exits immediately).
Tier-0 authority: `src/genlock_backlog.rs BackwardStepGuard` + its FIFO-sim acceptance tests.

## Repair

**Post-#1009 builds SELF-HEAL:** when the condition clears, the guard zeroes the locked cadence
boundary and the release re-ACQUIREs the configured hold (a bounded ~latency_ms transient; the
`regime ENDED` WARN marks it) — no operator action needed for the collapse itself.

**Pre-#1009 builds (and the audio-ratchet reset on any build):** a `SetInputSettings` latency
nudge re-inits the receiver but does NOT clear the collapse (latch + condition persist —
verified live). The repair is a **full OBS relaunch** via
`scripts/launch-obs-genlock.sh --box <box> --force` (win-* MCP Shell, never ssh). Side benefit on
any build: resets the libobs audio-buffering ratchet — per-session state every calibration
silently bakes in (149 ms vs 0 ms shifted the measured baseline by ~+87 ms across the relaunch).

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
