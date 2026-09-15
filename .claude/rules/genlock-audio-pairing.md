---
paths:
  - "src/genlock_audio_pairing.rs"
  - "tests/genlock_audio_pairing_parity.rs"
  - "src/genlock_forced_table_audit.rs"
  - "scripts/lib/genlock-forced-table-audit.sh"
  - "tests/genlock_forced_table_audit_1303.rs"
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

## LOCK-indicator audio DEGRADE term — audible-but-expected-silent (#1303 part 3b/c — DONE)

Part 3c (the `audio_unexpected` axis) landed atop part 3b: `GenlockFacets`/`genlock_lock_facets_t`
gained a second bool `audio_unexpected` → `LockReason::AudioUnexpected` (=10), the LOWEST-precedence
DEGRADED branch (below `AudioPairing`), parity-gated in the now-2^9 sweep. The widget flags an
audio-ENABLED source that is silent-by-contract per the certified table — the SHIPPED subset is
box-class-agnostic (an audible CAMERA input via the parity-gated C mirror `genlock_name_is_camera`
of `genlock_forced_table_audit::is_camera_input`), named on the human `genlock-lock:` line
(`audio unexpected: <src>`) and the v3→v4 `genlock-lock-json:` line
(`audio_unexpected_inputs:[{name}]`, omit-when-empty), parsed by `bundle_state_gather`, enriched to
`audio_unexpected:<name>` by `genlock_lock_decision.analyze`. The box-class-DEPENDENT cases (a
non-camera audible on a Dante-fed box; a program source silent on the cg box) are DEFERRED — the
widget has no box identity today — and stay covered at DEPLOY time by the part-4 preflight below.
Full contract: `genlock-lock-indicator.md` + `genlock-lock-facet.md`.

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

## Per-box certified AUDIO table (#1303 part 4 — DONE)

**The audit is NOT a name heuristic — it is a per-box CERTIFIED table.** Owner ruling 2026-09-15
15:10, verbatim: „žiadny — zvuk na strih/stream ide cez Dante, NDI audio ostáva vypnuté (odporúčam,
inak hrozí dvojitý zvuk)". Program audio over NDI exists on the cg OBS (RESOLUME-SNV) ONLY; on
strih/stream/imag the mastered mix arrives over Dante/ASIO (VB-Matrix, `mbc`), so NDI audio on ANY
input there would be DOUBLE audio in the mix.

| box | expected `ndi_audio` |
|---|---|
| **resolume** (cg OBS) | camera inputs → silent; `sp-*`/SongPlayer/`cg`/music program inputs (the 9 keys) → **audio**; any other input → audio (the cg-box default). The ONLY program-audio box. |
| **strih** / **stream** / **imag** | **EVERY** NDI input silent — cameras AND `cg` AND `2ME PGM` AND `NDI obs hudba` alike. Program audio comes from Dante/ASIO, never NDI. |

The classifier `src/genlock_forced_table_audit.rs` (canonical) + the byte-for-byte bash replica
`scripts/lib/genlock-forced-table-audit.sh` encode exactly this; `tests/genlock_forced_table_audit_1303.rs`
pins the two together over a fixed vector set (bash verdict == Rust `audio_verdict` for every
box×name×`ndi_audio`). Verdicts: `OK`, `MISMATCH-PROGRAM-SILENT` (a cg program source with audio off
— the #1295 event-morning defect), `MISMATCH-CAMERA-AUDIBLE` (a camera audible), and
`MISMATCH-AUDIBLE` (a NON-camera input audible on a Dante-fed silent box — the double-audio hazard;
added per the owner's own term). `deploy-genlock-fleet.sh` emits the report-only preflight
(`PREFLIGHT (report-only, #1303 part 4)`) that pipes the box's live `GetInputList`/`GetInputSettings`
TSV into the classifier BEFORE the swap; it NEVER writes and NEVER gates.

**Why a static table, not a live WS read or a data file:** the audit is emitted by the plan builder
as deterministic pre-swap guidance, so it must be pure (no live-box coupling) and self-contained (no
runtime file lookup); the owner handed down a FIXED per-box table, so a static two-replica table
pinned by the parity gate is exactly the right shape. The old heuristic assumed "a program input
carries audio on every box" and produced five false `MISMATCH-PROGRAM-SILENT` rows on strih/stream
— that assumption is now dead.

### Gotcha — narrowing the audit's classification can silently disable an advisory gated on it

`classify` derives TWO things: the audio verdict AND the report-only `yuv_range=partial` advisory.
The advisory was originally gated on `expected == ExpectedAudio`. When the certified-table change
flipped every strih/stream/imag input to `ExpectedSilent`, that advisory silently STOPPED firing
for those boxes' program VIDEO inputs (`cg`, `NDI 2ME PGM`) — a coverage loss a self-review missed
and a fresh-context review caught. The advisory is a VIDEO concern; keying it on the AUDIO
expectation coupled it to a table that legitimately changed. It is now decoupled via
`is_program_video_input(box, name)` (`!camera && (program_keyed || box==resolume)`), computed
independently of `expected_audio`. **General rule: when you NARROW a classification (more inputs
land in a "silent"/"off"/"excluded" bucket), audit every report-only NOTE/advisory/secondary
signal gated on the OLD classification — a `matches!(expected, …)`-style gate can silently go dark.**

## Deferred followups (NOT in the #1303 code lane)

- **Box-class-aware LIVE audio-mismatch** — surface the box-class-DEPENDENT cert-table cases in
  the LOCK indicator LIVE: a NON-camera input audible on a Dante-fed box (strih/stream/imag) and a
  program source SILENT on the cg box (resolume). Part 3c wired only the box-class-AGNOSTIC
  audible-camera subset (the widget has no box identity); a robust version needs a deploy-written
  box-role marker (read like `GENLOCK_BUILD_SHA.txt`). Already covered at DEPLOY time by the part-4
  preflight, so the live version is defense-in-depth.
- **Audio DEGRADE full taxonomy** — surface `decide_audio_health`'s AudioDisabledOnProgram +
  AsrcSaturated branches in the LOCK indicator; needs the widget to know is-program-source +
  per-source asrc-saturation, neither in `obs_genlock_stats` v2.
- Live A/V soak acceptance (±20 ms over 1 h, cg OBS `locked=1` + audio facet green) is a
  post-merge SUPERVISOR rig step — never rig-verified from the code lane.
