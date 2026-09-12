---
paths:
  - "src/genlock_stamp.rs"
  - "src/genlock_pacing.rs"
  - "src/ndi.rs"
  - "vendor/distroav/src/ndi-output.cpp"
  - "vendor/distroav/src/ndi-source.cpp"
---

# Genlocked NDI sender contract (`docs/genlock-sender-contract.md`)

The normative, external-facing spec for what ANY NDI sender (SongPlayer, the `cg` OBS, a future
sender) MUST do to lock to the fleet clock lives in **`docs/genlock-sender-contract.md`** (issue
1294). The files this rule scopes are the camera-box **reference implementation** that contract
points at — if you change the stamp, pacing, or sender behaviour here, the contract's cited
facts must stay true, so re-read the contract before editing.

The load-bearing invariants the contract pins to this code (keep them honest):

- **Timecode = FLOOR boundary, 100 ns since the Unix epoch — never ceil, never
  `NDIlib_send_timecode_synthesize`, never 0, never a monotonic counter.** `floor_boundary_100ns`
  (`src/ndi.rs`) and its #1009 doctrine comment; `genlock_emit_timecode_100ns`
  (`src/genlock_stamp.rs`); the OBS-as-sender twin `genlock_emit_timecode_100ns` /
  `genlock_floor_boundary_100ns` (`vendor/distroav/src/ndi-output.cpp`). A ceil/future stamp
  trips the receiver's issue-147 backward-step guard (the 2026-08-07 −900 ms collapse, #1009).
- **Sender create `clock_video=false, clock_audio=false`** (`src/ndi.rs`) — the app owns cadence,
  never the NDI SDK's free-running clock.
- **Pacing on the epoch grid** — one frame per boundary, catch-up `GENLOCK_MAX_CATCHUP_INTERVALS`
  ≤ 8, grid-resync beyond, backward-step re-latch, starvation repeat with the NEW boundary
  timecode (`src/genlock_pacing.rs`: `genlock_emit_gate` / `genlock_latched_boundary` /
  `starvation_repeat_timecode_100ns`).
- **Audio timecode = raw wall clock, no snap** (`vendor/distroav/src/ndi-output.cpp`), delivered
  at real-time rate; the receiver ASRC paces by arrival rate, not sender audio timecodes.
- **Receiver side** (why all of the above matters): DistroAV maps the forced
  `PROP_SYNC_NDI_SOURCE_TIMECODE` ×100 → `obs_source_frame.timestamp`
  (`vendor/distroav/src/ndi-source.cpp`); ts-aligned release engages only for an epoch-ns
  wall-clock timestamp (`genlock_is_wallclock_ts`), releasing at
  `present_ts = wall_now − latency_ms` (`vendor/obs-studio/libobs/obs-source.c`). A non-wall-clock
  stamp falls back to the weak fixed-depth count gate (`genlock_decide`, ~300 ms / 9-frame
  cross-source spread).

Acceptance is ground truth on the RECEIVER's `genlock-fifo audit` counters, not the sender's own
— the verdict is `src/resolume_playback.rs` `evaluate()` over the `src/jitter_audit.rs`-parsed
counters (±20 ms `ts_head_skew_ms`, zero `underruns`/`dropped_due`/`relocks`/`late_holds`/
`backward_steps`). See `.claude/rules/jitter-audit-parser.md` for the parser itself.
