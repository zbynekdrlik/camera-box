---
paths:
  - "src/av_window.rs"
  - "src/probe/av_sync_recording.rs"
  - "src/bin/recording-verdict.rs"
  - "src/qpsk_probe_decision.rs"
  - "scripts/lib/marker-decodability-preflight.sh"
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


## In-preflight AUDIO-ONLY decodability probe + the `[4b3/8]` gate (#1324)

The discriminator above is a VERDICT-time (post-recording) read. #1324 adds the PRE-record sibling so
an undecodable mbc chain never burns ~40 min: `recording-verdict --qpsk-probe <wav|mkv>` reuses the
SAME `qpsk_marker::decode_markers_with_stats` demod on a short AUDIO-ONLY capture (no emit-log / video
pairing) and prints ONE JSON line `{preamble_screens,candidates,cluster_samples,crc_ok,crc_fail,
peak_dbfs,verdict}` (verdict ∈ OK/UNDECODED/SILENT/POLLUTED), exit 0 (a pure reporter). The pure
decision is `src/qpsk_probe_decision.rs` (crate-root, default-feature, Tier-0): `consistency_cluster_size`,
`peak_dbfs`, `classify`, `build_report`, `report_json`.

- **Why a SELF-CONSISTENCY cluster, not `crc_ok`:** on a drowned chain the demod fires MORE CRC-valid
  decodes than a healthy one (real-data 16.9: failed 480/551 false vs green 107 real, and per-cam
  `cluster_samples=0`), so a raw count PASSES the broken chain. `consistency_cluster_size` counts the
  longest chain of consecutive-in-time decoded markers sharing ONE emit cadence (modal index-step S +
  modal gap G, tolerating a single miss via 2S/2G), SELF-CALIBRATED from the window (bakes in no fixed
  painter cadence). Measured over 20/25 s windows: GREEN ∈ [5,9], FAILED ∈ [1,3] → clean split at N=4.
- **Decodability is PRIMARY (supervisor correction 16.9):** `cluster_samples >= min_clusters ⇒ OK at
  ANY level` (a loud-but-decodable capture is the healthy marker ≈ −19 dB; issue 1323's −20 bar was
  miscalibrated on the broken chain, so it is a COVARIATE here). Only when UNDECODABLE do the level
  bars name WHY: `peak < −60` (#748 floor, single-sourced) ⇒ SILENT; `peak > −20` (loud covariate,
  single-sourced) ⇒ POLLUTED; else ⇒ UNDECODED. Verdict words are DISJOINT so the abort names the class.
- **`[4b3/8]` step** (`scripts/lib/marker-decodability-preflight.sh` + a thin block in `recording-e2e.sh`,
  after the #1323 ceiling, before StartRecord): make a ~25 s stream probe recording, ffmpeg-extract the
  mbc `a:0` track to a mono-f32 WAV on the stream box (`win_ssh_run`), `win_ssh_download` to dev1, run
  the probe from `$PROBE_BIN_DIR`, abort (`exit 1`) naming the class on any non-OK verdict. SKIP only
  when the probe binary is absent (a loud UNVERIFIED, never a silent pass). Every knob env-overridable
  (`AUDIO_DECODABILITY_*`); the −60/−20 bars are READ from `audio-presence-preflight.sh`, never retyped.
- **Tier-0:** the pure decision + demod are default-feature, so the full synthesized-audio → demod →
  decision path (incl. loud+decodable=OK) is verifiable via a rustc `--test` replica; the bash lib +
  `[4b3/8]` wiring via `tests/python/test_marker_decodability_preflight_1324.py`. The probe-gated
  `--qpsk-probe` ffmpeg glue in `recording-verdict.rs` is CI-only.
- **TRAP — do NOT "fix" `consistency_cluster_size`'s strict-consecutive run with a gap-budget.** The
  chain resets on ANY interspersed off-cadence pair BY DESIGN. A reviewer will suggest tolerating K
  intervening non-matching pairs (to survive a false decode mid-sequence); it was TESTED against the
  real 16.9 recordings and FALSE-PASSES the drowned chain — a 1-pair budget lifts the failed-window
  cluster 3→5, above the N=4 floor (the scattered false decodes chain across the skipped pair). Strict
  is exactly what keeps a drowned window's coincidental runs short. Residual risk is a slightly-more-
  degraded-but-healthy chain false-ABORTING (fails SAFE, never a bad run through; class named loudly;
  `min_clusters`/window/ENABLE env-overridable). To widen the green margin, widen the WINDOW (25 s →
  green ∈ [7,9]), never relax the run.