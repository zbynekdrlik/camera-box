---
paths:
  - "vendor/obs-studio/libobs/obs-genlock-audio-buffering.h"
  - "vendor/obs-studio/libobs/obs-audio.c"
  - "vendor/obs-studio/libobs/obs.c"
  - "src/genlock_audio_buffering.rs"
  - "tests/genlock_audio_buffering_parity_1367.rs"
  - "tests/genlock_audio_buffering_wiring_1367.rs"
  - "scripts/obs-guarded-launch.ps1"
  - "scripts/rig-health-audit.py"
---

# The genlock audio-buffering FLOOR (issue 1367, ROZHODNUTÉ 5857354949)

## Why

Stock libobs starts the mix window with 0 audio buffering. It grows the buffering only when a
source happens to arrive late at startup (`add_audio_buffering`, the dynamic increase), so every
launch drew its own value. On the stream box that draw decided whether the `mbc` ASRC could reach
its #1355 absolute level target (100 ms + the sync offset) at all. The two stream OBS logs of
27.9.2026:

| Session | Buffering | `mbc` result |
|---|---|---|
| `01-33-25` | `adding 85 milliseconds … (source: ASIO Input Capture)` | Target held for 10 h, through the trims 41 → 26 → 23 → 18 ms. `level_avg` 118.1 at target 118, 0 fallbacks. |
| `12-18-15` | none | Level 27 ms against 118. The restore pushed −136 … −141 ppm for 40 min; the level sawtoothed 27 → 78 → 36 → 86 ms. Two `UNREACHABLE … fell back` lines re-latched the target at 54.8 and 73.5 ms. The release E2E then read −28 ms on every camera. |

## The physics the constant is sized by

- **Natural depth.** The depth a direct-timestamp source (`mbc`, ASIO) settles on without a servo
  correction is `buffering + base + sync_offset`.
  - `base` is its arrival latency against the mix window plus the mean of the 21.33 ms tick
    sawtooth.
  - It measured 8.4 ms (`01-33-25`, first `level_avg` 134.74 at offset 41) and 8.9 ms
    (`12-18-15`, 26.94 at offset 18), with the buffering 85 ms apart.
- **The sync offset CANCELS.** The #1355 target is `100 + last_sync_offset`, and the placement adds
  the same offset to the depth (`in.timestamp += sync_offset`), so the servo must bridge
  `100 − (buffering + base)`, whatever the #1333 split / #856 trim writes.
  - The first design said "100 + the largest sync offset + headroom ≤ buffering". With the code's
    own ±500 ms clamp (`AUDIO_OFFSET_CLAMP_MS`) that asked for ~600 ms of buffering and a target out
    of reach from ABOVE (Design-question 5857343807).
- **Reach is limited in BOTH directions.** Stretching or compressing moves the source's smoothed
  timeline off its raw stamps; at `TS_SMOOTHING_THRESHOLD` (70 ms, a symmetric `uint64_diff` in
  `source_output_audio_data`) the next packet is re-placed at its raw stamp and the correction is
  gone. Live, the `12-18-15` restore got the level at most +51 … +59 ms above natural before the
  snap.
- **The band:** `|100 − (buffering + base)| ≤ 35 ms` (half the threshold), for a base of 0–25 ms,
  at 44.1 and 48 kHz.
  - `FLOOR_MS` 85 = 4 ticks = 85.33 ms at 48 kHz (92.88 ms at 44.1 kHz): the servo stretches ~6 ms
    at the measured base. This is the live-proven `01-33-25` configuration.
  - 0, 3 ticks (64 ms) and 6 ticks (128 ms) all leave the band. 5 ticks (106.67 ms) would still
    hold it.

## What ships

| Piece | C | Rust / test |
|---|---|---|
| the plan (floor, max, override) | `genlock_audio_buffering_plan` in `obs-genlock-audio-buffering.h`, called by `obs_reset_audio2` | `plan` in `src/genlock_audio_buffering.rs` |
| the tick rule (floor first, then OBS's dynamic increase) | `genlock_audio_buffering_action` → `set_floor_audio_buffering` / `add_audio_buffering` in `audio_callback` | `action` |
| the band | `genlock_audio_buffering_band_error_ns` / `_band_ok` | `band_error_ns` / `band_ok` / `floor_holds_band` |
| raise-to-N-ticks body | `raise_audio_buffering` (upstream's fixed-mode body, shared by fixed and floor) | the C lift in `tests/genlock_audio_buffering_wiring_1367.rs` |

- **A FLOOR, not a cap.** The resolume cg OBS legitimately grows 128–362 ms on media / `NDI test`
  starts (logs `2026-09-26 09-29-12`, `14-50-56`, `16-35-04`); a hard 85 ms cap would drop that
  late audio on FOH/VBAN. The maximum stays the caller's (45 ticks by default).
- **Fixed buffering is never used.** The frontend LowLatencyAudioBuffering toggle (fixed 20 ms) is
  overridden: one `genlock audio buffering (issue 1367): … OVERRIDDEN` WARNING at reset, max back to
  45 ticks.
- **The log:**
  - `buffering type:  fixed floor 85 ms, dynamically increasing above` in the reset block;
  - `genlock audio buffering floor (issue 1367): total audio buffering is now 85 milliseconds …`
    at the first tick;
  - every later increase is ONE `LOG_WARNING` line, `genlock audio buffering ABOVE the floor
    (issue 1367): adding N … total audio buffering is now M milliseconds (source: <name>); ASRC
    level band ok|BROKEN: …`. BROKEN means buffering + the 9 ms nominal base left the band (M above
    126 ms): a mixed source on an absolute ASRC level target (the stream `mbc`) is out of reach until
    OBS restarts.
- **The #786 launch gates keep working.** `obs-guarded-launch.ps1`, `launch-obs-genlock.sh` (3b)
  and `rig-health-audit.py` parse `total audio buffering is now (\d+) milliseconds` against a 100 ms
  bound.
  - Both new lines keep that text.
  - The floor alone reads 85 / 92 ms, a clean draw.
  - A late-source increase above 100 ms is still a BAD draw, as before (the 960 ms ASIO ratchet of
    #786 is unchanged: the dynamic increase above the floor is stock OBS).
  - The "box standard 64 ms" wording in those scripts' comments predates the floor: a clean launch
    now logs exactly the floor line, 85 ms at 48 kHz.
- **The #1355 UNREACHABLE fallback is only a logged safety net now.** Its line also names
  `total_audio_buffering=` and `floor=` at that moment: above the floor, a late source raised the
  buffering (its own ABOVE line says which); at the floor, the cause is not the buffering.

## Tests and Tier-0

- `src/genlock_audio_buffering.rs`, the authority: the rounding, the plan and override, the tick
  rule, the band (4/5 ticks hold, 0/3/6 fail), the offset cancellation over ±500 ms, and the two
  measured sessions.
- `tests/genlock_audio_buffering_parity_1367.rs` compiles the header as-is against the Rust over
  vector spreads. It then holds the band on LIFTED values:
  - `ASRC_LEVEL_TARGET_MS` (and equal to `asrc_bench::LEVEL_TARGET_MS`);
  - `TS_SMOOTHING_THRESHOLD`, `AUDIO_OUTPUT_FRAMES`;
  - the split's `AUDIO_OFFSET_CLAMP_MS`;
  - the three launch-gate bounds.
- `tests/genlock_audio_buffering_wiring_1367.rs` is std-only.
  - It checks the wiring anchors and the retired upstream lines.
  - It checks the pwsh mirror in both `windows-genlock*.yml` (ONE `WIRING` list).
  - It runs a C LIFT of the shipped reset block, tick decision and buffering functions, driven
    through a launch, late sources, the low-latency toggle, 44.1 kHz, a maximum at the floor and a
    clamp past the maximum.
  - The lift runs ONCE per test binary (`OnceLock`): two parallel tests compiling into one scratch
    dir raced.
- Local runs:
  - the module: `rustc --test` + `clippy-driver --test -D warnings`;
  - the parity gate: a stub `camera_box` rlib of `genlock_audio_buffering` + `asrc_bench`;
  - the wiring file: plain `rustc --test` with `CARGO_MANIFEST_DIR`;
  - the three C files: the `obs-drm-output.md` `-fsyntax-only` recipe.
- Mutation proof: point `CARGO_MANIFEST_DIR` at a scratch tree holding the mutated header /
  `obs.c` / `obs-audio.c` plus the lifted files, and recompile each test per mutant. 19/19 C mutants
  died at landing, and 3/3 Rust constant mutants.

## Live acceptance (supervisor)

Full-bundle deploy on stream (and resolume + strih-lx, same libobs), then at least two OBS
restarts. After each:
- the log shows `buffering type:  fixed floor 85 ms` and exactly one floor line;
- `mbc` `target=` = 100 + offset and `level_avg` within ~5 ms of it after ~15 min, `fallbacks=0`;
- the release A/V gate is green and the per-camera A/V is the same across the restarts.
