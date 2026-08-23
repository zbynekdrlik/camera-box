---
paths:
  - "src/av_sync_dock.rs"
  - "vendor/av-sync-dock/src/camera-box-audio.hpp"
  - "tests/av_sync_dock_latency_display_*.rs"
  - "tests/av_sync_dock_cpp_mirror_gate.rs"
---

# A/V-sync dock display: Rust↔C++ parity seam, which box it locks on, residual pairing

## The dock's display math is a Rust↔C++ PARITY-MIRRORED VALUE seam — never change the value one-sided
`dock_lock_display_offset_ms` / `dock_latency_display_ms` in `src/av_sync_dock.rs` are byte-for-byte
mirrored by `cb_dock_lock_display_offset_ms` / `cb_dock_latency_display_ms` in
`vendor/av-sync-dock/src/camera-box-audio.hpp` (the deployed dock is C++; it can't be rig-verified in
CI). `tests/av_sync_dock_cpp_mirror_gate.rs` compiles the header standalone with
`g++ -std=c++11 -Wall -Wextra -Werror` and runs `vendor/av-sync-dock/test/camera-box-selftest.cpp`,
which cross-checks the C++ VALUES against the Rust results.
- Changing a computed VALUE (e.g. adding an additive term to the negation) requires updating BOTH
  files in lockstep AND keeping the self-test green — plus it can trip the vendored-libobs
  build (`.claude/rules/vendored-libobs-change-safety.md`). High-risk.
- **Doc-comment / new-`const` / `constexpr` + `static_assert` changes are SAFE** — the self-test
  compares values only (a comment can't break `-Werror`; an unused `pub const` / `static_assert`-
  referenced `constexpr` won't be flagged unused). Locally: `cargo test --test
  av_sync_dock_cpp_mirror_gate # airuleset:build-ok` (needs g++; ~12–80s depending on box load).
- Gotcha: making the additive constant load-bearing (`-x + ADDITIVE_MS`) is NOT free — for `x==0`
  it flips the result from IEEE `-0.0` to `+0.0`, interacting with `dock_latency_display_ms`'s own
  `-0.0→0.0` normalization. Keep such a constant DECORATIVE + guarded by a test unless a
  measurement genuinely needs it applied.

## The dock locks on the STREAM box, NOT strih (#1004)
The `av-sync-dock` cluster estimator needs the `mbc` marker source, which lives on the **stream**
OBS (cam2 QPSK marker audio). Read its live values from the stream box OBS log:
`Select-String '$env:APPDATA\obs-studio\logs\<latest>.txt' -Pattern 'av-sync-dock: (LOCKED|UPDATED) offset='`.
The **strih** box logs `av-sync-dock: ASRC section unavailable -- source 'mbc' not found on this box`
and `locked=no` — it never locks in normal operation, so don't hunt for dock LOCKED lines there.
Use the win-stream-snv / win-strih MCP (agent session), not ssh, for these logs.

## Quantifying dock-vs-gate residual (#952/#1004)
The `LOCKED/UPDATED offset=` value is ALREADY #953 sign-corrected (gate convention). Pair it against
the offline OPTICAL truth `all_cambox_av_sync.<camN>.av_offset_ms` in the verdict JSON
(`/tmp/recording-e2e-*/verdict-*.json`) for the SAME recording window (segments' `start_ns` →
wallclock). #1004 result: residual is UNSTABLE (dock's own within-window swing 24–75ms, cluster mad
25–35ms, exceeds the +9..+53ms run-to-run residual spread) — #952's ~55ms is NOT a stable constant.
Decision locked in `DOCK_LOCK_DISPLAY_ADDITIVE_MS = 0.0`: the dock is a coarse MONITOR, the offline
optical `--av-sync` chain is the sole authoritative gate. Never compensate with a guessed constant.

## Live-dock cluster CONSTANTS can't be recalibrated from an offline gate-run audit (#735)
`DOCK_CLUSTER_TOL_MS` / `DOCK_CLUSTER_MAX_MAD_MS` / `DOCK_CLUSTER_MIN_MATCHED` are the live dock's
own densest-cluster window/honesty gates. Do NOT port an offline `recording-verdict --av-sync`
audit value onto them (issue 735 evaluated exactly that — tightening `DOCK_CLUSTER_TOL_MS` 60→25 to
match issue 733's offline default — and rejected it). The live continuous-rolling dock is
intrinsically much noisier than a clean offline gate-run decode: mining 1381 live `LOCKED/UPDATED
offset=... matched=... mad=` estimates off the running STREAM-box dock showed `mad_ms` median ~29ms
(min 12.9, bulk 25–40, 77% > the 25ms entry ceiling, held only by the #999 hold multiplier) with
the offset CENTER wobbling >150ms — vs the tight 7–9ms `mad` of the 3 clean runs issue 733's 25ms
was calibrated on. So issue 733's "wide window blends a nearby sub-cluster → tighten to reveal a 7–9ms
core" pattern does NOT reproduce live (no hidden tight core over 1381 samples), and tightening the
window below the cluster's natural width would only drop `matched` toward the `MIN_MATCHED=8` floor
and lock LESS reliably — worse for a MONITOR that gates nothing.
**How to characterize it:** the dock logs NO raw candidate stream — the post-cluster `LOCKED/UPDATED
offset=... matched=... mad=` lines ARE the characterization; mine their distribution (agent-session
win-stream-snv MCP, not ssh) from `$env:APPDATA\obs-studio\logs\<latest>.txt`. Any live-dock-constant
change is a VALUE seam mirrored byte-for-byte in `camera-box-audio.hpp` (`CB_CLUSTER_TOL_MS` etc.) +
the ~150min genlock vendored-OBS build — high cost for a monitor whose precision gates nothing.

## The dock's lock/offset display is DECODED-MARKER-driven — an "input gone" state needs a SEPARATE counter-advance detector (#1177)
`cb_lock_state` (the diag `locked=`), the dock's `statusLabel`, and `latencyDisplay` are updated
ONLY inside `st_raw_audio_camera_box`'s per-marker loop, via `cb_lock_audit.push(est)` transitions —
so EVERY unlock / no-signal path is driven by a DECODED audio marker. When the measurement INPUT
itself disappears (EVENT mode: cam2 QPSK marker + dual-QR off), `markers` is empty on every audio
callback, the loop body never runs, no `Unlocked`/`signal_lock_state_changed(false)` ever fires, and
the last locked offset (+ `locked=yes`) is held for hours — an operator reads a frozen number as a
live A/V-sync measurement. **So any "the instrument is blind" condition CANNOT be surfaced from a
marker-driven path — it needs a detector that watches whether the decode COUNTERS advance, not one
that waits for a decoded marker (which by definition can't arrive when the input is what went away).**
`DockInputStaleness` (Rust) / `CbDockInputStaleness` (`camera-box-audio.hpp`) is that seam: fed the
#690 diag heartbeat's `video_decoded` + `crc_ok` once per ~10s diag tick (the audio thread keeps
ticking in EVENT mode even though the counters do not), it flips STALE after `CB_DOCK_INPUT_STALE_NS`
(30s) of no advance and recovers on any advance. It surfaces via the diag line's `state=LIVE|STALE`
token, a one-shot `av-sync-dock: measurement input LOST -> STALE` blog() line, the NEW
`sync_stale_changed(bool)` OBS signal (distinct from `lock_state_changed`, which is marker-driven),
and the dock's `Status.Stale` + greyed/labelled `latencyDisplay`. It is a parity-mirrored seam like
the rest of this file — move both sides + the `camera-box-selftest.cpp` CHECK in lockstep. Purely a
display-layer classifier; it never touches the demod, the cluster, or the gate.
