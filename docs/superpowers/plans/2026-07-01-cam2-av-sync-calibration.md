# cam2 QR-synced audio → automated A/V-sync calibration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the manual genlock video-delay nudge with a one-shot calibration that MEASURES the true video↔audio offset from a stream-OBS recording and AUTO-SETS the genlock video-delay to zero it.

**Architecture:** cam2's painter emits a QR-frame-aligned audio marker (a short chirp) on cadence via cam2 USB audio, logging marker↔frame_id. The operator captures it through the real hand-mic → mastering (~1 s) → Dante → stream-OBS audio mix, so it lands in the stream-OBS recording alongside the QR video. `recording-verdict --av-sync` decodes the QR ticks (video positions) + detects the chirps in the audio track (audio positions) and computes `offset = video_time − audio_time`. A controller sets `genlock_latency_ms_src` on the stream 'NDI 2ME PGM' source via the OBS WebSocket (snapshot/verify safety) so the offset → 0. A #188 OBS dock surfaces the last-measured offset + live genlock latency.

**Tech Stack:** Rust (pure Tier-0 modules at crate root + `probe`-gated glue), ALSA (`alsa` crate, already used by `src/intercom.rs`), ffmpeg/ffprobe (subprocess, same style as `src/probe/colour_sample.rs`), Python + obs-websocket (`scripts/obs_phase2.py` pattern), C++ (DistroAV OBS plugin, `vendor/distroav/`).

## Global Constraints

- **Version bump FIRST** — bump `Cargo.toml` version on `dev` before any code (currently dev.182; the next commit bumps it).
- **Tier-0 local checks only** — `cargo fmt --all --check`, `cargo check`, `cargo clippy --all-targets -- -D warnings` (NO `--all-features`), `cargo test --no-run`. NEVER compile `--features probe` / `--all-features` locally (balloons the shared dev1 `target/`, #185).
- **Pure logic at the crate root, probe I/O gated.** New pure modules (`src/av_sync.rs`) are declared UNCONDITIONALLY in `src/lib.rs` (no feature gate, no OS gate) and unit-tested on default features. Device/ffmpeg glue lives in `src/probe/…` behind `#[cfg(feature = "probe")]` (+ `#[cfg(target_os = "linux")]` for ALSA) and is compile-checked + built ON CI only.
- **Observe RED→GREEN on the pure tests via the one-off bypass:** `cargo test --lib av_sync # airuleset:build-ok` (the Tier-0 hook blocks any `cargo test` that RUNS otherwise).
- **Strict-test mandate — NEVER weaken a gate to force a pass.** The A/V offset estimator must FAIL (return `None`) on too-few / inconsistent markers rather than emit a bad delay. Tolerances are real signals; calibrate to rig physics, never loosen to pass.
- **Latency is the user's A/V-align domain** — this automation SETS the video-delay to the MEASURED value (up OR down); it NEVER "reduces latency" as an optimization. The measured offset is the only driver.
- **Genlock latency clamp:** `genlock_latency_ms_src` ∈ [3, 2000] ms (DistroAV `PROP_GENLOCK_LATENCY_MS_MIN`=3, `PROP_GENLOCK_SOURCE_LATENCY_MS_MAX`=2000). The controller clamps to this range.
- **Chirp template is ONE source of truth** — `av_sync::generate_chirp(...)` is used BOTH by the painter (to emit) and the verdict (as the cross-correlation template). Never duplicate it.
- **Marker emission is TEST-mode only** (audible) — the painter `--audio-marker` flag defaults OFF; `rig-mode.sh test` enables it; EVENT mode never emits it.

---

## File Structure

| File | Responsibility | Tier |
|---|---|---|
| `src/av_sync.rs` (create) | Pure: chirp template gen, cross-correlation onset detection, offset estimator (vote-align + median + MAD guard), controller delay math (sign + clamp), marker-log serialize/parse | Tier-0, crate root |
| `src/lib.rs` (modify) | Declare `pub mod av_sync;` unconditionally | — |
| `src/probe/av_sync_extract.rs` (create) | Probe glue: ffmpeg audio-track → `Vec<f32>` (input-seek style) | CI-only (`probe`) |
| `src/probe/audio_marker_io.rs` (create) | Probe glue: ALSA playback on cam2 USB audio, emit chirp on cadence, log (frame_id, wall_ts) | CI-only (`probe` + linux) |
| `src/probe/mod.rs` (modify) | Declare the two new probe submodules | — |
| `src/probe/painter.rs` (modify) | Publish current `logical_id` to a shared `AtomicU32`; accept an audio-marker handle | CI-only |
| `src/probe/run.rs` (modify) | `run_paint_only`: spawn the audio-marker thread when enabled; serialize the marker log | CI-only |
| `src/bin/recording-verdict.rs` (modify) | `--av-sync` + `--av-sync-marker-log` flags; wire extraction + pure estimator; emit offset JSON | CI-only |
| `src/bin/frame-probe.rs` (modify) | `--audio-marker` + `--audio-marker-device` + `--marker-log` flags | CI-only |
| `scripts/av_sync_calibrate.py` (create) | Controller: read offset JSON + current latency → compute new delay → set via WS (snapshot/verify) → optionally update drift-guard pin | script |
| `scripts/test_av_sync_calibrate.py` (create) | pytest: locks the controller delay math sign + clamp | script test |
| `scripts/av-sync-calibrate.sh` (create) | Supervisor orchestration: TEST-mode record → verdict → set → re-measure | script |
| `tests/av_sync_extract.rs` (create) | CI content/structure test for the extraction glue | CI test |
| `tests/harness_av_sync.rs` (create) | CI content-assert: recording-verdict `--av-sync` wiring + script content | CI test |
| `vendor/distroav/src/av-sync-dock.hpp` / `.cpp` (create) | #188 net-new `QDockWidget`: last A/V offset + live genlock latency | CI/rig build |
| `vendor/distroav/src/plugin-main.cpp` (modify) | Register the dock via `obs_frontend_add_dock_by_id` | CI/rig build |

---

## Task 1: Pure chirp template + cross-correlation onset detector

**Files:**
- Create: `src/av_sync.rs`
- Modify: `src/lib.rs` (add `pub mod av_sync;`)
- Test: inline `#[cfg(test)] mod tests` in `src/av_sync.rs`

**Interfaces:**
- Produces:
  - `pub fn generate_chirp(sample_rate: u32, dur_ms: u32, f0_hz: f32, f1_hz: f32) -> Vec<f32>` — normalized (peak ±1.0) linear sweep, Hann-windowed (survives a mastering limiter far better than a bare click).
  - `pub fn detect_chirp_onsets(samples: &[f32], template: &[f32], threshold: f32, min_spacing: usize) -> Vec<usize>` — normalized cross-correlation; returns sample indices of peaks ≥ `threshold` (normalized 0..1), each ≥ `min_spacing` apart, index = START of the matched template.

- [ ] **Step 1: Declare the module**

In `src/lib.rs`, in the pure-Tier-0 block (near `pub mod colour_scale;`, ~line 37), add:

```rust
pub mod av_sync; // #188/#145 A/V-sync calibration: pure chirp gen + offset estimate + controller math
```

- [ ] **Step 2: Write the failing tests**

Create `src/av_sync.rs`:

```rust
//! Pure Tier-0 logic for the cam2 QR-synced audio → A/V-sync calibration (#188/#145).
//!
//! One source of truth for the chirp template (used BOTH by the painter to EMIT and by
//! recording-verdict to DETECT), the cross-correlation onset detector, the offset estimator,
//! and the controller delay math. No I/O, no probe deps — unit-tested on default features.

/// Generate a Hann-windowed linear chirp, peak-normalized to ±1.0.
/// Used as the emitted marker AND the cross-correlation template — one source of truth.
pub fn generate_chirp(sample_rate: u32, dur_ms: u32, f0_hz: f32, f1_hz: f32) -> Vec<f32> {
    unimplemented!()
}

/// Normalized cross-correlation onset detector.
/// Returns the start-sample index of each template match whose normalized correlation
/// (0..=1) is >= `threshold`, keeping peaks at least `min_spacing` samples apart
/// (highest-correlation-wins within a spacing window).
pub fn detect_chirp_onsets(
    samples: &[f32],
    template: &[f32],
    threshold: f32,
    min_spacing: usize,
) -> Vec<usize> {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chirp_is_peak_normalized_and_correct_length() {
        let c = generate_chirp(48_000, 50, 1000.0, 3000.0);
        assert_eq!(c.len(), 48_000 * 50 / 1000); // 2400 samples
        let peak = c.iter().cloned().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!((peak - 1.0).abs() < 1e-3, "peak {peak} not ~1.0");
        // Hann window → ends taper to ~0
        assert!(c[0].abs() < 0.05 && c[c.len() - 1].abs() < 0.05);
    }

    #[test]
    fn detects_two_chirps_at_known_positions() {
        let sr = 48_000;
        let template = generate_chirp(sr, 50, 1000.0, 3000.0);
        let mut buf = vec![0.0f32; sr as usize * 3]; // 3 s of silence
        let p1 = 20_000usize;
        let p2 = 150_000usize;
        for (i, &s) in template.iter().enumerate() {
            buf[p1 + i] += s * 0.6;
            buf[p2 + i] += s * 0.6;
        }
        let onsets = detect_chirp_onsets(&buf, &template, 0.5, template.len());
        assert_eq!(onsets.len(), 2);
        assert!((onsets[0] as i64 - p1 as i64).abs() <= 5);
        assert!((onsets[1] as i64 - p2 as i64).abs() <= 5);
    }

    #[test]
    fn survives_noise_and_gain() {
        let sr = 48_000;
        let template = generate_chirp(sr, 50, 1000.0, 3000.0);
        let mut buf = vec![0.0f32; sr as usize * 2];
        let p = 44_100usize;
        // low-amplitude marker + additive pseudo-noise (deterministic, no rng)
        for (i, &s) in template.iter().enumerate() {
            let noise = (((i as f32) * 12.9898).sin() * 43758.545).fract() * 0.1 - 0.05;
            buf[p + i] += s * 0.3 + noise;
        }
        let onsets = detect_chirp_onsets(&buf, &template, 0.4, template.len());
        assert_eq!(onsets.len(), 1);
        assert!((onsets[0] as i64 - p as i64).abs() <= 10);
    }

    #[test]
    fn silence_yields_no_onsets() {
        let template = generate_chirp(48_000, 50, 1000.0, 3000.0);
        let buf = vec![0.0f32; 48_000];
        assert!(detect_chirp_onsets(&buf, &template, 0.4, template.len()).is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib av_sync::tests # airuleset:build-ok`
Expected: FAIL (`unimplemented!()` panics).

- [ ] **Step 4: Implement**

Replace the two `unimplemented!()` bodies:

```rust
pub fn generate_chirp(sample_rate: u32, dur_ms: u32, f0_hz: f32, f1_hz: f32) -> Vec<f32> {
    let n = (sample_rate as u64 * dur_ms as u64 / 1000) as usize;
    if n == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f32;
    let dur_s = n as f32 / sr;
    let k = (f1_hz - f0_hz) / dur_s; // sweep rate Hz/s
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / sr;
        // instantaneous phase of a linear sweep
        let phase = 2.0 * std::f32::consts::PI * (f0_hz * t + 0.5 * k * t * t);
        // Hann window
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos();
        out.push(phase.sin() * w);
    }
    // peak-normalize to ±1.0
    let peak = out.iter().cloned().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 0.0 {
        for s in &mut out {
            *s /= peak;
        }
    }
    out
}

pub fn detect_chirp_onsets(
    samples: &[f32],
    template: &[f32],
    threshold: f32,
    min_spacing: usize,
) -> Vec<usize> {
    let m = template.len();
    if m == 0 || samples.len() < m {
        return Vec::new();
    }
    let t_energy: f32 = template.iter().map(|x| x * x).sum::<f32>().sqrt();
    if t_energy == 0.0 {
        return Vec::new();
    }
    // normalized cross-correlation at each lag
    let last = samples.len() - m;
    let mut corr = vec![0.0f32; last + 1];
    for lag in 0..=last {
        let win = &samples[lag..lag + m];
        let mut dot = 0.0f32;
        let mut w_energy = 0.0f32;
        for i in 0..m {
            dot += win[i] * template[i];
            w_energy += win[i] * win[i];
        }
        let denom = t_energy * w_energy.sqrt();
        corr[lag] = if denom > 0.0 { (dot / denom).abs() } else { 0.0 };
    }
    // greedy peak pick: strongest first, suppress neighbours within min_spacing
    let mut idx: Vec<usize> = (0..corr.len()).filter(|&i| corr[i] >= threshold).collect();
    idx.sort_by(|&a, &b| corr[b].partial_cmp(&corr[a]).unwrap());
    let mut picked: Vec<usize> = Vec::new();
    for cand in idx {
        if picked.iter().all(|&p| (p as i64 - cand as i64).abs() as usize >= min_spacing) {
            picked.push(cand);
        }
    }
    picked.sort_unstable();
    picked
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib av_sync::tests # airuleset:build-ok`
Expected: PASS (4 tests).

- [ ] **Step 6: Tier-0 gate + commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/av_sync.rs src/lib.rs
git commit -m "feat: #188 pure chirp template + cross-correlation onset detector (av_sync)"
```

---

## Task 2: Pure A/V offset estimator + controller delay math

**Files:**
- Modify: `src/av_sync.rs`
- Test: inline tests in `src/av_sync.rs`

**Interfaces:**
- Consumes: nothing new (same file as Task 1).
- Produces:
  - `pub struct EmittedMarker { pub frame_id: u32, pub video_time_s: f64 }` — a marker's video position in the recording (from the QR-tick → frame_index lookup).
  - `pub struct AvOffsetEstimate { pub offset_ms: f64, pub matched: usize, pub mad_ms: f64 }`
  - `pub struct OffsetSearch { pub min_ms: f64, pub max_ms: f64, pub step_ms: f64, pub tol_ms: f64, pub min_matches: usize, pub max_mad_ms: f64 }` with `pub fn default_search() -> OffsetSearch`.
  - `pub fn estimate_av_offset_ms(emitted: &[EmittedMarker], onsets_s: &[f64], search: &OffsetSearch) -> Option<AvOffsetEstimate>` — 1-D vote: for each candidate D, count emitted markers whose `video_time_s − D` lands within `tol_ms` of some onset; pick max-vote D, refine with the median of matched residuals; return `None` if `matched < min_matches` or `mad_ms > max_mad_ms`. (Robust to missed/extra onsets — no reliance on index correspondence.)
  - `pub fn required_delay_ms(current_delay_ms: i32, offset_ms: f64) -> i32` — `clamp(round(current − offset), 3, 2000)`. `offset = video_time − audio_time`; positive = video lags audio → REDUCE video delay.

- [ ] **Step 1: Write the failing tests**

Append to `src/av_sync.rs` (above the existing `#[cfg(test)]` or in it):

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmittedMarker {
    pub frame_id: u32,
    pub video_time_s: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvOffsetEstimate {
    pub offset_ms: f64,
    pub matched: usize,
    pub mad_ms: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct OffsetSearch {
    pub min_ms: f64,
    pub max_ms: f64,
    pub step_ms: f64,
    pub tol_ms: f64,
    pub min_matches: usize,
    pub max_mad_ms: f64,
}

pub fn default_search() -> OffsetSearch {
    OffsetSearch {
        min_ms: -500.0,
        max_ms: 2500.0,
        step_ms: 5.0,
        tol_ms: 60.0,
        min_matches: 4,
        max_mad_ms: 40.0,
    }
}
```

Add these tests inside `mod tests`:

```rust
    fn markers(times: &[f64]) -> Vec<EmittedMarker> {
        times
            .iter()
            .enumerate()
            .map(|(i, &t)| EmittedMarker { frame_id: i as u32, video_time_s: t })
            .collect()
    }

    #[test]
    fn recovers_constant_offset() {
        // video at 1,5,9,13,17 s; audio 300 ms EARLIER (video lags by +300 ms)
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        let onsets: Vec<f64> = em.iter().map(|m| m.video_time_s - 0.300).collect();
        let est = estimate_av_offset_ms(&em, &onsets, &default_search()).unwrap();
        assert!((est.offset_ms - 300.0).abs() <= 5.0, "offset {}", est.offset_ms);
        assert_eq!(est.matched, 5);
        assert!(est.mad_ms <= 5.0);
    }

    #[test]
    fn tolerates_one_missed_onset_and_one_spurious() {
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        let mut onsets: Vec<f64> = em.iter().map(|m| m.video_time_s - 0.300).collect();
        onsets.remove(2); // a missed marker
        onsets.push(3.7); // a spurious onset
        let est = estimate_av_offset_ms(&em, &onsets, &default_search()).unwrap();
        assert!((est.offset_ms - 300.0).abs() <= 10.0);
        assert!(est.matched >= 4);
    }

    #[test]
    fn too_few_matches_returns_none() {
        let em = markers(&[1.0, 5.0]);
        let onsets = vec![0.7, 4.7];
        assert!(estimate_av_offset_ms(&em, &onsets, &default_search()).is_none());
    }

    #[test]
    fn inconsistent_markers_return_none() {
        let em = markers(&[1.0, 5.0, 9.0, 13.0, 17.0]);
        // onsets scattered — no constant offset aligns >= min_matches within tol
        let onsets = vec![0.1, 4.9, 8.2, 13.9, 15.5];
        assert!(estimate_av_offset_ms(&em, &onsets, &default_search()).is_none());
    }

    #[test]
    fn required_delay_sign_and_clamp() {
        // video lags audio (+120 ms) → reduce delay
        assert_eq!(required_delay_ms(1000, 120.0), 880);
        // video leads audio (-120 ms) → increase delay
        assert_eq!(required_delay_ms(1000, -120.0), 1120);
        // clamp low / high
        assert_eq!(required_delay_ms(1000, 5000.0), 3);
        assert_eq!(required_delay_ms(1000, -5000.0), 2000);
        assert_eq!(required_delay_ms(3, 0.0), 3);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib av_sync::tests # airuleset:build-ok`
Expected: FAIL (`estimate_av_offset_ms` / `required_delay_ms` not defined).

- [ ] **Step 3: Implement**

Add to `src/av_sync.rs`:

```rust
pub fn estimate_av_offset_ms(
    emitted: &[EmittedMarker],
    onsets_s: &[f64],
    search: &OffsetSearch,
) -> Option<AvOffsetEstimate> {
    if emitted.is_empty() || onsets_s.is_empty() {
        return None;
    }
    let tol_s = search.tol_ms / 1000.0;
    let mut best_d_ms = search.min_ms;
    let mut best_votes = 0usize;

    let steps = ((search.max_ms - search.min_ms) / search.step_ms).round().max(0.0) as i64;
    for k in 0..=steps {
        let d_ms = search.min_ms + k as f64 * search.step_ms;
        let d_s = d_ms / 1000.0;
        let mut votes = 0usize;
        for m in emitted {
            let expected_audio = m.video_time_s - d_s;
            if onsets_s.iter().any(|&o| (o - expected_audio).abs() <= tol_s) {
                votes += 1;
            }
        }
        if votes > best_votes {
            best_votes = votes;
            best_d_ms = d_ms;
        }
    }
    if best_votes < search.min_matches {
        return None;
    }

    // refine: median of per-marker residuals at the winning D, nearest-onset matched
    let best_d_s = best_d_ms / 1000.0;
    let mut residuals_ms: Vec<f64> = Vec::new();
    for m in emitted {
        let expected_audio = m.video_time_s - best_d_s;
        if let Some(&nearest) = onsets_s
            .iter()
            .min_by(|a, b| {
                (**a - expected_audio)
                    .abs()
                    .partial_cmp(&(**b - expected_audio).abs())
                    .unwrap()
            })
        {
            if (nearest - expected_audio).abs() <= tol_s {
                // actual offset for this marker = video - audio
                residuals_ms.push((m.video_time_s - nearest) * 1000.0);
            }
        }
    }
    if residuals_ms.len() < search.min_matches {
        return None;
    }
    let offset_ms = median(&mut residuals_ms.clone());
    let mut abs_dev: Vec<f64> = residuals_ms.iter().map(|r| (r - offset_ms).abs()).collect();
    let mad_ms = median(&mut abs_dev);
    if mad_ms > search.max_mad_ms {
        return None;
    }
    Some(AvOffsetEstimate { offset_ms, matched: residuals_ms.len(), mad_ms })
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Required genlock video-delay to zero the measured offset.
/// `offset_ms = video_time - audio_time`; positive = video lags audio → REDUCE the delay.
pub fn required_delay_ms(current_delay_ms: i32, offset_ms: f64) -> i32 {
    let raw = (current_delay_ms as f64 - offset_ms).round() as i32;
    raw.clamp(3, 2000)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib av_sync # airuleset:build-ok`
Expected: PASS (all Task 1 + Task 2 tests).

- [ ] **Step 5: Tier-0 gate + commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/av_sync.rs
git commit -m "feat: #188 pure A/V offset estimator (vote-align) + controller delay math"
```

---

## Task 3: Probe-gated ffmpeg audio-track extraction

**Files:**
- Create: `src/probe/av_sync_extract.rs`
- Modify: `src/probe/mod.rs` (declare the module)
- Test: `tests/av_sync_extract.rs` (CI content/structure test — the ffmpeg subprocess is exercised on the rig, not Tier-0)

**Interfaces:**
- Consumes: `crate::av_sync` (pure).
- Produces:
  - `pub fn extract_audio_mono_f32(path: &Path, sample_rate: u32) -> Result<Vec<f32>>` — decode the recording's first audio stream to mono f32le at `sample_rate` via ffmpeg, returning the full PCM buffer.
  - `pub fn measure_av_offset(recording: &Path, emitted: &[crate::av_sync::EmittedMarker], sample_rate: u32, chirp: &ChirpParams, search: &crate::av_sync::OffsetSearch) -> Result<Option<crate::av_sync::AvOffsetEstimate>>` — extract audio → detect onsets (`av_sync::detect_chirp_onsets`) → convert to seconds → `av_sync::estimate_av_offset_ms`.
  - `pub struct ChirpParams { pub dur_ms: u32, pub f0_hz: f32, pub f1_hz: f32, pub threshold: f32 }` with `pub fn default_chirp() -> ChirpParams` (dur 50, 1000→3000 Hz, threshold 0.4).

- [ ] **Step 1: Declare the module**

In `src/probe/mod.rs`, in the cross-platform probe block (near `pub mod colour_sample;`):

```rust
pub mod av_sync_extract; // #188 ffmpeg audio extraction + A/V offset measurement glue
```

- [ ] **Step 2: Write the extraction glue**

Create `src/probe/av_sync_extract.rs`:

```rust
//! Probe-gated glue for A/V-sync measurement (#188): ffmpeg audio-track extraction +
//! onset detection wired to the pure `crate::av_sync` estimator. CI-only (feature = "probe").

use crate::av_sync::{
    detect_chirp_onsets, estimate_av_offset_ms, generate_chirp, AvOffsetEstimate, EmittedMarker,
    OffsetSearch,
};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

pub struct ChirpParams {
    pub dur_ms: u32,
    pub f0_hz: f32,
    pub f1_hz: f32,
    pub threshold: f32,
}

pub fn default_chirp() -> ChirpParams {
    ChirpParams { dur_ms: 50, f0_hz: 1000.0, f1_hz: 3000.0, threshold: 0.4 }
}

/// Decode the recording's first audio stream to mono f32le @ `sample_rate` via ffmpeg.
/// Mirrors `colour_sample::decode_one_rgb_frame_at`'s subprocess style (stderr inherited to
/// avoid pipe deadlock; stdout piped and drained).
pub fn extract_audio_mono_f32(path: &Path, sample_rate: u32) -> Result<Vec<f32>> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-nostdin",
            "-i", &path.to_string_lossy(),
            "-map", "0:a:0",
            "-f", "f32le",
            "-ac", "1",
            "-ar", &sample_rate.to_string(),
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("spawn ffmpeg for audio extraction")?;
    let mut raw = Vec::new();
    child
        .stdout
        .take()
        .context("ffmpeg stdout")?
        .read_to_end(&mut raw)
        .context("read ffmpeg audio pcm")?;
    let status = child.wait().context("wait ffmpeg")?;
    if !status.success() {
        anyhow::bail!("ffmpeg audio extraction failed for {}", path.display());
    }
    let mut out = Vec::with_capacity(raw.len() / 4);
    for chunk in raw.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

pub fn measure_av_offset(
    recording: &Path,
    emitted: &[EmittedMarker],
    sample_rate: u32,
    chirp: &ChirpParams,
    search: &OffsetSearch,
) -> Result<Option<AvOffsetEstimate>> {
    let samples = extract_audio_mono_f32(recording, sample_rate)?;
    let template = generate_chirp(sample_rate, chirp.dur_ms, chirp.f0_hz, chirp.f1_hz);
    let onsets_idx = detect_chirp_onsets(&samples, &template, chirp.threshold, template.len());
    let onsets_s: Vec<f64> = onsets_idx.iter().map(|&i| i as f64 / sample_rate as f64).collect();
    Ok(estimate_av_offset_ms(emitted, &onsets_s, search))
}
```

- [ ] **Step 3: Write a CI structure test**

Create `tests/av_sync_extract.rs`:

```rust
//! CI content-assert for the A/V-sync extraction glue (#188). The ffmpeg subprocess itself is
//! exercised on the rig; here we lock the module's shape + the pure integration.
#![cfg(feature = "probe")]

use camera_box::av_sync::{default_search, generate_chirp, EmittedMarker};
use camera_box::probe::av_sync_extract::{default_chirp, measure_av_offset};
use std::path::Path;

#[test]
fn default_chirp_is_audible_band() {
    let c = default_chirp();
    assert!(c.f0_hz >= 500.0 && c.f1_hz <= 6000.0 && c.f1_hz > c.f0_hz);
}

#[test]
fn missing_recording_errors_not_panics() {
    let em = [EmittedMarker { frame_id: 0, video_time_s: 1.0 }];
    let r = measure_av_offset(
        Path::new("/nonexistent/rec.mkv"),
        &em,
        48_000,
        &default_chirp(),
        &default_search(),
    );
    assert!(r.is_err());
}

#[test]
fn template_generation_is_deterministic() {
    let a = generate_chirp(48_000, 50, 1000.0, 3000.0);
    let b = generate_chirp(48_000, 50, 1000.0, 3000.0);
    assert_eq!(a, b);
}
```

- [ ] **Step 4: Verify it compiles on CI**

Run (CI only — do NOT run locally): `cargo test --features probe --test av_sync_extract`
Expected: PASS (compiles under the probe feature; `missing_recording_errors_not_panics` returns `Err`).

Locally, only confirm default-feature compile is unaffected:

Run: `cargo check`
Expected: OK (av_sync_extract is probe-gated, not compiled).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add src/probe/av_sync_extract.rs src/probe/mod.rs tests/av_sync_extract.rs
git commit -m "feat: #188 probe glue — ffmpeg audio extraction + A/V offset measurement"
```

---

## Task 4: Wire A/V-sync into recording-verdict

**Files:**
- Modify: `src/bin/recording-verdict.rs`
- Test: `tests/harness_av_sync.rs` (CI content-assert of the CLI wiring)

**Interfaces:**
- Consumes: `crate::av_sync::EmittedMarker`, `probe::av_sync_extract::{measure_av_offset, default_chirp}`, `av_sync::default_search`; the existing per-frame `tick` → `frame_index` decode already in the verdict.
- Produces: two CLI flags + a JSON block `{ "av_offset_ms": f64, "matched": usize, "mad_ms": f64 }` (or `"av_offset": null` when the estimate fails) written into the verdict JSON and printed to stdout.

- [ ] **Step 1: Add the CLI flags**

In the `Args` struct (`src/bin/recording-verdict.rs`, after `colour_samples`, ~line 232):

```rust
    /// Measure the video↔audio offset (#188): needs --stream + --av-sync-marker-log.
    #[arg(long)]
    av_sync: bool,

    /// Painter marker log (frame_ids where the audio chirp was emitted, in order).
    #[arg(long)]
    av_sync_marker_log: Option<PathBuf>,

    /// Audio sample rate for extraction (Hz).
    #[arg(long, default_value_t = 48_000)]
    av_sync_sample_rate: u32,
```

- [ ] **Step 2: Write the marker-log parse + tick→time helper (pure, add to `src/av_sync.rs`) with a failing test**

The marker log is a CSV `frame_id,emit_wall_ts_ns` (one chirp per line, in emit order). Add to `src/av_sync.rs`:

```rust
/// Parse a painter marker-log CSV (`frame_id,emit_wall_ts_ns` per line, header allowed) into
/// the ordered list of emitted chirp frame_ids.
pub fn parse_marker_log(contents: &str) -> Vec<u32> {
    contents
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with("frame_id") || l.starts_with('#') {
                return None;
            }
            l.split(',').next()?.trim().parse::<u32>().ok()
        })
        .collect()
}

/// Build the EmittedMarker set: for each logged chirp frame_id, look up its recorded
/// frame_index (video position) and convert to seconds via the recording fps.
/// A frame_id with no recorded tick (optical dropout) is skipped.
pub fn emitted_markers_from_ticks(
    marker_frame_ids: &[u32],
    tick_to_frame_index: &std::collections::HashMap<u32, u64>,
    fps: f64,
) -> Vec<EmittedMarker> {
    marker_frame_ids
        .iter()
        .filter_map(|&fid| {
            tick_to_frame_index
                .get(&fid)
                .map(|&idx| EmittedMarker { frame_id: fid, video_time_s: idx as f64 / fps })
        })
        .collect()
}
```

Add tests in `mod tests`:

```rust
    #[test]
    fn parse_marker_log_skips_header_and_blanks() {
        let s = "frame_id,emit_wall_ts_ns\n10,111\n\n# note\n25,222\n";
        assert_eq!(parse_marker_log(s), vec![10, 25]);
    }

    #[test]
    fn emitted_markers_map_ticks_to_time() {
        let mut map = std::collections::HashMap::new();
        map.insert(10u32, 300u64); // frame 300 @ 30 fps = 10.0 s
        map.insert(25u32, 900u64); // frame 900 @ 30 fps = 30.0 s
        let em = emitted_markers_from_ticks(&[10, 25, 99], &map, 30.0);
        assert_eq!(em.len(), 2); // 99 not recorded → skipped
        assert!((em[0].video_time_s - 10.0).abs() < 1e-9);
        assert!((em[1].video_time_s - 30.0).abs() < 1e-9);
    }
```

- [ ] **Step 3: Run the pure tests (fail → implement → pass)**

Run: `cargo test --lib av_sync # airuleset:build-ok`
Expected: FAIL first (functions absent), then PASS after Step 2's code is in.

- [ ] **Step 4: Wire into the verdict main flow**

In `recording-verdict.rs`, after the stream recording is decoded into frames (where `tick` values are available) and only when `args.av_sync`:

```rust
    if args.av_sync {
        let stream_path = args.stream.clone().expect("--av-sync requires --stream");
        let marker_log = args
            .av_sync_marker_log
            .clone()
            .expect("--av-sync requires --av-sync-marker-log");
        let contents = std::fs::read_to_string(&marker_log)?;
        let marker_ids = camera_box::av_sync::parse_marker_log(&contents);
        // tick_to_frame_index: built from the already-decoded stream frames
        let tick_to_frame_index: std::collections::HashMap<u32, u64> = stream_frames
            .iter()
            .filter_map(|f| f.tick.map(|t| (t, f.frame_index)))
            .collect();
        let emitted = camera_box::av_sync::emitted_markers_from_ticks(
            &marker_ids,
            &tick_to_frame_index,
            args.stream_capture_fps,
        );
        let est = camera_box::probe::av_sync_extract::measure_av_offset(
            &stream_path,
            &emitted,
            args.av_sync_sample_rate,
            &camera_box::probe::av_sync_extract::default_chirp(),
            &camera_box::av_sync::default_search(),
        )?;
        match &est {
            Some(e) => println!(
                "AV-SYNC offset_ms={:.1} matched={} mad_ms={:.1}",
                e.offset_ms, e.matched, e.mad_ms
            ),
            None => println!("AV-SYNC offset=UNRESOLVED (too few / inconsistent markers)"),
        }
        // include in the JSON summary object (serde) under key "av_sync"
        av_sync_json = serde_json::json!(est.map(|e| serde_json::json!({
            "offset_ms": e.offset_ms, "matched": e.matched, "mad_ms": e.mad_ms
        })));
    }
```

(Adapt `stream_frames` / the JSON-summary variable names to the actual verdict code; the exact decoded-frame vector is what `analyze_recording_with_burns` already produces for the stream recording.)

- [ ] **Step 5: Write the CI content-assert harness**

Create `tests/harness_av_sync.rs`:

```rust
//! CI content-assert that recording-verdict exposes the #188 A/V-sync flags + honest UNRESOLVED
//! path. The real measurement runs on the rig; here we lock the CLI surface + reporting strings.
use std::fs;

#[test]
fn recording_verdict_exposes_av_sync_flags() {
    let src = fs::read_to_string("src/bin/recording-verdict.rs").unwrap();
    assert!(src.contains("av_sync"), "missing --av-sync flag");
    assert!(src.contains("av_sync_marker_log"), "missing --av-sync-marker-log");
    assert!(src.contains("measure_av_offset"), "verdict must call the measurement glue");
    assert!(src.contains("UNRESOLVED"), "must report honest UNRESOLVED, never a fake 0");
}
```

- [ ] **Step 6: Verify + commit**

Run: `cargo test --lib av_sync # airuleset:build-ok` (pure), `cargo test --test harness_av_sync # airuleset:build-ok` (content-assert), `cargo check`.
Expected: PASS.

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/av_sync.rs src/bin/recording-verdict.rs tests/harness_av_sync.rs
git commit -m "feat: #188 recording-verdict --av-sync — measure video/audio offset from stream recording"
```

---

## Task 5: Painter audio-marker emission (cadence + ALSA thread + wiring)

**Files:**
- Modify: `src/av_sync.rs` (pure cadence + marker-log serialize)
- Create: `src/probe/audio_marker_io.rs` (ALSA emit thread)
- Modify: `src/probe/mod.rs`, `src/probe/painter.rs`, `src/probe/run.rs`, `src/bin/frame-probe.rs`

**Interfaces:**
- Consumes: `av_sync::generate_chirp`, the painter's per-frame `logical_id`.
- Produces:
  - Pure `pub fn should_emit_marker(refresh_tick: u64, cadence_ticks: u64) -> bool` — `cadence_ticks>0 && refresh_tick % cadence_ticks == 0 && refresh_tick>0`.
  - Pure `pub fn serialize_marker_log(entries: &[(u32, i64)]) -> String` — `frame_id,emit_wall_ts_ns` CSV with header (round-trips with `parse_marker_log`).
  - Probe `pub struct AudioMarkerEmitter` with `pub fn spawn(device: String, sample_rate: u32, chirp: ChirpParams, current_id: Arc<AtomicU32>, stop: Arc<AtomicBool>, start: Instant, wall_clock: bool, cadence_ticks: u64, refresh: Arc<AtomicU64>) -> Result<AudioMarkerEmitter>` and `pub fn join(self) -> Vec<(u32, i64)>` (the marker log).
  - Painter publishes `logical_id` + `refresh_tick` into shared atomics each iteration.

- [ ] **Step 1: Pure cadence + marker-log serialize — failing tests**

Add to `src/av_sync.rs` + `mod tests`:

```rust
pub fn should_emit_marker(refresh_tick: u64, cadence_ticks: u64) -> bool {
    cadence_ticks > 0 && refresh_tick > 0 && refresh_tick % cadence_ticks == 0
}

pub fn serialize_marker_log(entries: &[(u32, i64)]) -> String {
    let mut s = String::from("frame_id,emit_wall_ts_ns\n");
    for (fid, ts) in entries {
        s.push_str(&format!("{fid},{ts}\n"));
    }
    s
}
```

Tests:

```rust
    #[test]
    fn cadence_fires_on_multiples_only() {
        assert!(!should_emit_marker(0, 300));
        assert!(!should_emit_marker(299, 300));
        assert!(should_emit_marker(300, 300));
        assert!(should_emit_marker(600, 300));
        assert!(!should_emit_marker(300, 0)); // disabled
    }

    #[test]
    fn marker_log_round_trips() {
        let e = vec![(300u32, 111i64), (600u32, 222i64)];
        let s = serialize_marker_log(&e);
        assert_eq!(parse_marker_log(&s), vec![300u32, 600u32]);
    }
```

Run: `cargo test --lib av_sync # airuleset:build-ok` → fail, implement (above), pass.

- [ ] **Step 2: Painter publishes current id (modify `src/probe/painter.rs`)**

Extend `run_painter` to accept optional shared atomics and update them each iteration:

```rust
// add params: current_id: Option<Arc<AtomicU32>>, refresh_out: Option<Arc<AtomicU64>>
// inside the loop, after computing (logical_id, ...):
if let Some(ref c) = current_id {
    c.store(logical_id, Ordering::Relaxed);
}
if let Some(ref r) = refresh_out {
    r.store(refresh_tick, Ordering::Relaxed);
}
```

(Keep the existing `emitted` push unchanged. The audio thread reads these atomics; it does NOT touch the vblank-locked paint path.)

- [ ] **Step 3: ALSA emit thread (create `src/probe/audio_marker_io.rs`)**

```rust
//! Probe+Linux glue (#188): a dedicated thread that plays the A/V-sync chirp on cam2 USB audio
//! at a fixed cadence, logging (frame_id, wall_ts_ns) per emitted marker. OFF the capture core.
#![cfg(target_os = "linux")]

use crate::av_sync::generate_chirp;
use crate::probe::av_sync_extract::ChirpParams;
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

pub struct AudioMarkerEmitter {
    handle: JoinHandle<()>,
    log: Arc<Mutex<Vec<(u32, i64)>>>,
}

impl AudioMarkerEmitter {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        device: String,
        sample_rate: u32,
        chirp: ChirpParams,
        current_id: Arc<AtomicU32>,
        refresh: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
        start: Instant,
        wall_clock: bool,
        cadence_ticks: u64,
    ) -> Result<AudioMarkerEmitter> {
        let log = Arc::new(Mutex::new(Vec::new()));
        let log_thread = log.clone();
        let handle = std::thread::spawn(move || {
            crate::probe::pin_off_capture_core("audio-marker");
            if let Err(e) = run_emit(
                &device, sample_rate, &chirp, &current_id, &refresh, &stop, start, wall_clock,
                cadence_ticks, &log_thread,
            ) {
                eprintln!("[audio-marker] emit thread error: {e:#}");
            }
        });
        Ok(AudioMarkerEmitter { handle, log })
    }

    pub fn join(self) -> Vec<(u32, i64)> {
        let _ = self.handle.join();
        Arc::try_unwrap(self.log).map(|m| m.into_inner().unwrap()).unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
fn run_emit(
    device: &str,
    sample_rate: u32,
    chirp: &ChirpParams,
    current_id: &AtomicU32,
    refresh: &AtomicU64,
    stop: &AtomicBool,
    start: Instant,
    wall_clock: bool,
    cadence_ticks: u64,
    log: &Mutex<Vec<(u32, i64)>>,
) -> Result<()> {
    let pcm = open_playback(device, sample_rate)?;
    let io = pcm.io_i16()?;
    // pre-render the chirp once, as stereo i16
    let mono = generate_chirp(sample_rate, chirp.dur_ms, chirp.f0_hz, chirp.f1_hz);
    let mut stereo: Vec<i16> = Vec::with_capacity(mono.len() * 2);
    for s in &mono {
        let v = (s * 30_000.0) as i16; // headroom below i16::MAX
        stereo.push(v);
        stereo.push(v);
    }
    let mut last_fired = 0u64;
    while !stop.load(Ordering::Relaxed) {
        let tick = refresh.load(Ordering::Relaxed);
        if crate::av_sync::should_emit_marker(tick, cadence_ticks) && tick != last_fired {
            last_fired = tick;
            let fid = current_id.load(Ordering::Relaxed);
            let ts = crate::probe::painter::clock_ns(start, wall_clock);
            log.lock().unwrap().push((fid, ts));
            if let Err(e) = io.writei(&stereo) {
                let _ = pcm.recover(e.errno() as i32, true);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    Ok(())
}

fn open_playback(device: &str, sample_rate: u32) -> Result<PCM> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("open ALSA playback {device}"))?;
    {
        let hwp = HwParams::any(&pcm)?;
        hwp.set_channels(2)?;
        hwp.set_rate(sample_rate, ValueOr::Nearest)?;
        hwp.set_format(Format::s16())?;
        hwp.set_access(Access::RWInterleaved)?;
        pcm.hw_params(&hwp)?;
    }
    pcm.prepare()?;
    Ok(pcm)
}
```

(If `clock_ns` / `pin_off_capture_core` are not already `pub(crate)`, widen their visibility in the same commit — they exist in `painter.rs` / the probe/affinity path.)

- [ ] **Step 4: Declare the module + wire run_paint_only (modify `src/probe/mod.rs`, `src/probe/run.rs`, `src/bin/frame-probe.rs`)**

`src/probe/mod.rs` (linux-glue block):

```rust
#[cfg(target_os = "linux")]
pub mod audio_marker_io;
```

`src/probe/run.rs` `run_paint_only` — when the config enables the marker: create the shared atomics, pass them to `run_painter`, spawn `AudioMarkerEmitter`, and after the paint duration join it + `serialize_marker_log` to the configured `--marker-log` path.

`src/bin/frame-probe.rs` — add flags:

```rust
    #[arg(long)] audio_marker: bool,
    #[arg(long, default_value = "hw:CARD=cam2usb,DEV=0")] audio_marker_device: String,
    #[arg(long, default_value_t = 300)] audio_marker_cadence_ticks: u64, // ~5 s @ 60 Hz
    #[arg(long)] marker_log: Option<PathBuf>,
```

(The `audio_marker_device` default is a placeholder — the real cam2 USB audio card string is enumerated on-box with `aplay -l` during the rig proof, Task 7; wire it through `RunConfig`.)

- [ ] **Step 5: Verify default-feature build unaffected + probe compiles on CI**

Run locally: `cargo check` (av_sync pure only compiled), `cargo test --lib av_sync # airuleset:build-ok`.
Run on CI: `cargo clippy --features probe --all-targets -- -D warnings`.
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add src/av_sync.rs src/probe/audio_marker_io.rs src/probe/mod.rs src/probe/painter.rs src/probe/run.rs src/bin/frame-probe.rs
git commit -m "feat: #188 painter audio-marker emission — chirp on cadence via cam2 USB audio + marker log"
```

---

## Task 6: Auto-set controller (offset → genlock video-delay via OBS WS)

**Files:**
- Create: `scripts/av_sync_calibrate.py`
- Create: `scripts/test_av_sync_calibrate.py`

**Interfaces:**
- Consumes: the verdict JSON (`av_sync.offset_ms`), the OBS WS helpers pattern from `scripts/obs_phase2.py` (`_conn`, `_rpc`), the DistroAV source property `genlock_latency_ms_src`.
- Produces: a CLI `av_sync_calibrate.py --host <ip> --source "NDI 2ME PGM" --offset-ms <f> [--apply] [--update-drift-pin]` that reads current latency, computes `new = clamp(round(current - offset), 3, 2000)`, and (with `--apply`) sets it via `SetInputSettings` with read-back verification; without `--apply` it prints the plan (dry-run default).

- [ ] **Step 1: Write the pure delay-math + its failing pytest**

Create `scripts/av_sync_calibrate.py` with the pure function first:

```python
#!/usr/bin/env python3
"""#188 A/V-sync controller: measured offset -> genlock video-delay on the stream source.

The pure `required_delay_ms` MIRRORS src/av_sync.rs::required_delay_ms (same sign + clamp) —
keep the two in lock-step. offset_ms = video_time - audio_time; positive = video lags audio
-> REDUCE the delay.
"""
import argparse
import json
import sys

GENLOCK_SRC_LATENCY_KEY = "genlock_latency_ms_src"
LATENCY_MIN = 3
LATENCY_MAX = 2000


def required_delay_ms(current_delay_ms: int, offset_ms: float) -> int:
    raw = round(current_delay_ms - offset_ms)
    return max(LATENCY_MIN, min(LATENCY_MAX, raw))
```

Create `scripts/test_av_sync_calibrate.py`:

```python
import importlib.util
import pathlib

spec = importlib.util.spec_from_file_location(
    "av_sync_calibrate", pathlib.Path(__file__).parent / "av_sync_calibrate.py"
)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def test_required_delay_sign_and_clamp():
    assert m.required_delay_ms(1000, 120.0) == 880    # video lags -> reduce
    assert m.required_delay_ms(1000, -120.0) == 1120  # video leads -> increase
    assert m.required_delay_ms(1000, 5000.0) == 3     # clamp low
    assert m.required_delay_ms(1000, -5000.0) == 2000 # clamp high
    assert m.required_delay_ms(3, 0.0) == 3
```

- [ ] **Step 2: Run the pytest (fail → the file exists but the CLI parts below still needed → the math test passes now)**

Run: `python3 -m pytest scripts/test_av_sync_calibrate.py -v`
Expected: PASS (the pure math is present). This test locks the sign/clamp equivalence with the Rust side.

- [ ] **Step 3: Add the WS apply path (reuse the obs_phase2.py pattern)**

Append to `scripts/av_sync_calibrate.py` — reuse the `_conn`/`_rpc` shape from `scripts/obs_phase2.py` (op-1 identify with `eventSubscriptions:0`, op-6 request/op-7 response, SHA-256 auth). Then:

```python
def read_current_latency(ws, source):
    from obs_phase2 import _rpc  # reuse the canonical RPC
    s = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    return int(s.get(GENLOCK_SRC_LATENCY_KEY, 3))


def apply_latency(ws, source, new_ms):
    from obs_phase2 import _rpc
    _rpc(ws, "SetInputSettings", {
        "inputName": source,
        "inputSettings": {GENLOCK_SRC_LATENCY_KEY: new_ms},
        "overlay": True,
    })
    back = _rpc(ws, "GetInputSettings", {"inputName": source}).get("inputSettings", {})
    actual = int(back.get(GENLOCK_SRC_LATENCY_KEY, -1))
    if actual != new_ms:
        raise SystemExit(f"read-back mismatch: set {new_ms}, got {actual}")
    return actual


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--source", default="NDI 2ME PGM")
    group = ap.add_mutually_exclusive_group(required=True)
    group.add_argument("--offset-ms", type=float)
    group.add_argument("--verdict-json", type=str, help="read av_sync.offset_ms from a verdict JSON")
    ap.add_argument("--apply", action="store_true", help="actually set (default: dry-run)")
    args = ap.parse_args()

    offset = args.offset_ms
    if offset is None:
        with open(args.verdict_json) as f:
            j = json.load(f)
        av = j.get("av_sync")
        if not av or av.get("offset_ms") is None:
            raise SystemExit("verdict JSON has no resolved av_sync.offset_ms — measurement UNRESOLVED")
        offset = float(av["offset_ms"])

    from obs_phase2 import _conn
    ws = _conn(args.host, args.password)
    current = read_current_latency(ws, args.source)
    new_ms = required_delay_ms(current, offset)
    print(f"[av-sync] source='{args.source}' current={current}ms offset={offset:.1f}ms -> new={new_ms}ms")
    if args.apply:
        got = apply_latency(ws, args.source, new_ms)
        print(f"[av-sync] APPLIED + verified: {args.source} genlock_latency_ms_src={got}")
    else:
        print("[av-sync] dry-run (pass --apply to set)")


if __name__ == "__main__":
    main()
```

- [ ] **Step 4: Commit**

```bash
git add scripts/av_sync_calibrate.py scripts/test_av_sync_calibrate.py
git commit -m "feat: #188 A/V-sync controller — offset -> genlock video-delay via OBS WS (snapshot/verify)"
```

---

## Task 7: Rig calibration proof (supervisor-driven orchestration)

**Files:**
- Create: `scripts/av-sync-calibrate.sh`

**This task is DRIVEN BY THE SUPERVISOR on the live rig (drive-rig-steps-in-supervisor), NOT delegated to a worker.** It ties the units together and produces the honest end-to-end proof.

**Interfaces:**
- Consumes: `rig-mode.sh test` (paint QR + burns), the painter `--audio-marker` path, `recording-verdict --av-sync`, `av_sync_calibrate.py --apply`.

- [ ] **Step 1: Enumerate cam2 USB audio device**

On cam2 (10.77.9.62): `aplay -l` → identify the USB audio card name for the marker output; set the real `hw:CARD=<name>,DEV=0` string (replaces the Task-5 placeholder) in `rig-mode.sh test`'s painter launch.

- [ ] **Step 2: Orchestration script**

Create `scripts/av-sync-calibrate.sh` (`set -euo pipefail`) that:
1. `rig-mode.sh test` with `--audio-marker` on cam2 (paints QR + colour scale + emits the chirp; prints the OBS burns-ON step).
2. Records the stream OBS program for ~60 s (reuse the `recording-e2e.sh` stream-record path / obs_phase2 prod-scene).
3. Pulls the marker log from cam2 + the stream recording to the decode box.
4. `recording-verdict --stream <rec> --av-sync --av-sync-marker-log <log> --json <out>` → `av_sync.offset_ms`.
5. `av_sync_calibrate.py --host <stream> --source "NDI 2ME PGM" --verdict-json <out> --apply`.
6. Re-record + re-measure; assert `|offset_ms| < 17` (half a 30 fps frame). Iterate once if not converged.
7. `rig-mode.sh event` to restore clean broadcast; verify cam2/OBS restored (drive-rig-steps `#281` reset).

- [ ] **Step 3: Supervisor runs it on the rig + reports the measured offsets (before → after)**

Record the actual before/after offsets as the proof. Confirm the chirp survives the mastering chain (if the rig shows the chirp squashed / undetected → the marker signal is upgraded to a longer chirp or an FSK id-burst; only the rig can decide this — do NOT pre-optimize).

- [ ] **Step 4: Commit the script**

```bash
git add scripts/av-sync-calibrate.sh
git commit -m "feat: #188 supervisor A/V-sync calibration orchestration (record -> measure -> set -> re-measure)"
```

---

## Task 8: #188 OBS dock (net-new QDockWidget) — independently shippable

**Files:**
- Create: `vendor/distroav/src/av-sync-dock.hpp`, `vendor/distroav/src/av-sync-dock.cpp`
- Modify: `vendor/distroav/src/plugin-main.cpp` (register the dock), the DistroAV CMake source list

**Note:** This is C++ built ON CI / verified on the rig (no Tier-0 test). The calibration (Tasks 1–7) is fully functional via CLI WITHOUT this dock — so the dock MAY ship as its own PR if it balloons. Keep it minimal: show (1) the live per-source genlock latency (`obs_source_get_genlock_latency_ms`), and (2) the last measured A/V offset + timestamp (read from a small JSON the controller writes, e.g. `%PROGRAMDATA%/camera-box/av-sync-last.json`).

**Interfaces:**
- Consumes: `obs_frontend_add_dock_by_id(const char*, const char*, void* /*QWidget*/)` (obs-frontend-api.h:153); `obs_source_get_genlock_latency_ms(obs_source_t*)` (obs.h:1581).

- [ ] **Step 1: Controller writes the last-offset JSON**

Extend `scripts/av_sync_calibrate.py` `apply_latency` to also write `%PROGRAMDATA%/camera-box/av-sync-last.json` = `{ "source": ..., "offset_ms": ..., "applied_latency_ms": ..., "ts": ... }`. Add a pytest asserting the JSON shape.

- [ ] **Step 2: Dock skeleton (`av-sync-dock.hpp` / `.cpp`)**

A `QWidget` (or `QDockWidget`) with a `QLabel` grid + a `QTimer` (1 Hz) that:
- reads the 'NDI 2ME PGM' source via `obs_get_source_by_name`, calls `obs_source_get_genlock_latency_ms`, releases the source;
- reads `av-sync-last.json` for the last measured offset + timestamp;
- renders `Genlock latency: N ms  ·  Last A/V offset: ±M ms @ <ts>`.

- [ ] **Step 3: Register in `plugin-main.cpp`**

In `obs_module_load()` (after the source is registered):

```cpp
#include "av-sync-dock.hpp"
// ...
auto *dock = create_av_sync_dock(); // returns a QWidget*
obs_frontend_add_dock_by_id("camera_box_av_sync", "A/V Sync (#188)", dock);
```

Add both new files to the DistroAV CMake `target_sources` list.

- [ ] **Step 4: CI builds the plugin; supervisor verifies on the rig**

CI builds the Windows plugin artifact. Supervisor loads it in stream OBS, confirms the dock appears, shows the live genlock latency, and updates the A/V offset after a calibration run. (Constraint: NEVER rebuild/redeploy prod OBS or install plugins before a live event — the user guards timing.)

- [ ] **Step 5: Commit**

```bash
git add vendor/distroav/src/av-sync-dock.hpp vendor/distroav/src/av-sync-dock.cpp vendor/distroav/src/plugin-main.cpp
git commit -m "feat: #188 OBS dock — live genlock latency + last measured A/V offset"
```

---

## Self-Review

**1. Spec coverage** (`docs/superpowers/specs/2026-07-01-cam2-av-sync-calibration-design.md`):
- Component (a) painter audio-marker generator → **Task 5** (+ chirp template Task 1). ✓
- Component (b) A/V offset measurement (pure Tier-0 + probe glue) → **Tasks 1, 2, 3, 4**. ✓
- Component (c) auto-set controller (offset → delay, #358 snapshot/verify) → **Task 6**. ✓
- Component (d) #188 dock → **Task 8**. ✓
- Calibration loop (marker cadence, measure, auto-set, dock) → Tasks 5→4→6→8. ✓
- Tier-0 seam (pure crate-root + probe glue) → av_sync.rs pure (Tasks 1,2,5-pure), av_sync_extract.rs / audio_marker_io.rs probe (Tasks 3,5). ✓
- Rig calibration proof (supervisor) → **Task 7**. ✓
- Marker signal decided/rig-verified (chirp; FSK upgrade only if squashed) → Task 1 (chirp) + Task 7 (rig decision). ✓
- Out of scope (live-drift correction; changing the mastered audio) → not planned. ✓

**2. Placeholder scan:** The cam2 USB audio device string is an explicit, called-out placeholder resolved in Task 7 Step 1 (enumerated on-box — genuinely rig-dependent, not a plan gap). No TBD/TODO code steps.

**3. Type consistency:** `EmittedMarker`, `AvOffsetEstimate`, `OffsetSearch`, `ChirpParams`, `generate_chirp`, `detect_chirp_onsets`, `estimate_av_offset_ms`, `required_delay_ms`, `parse_marker_log`, `emitted_markers_from_ticks`, `serialize_marker_log`, `should_emit_marker` — used consistently across Tasks 1–6. The Python `required_delay_ms` mirrors the Rust sign/clamp and is locked by both a Rust test (Task 2) and a pytest (Task 6). `genlock_latency_ms_src` is the single property name across controller + dock. `av_sync.offset_ms` JSON key is consistent verdict→controller→dock.

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-01-cam2-av-sync-calibration.md`.
