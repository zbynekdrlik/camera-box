---
paths:
  - "src/genlock_audio_pairing.rs"
  - "tests/genlock_audio_pairing_parity.rs"
---

# Receiver-side AUDIO genlock parity (#1303)

The owner directive (2026-09-13): genlock must lock BOTH audio and video. The video leg was
already genlocked (the FIFO holds every frame to `present_ts = wall_now − latency_ms`); #1303 makes
the AUDIO leg a first-class genlocked signal with the same evidence bar.

## The mechanism (three orthogonal pieces — don't conflate)

1. **The audio HOLD (phase)** — for a `genlock_fifo` source, the audio is delayed at ingest by the
   SAME effective `latency_ms` the video FIFO holds video, so the A/V pair is presented at the same
   wall instant and survives the hold. Wired at the ONE seam `source_output_audio_data`
   (`vendor/obs-studio/libobs/obs-source.c`, right after the `sync_offset`/`resample_offset`
   adjusts): `in.timestamp += (int64_t)genlock_audio_present_delay_ns(source->genlock_latency_ms)`.
   A pure duration shift — the timeline basis (`in.timestamp` is already on OBS's converted
   monotonic timeline at that point) is irrelevant to a fixed delay. Default-safe: camera inputs
   keep `ndi_audio=false` so only program-audio sources (cg/SongPlayer) carry a material delay; at
   the 3 ms floor the delay is negligible.
2. **The ASRC servo (rate)** — `media-io/asrc-compensator.{h,c}` (#803/#912/#1084) disciplines the
   audio sample-clock RATE (ppm) against `genlock_wall_now_ns()`. ALREADY default-on and correct;
   #1303 changed NOTHING here. It is ORTHOGONAL to the hold above (rate vs phase). NB the constants
   are `ASRC_MAX_PPM`/`ASRC_MAX_SLEW_PPM_PER_S`/`ASRC_REGRESSION_*` — the old
   `ASRC_TIME_CONSTANT_S`/`ASRC_MIN_LOCK_S` were REMOVED in #1084 (a stale brief may still name
   them).
3. **Observability** — `obs_genlock_stats` (v2) + the `genlock-fifo audit` line carry
   `audio_enabled` / `audio_delay_ms` (= `latency_ms` when held, 0 = not held) /
   `audio_pairing_offset_ms` (0 = paired, `-latency_ms` = the hold never fired). Parsed by
   `src/jitter_audit.rs` (additive, forward-compatible with pre-#1303 logs).

## The pure decision + parity discipline

`src/genlock_audio_pairing.rs` is the Tier-0 authority: `genlock_audio_delay_ns(latency_ms)` (=
`latency_ms·1e6`), `pairing_offset_ms`, and `decide_audio_health` (Ok / AudioDisabledOnProgram /
AsrcSaturated / PairingOffsetExceeded — precedence in that order). The C mirror is three contiguous
`static inline` helpers in `obs-source.c`; `tests/genlock_audio_pairing_parity.rs` lifts them,
`cc`-compiles under `-Wall -Wextra -Wconversion -Wformat=2 -Werror`, and requires byte-identical
delay/offset/health over a vector spread (the #1003 lift-and-compile recipe). Keep the three
helpers CONTIGUOUS and numerically identical to the Rust.

**Lock-step anchors** (the #269 3-copy discipline): `tests/genlock_preload.rs`
(`audio_genlock_parity_present_1303`) + the pwsh gate in BOTH `windows-genlock.yml` and
`windows-genlock-fast.yml`. A change to the wiring / the helper signatures / the audit tokens must
update all three.

## Tier-0 (worktree worker: no cargo, no local sourced-lib)

- Pure module RED→GREEN: `rustc --test --edition 2021 src/genlock_audio_pairing.rs -o /tmp/t && /tmp/t`.
- C helpers: extract the contiguous block, `gcc -std=gnu99 -Wall -Wextra -Wconversion -Wformat=2
  -Werror` a driver that asserts a spread (proves the shipped bytes compile + compute right).
- `jitter_audit.rs` uses `serde_json` (not pure-std) — a plain `rustc --test` fails E0463; strip
  the `summaries_to_json` fn + its tests into a copy, or point rustc at a sibling worktree's
  `libserde_json-*.rlib`.
- The parity gate + genlock_preload guard run on CI (they `use camera_box::…` / are probe-gated).

## LOCK-indicator audio DEGRADE term (#1303 part 3b — DONE)

Landed as an additive term in the parity-gated LOCK decision: `GenlockFacets` (Rust
`src/genlock_lock_state.rs` + the C `genlock_lock_facets_t` in `GenlockLockState.hpp`) gained a bool
`audio_unpaired`; `decide` / `genlock_decide_lock_state` gained a lowest-precedence DEGRADED branch
mapping it to a new `LockReason::AudioPairing` (=9); the statusbar widget `OBSBasicStatusBar.cpp`
aggregates the per-source pairing-offset breach (`st.version >= 2 && st.audio_enabled &&
|audio_pairing_offset_ms| > GENLOCK_AUDIO_PAIRING_BOUND_MS`, 33 ms) into that one facet — the twin of
its existing qpc-drift reduction — so an unpaired audio leg turns the indicator DEGRADED (`audio
unpaired: <src>`). The C↔Rust decision stays parity-gated (`tests/genlock_lock_state_parity.rs`, now
a 2^8 flag sweep), lock-stepped by `tests/genlock_lock_indicator_guards.rs` +
`tests/genlock_preload.rs` + the #1298 pwsh gate in BOTH `windows-genlock{,-fast}.yml`. Per the
scope, audio disabled/absent never degrades (the `audio_enabled` guard); `decide_audio_health`'s
AudioDisabledOnProgram + AsrcSaturated branches are NOT surfaced here (they need is-program-source /
asrc-ppm data the v2 stats don't carry) — a followup. Full contract: `genlock-lock-indicator.md`.

## Deferred followups (NOT in the #1303 code lane)

- **Audio DEGRADE full taxonomy** — surface `decide_audio_health`'s AudioDisabledOnProgram +
  AsrcSaturated branches in the LOCK indicator (part 3b only wired the pairing-offset branch);
  needs the widget to know is-program-source + per-source asrc-saturation, neither in `obs_genlock_stats` v2.
- **Per-box certified-table audit + `deploy-genlock-fleet.sh` preflight** (part 4) — a report-only
  per-box `ndi_audio`/`yuv_*` audit + a pre-swap input listing; a separable large shell piece.
- Live A/V soak acceptance (±20 ms over 1 h, cg OBS `locked=1` + audio facet green) is a
  post-merge SUPERVISOR rig step — never rig-verified from the code lane.
