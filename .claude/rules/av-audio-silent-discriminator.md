---
paths:
  - "src/av_window.rs"
  - "src/probe/av_sync_recording.rs"
  - "src/bin/recording-verdict.rs"
  - "scripts/e2e_discord_report.py"
---

# A/V silent-vs-undecoded discriminator (#748 point 3)

When the fused verdict has `all_cambox_av_sync` with EVERY judged camera `candidates == 0`, do NOT
assume the mbc measurement chain is muted — `candidates == 0` conflates a genuinely SILENT chain
(mbc mute / Dante misroute) with audio that is PRESENT but whose QPSK marker never decoded (a broken
emit/painter side, or a decode regression). The verdict carries the discriminator so the operator
alert blames the right link:

- **Signal source:** `qpsk_marker::DecodeStats::preamble_screens_passed` (the #690 dock capability),
  the whole-recording preamble-onset count — `== 0` means the demod saw no preamble energy
  (no/near-silent signal). Measured from the ACTUAL recorded audio, so it also catches a chain that
  went silent MID-record (the pre-record `[4b2/8]` audio preflight only guards the START).
- **Pure decision (Tier-0):** `av_window::classify_av_audio_state(judged_cameras,
  all_judged_candidates_zero, preamble_screens_passed) -> AvAudioState {Measured|Silent|
  PresentUndecoded}` + `av_audio_silent_flag() -> Option<bool>`. Fails closed: `judged==0` (vacuous
  all-zero) → `Measured`. Reuse this — never re-derive the classification.
- **Carrier:** `AvMarkerInputs.audio_preamble_screens_passed: u64` (`#[serde(default)]` so an older
  partial JSON, or a rollout where the stream box still runs an old binary, deserializes to 0 = the
  LOUD fail-safe: treated as Silent → "check mbc mute"). `decode_av_marker_inputs` must use
  `decode_markers_with_stats` (not `decode_markers`) to keep the stat.
- **Verdict JSON:** the emission inserts `av_audio_silent` (`true` silent / `false` present-undecoded
  / `null` N-A) + `av_audio_preamble_screens` into the `all_cambox_av_sync` block.
- **Consumer:** `e2e_discord_report.py` — `_section_av_sync` and `_av_reason(node, av_audio_silent)`
  branch on it: `False` → "značka nedekódovaná / QPSK-emit, NIE mute mbc"; `True`/`null`/absent →
  the loud "MERACÍ ZVUK TICHÝ / check mbc mute" default. Keep BOTH the summary line and the
  per-camera reason consistent (do not let the per-camera line say "tichá stopa" when audio is present).

## Gotcha — adding a field to `AvMarkerInputs` (probe-gated, no local compile, #477)

`AvMarkerInputs` has NO `Default` derive, so a new field must be added to EVERY struct-literal site
(there were 13: 9 in `recording-verdict.rs` tests, 3 in `tests/recording_verdict_merge_gate_exit_code.rs`,
1 in `recording_partial.rs`) plus the real `decode_av_marker_inputs` constructor, or CI fails to
compile. For a shorthand `audio_markers,` site, place `audio_preamble_screens_passed:
audio_markers.len() as u64` BEFORE the `audio_markers` move (struct fields evaluate in source order,
so the `.len()` borrow is released before the move — no use-after-move). `AvMarkerInputs` derives only
`PartialEq`, not `Eq`, so a future `f64` field is safe (the #726 Eq-derive trap does not bite here).
Verify locally with `cargo fmt --all --check` (rustfmt parses cfg-gated files); CI is the first type check.
