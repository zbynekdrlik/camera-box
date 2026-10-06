---
paths:
  - "src/qpsk_marker.rs"
  - "src/qpsk_marker_scan.rs"
  - "vendor/av-sync-dock/src/camera-box-marker-scan.hpp"
  - "tests/qpsk_marker_scan_1381.rs"
  - "tests/av_sync_dock_streaming_decoder_1381.rs"
  - "src/qpsk_channel_select.rs"
  - "src/qpsk_probe_decision.rs"
  - "src/probe/av_sync_recording.rs"
  - "scripts/lib/marker-decodability-preflight.sh"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "vendor/av-sync-dock/src/camera-box-channel-pick.hpp"
  - "vendor/av-sync-dock/test/camera-box-selftest.cpp"
  - "vendor/av-sync-dock/test/channel-pick-parity.cpp"
  - "src/av_sync_dock_channels.rs"
  - "tests/qpsk_channel_pick_parity_1367.rs"
  - "tests/fixtures/qpsk_channel_pick_parity.tsv"
---

# QPSK A/V-sync marker demod — the word's full redundancy + Tier-0 verification of a gate change

## The 20-bit marker word carries 12 bits of redundancy over an 8-bit index — gate ALL of it (#1153)
`payload_word(index) = (0xF000 | index) << 4 | crc4(...)`. Bit layout (verify with a rustc scratch, the
docstring at `payload_word` is imprecise): **preamble nibble bits[19:16]=0xF (symbols 0,1), ZERO nibble
bits[15:12]=0 (symbols 2,3), index bits[11:4] (`(word>>4)&0xFF`), CRC-4 bits[3:0]**. Every valid emitted
marker has the zero nibble == 0 by construction.
- The accept gate MUST use ALL the redundancy: `(word>>16)&0xF==0xF && (word>>12)&0xF==0 && crc4_check(word,20)==0`.
  Before #1153 it checked only preamble + CRC (8 bits), leaving the zero nibble unchecked → of the 4096
  words that pass preamble+CRC, only **256 are valid vs 3840 "poison"** (nonzero zero-nibble) that a music
  mix decodes from noise → a **16× false-positive flood** that drowns the offset cluster (live dock:
  matched only ~26, mad ~30ms). The gate lives in ONE Rust kernel (`qpsk_marker_scan::scan_markers`,
  behind `decode_markers_with_stats` for the offline `recording-verdict --av-sync` AND behind the
  live-dock `StreamingMarkerDecoder`) mirrored byte-for-byte into `cb_scan_markers`
  (`camera-box-marker-scan.hpp`). Change BOTH in lockstep.
- **"98.7% CRC fail" is inherent CRC-4 physics, NOT a bug** — a 4-bit CRC passes ~1/16 of preamble-screened
  noise, so a high crc_fail rate is expected and can never go >50% on a music mix. The reliability metric is
  the CLUSTER (matched size / mad / offset stability), never the crc_ok/crc_fail ratio (a stronger gate
  correctly LOWERS that ratio by moving false decodes into crc_fail). Don't tune to the ratio.
- **A raw crc_ok count is not marker presence either, and neither is the self-consistency cluster on
  tonal audio** (issue 1404). A held in-band chord decodes runs of the SAME index every ~25 ms (up to
  80 CRC-valid words per 2 s; `cb_consistency_cluster_size` reads such a run as consistent). Any new
  "is the marker there" consumer must use the program-audio guard's rule: merge same-index re-hits,
  drop indices that repeat inside a span shorter than one wrap (256/60 s), then a 60/s ±2 timecode
  chain (`scripts/program_audio.py`, `.claude/rules/program-audio-guard.md`).

## The scan kernel and the streaming decoder (issue 1381)
Issue 1381 split the kernel out (`src/qpsk_marker_scan.rs`, `camera-box-marker-scan.hpp`) and made it
cheap without changing a decoded marker or a counter of the batch decode:
- **The refine is a sliding-window maximum** (`RefineWindow` / `CbRefineWindow`): each position's
  preamble magnitude is computed once per scan, and a monotonic queue returns the LEFTMOST maximum of
  `[i-4, min(i+span, last)]`; `i` itself wins a tie. That is exactly where the old linear refine
  landed (it moved only on a strictly larger magnitude). The C++ magnitude is now
  `sqrt(re^2 + im^2)` like the Rust `cmag` (it was `std::abs` = hypot, last-bit differences).
- **`scan_markers(samples, p, thr, start)` returns a `resume`**: the first visited position whose
  screen passed while its refine range was cut by the end of the samples, else where the scan stopped.
  The streaming decoder scans from `next_scan` (that resume, in absolute samples): the new positions
  plus the cut tail. Re-screening the cut tail is what keeps its markers the same as the old
  whole-window re-decode's, whose first sight of a marker can be such a cut refine (a pure "screen
  each position once" decoder can report a marker a few samples off). That sameness is CHECKED push
  by push on every fixture and noisy case below, not proven for every input; the batch decode's
  sameness (markers and counters) follows from the refine rule.
- **Stats changed meaning for the streaming decoder**: each screen counts once, a cut-tail position
  again when re-screened -- about half the old whole-window over-count. The dock diag's `preambles=`
  dropped accordingly (still monotonic; the #1153 watchdog only needs "advanced > 0").
- **Proof**: `tests/qpsk_marker_scan_1381.rs` = the batch decode vs a FROZEN copy of the old Rust
  kernel (markers and counters, noise/music/tone/every index/non-finite/the stereo fixture);
  `tests/av_sync_dock_streaming_decoder_1381.rs` = streaming vs whole-window, callback by callback;
  the bench checks the C++ decoder against a frozen copy of the old C++ kernel (including music +
  markers, where false decodes and skips exercise the scan path); the refine window vs the linear
  rule on tie-heavy data in both `qpsk_marker_scan`'s tests and the camera-box self-test.
- **Cost** (THREAD CPU, N100 at load ~20, per 1024-frame stereo push, the bench's RED run vs GREEN):
  music 6.8 -> 0.18-0.24 ms, a 442 Hz tone (every position passes the screen, the worst case)
  34.8 -> 0.56-0.7 ms. The investigation bench's 11.5 / 65 ms were WALL time on the same loaded box.

## Reading the live dock's own decode health (diagnosis before touching code)
The deployed stream-box dock logs a ~10s diag line — read it via win-stream-snv MCP, not ssh:
`Select-String "$env:APPDATA\obs-studio\logs\<latest>.txt" -Pattern 'av-sync-dock: (diag|LOCKED|UPDATED)'`.
`preambles == crc_ok + crc_fail` (screened candidates); `ring_hit` is NOT a false-positive filter (the audio
index is only the 8-bit frame_id low byte, which cycles every ~4.3s, so any index matches some recent frame)
→ the CLUSTER is the only real discriminator. `locked=yes` + crc_ok flowing ⇒ audio level is fine (rules out
the #689 silence/clipping class), so a weak matched/mad is a discrimination problem, not a level one.

## Tier-0 verification of a demod / parity-kernel change (cargo is BLOCKED, incl. --no-run per #557)
No local cargo compiles the probe/vendor code. The working proof chain, all local:
1. **rustc scratch** — copy the pure decode kernel + emitter into a standalone `.rs`, add OLD/NEW gate
   variants, render the case (a synthesized word) + a real marker + noise, `rustc -O` it and run. Proves
   RED→GREEN (OLD accepts / NEW rejects) and real-marker preservation without cargo. Enumerate the accept
   space exhaustively here too to QUANTIFY the change (e.g. 4096→256 = 16×), never estimate it.
2. **Direct g++ self-test** — the C++ mirror's self-test is dependency-free STL; compile it DIRECTLY (this is
   NOT cargo, so Tier-0 allows it): `g++ -std=c++17 -Wall -Wextra -Werror -o /tmp/st
   vendor/av-sync-dock/test/camera-box-selftest.cpp && /tmp/st` → `ALL PASS`. Add a CHECK for your new gate
   case; the "all 256 indices round-trip" case proves no real marker is dropped. This is the SAME proof the
   `av_sync_dock_cpp_mirror_gate` CI job runs.
3. **`cargo fmt --all --check`** (non-compiling, Tier-0-allowed) — proves the Rust (incl. probe-gated + new
   test files) parses / is brace-balanced. CI is the FIRST place the Rust actually type-checks + runs.
To render an ARBITRARY (non-marker) word in a test, both sides expose a pure `marker_signal_for_word(word,p)`
(Rust) / `marker_signal_from_word(word)` (C++ self-test); `marker_signal(index)` delegates to it.

## NEVER downmix the measurement audio — decode every channel, keep the best one (issue 1367)
The stream program recording's `mbc` audio is STEREO and carries the SAME cam2 marker on L and R, with R
10.17 ms (488 samples @ 48 kHz) behind L (zero-lag L/R correlation ≈ 0; measured 27.9.2026 on
`2026-09-27 14-29-50.mp4`, release E2E run 36317806422). One symbol is one 442 Hz carrier cycle
(2.26 ms), so the copies sit ~4.5 symbols apart at ~178° carrier phase (nearly anti-phase), and a mono
SUM smears every symbol into another: on the
real 4 s clip the old `-ac 1` path read `preamble_screens 9212, cluster 2, crc_ok 3` → **POLLUTED**,
while L alone read `653 / cluster 7 / crc_ok 7` and R alone `8 / cluster 8 / crc_ok 8`, both OK. The owner
ruled the skew is not his to fix — the gate must be robust to it.
- **The ONE pick:** `src/qpsk_channel_select.rs` (crate root, default features) — `decode_best_channel`
  runs the unchanged demod + the #1324 `consistency_cluster_size` on EACH channel, and
  `pick_marker_channel` keeps the LOWEST-index channel whose cluster clears the decodability floor;
  when none clears it, the largest cluster, ties to the lowest (design 5856569255); a mono track is
  the identity (chosen 0, byte-identical report). The `[4b3/8]` preflight (`--qpsk-probe`),
  `--av-sync`, the fused all-cambox A/V gate (`decode_av_marker_inputs`) and the live dock all apply
  it; the offset is paired from the CHOSEN channel's markers only (a skew can never average two
  arrival times).
- **Why floor-first, not the largest cluster:** R arrives 10.17 ms after L. When both channels
  decode (the live 4 s clip: L 7, R 8), the largest-cluster winner is decided by one false decode
  more or less in a strictly-consecutive chain, so the chosen channel, and with it the measured
  offset (~10 ms, a third of the ±30 ms band; ~4 ms after the 0.4 loop gain), could flip between
  runs and read as drift. Both channels clear the same floor, so preferring the lower one costs
  nothing. On the 2 s fixture only R clears (L 3), so R is still chosen there.
- **The floor is ONE value:** `qpsk_probe_decision::DEFAULT_MIN_CLUSTERS` (4) is the
  `--qpsk-min-clusters` clap default, the `--av-sync` pick floor and the dock's
  `CB_MARKER_MIN_CLUSTERS`; the shell `marker_decodability_default_min_clusters` is pinned to it by
  `tests/qpsk_channel_pick_parity_1367.rs`. The probe passes the operator's `--qpsk-min-clusters`
  to the pick, so its pick and its verdict use the same floor.
- **One rule, not always one channel:** the preflight picks with the operator's floor,
  `--av-sync` / the fused gate with the default; the offline paths judge a channel over their whole
  window (25 s probe capture, ~300 s recording), the dock over a rolling 25 s window. Under a floor
  override or with a channel hovering at the floor they can choose different channels ~10 ms
  apart, so compare `chosen_channel` before comparing offsets.
- **Level and silence are judged over the WHOLE track, decodability on the chosen channel:** the probe's
  `peak_dbfs` covariate and the #748 `audio_preamble_screens_passed` are the MAX over channels, so a
  silent channel 0 never reads "the chain is silent" while channel 1 carries audio.
- **Extract:** the probe glue asks ffprobe for the stream's channel count and passes `-ac <that count>`
  (a known raw stride, never a downmix); the preflight's Windows-side ffmpeg keeps every channel (no
  `-ac`). The one-line JSON keeps every pre-1367 key FIRST with the chosen channel's values, then
  `channels`, `chosen_channel`, `per_channel` (keys `ch_*`, so a first-match grep can never read one);
  the shell parse also strips `"per_channel":[...]` before it greps, and the `[4b3/8]` ok/abort line
  appends `chosen channel C of N (cluster ch0=a ch1=b)` (`marker_decodability_channel_note`). Two key
  spellings on purpose: the grep-parsed probe line uses `ch_*`, the serde-built `--av-sync` JSON and
  the fused block use the plain `ChannelMarkerStats` names under `audio_channel_pick.per_channel`.
- **An empty pick means "not recorded"** (an older partial): `per_channel` is `[]` and the log line
  reads `marker channel pick not recorded`, never a real channel 0.
- **No `tracing::info!` in the `--qpsk-probe` path.** `recording-verdict` initialises
  `tracing_subscriber::fmt()`, which writes to STDOUT, and the preflight captures the probe's
  stdout (2>&1) and greps it; the probe-gated test takes the last `{` line. The pick is carried in
  the JSON line instead. `--av-sync` logs its pick in the info line printed AFTER its JSON.
- **Which channel wins moves the offset by ~10 ms** (R arrives later than L), which is why the
  rule is floor-first. The pick is carried as `audio_channel_pick` in the `--av-sync` JSON and the
  fused `all_cambox_av_sync` block — read it when comparing runs; a change of `chosen_channel`
  between two runs now means channel 0 crossed the floor.
- **Real-signal fixture:** `tests/fixtures/mbc-stereo-skew-1367/mbc-stereo-2s.wav` (the first 2 s of
  that recording): downmix cluster 0, L 3 (below the floor of 4), R 4 — so both a downmix and a fixed
  "always channel 0" pick FAIL there and only the per-channel pick passes. Tier-0 proof: a plain
  `rustc --test` crate that `#[path]`-includes the real `qpsk_marker` / `qpsk_probe_decision` /
  `qpsk_channel_select` (serde derives sed-stripped) + the real test files, with
  `CARGO_MANIFEST_DIR` set for the fixture path; `clippy-driver --test -D warnings` on the same crate.
- **Not yet re-measured:** the 16.9 drowned-chain recordings were never re-run per channel (they are
  not on dev1). Per-channel reads of two local noise-flooded stereo captures (17 s and 120 s, 18.9)
  stayed at cluster 2, below the floor of 4.

## The live dock applies the same rule per channel (issue 1367, design 5856569255)
`st_raw_audio_camera_box` (`vendor/av-sync-dock/src/sync-test-output-audio.cpp`) no longer averages the
channels. It hands every channel's plane to `camerabox::ChannelMarkerPicker`
(`vendor/av-sync-dock/src/camera-box-channel-pick.hpp`, split out of the over-budget
`camera-box-audio.hpp`) and pairs only the markers the picker returns.
- **The picker:** one `StreamingMarkerDecoder` per channel; each channel's decoded markers kept for
  the last `CB_CHANNEL_PICK_WINDOW_S` (25 s, the #1324 calibration window = the shell
  `marker_decodability_default_probe_secs`); each channel's cluster recomputed only when its window
  changes (a new marker, a window eviction, or the cap); `cb_pick_marker_channel` re-applied after
  every push; only the chosen channel's NEW markers returned. A mono input is the single decoder
  exactly.
- **One physical marker is paired once:** on a pick switch the newly chosen channel's copy of the
  marker the picker returned last (same index, within one dedup gap = 1085 samples, wider than the
  488-sample L/R skew) is dropped. Without it (review round 1) the real 2 s fixture returned markers
  155 and 185 twice, 10.17 ms apart, both into the offset cluster. A different index inside the gap
  is still returned.
- **The history is capped:** each channel keeps at most `CB_CHANNEL_PICK_MAX_MARKERS` (256, the
  newest). The cluster is O(n²) on the dock's audio decode worker (the OBS audio thread until issue
  1381); a decode flood (one marker per dedup gap) measured ~1.4 ms per recompute uncapped. A real
  chain has ~8 markers in 25 s.
- **Consequences to expect:** a marker a channel decodes while another channel is chosen feeds
  nothing, so when the marker rides only on R its first two markers are not paired (a chain needs
  three). A channel that stops decoding hands over once its markers age out of the window. The
  offset cluster is NOT reset on a hand-over; the ~10 ms step between L and R walks through the
  180 s offset window. A switch is therefore logged when it happens (`cb_note_channel_switch`):
  `av-sync-dock: marker channel A -> B (channel_clusters=..., N switch(es) since the last line)`,
  the first at once, then at most one line per diag interval; `channel_switches=` on the diag line
  is the running total, so an L/R flip-flop near the floor shows without flooding the log. The
  decision is the pure mirrored seam `ChannelSwitchLog` / `CbChannelSwitchLog` (round 3 moved it
  out of the glue, where three rate-limit mutants passed every token anchor). A suppressed switch
  is only named by the NEXT switch line, so after a burst the last switch line can be stale; the
  diag line's `marker_channel=` shows the truth within ~10 s. A channel-layout change rebuilds the
  picker at channel 0 and is not counted as a switch.
- **Diag line:** `preambles/crc_ok/crc_fail` are now SUMMED over the channels (monotonic, so the
  staleness detector and the pairing watchdog keep working; a mono input reads its one decoder);
  `marker_channel=<c> channel_clusters=<a,b> channel_switches=<n>` are appended after
  `publish_max_us` (0-based channel).
  A channel-count change restarts the decode and `cb_audio_pushed`. `reset_window()` (the pairing
  recovery) resets every decoder but keeps the histories and the pick.
- **The Rust reference** is `src/av_sync_dock_channels.rs` (`ChannelMarkerPicker::dock`).
  `tests/qpsk_channel_pick_parity_1367.rs` compiles `vendor/av-sync-dock/test/channel-pick-parity.cpp`
  with g++ and compares it with the Rust: the pick over the shared table
  `tests/fixtures/qpsk_channel_pick_parity.tsv`, the cluster over 400 generated + crafted (even-count
  median, long equal-timestamp groups) + the real fixture's decodes, and whole streaming transcripts
  push by push (real fixture, both clear, R only, hand-over, three channels, mono, the switch with
  and without a same-index copy, a decode flood past the cap, the window boundary via the tool's
  optional `<window_samples>` argument, and the dead-pairing `reset_window()` via its optional
  `<reset_after>`), plus the switch log over a generated pick sequence (the tool's `switchlog`
  command). Eighteen C++ mutations (floor `>`, tie to highest, lower median, unstable sort, no
  missed-marker branch, return channel 0, evict `<=`, window 20, stats of one channel, last modal
  step, no switch dedup, dedup ignoring the index, dedup boundary `<`, cap off by one, a no-op reset,
  and in the switch log: the suppressed count not kept, the rate limit flipped, the last log time
  not stored) and seven Rust-reference mutations (no-op reset, boundary `<`, no dedup, no cap, the
  three switch-log ones) are all caught. A mutation that BOTH mirrors share is invisible to parity: the round-1 double return was
  one, and in round 2 a no-op `reset_window()` survived both languages until the reset got its own
  tests. So every picker behaviour (the pick, the window, the cap, the switch dedup and its
  boundary, the reset) also has its own Rust unit test. Each parity test compiles the tool into its
  own temp dir and removes it on drop.
- **The glue:** `st_raw_audio_camera_box` calls `cb_ensure_audio_picker` (one picker per channel
  layout), `cb_note_channel_switch` and `cb_audio_diag_tick` (the ~10 s staleness / diag line /
  pairing recovery, split out in review round 1). The callback is ~298 lines: the next addition to
  it must split it further first.
- **The glue anchors:** `tests/av_sync_dock_channel_pick_1367.rs` (with the shared
  `tests/support/cpp_source.rs` helpers, also used by `av_sync_dock_decode_mailbox_1367.rs`) + the
  pwsh step "Assert dock decodes the marker per channel (issue 1367)" in BOTH windows-genlock
  workflows, which checks the same list (including the two ADJACENCY needles that pin the
  `prev_channel` capture before the push and the switch note right after it), slicing the same three
  function bodies (the callback, `cb_ensure_audio_picker`, `cb_note_channel_switch`) with a
  brace-matching `Get-DockBody` helper (issue 1386: shared, in `vendor/av-sync-dock/test/dock-output-source.ps1`), so a slice never runs into the next function's comment. Replay the pwsh
  checks from the YAML text itself before pushing (a python `re.sub(r"\s+", " ", …)` + the literal
  lists), since pwsh only runs on the Windows runner. The pwsh slices keep comments, so a comment
  inside `st_raw_audio_camera_box` must never spell `acc +=`, `/ (float)ch`,
  `std::vector<float> mono`, `mono.data()` or `camerabox::StreamingMarkerDecoder(`.
- **Tier-0 verify:** the plain-rustc replica above (with the new test files included; add
  `-A clippy::duplicate_mod` to its clippy-driver run, since two test files there share one
  `#[path]` module — in the real layout each test file is its own crate), each std-only anchor test
  also through `clippy-driver --test -D warnings` as its own crate, `g++ -std=c++11 -Wall -Wextra
  -Werror` on both test programs, and `-fsyntax-only` of each dock output TU (`sync-test-output{,-video,-audio}.cpp`) with the two
  stub headers (`.claude/rules/av-sync-dock-decode-worker.md`). The dock change is live only after a
  FULL-bundle Windows deploy (`.claude/rules/rig-state-inspection.md`).
