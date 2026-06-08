# NDI Frame-Loss E2E Harness — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect true single-frame loss + per-hop latency at the OBS hops (cam2→strih→stream) by differencing each node's NDI output, tapped concurrently on dev1.

**Architecture:** A pure `differ` module computes `IDs(upstream) − IDs(downstream)` between adjacent NDI taps (resample cancels → real per-hop drops). A `multi_reader` glue module runs one `NdiReceiver`+thread per tap on dev1's single clock (so per-hop latency = Δ recv_ts is valid). A `multitap-probe` binary wires taps→differ→JSON. The cam2 painter is the existing `frame-probe` gaining `--run-id`/`--paint-only`. `scripts/multitap-e2e.sh` orchestrates OBS setup (MCP+WebSocket), the cam2 painter, the dev1 taps, and the gate.

**Tech Stack:** Rust, `--features probe` (serde_json, crc, qrcode, rqrr, image), libndi.so.6 on dev1, DistroAV NDI output on OBS, bash + remoteos MCP + OBS WebSocket.

**Spec:** `docs/superpowers/specs/2026-06-08-ndi-frame-loss-e2e-phase2.md`. Implements issue **#6**.

**Conventions (Phase-1, keep):** pure modules (`payload/luma/qr/analyzer/differ`) are unit-tested and stay IN coverage+mutants; hardware glue (`painter/reader/run/fb/multi_reader`) and bins are EXCLUDED. Freeze grouping uses `slice::chunk_by` (no manual index loop) to keep mutation testing free of infinite-loop timeout mutants.

---

### Task 1: Version bump to 1.7.0-dev.1

**Files:**
- Modify: `Cargo.toml:3`

- [ ] **Step 1: Bump the version** (new feature → minor bump; dev must stay strictly above main `1.5.0-dev.1`)

In `Cargo.toml` change:
```toml
version = "1.6.0-dev.1"
```
to:
```toml
version = "1.7.0-dev.1"
```

- [ ] **Step 2: Commit**

```bash
git add Cargo.toml
git commit -m "chore: bump version to 1.7.0-dev.1 for Phase 2 (#6)"
```

---

### Task 2: Extract pure analyzer helpers (DRY for the differ)

The differ needs reorder, freeze, and percentile/latency math that currently live inline inside `analyze()`. Extract them as `pub` helpers and refactor `analyze()` to call them — no behavior change, existing tests stay green.

**Files:**
- Modify: `src/probe/analyzer.rs`

- [ ] **Step 1: Write failing tests for the new public helpers**

Add to the `#[cfg(test)] mod tests` block in `src/probe/analyzer.rs`:

```rust
    #[test]
    fn detect_reorders_flags_backwards_pairs() {
        let obs = vec![obs(0, 0, 1), obs(2, 0, 2), obs(1, 0, 3), obs(3, 0, 4)];
        assert_eq!(detect_reorders(&obs), vec![(2, 1)]);
    }

    #[test]
    fn detect_reorders_empty_when_monotonic() {
        let obs = vec![obs(0, 0, 1), obs(1, 0, 2), obs(1, 0, 3), obs(2, 0, 4)];
        assert!(detect_reorders(&obs).is_empty());
    }

    #[test]
    fn detect_freezes_groups_runs_over_threshold() {
        // id 1 repeats 5x (> 3) at 30 fps -> one freeze, 5*33.333ms.
        let obs = vec![
            obs(0, 0, 1),
            obs(1, 0, 2), obs(1, 0, 3), obs(1, 0, 4), obs(1, 0, 5), obs(1, 0, 6),
            obs(2, 0, 7),
        ];
        let f = detect_freezes(&obs, 30.0, 3.0);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].frame_id, 1);
        assert_eq!(f[0].repeat_count, 5);
        assert!((f[0].duration_ms - 166.6667).abs() < 0.01);
    }

    #[test]
    fn latency_stats_none_on_empty() {
        assert!(latency_stats(&[]).is_none());
    }

    #[test]
    fn latency_stats_computes_fields() {
        let s = latency_stats(&[10.0, 20.0, 30.0]).unwrap();
        assert_eq!(s.samples, 3);
        assert_eq!(s.min_ms, 10.0);
        assert!((s.mean_ms - 20.0).abs() < 0.001);
        assert_eq!(s.max_ms, 30.0);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features probe -p camera-box analyzer 2>&1 | tail -20`
Expected: FAIL — `detect_reorders`, `detect_freezes`, `latency_stats` not found.

- [ ] **Step 3: Extract the helpers and make `percentile` public**

In `src/probe/analyzer.rs`, change `fn percentile` to `pub fn percentile`, and add these three public functions (above `pub fn analyze`):

```rust
/// Backwards-going adjacent pairs in capture order = reordering.
pub fn detect_reorders(observed: &[Observed]) -> Vec<(u32, u32)> {
    let mut reorders = Vec::new();
    for w in observed.windows(2) {
        if w[1].frame_id < w[0].frame_id {
            reorders.push((w[0].frame_id, w[1].frame_id));
        }
    }
    reorders
}

/// Runs of consecutive-equal frame IDs longer than `freeze_periods` capture
/// periods. `chunk_by` avoids a manual index loop (no infinite-loop mutants).
pub fn detect_freezes(observed: &[Observed], capture_fps: f64, freeze_periods: f64) -> Vec<Freeze> {
    let period_ms = 1000.0 / capture_fps;
    observed
        .chunk_by(|a, b| a.frame_id == b.frame_id)
        .filter(|run| (run.len() as f64) > freeze_periods)
        .map(|run| Freeze {
            frame_id: run[0].frame_id,
            repeat_count: run.len(),
            duration_ms: run.len() as f64 * period_ms,
        })
        .collect()
}

/// min/mean/p50/p95/p99/max over a set of millisecond samples (None if empty).
pub fn latency_stats(samples_ms: &[f64]) -> Option<LatencyStats> {
    if samples_ms.is_empty() {
        return None;
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let sum: f64 = sorted.iter().sum();
    Some(LatencyStats {
        samples: sorted.len(),
        min_ms: sorted[0],
        mean_ms: sum / sorted.len() as f64,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
        p99_ms: percentile(&sorted, 0.99),
        max_ms: *sorted.last().unwrap(),
    })
}
```

- [ ] **Step 4: Refactor `analyze()` to use the helpers**

In `pub fn analyze`, replace the inline `reorders` loop, the inline `freezes` block, and the inline latency-stats block with calls to the helpers. The body of `analyze` becomes:

```rust
pub fn analyze(input: AnalysisInput) -> AnalysisReport {
    let emitted_set: HashSet<u32> = input.emitted_ids.iter().copied().collect();
    let observed_set: HashSet<u32> = input.observed.iter().map(|o| o.frame_id).collect();

    let missing_ids: Vec<u32> = input
        .emitted_ids
        .iter()
        .copied()
        .filter(|id| !observed_set.contains(id))
        .collect();

    let reorders = detect_reorders(&input.observed);
    let freezes = detect_freezes(&input.observed, input.capture_fps, input.freeze_periods);

    let mut seen = HashSet::new();
    let mut lat_ms: Vec<f64> = Vec::new();
    for o in &input.observed {
        if seen.insert(o.frame_id) {
            lat_ms.push((o.recv_ts_ns - o.gen_ts_ns) as f64 / 1_000_000.0);
        }
    }
    let latency = latency_stats(&lat_ms);

    let verdict_pass = match input.mode {
        PaintMode::Coverage => {
            !emitted_set.is_empty() && missing_ids.is_empty() && reorders.is_empty()
        }
        PaintMode::FullRate => reorders.is_empty(),
    };

    AnalysisReport {
        mode: input.mode,
        emitted_count: emitted_set.len(),
        observed_count: input.observed.len(),
        unique_observed: observed_set.len(),
        missing_ids,
        reorders,
        freezes,
        latency,
        verdict_pass,
    }
}
```

- [ ] **Step 5: Run all analyzer tests to verify they pass**

Run: `cargo test --features probe -p camera-box analyzer 2>&1 | tail -20`
Expected: PASS — all prior tests + 5 new helper tests green.

- [ ] **Step 6: Commit**

```bash
git add src/probe/analyzer.rs
git commit -m "refactor: extract pub analyzer helpers (detect_reorders/freezes, latency_stats)"
```

---

### Task 3: Extract pure `decode_capture` (DRY tap decode) and refactor reader.rs

Both `reader.rs` (Phase 1) and the new `multi_reader.rs` turn a captured NDI frame into an `Observed`. Extract the fourcc-dispatch → luma → crop → QR-decode into one pure, testable function in `qr.rs`, used by both.

**Files:**
- Modify: `src/probe/qr.rs`
- Modify: `src/probe/reader.rs`

- [ ] **Step 1: Write a failing test for `decode_capture`**

Add to the `#[cfg(test)] mod tests` in `src/probe/qr.rs`:

```rust
    #[test]
    fn decode_capture_roundtrips_bgra_frame() {
        let p = Payload { run_id: 3, frame_id: 99, gen_ts_ns: 42 };
        // 1920x1080 BGRA frame carrying a centered QR, tight stride.
        let bgra = render_qr_bgra(&p, 1920, 1080, 700);
        let fourcc = u32::from_le_bytes(*b"BGRA");
        let got = decode_capture(fourcc, &bgra, 1920, 1080, 1920 * 4, 820);
        assert_eq!(got, Some(p));
    }

    #[test]
    fn decode_capture_none_on_blank() {
        let blank = vec![255u8; (640 * 480 * 4) as usize];
        let fourcc = u32::from_le_bytes(*b"BGRA");
        assert_eq!(decode_capture(fourcc, &blank, 640, 480, 640 * 4, 400), None);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --features probe -p camera-box qr::tests::decode_capture 2>&1 | tail -20`
Expected: FAIL — `decode_capture` not found.

- [ ] **Step 3: Implement `decode_capture` in `qr.rs`**

Add these imports at the top of `src/probe/qr.rs` (next to existing `use` lines):

```rust
use crate::probe::luma::{bgra_to_luma, crop_center, uyvy_to_luma};
```

Add the function (after `decode_qr_luma`):

```rust
/// Turn one captured NDI frame into a decoded `Payload`, or None.
/// Dispatches BGRA/BGRX vs UYVY by fourcc, converts to luma (padded-stride
/// aware), restricts the QR decode to the centered `decode_crop` square (the
/// ROI speed fix), and decodes. Shared by the single-tap reader and the
/// multi-tap reader so the decode path has one tested implementation.
pub fn decode_capture(
    fourcc: u32,
    data: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    decode_crop: u32,
) -> Option<Payload> {
    let full = match &fourcc.to_le_bytes() {
        b"BGRA" | b"BGRX" => bgra_to_luma(data, width, height, stride),
        _ => uyvy_to_luma(data, width, height, stride),
    };
    let img = crop_center(&full, decode_crop, decode_crop);
    decode_qr_luma(img)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --features probe -p camera-box qr::tests::decode_capture 2>&1 | tail -20`
Expected: PASS — both new tests green.

- [ ] **Step 5: Refactor `reader.rs` to call `decode_capture`**

In `src/probe/reader.rs`, replace the body of the `while` loop (the lines computing `full`, `img`, and decoding) so the loop reads:

```rust
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = start.elapsed().as_nanos() as i64;
        if let Some(p) = crate::probe::qr::decode_capture(
            frame.fourcc,
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            params.decode_crop,
        ) {
            if p.run_id == params.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                });
            }
        }
    }
```

Then delete the now-unused imports in `reader.rs`:
```rust
use crate::probe::luma::{bgra_to_luma, crop_center, uyvy_to_luma};
use crate::probe::qr::decode_qr_luma;
```

- [ ] **Step 6: Verify the whole library still compiles and tests pass**

Run: `cargo test --features probe -p camera-box 2>&1 | tail -20`
Expected: PASS — no regressions; `reader.rs` compiles with the shared decode.

- [ ] **Step 7: Commit**

```bash
git add src/probe/qr.rs src/probe/reader.rs
git commit -m "refactor: extract pure decode_capture, share between single/multi-tap readers"
```

---

### Task 4: Per-hop differ (pure core — the Phase-2 capability)

**Files:**
- Create: `src/probe/differ.rs`
- Modify: `src/probe/mod.rs`

- [ ] **Step 1: Declare the module**

In `src/probe/mod.rs`, add `differ` to the pure (tested) group so the doc comment and module list read:

```rust
//! Frame-loss & latency E2E probe (Phases 1–2).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`, `differ`.
//! Hardware glue (excluded from coverage): `fb`, `painter`, `reader`, `run`,
//! `multi_reader`.

pub mod analyzer;
pub mod differ;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod fb;
pub mod multi_reader;
pub mod painter;
pub mod reader;
pub mod run;
```

(`multi_reader` is created in Task 5; declaring it now lets this task compile only after Task 5. To keep tasks independently compilable, add only `pub mod differ;` in THIS task and add `pub mod multi_reader;` in Task 5. Use this minimal form here:)

```rust
pub mod analyzer;
pub mod differ;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod fb;
pub mod painter;
pub mod reader;
pub mod run;
```

- [ ] **Step 2: Write the failing tests for the differ**

Create `src/probe/differ.rs`:

```rust
//! Per-hop NDI-output differencing: detect real single-frame loss between two
//! NDI taps sharing one upstream capture. Pure / unit-tested.
//!
//! Both taps are downstream of the same HDMI/ShadowCast resample, so the
//! resample cancels: `IDs(upstream) − IDs(downstream)` is a clean count of the
//! frames that the hop between the two taps actually dropped — the single-frame
//! accuracy Phase 1's single point could not provide.

use crate::probe::analyzer::{
    detect_freezes, detect_reorders, latency_stats, Freeze, LatencyStats, Observed,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

pub struct HopInput<'a> {
    pub name: String,
    pub upstream: &'a [Observed],
    pub downstream: &'a [Observed],
    pub capture_fps: f64,
    pub freeze_periods: f64,
    /// A tap that saw fewer than this many run_id-matching frames is treated as
    /// disconnected — the hop FAILS rather than vacuously passing on no data.
    pub min_frames: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct HopReport {
    pub name: String,
    pub upstream_unique: usize,
    pub downstream_unique: usize,
    pub dropped_ids: Vec<u32>,
    pub reorders: Vec<(u32, u32)>,
    pub freezes: Vec<Freeze>,
    pub latency: Option<LatencyStats>,
    pub pass: bool,
}

fn first_recv(observed: &[Observed]) -> HashMap<u32, i64> {
    let mut m: HashMap<u32, i64> = HashMap::new();
    for o in observed {
        m.entry(o.frame_id).or_insert(o.recv_ts_ns);
    }
    m
}

pub fn diff_hop(input: HopInput) -> HopReport {
    let up_unique: HashSet<u32> = input.upstream.iter().map(|o| o.frame_id).collect();
    let down_unique: HashSet<u32> = input.downstream.iter().map(|o| o.frame_id).collect();

    let mut dropped_ids: Vec<u32> = up_unique.difference(&down_unique).copied().collect();
    dropped_ids.sort_unstable();

    let reorders = detect_reorders(input.downstream);
    let freezes = detect_freezes(input.downstream, input.capture_fps, input.freeze_periods);

    // Per-hop latency: downstream arrival − upstream arrival on dev1's single
    // clock, per id present in both taps. First occurrence of each id.
    let up_first = first_recv(input.upstream);
    let down_first = first_recv(input.downstream);
    let mut deltas: Vec<f64> = Vec::new();
    for (id, d_recv) in &down_first {
        if let Some(u_recv) = up_first.get(id) {
            deltas.push((d_recv - u_recv) as f64 / 1_000_000.0);
        }
    }
    let latency = latency_stats(&deltas);

    let pass = up_unique.len() >= input.min_frames
        && down_unique.len() >= input.min_frames
        && dropped_ids.is_empty()
        && reorders.is_empty();

    HopReport {
        name: input.name,
        upstream_unique: up_unique.len(),
        downstream_unique: down_unique.len(),
        dropped_ids,
        reorders,
        freezes,
        latency,
        pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(frame_id: u32, recv_ms: i64) -> Observed {
        Observed { frame_id, gen_ts_ns: 0, recv_ts_ns: recv_ms * 1_000_000 }
    }

    fn input<'a>(up: &'a [Observed], down: &'a [Observed]) -> HopInput<'a> {
        HopInput {
            name: "cam→strih".to_string(),
            upstream: up,
            downstream: down,
            capture_fps: 30.0,
            freeze_periods: 3.0,
            min_frames: 2,
        }
    }

    #[test]
    fn clean_hop_passes_no_drops() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(r.pass);
        assert!(r.dropped_ids.is_empty());
        assert_eq!(r.upstream_unique, 4);
        assert_eq!(r.downstream_unique, 4);
    }

    #[test]
    fn single_frame_drop_downstream_is_detected() {
        // id 2 present upstream, absent downstream → the hop dropped it.
        let up = vec![o(0, 0), o(1, 33), o(2, 66), o(3, 99)];
        let down = vec![o(0, 10), o(1, 43), o(3, 109)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.dropped_ids, vec![2]);
    }

    #[test]
    fn resample_dups_present_in_both_are_not_drops() {
        // id 1 duplicated by the resample at both taps → no drop, PASS.
        let up = vec![o(0, 0), o(1, 33), o(1, 40), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(1, 50), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(r.pass);
        assert!(r.dropped_ids.is_empty());
    }

    #[test]
    fn reorder_on_downstream_fails() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(2, 43), o(1, 76)];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.reorders, vec![(2, 1)]);
    }

    #[test]
    fn empty_downstream_fails_min_frames_not_vacuous() {
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down: Vec<Observed> = vec![];
        let r = diff_hop(input(&up, &down));
        assert!(!r.pass);
        assert_eq!(r.downstream_unique, 0);
    }

    #[test]
    fn per_hop_latency_is_downstream_minus_upstream() {
        // each id arrives 10 ms later downstream → mean 10 ms.
        let up = vec![o(0, 0), o(1, 33), o(2, 66)];
        let down = vec![o(0, 10), o(1, 43), o(2, 76)];
        let r = diff_hop(input(&up, &down));
        let l = r.latency.unwrap();
        assert_eq!(l.samples, 3);
        assert!((l.mean_ms - 10.0).abs() < 0.001);
        assert!((l.max_ms - 10.0).abs() < 0.001);
    }
}
```

- [ ] **Step 3: Run the differ tests to verify they fail then pass**

Run: `cargo test --features probe -p camera-box differ 2>&1 | tail -25`
Expected: the module compiles and all 6 tests PASS (the implementation is included above; if a test fails, fix the implementation, not the test).

- [ ] **Step 4: Commit**

```bash
git add src/probe/differ.rs src/probe/mod.rs
git commit -m "feat: per-hop NDI-output differ — detect real single-frame loss (#6)"
```

---

### Task 5: Multi-source NDI reader (glue)

**Files:**
- Create: `src/probe/multi_reader.rs`
- Modify: `src/probe/mod.rs`

- [ ] **Step 1: Declare the module**

In `src/probe/mod.rs`, add `multi_reader` to the glue group and update the doc comment:

```rust
//! Hardware glue (excluded from coverage): `fb`, `painter`, `reader`, `run`,
//! `multi_reader`.
```
and in the glue `pub mod` list add:
```rust
pub mod multi_reader;
```

- [ ] **Step 2: Implement `multi_reader.rs`**

Create `src/probe/multi_reader.rs`:

```rust
//! Multi-source NDI reader: one `NdiReceiver` + thread per tap. All taps share
//! one `Instant` start, so every `recv_ts_ns` is on dev1's single monotonic
//! clock — which makes the differ's per-hop latency (Δ recv_ts) a valid
//! single-clock measurement. Hardware glue: excluded from coverage/mutants.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use crate::probe::qr::decode_capture;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;

/// One tap: a named NDI source to subscribe to, filtered to `run_id`.
pub struct TapSpec {
    pub name: String,
    pub source: String,
    pub run_id: u32,
    pub connect_timeout_secs: u32,
    /// Side of the centered square the QR decode is restricted to (ROI speed fix).
    pub decode_crop: u32,
}

/// A tap's accumulating buffer, readable by the differ after the run.
pub struct TapResult {
    pub name: String,
    pub observed: Arc<Mutex<Vec<Observed>>>,
}

/// Spawn a reader thread for one tap. Returns its join handle plus a handle to
/// its observed buffer. The thread runs until `stop` is set.
pub fn spawn_tap(
    spec: TapSpec,
    start: Instant,
    stop: Arc<AtomicBool>,
) -> (JoinHandle<Result<()>>, TapResult) {
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let result = TapResult {
        name: spec.name.clone(),
        observed: observed.clone(),
    };
    let handle = std::thread::spawn(move || tap_loop(spec, start, stop, observed));
    (handle, result)
}

fn tap_loop(
    spec: TapSpec,
    start: Instant,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<Observed>>>,
) -> Result<()> {
    let mut rx = NdiReceiver::connect(&spec.source, spec.connect_timeout_secs)?;
    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = start.elapsed().as_nanos() as i64;
        if let Some(p) = decode_capture(
            frame.fourcc,
            &frame.data,
            frame.width,
            frame.height,
            frame.stride,
            spec.decode_crop,
        ) {
            if p.run_id == spec.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                });
            }
        }
    }
    tracing::info!(tap = %spec.name, source = %spec.source, "tap finished");
    Ok(())
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --features probe -p camera-box 2>&1 | tail -20`
Expected: compiles clean (no warnings-as-errors issues; glue is not unit-tested).

- [ ] **Step 4: Commit**

```bash
git add src/probe/multi_reader.rs src/probe/mod.rs
git commit -m "feat: multi-source NDI reader (one tap thread per node, shared clock)"
```

---

### Task 6: `frame-probe` gains `--run-id` and `--paint-only`

The cam2 painter and dev1 taps must share a `run_id`; and on cam2 we only need to paint (the QR reaches NDI via camera-box's own capture→NDI), not run the self-loopback reader.

**Files:**
- Modify: `src/probe/run.rs`
- Modify: `src/bin/frame-probe.rs`

- [ ] **Step 1: Add a paint-only path in `run.rs`**

In `src/probe/run.rs`, add `pub run_id_override: Option<u32>` is NOT needed (the bin passes `run_id`); instead add a paint-only entry point. Append to `src/probe/run.rs`:

```rust
/// Paint QR frames for `duration` without receiving/analyzing — used on the
/// camera box in Phase 2, where the QR reaches NDI via camera-box's own
/// capture→NDI path and the taps run elsewhere (dev1).
pub fn run_paint_only(cfg: &RunConfig) -> Result<u64> {
    use crate::probe::painter::{run_painter, PaintParams};

    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64)>>> = Arc::new(Mutex::new(Vec::new()));

    let painter_handle = {
        let stop = stop.clone();
        let emitted = emitted.clone();
        let params = PaintParams {
            run_id: cfg.run_id,
            fb_device: cfg.fb_device.clone(),
            paint_fps: cfg.paint_fps,
            canvas_w: cfg.canvas_w,
            canvas_h: cfg.canvas_h,
            qr_size: cfg.qr_size,
        };
        std::thread::spawn(move || run_painter(params, start, stop, emitted))
    };

    let deadline = Instant::now() + cfg.duration;
    while Instant::now() < deadline {
        if painter_handle.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    painter_handle.join().expect("painter panicked")?;

    let count = emitted.lock().unwrap().len() as u64;
    Ok(count)
}
```

- [ ] **Step 2: Add the two flags and the paint-only branch in `frame-probe.rs`**

In `src/bin/frame-probe.rs`, add two fields to `struct Args` (after `out`):

```rust
    /// Shared run id (default: derived from the clock). Set it so taps on other
    /// machines can filter to this painter's frames.
    #[arg(long)]
    run_id: Option<u32>,
    /// Only paint the framebuffer; do not receive/analyze NDI. Used on the
    /// camera box in Phase 2 (taps run on dev1).
    #[arg(long, default_value_t = false)]
    paint_only: bool,
```

Change the `run_id` line from:
```rust
    let run_id = (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32;
```
to:
```rust
    let run_id = match args.run_id {
        Some(r) => r,
        None => (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32,
    };
```

Then, immediately after building `RunConfig { ... }` is currently inlined into `run(...)`. Replace the `let report = run(RunConfig { ... })?;` block and everything after it with a branch. The new tail of `main` becomes:

```rust
    let cfg = RunConfig {
        mode,
        run_id,
        source: args.source,
        fb_device: args.fb_device,
        duration: Duration::from_secs(args.duration_secs),
        paint_fps,
        capture_fps: args.capture_fps,
        canvas_w: 1920,
        canvas_h: 1080,
        qr_size: args.qr_size,
        freeze_periods: args.freeze_periods,
        connect_timeout_secs: args.connect_timeout_secs,
        settle_ms: args.settle_ms,
    };

    if args.paint_only {
        let painted = camera_box::probe::run::run_paint_only(&cfg)?;
        println!("PAINT_ONLY run_id={} painted={}", run_id, painted);
        return Ok(());
    }

    let report = run(cfg)?;

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&args.out, &json)?;

    println!(
        "VERDICT={} emitted={} observed={} unique={} missing={} reorders={} freezes={}",
        if report.verdict_pass { "PASS" } else { "FAIL" },
        report.emitted_count,
        report.observed_count,
        report.unique_observed,
        report.missing_ids.len(),
        report.reorders.len(),
        report.freezes.len(),
    );
    if let Some(l) = &report.latency {
        println!(
            "LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={})",
            l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
        );
    }
    println!("ARTIFACT={}", args.out);

    if report.verdict_pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
```

(The `--settle-ms < duration` guard at the top of `main` stays as-is. The `run` import is already present.)

- [ ] **Step 3: Verify it builds**

Run: `cargo build --features probe -p camera-box 2>&1 | tail -20`
Expected: `frame-probe` builds with the new flags.

- [ ] **Step 4: Smoke-test the flag parsing (no hardware)**

Run: `cargo run --features probe --bin frame-probe -- --help 2>&1 | grep -E "run-id|paint-only"`
Expected: both `--run-id` and `--paint-only` appear in the help.

- [ ] **Step 5: Commit**

```bash
git add src/probe/run.rs src/bin/frame-probe.rs
git commit -m "feat: frame-probe --run-id + --paint-only (Phase 2 painter sharing)"
```

---

### Task 7: `multitap-probe` binary

**Files:**
- Create: `src/bin/multitap-probe.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Register the binary**

In `Cargo.toml`, after the existing `[[bin]] name = "frame-probe"` block, add:

```toml
[[bin]]
name = "multitap-probe"
path = "src/bin/multitap-probe.rs"
required-features = ["probe"]
```

- [ ] **Step 2: Implement the binary**

Create `src/bin/multitap-probe.rs`:

```rust
//! multitap-probe: subscribe to N NDI taps on dev1, difference adjacent pairs,
//! emit one JSON artifact with a per-hop frame-loss + latency report, exit
//! non-zero on any real per-hop drop/reorder. Painter runs separately on the
//! camera box (frame-probe --paint-only) with the same --run-id.

use anyhow::{bail, Result};
use camera_box::probe::differ::{diff_hop, HopInput, HopReport};
use camera_box::probe::multi_reader::{spawn_tap, TapResult, TapSpec};
use clap::Parser;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "Multi-tap NDI per-hop frame-loss/latency probe (Phase 2)")]
struct Args {
    /// Shared run id (must match the frame-probe painter's --run-id).
    #[arg(long)]
    run_id: u32,
    /// A tap as NAME=NDI_SOURCE_SUBSTRING. Repeat; adjacent taps are
    /// differenced in the order given (e.g. cam="CAM2 (usb)" strih=STRIH-PHASE2
    /// stream=STREAM-PHASE2 → hops cam→strih, strih→stream).
    #[arg(long = "tap", value_parser = parse_tap)]
    taps: Vec<(String, String)>,
    /// Run duration in seconds.
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    /// Expected capture rate (for freeze duration math).
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// QR pixel size on the canvas (decode ROI = qr_size + 120).
    #[arg(long, default_value_t = 700)]
    qr_size: u32,
    /// Freeze threshold in capture periods.
    #[arg(long, default_value_t = 6.0)]
    freeze_periods: f64,
    /// NDI connect timeout (seconds).
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u32,
    /// Trailing settle window (ms): frames received this close to the end are
    /// trimmed so in-flight frames are not counted as hop drops.
    #[arg(long, default_value_t = 500)]
    settle_ms: u64,
    /// A tap with fewer than this many run_id-matching frames FAILS (not vacuous).
    #[arg(long, default_value_t = 100)]
    min_frames: usize,
    /// JSON artifact output path.
    #[arg(long, default_value = "/tmp/multitap-probe.json")]
    out: String,
}

fn parse_tap(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, src)) if !name.is_empty() && !src.is_empty() => {
            Ok((name.to_string(), src.to_string()))
        }
        _ => Err(format!("tap must be NAME=NDI_SOURCE (got '{s}')")),
    }
}

#[derive(Serialize)]
struct MultiTapReport {
    run_id: u32,
    duration_secs: u64,
    taps: Vec<TapSummary>,
    hops: Vec<HopReport>,
    absolute_latency: String,
    verdict_pass: bool,
}

#[derive(Serialize)]
struct TapSummary {
    name: String,
    unique_frames: usize,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    if args.taps.len() < 2 {
        bail!("need >= 2 taps to difference at least one hop (got {})", args.taps.len());
    }
    if args.settle_ms >= args.duration_secs.saturating_mul(1000) {
        bail!(
            "--settle-ms ({}) must be less than the run duration ({} s)",
            args.settle_ms,
            args.duration_secs
        );
    }
    let decode_crop = (args.qr_size + 120).min(1080);

    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));

    // Spawn one reader thread per tap.
    let mut handles = Vec::new();
    let mut results: Vec<TapResult> = Vec::new();
    for (name, source) in &args.taps {
        let (h, r) = spawn_tap(
            TapSpec {
                name: name.clone(),
                source: source.clone(),
                run_id: args.run_id,
                connect_timeout_secs: args.connect_timeout_secs,
                decode_crop,
            },
            start,
            stop.clone(),
        );
        handles.push(h);
        results.push(r);
    }

    // Run for the duration, short-circuit if any tap thread dies.
    let deadline = Instant::now() + Duration::from_secs(args.duration_secs);
    while Instant::now() < deadline {
        if handles.iter().any(|h| h.is_finished()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    stop.store(true, Ordering::Relaxed);
    let stop_ns = start.elapsed().as_nanos() as i64;
    for h in handles {
        h.join().expect("tap thread panicked")?;
    }

    // Snapshot + trim the trailing settle window (in-flight frames are not drops).
    let cutoff_ns = stop_ns - (args.settle_ms as i64) * 1_000_000;
    let trimmed: Vec<Vec<_>> = results
        .iter()
        .map(|r| {
            r.observed
                .lock()
                .unwrap()
                .iter()
                .filter(|o| o.recv_ts_ns <= cutoff_ns)
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();

    // Difference each adjacent pair.
    let mut hops: Vec<HopReport> = Vec::new();
    for i in 0..trimmed.len() - 1 {
        let name = format!("{}→{}", results[i].name, results[i + 1].name);
        hops.push(diff_hop(HopInput {
            name,
            upstream: &trimmed[i],
            downstream: &trimmed[i + 1],
            capture_fps: args.capture_fps,
            freeze_periods: args.freeze_periods,
            min_frames: args.min_frames,
        }));
    }

    let tap_summaries: Vec<TapSummary> = results
        .iter()
        .zip(&trimmed)
        .map(|(r, obs)| TapSummary {
            name: r.name.clone(),
            unique_frames: obs.iter().map(|o| o.frame_id).collect::<std::collections::HashSet<_>>().len(),
        })
        .collect();

    let verdict_pass = hops.iter().all(|h| h.pass);
    let report = MultiTapReport {
        run_id: args.run_id,
        duration_secs: args.duration_secs,
        taps: tap_summaries,
        hops,
        absolute_latency: "UNAVAILABLE — clock not synced (Phase 3 / #8)".to_string(),
        verdict_pass,
    };

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&args.out, &json)?;

    for h in &report.hops {
        println!(
            "HOP {} {} up_unique={} down_unique={} dropped={} reorders={} freezes={}",
            h.name,
            if h.pass { "PASS" } else { "FAIL" },
            h.upstream_unique,
            h.downstream_unique,
            h.dropped_ids.len(),
            h.reorders.len(),
            h.freezes.len(),
        );
        if let Some(l) = &h.latency {
            println!(
                "  REL_LATENCY_MS min={:.1} mean={:.1} p50={:.1} p95={:.1} p99={:.1} max={:.1} (n={})",
                l.min_ms, l.mean_ms, l.p50_ms, l.p95_ms, l.p99_ms, l.max_ms, l.samples
            );
        }
    }
    println!("VERDICT={} ARTIFACT={}", if verdict_pass { "PASS" } else { "FAIL" }, args.out);

    if verdict_pass {
        Ok(())
    } else {
        std::process::exit(1);
    }
}
```

- [ ] **Step 3: Verify it builds and parses**

Run: `cargo build --features probe -p camera-box 2>&1 | tail -20 && cargo run --features probe --bin multitap-probe -- --help 2>&1 | grep -E "tap|run-id|min-frames"`
Expected: builds; help lists `--tap`, `--run-id`, `--min-frames`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/bin/multitap-probe.rs
git commit -m "feat: multitap-probe binary — taps → per-hop differ → JSON gate (#6)"
```

---

### Task 8: CI excludes for the new glue

Keep coverage and mutation gates meaningful: the pure `differ` stays in; the new glue (`multi_reader`, `multitap-probe` bin) is excluded exactly like `painter/reader/run/fb/frame-probe`.

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add `multi_reader` + `multitap-probe` to the coverage ignore regex**

In `.github/workflows/ci.yml`, change the coverage line (currently):
```
        run: cargo llvm-cov --all-features --lcov --output-path lcov.info --ignore-filename-regex 'src/probe/(painter|reader|run|fb)\.rs|src/bin/frame-probe\.rs' --fail-under-lines ${{ vars.COVERAGE_THRESHOLD || '47' }}
```
to:
```
        run: cargo llvm-cov --all-features --lcov --output-path lcov.info --ignore-filename-regex 'src/probe/(painter|reader|run|fb|multi_reader)\.rs|src/bin/(frame-probe|multitap-probe)\.rs' --fail-under-lines ${{ vars.COVERAGE_THRESHOLD || '47' }}
```

- [ ] **Step 2: Add the mutants `-e` excludes**

In the `mutants` job, the `cargo mutants` invocation lists `-e 'src/probe/painter.rs' -e 'src/probe/reader.rs' ...`. Add two more `-e` lines so the full exclude set is painter, reader, run, fb, multi_reader, frame-probe bin, multitap-probe bin. The invocation becomes:

```yaml
            cargo mutants --in-diff pr.diff --features probe \
              -e 'src/probe/painter.rs' \
              -e 'src/probe/reader.rs' \
              -e 'src/probe/run.rs' \
              -e 'src/probe/fb.rs' \
              -e 'src/probe/multi_reader.rs' \
              -e 'src/bin/frame-probe.rs' \
              -e 'src/bin/multitap-probe.rs' \
              --timeout 60
```

(Match the exact existing flags/indentation in the file; only ADD the `multi_reader.rs` and `multitap-probe.rs` `-e` lines. Keep any existing lines such as `run.rs`/`fb.rs` as they already are.)

- [ ] **Step 3: Verify YAML is well-formed**

Run: `python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml')); print('ci.yml OK')"`
Expected: `ci.yml OK`.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: exclude multi_reader + multitap-probe glue from coverage/mutants"
```

---

### Task 9: Orchestration script `scripts/multitap-e2e.sh`

One dev1 command: set up OBS on both boxes (MCP + OBS WebSocket), run the cam2 painter, run the dev1 taps, assert the per-hop gate, restore all state via a trap. OBS setup uses `obs-websocket` over `ws://HOST:4455`; if no WebSocket CLI is available the script calls a small Python helper using the `websocket-client` lib (documented inline). This task wires the SHELL/ORCHESTRATION; the per-hop correctness is already unit-tested in `differ.rs`.

**Files:**
- Create: `scripts/multitap-e2e.sh`

- [ ] **Step 1: Write the script**

Create `scripts/multitap-e2e.sh`:

```bash
#!/usr/bin/env bash
# Phase 2 multi-tap NDI per-hop frame-loss/latency E2E (dev1-orchestrated).
#
# Topology: cam2 paints QR (frame-probe --paint-only) -> camera-box capture->NDI
# "CAM2 (usb)" -> OBS strih ingests it, re-emits NDI "STRIH-PHASE2" -> OBS stream
# ingests that, re-emits NDI "STREAM-PHASE2". dev1 taps all three and differences
# adjacent pairs. strih + stream are off-air-freely during the run; their OBS
# scene/output state is saved and restored by the trap.
#
# Prereqs (dev1): NDI_RUNTIME_DIR_V6=/usr/lib/ndi (libndi.so.6), cargo, sshpass.
# OBS setup is performed by scripts/obs_phase2.py (obs-websocket; see its header).
set -euo pipefail

CAM2=10.77.9.62
STRIH=10.77.9.202
STREAM=10.77.9.204
CAM_PW=newlevel
RUN_ID=$(( RANDOM << 16 | RANDOM ))
DURATION="${DURATION:-300}"
OUT="${OUT:-/tmp/multitap-probe.json}"
export NDI_RUNTIME_DIR_V6="${NDI_RUNTIME_DIR_V6:-/usr/lib/ndi}"

cleanup() {
  set +e
  echo "[cleanup] restoring OBS state + cam2 service"
  python3 scripts/obs_phase2.py teardown --strih "$STRIH" --stream "$STREAM"
  sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
    "pkill -f 'frame-probe' 2>/dev/null; systemctl restart camera-box 2>/dev/null; true"
}
trap cleanup EXIT HUP INT TERM

echo "[1/5] OBS setup (strih ingests CAM2, stream ingests STRIH-PHASE2)"
python3 scripts/obs_phase2.py setup --strih "$STRIH" --stream "$STREAM" \
  --cam-source "CAM2 (usb)" --strih-out STRIH-PHASE2 --stream-out STREAM-PHASE2

echo "[2/5] build frame-probe + multitap-probe"
cargo build --release --features probe --bin frame-probe --bin multitap-probe

echo "[3/5] start cam2 painter (run_id=$RUN_ID), camera-box keeps capture->NDI"
sshpass -p "$CAM_PW" scp -o StrictHostKeyChecking=no \
  target/release/frame-probe root@"$CAM2":/tmp/frame-probe
sshpass -p "$CAM_PW" ssh -o StrictHostKeyChecking=no root@"$CAM2" \
  "mount -o remount,rw / 2>/dev/null; systemctl stop camera-box; \
   (NDI_RUNTIME_DIR_V6=/usr/lib/ndi nohup /usr/local/bin/camera-box >/tmp/cbox.log 2>&1 &); \
   sleep 3; \
   (nohup /tmp/frame-probe --paint-only --run-id $RUN_ID --duration-secs $((DURATION+15)) \
      >/tmp/painter.log 2>&1 &)"
# NOTE: camera-box is started WITHOUT --display so /dev/fb0 is free for the
# painter; it still runs capture->NDI, carrying the QR frames onto the network.

echo "[4/5] dev1 taps (run_id=$RUN_ID, ${DURATION}s)"
sleep 5  # let OBS NDI outputs become discoverable
./target/release/multitap-probe \
  --run-id "$RUN_ID" \
  --tap cam="CAM2 (usb)" \
  --tap strih=STRIH-PHASE2 \
  --tap stream=STREAM-PHASE2 \
  --duration-secs "$DURATION" \
  --out "$OUT"
GATE=$?

echo "[5/5] artifact: $OUT"
cat "$OUT"
exit $GATE
```

- [ ] **Step 2: Make it executable and lint it**

```bash
chmod +x scripts/multitap-e2e.sh
bash -n scripts/multitap-e2e.sh && echo "syntax OK"
```
Expected: `syntax OK`.

- [ ] **Step 3: Commit**

```bash
git add scripts/multitap-e2e.sh
git commit -m "feat: scripts/multitap-e2e.sh — dev1 multi-tap orchestration (#6)"
```

---

### Task 10: OBS WebSocket helper `scripts/obs_phase2.py`

The orchestration needs to set up and tear down OBS scenes + DistroAV NDI outputs on strih and stream via obs-websocket. Keep this in its own small Python helper (obs-websocket v5 JSON protocol over `websocket-client`).

**Files:**
- Create: `scripts/obs_phase2.py`

- [ ] **Step 1: Write the helper**

Create `scripts/obs_phase2.py`:

```python
#!/usr/bin/env python3
"""OBS setup/teardown for Phase 2 NDI taps via obs-websocket v5.

Setup: on each OBS, ensure a dedicated Phase-2 scene with an NDI *source* (the
upstream node's NDI name), make it the program scene, and enable the DistroAV
"Main Output" so OBS re-emits an NDI *output* with the given name. Teardown:
restore the previously-active scene and disable the Phase-2 NDI output.

State to restore is written to /tmp/obs_phase2_state.json between setup/teardown.

Requires: pip install websocket-client. OBS WebSocket on :4455 (no auth on LAN;
pass --password if your boxes require one). DistroAV provides the
'distroav_main_output' / NDI source input kind 'distroav_ndi_source'.
"""
import argparse
import json
import sys

try:
    from websocket import create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")

STATE = "/tmp/obs_phase2_state.json"
PORT = 4455


def _rpc(host, requests, password=""):
    """Minimal obs-websocket v5 client: connect, (no-auth) identify, run reqs."""
    import base64, hashlib
    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())  # op 0
    ident = {"op": 1, "d": {"rpcVersion": 1}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())  # op 2 Identified
    out = []
    for rtype, rdata in requests:
        rid = rtype
        ws.send(json.dumps({"op": 6, "d": {"requestType": rtype,
                                           "requestId": rid, "requestData": rdata}}))
        while True:
            msg = json.loads(ws.recv())
            if msg["op"] == 7 and msg["d"]["requestId"] == rid:
                out.append(msg["d"])
                break
    ws.close()
    return out


def setup(args):
    state = {}
    for host, scene, ndi_source, out_name in (
        (args.strih, "PHASE2", args.cam_source, args.strih_out),
        (args.stream, "PHASE2", args.strih_out, args.stream_out),
    ):
        cur = _rpc(host, [("GetCurrentProgramScene", {})], args.password)[0]
        state[host] = {"prev_scene": cur["responseData"].get("currentProgramSceneName")}
        _rpc(host, [
            ("CreateScene", {"sceneName": scene}),
        ], args.password)  # ignore "already exists" via responseData status below
        _rpc(host, [
            ("CreateInput", {
                "sceneName": scene, "inputName": f"src-{ndi_source}",
                "inputKind": "distroav_ndi_source",
                "inputSettings": {"ndi_source_name": ndi_source},
            }),
            ("SetCurrentProgramScene", {"sceneName": scene}),
            ("SetOutputSettings", {
                "outputName": "distroav_main_output",
                "outputSettings": {"ndi_name": out_name},
            }),
            ("StartOutput", {"outputName": "distroav_main_output"}),
        ], args.password)
        print(f"[obs] {host}: scene {scene} ingest '{ndi_source}' -> NDI out '{out_name}'")
    with open(STATE, "w") as f:
        json.dump(state, f)


def teardown(args):
    try:
        state = json.load(open(STATE))
    except FileNotFoundError:
        state = {}
    for host in (args.strih, args.stream):
        try:
            _rpc(host, [("StopOutput", {"outputName": "distroav_main_output"})], args.password)
            prev = state.get(host, {}).get("prev_scene")
            if prev:
                _rpc(host, [("SetCurrentProgramScene", {"sceneName": prev})], args.password)
            print(f"[obs] {host}: NDI output stopped, scene restored to {prev}")
        except Exception as e:  # teardown must never raise
            print(f"[obs] {host}: teardown warning: {e}", file=sys.stderr)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("setup", "teardown"):
        p = sub.add_parser(name)
        p.add_argument("--strih", required=True)
        p.add_argument("--stream", required=True)
        p.add_argument("--password", default="")
        if name == "setup":
            p.add_argument("--cam-source", required=True)
            p.add_argument("--strih-out", required=True)
            p.add_argument("--stream-out", required=True)
        else:
            p.add_argument("--strih-out", default="")
            p.add_argument("--stream-out", default="")
    args = ap.parse_args()
    (setup if args.cmd == "setup" else teardown)(args)


if __name__ == "__main__":
    main()
```

- [ ] **Step 2: Syntax-check the helper**

Run: `python3 -m py_compile scripts/obs_phase2.py && echo "py OK"`
Expected: `py OK`.

- [ ] **Step 3: Commit**

```bash
git add scripts/obs_phase2.py
git commit -m "feat: scripts/obs_phase2.py — OBS NDI setup/teardown via obs-websocket (#6)"
```

---

### Task 11: Live acceptance run on real hardware (cam2 + strih + stream)

This proves the harness end-to-end on the off-air rig. **Hardware task — run on dev1 against the live LAN.** If OBS WebSocket input kinds / output names differ from the assumptions in `obs_phase2.py`, fix the helper (the exact DistroAV input kind / output name is verified live here) and re-run; do NOT weaken the gate.

**Files:**
- None (runs the committed harness; any fix lands in `scripts/obs_phase2.py` or the script)

- [ ] **Step 1: Confirm the DistroAV WebSocket vocab on a live box**

Use the `win-strih` MCP + a quick obs-websocket probe (or OBS UI introspection) to confirm the NDI **source input kind** and the **NDI output name** DistroAV exposes (the plan assumes `distroav_ndi_source` and `distroav_main_output`). If they differ, update `scripts/obs_phase2.py` constants accordingly and commit the fix:
```bash
git commit -am "fix: correct DistroAV obs-websocket input kind / output name (#6)"
```

- [ ] **Step 2: Run the full multi-tap E2E (short run first)**

Run: `DURATION=60 ./scripts/multitap-e2e.sh`
Expected: setup logs for both OBS, painter starts on cam2, taps run 60 s, and the JSON shows two hops `cam→strih` and `strih→stream`, each `PASS` with `dropped=0 reorders=0` and a `REL_LATENCY_MS` block. Exit code 0.

- [ ] **Step 3: Run the 5-minute acceptance run**

Run: `DURATION=300 ./scripts/multitap-e2e.sh`
Expected: both hops PASS over 5 min, zero dropped, zero reorders; JSON artifact at `/tmp/multitap-probe.json` with both hops + per-hop latency. Capture the numbers for the completion report (per-hop drop counts, per-hop relative latency mean/p95).

- [ ] **Step 4: Confirm cleanup restored production state**

Verify via `win-strih` / `win-stream-snv` MCP that each OBS returned to its prior program scene and the Phase-2 NDI output is stopped, and that cam2's `camera-box` service is active again:
```bash
sshpass -p newlevel ssh root@10.77.9.62 "systemctl is-active camera-box"
```
Expected: `active`; both OBS on their prior scenes; no lingering `STRIH-PHASE2`/`STREAM-PHASE2` outputs.

- [ ] **Step 5 (no commit unless a live fix was needed):** record the acceptance numbers in the PR / completion report. If Step 1 required a helper fix, it is already committed.

---

## Self-Review

**Spec coverage:**
- §5.1 multi-source reader → Task 5. §5.2 differ → Task 4. §5.3 single-clock latency → Task 4 (`diff_hop` latency) + Task 7 (`absolute_latency: UNAVAILABLE`). §5.4 multitap-probe → Task 7. §5.5 frame-probe flags → Task 6. §5.6 orchestration → Tasks 9–10. §6 gate semantics → Task 4 verdict + Task 7 run verdict. §7 testing → Tasks 2–4 (pure tests) + Task 8 (CI excludes) + Task 11 (live). §8 non-goals respected (no production-path change; probe behind feature; absolute latency deferred to #8). §9 version → Task 1.
- DRY additions beyond the spec: Task 2 (extract analyzer helpers) and Task 3 (extract `decode_capture`) — both reduce duplication the differ/multi_reader would otherwise introduce, and ADD pure test/mutation coverage on previously-glue decode dispatch. In scope (≤spec intent, no new feature).

**Placeholder scan:** every code step has complete code; every run step has an exact command + expected output. No TBD/TODO.

**Type consistency:** `Observed{frame_id,gen_ts_ns,recv_ts_ns}` used identically across tasks. `diff_hop(HopInput)->HopReport`, `spawn_tap(TapSpec,Instant,Arc<AtomicBool>)->(JoinHandle,TapResult)`, `decode_capture(u32,&[u8],u32,u32,u32,u32)->Option<Payload>`, `latency_stats(&[f64])->Option<LatencyStats>`, `detect_reorders(&[Observed])`, `detect_freezes(&[Observed],f64,f64)` — names match between definition (Tasks 2–4) and use (Tasks 5,7). `run_paint_only(&RunConfig)->Result<u64>` defined Task 6, used same task.
