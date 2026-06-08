# NDI Frame-Loss & Latency E2E Harness — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a QR-frame-ID HDMI-loopback probe on cam2 that produces deterministic evidence of zero frame loss and per-frame latency through the camera-box capture→NDI path.

**Architecture:** A feature-gated `frame-probe` binary runs two threads on one monotonic clock on cam2: a *painter* draws a QR(frame_id, gen_ts) onto `/dev/fb0` (HDMI out → loop → ShadowCast HDMI in → camera-box → NDI), and a *reader* receives the NDI back, decodes the QR, and records arrival. A pure analyzer correlates emitted vs observed IDs → loss/freeze/reorder classification + latency stats → JSON artifact. All decision logic lives in pure, unit-tested modules; only thin hardware glue is untested.

**Tech Stack:** Rust, existing `camera_box` lib (`display.rs` framebuffer, `ndi.rs` NdiReceiver), `qrcode` (encode), `rqrr` (decode), `image` (buffers), `crc` (payload integrity), `serde`/`serde_json` (artifact). Optional deps behind a `probe` cargo feature so the production binary is unaffected.

**Reference spec:** `docs/superpowers/specs/2026-06-08-ndi-frame-loss-e2e-design.md`

**Environment:** cam2 = 10.77.9.62 (root/newlevel). Rig is off-air-freely. iGPU HDMI-out `/dev/fb0` (1920×1080 BGRA) ↔ GENKI ShadowCast 2 capture `/dev/video0`; camera-box already sends NDI `usb (CAM2)`. Tier-1: local `cargo build`/`cargo test` are allowed.

---

## File Structure

| File | Responsibility | Tested |
|---|---|---|
| `Cargo.toml` | `probe` feature, optional deps, `frame-probe` bin target | n/a |
| `src/lib.rs` | gate `pub mod probe` behind feature | n/a |
| `src/probe/mod.rs` | submodule declarations | n/a |
| `src/probe/payload.rs` | QR payload struct + string encode/decode + CRC | ✅ unit |
| `src/probe/luma.rs` | UYVY→luma, BGRA→luma extraction | ✅ unit |
| `src/probe/qr.rs` | render payload→BGRA QR; decode luma→payload | ✅ unit (incl. degraded) |
| `src/probe/analyzer.rs` | classify loss/freeze/reorder + latency stats | ✅ unit (core guard) |
| `src/probe/painter.rs` | hardware glue: draw to `/dev/fb0`, pace | ✖ hardware |
| `src/probe/reader.rs` | hardware glue: NdiReceiver → decode | ✖ hardware |
| `src/probe/run.rs` | orchestrate threads → AnalysisReport | ✖ hardware |
| `src/bin/frame-probe.rs` | CLI, write JSON, exit by verdict | ✖ hardware |
| `.github/workflows/ci.yml` | run probe unit tests; exclude hardware glue from coverage | n/a |
| `scripts/loopback-e2e.sh` | dev1 orchestration: build→deploy→run on cam2→pull artifact→assert | n/a |

**Deferred (not Phase 1, per spec §7.1):** plain large-digit overlay (debug aid, needs a font renderer; QR is the source of truth). Note it; do not build it.

---

## Task 1: Cargo deps + feature + skeleton modules

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/probe/mod.rs`

- [ ] **Step 1: Add feature, optional deps, and bin to `Cargo.toml`**

Add `serde_json`, `crc`, `qrcode`, `rqrr`, `image` as **optional** deps in `[dependencies]`:

```toml
# Frame-loss/latency probe (feature = "probe") — kept out of the production binary
serde_json = { version = "1", optional = true }
crc = { version = "3", optional = true }
qrcode = { version = "0.14", optional = true, features = ["image"] }
rqrr = { version = "0.9", optional = true }
image = { version = "0.25", optional = true, default-features = false }
```

After the `[dev-dependencies]` block, add the feature and bin target:

```toml
[features]
probe = ["dep:serde_json", "dep:crc", "dep:qrcode", "dep:rqrr", "dep:image"]

[[bin]]
name = "frame-probe"
path = "src/bin/frame-probe.rs"
required-features = ["probe"]
```

(`serde` is already a non-optional dependency, so the `Serialize` derives in `analyzer.rs` compile whenever the gated `probe` module is built — do **not** list `serde` in the feature array; Cargo rejects a non-optional dep there.)

- [ ] **Step 2: Gate the module in `src/lib.rs`**

Add at the end of `src/lib.rs`:

```rust
#[cfg(feature = "probe")]
pub mod probe;
```

- [ ] **Step 3: Create `src/probe/mod.rs`**

```rust
//! Frame-loss & latency E2E probe (Phase 1).
//!
//! Pure, unit-tested logic: `payload`, `luma`, `qr`, `analyzer`.
//! Hardware glue (excluded from coverage): `painter`, `reader`, `run`.

pub mod analyzer;
pub mod luma;
pub mod payload;
pub mod qr;

pub mod painter;
pub mod reader;
pub mod run;
```

- [ ] **Step 4: Verify the production build is unaffected and the feature resolves**

Run: `cargo build` (no feature) — Expected: PASS, unchanged.
Run: `cargo build --features probe` — Expected: FAIL (modules referenced in `mod.rs` don't exist yet). This confirms the feature wiring is active. If instead it fails on **dependency version resolution** (rqrr vs qrcode disagreeing on the `image` major), align them: pick an `image` version both accept (`cargo tree -i image` shows the conflict) and pin all three to that line. Do not proceed until `cargo build` (no feature) is green.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/probe/mod.rs
git commit -m "build: add probe feature, optional deps, frame-probe bin skeleton"
```

---

## Task 2: Payload encode/decode (pure)

**Files:**
- Create: `src/probe/payload.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/probe/payload.rs`:

```rust
//! QR payload: a compact ASCII string carrying frame identity + emission time.
//!
//! Wire format: `P{run_id}.{frame_id}.{gen_ts_ns}.{crc32}` where crc32 is the
//! ISO-HDLC CRC of the body `{run_id}.{frame_id}.{gen_ts_ns}`. ASCII (not binary)
//! so it round-trips cleanly through QR text decoding.

use crc::{Crc, CRC_32_ISO_HDLC};

const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Payload {
    pub run_id: u32,
    pub frame_id: u32,
    pub gen_ts_ns: i64,
}

impl Payload {
    fn body(&self) -> String {
        format!("{}.{}.{}", self.run_id, self.frame_id, self.gen_ts_ns)
    }

    /// Encode to the wire string (with CRC).
    pub fn encode(&self) -> String {
        let body = self.body();
        let crc = CRC32.checksum(body.as_bytes());
        format!("P{}.{}", body, crc)
    }

    /// Decode from the wire string. Returns None on malformed input or CRC mismatch.
    pub fn decode(s: &str) -> Option<Payload> {
        let rest = s.strip_prefix('P')?;
        let parts: Vec<&str> = rest.split('.').collect();
        if parts.len() != 4 {
            return None;
        }
        let run_id: u32 = parts[0].parse().ok()?;
        let frame_id: u32 = parts[1].parse().ok()?;
        let gen_ts_ns: i64 = parts[2].parse().ok()?;
        let crc: u32 = parts[3].parse().ok()?;
        let p = Payload { run_id, frame_id, gen_ts_ns };
        if CRC32.checksum(p.body().as_bytes()) != crc {
            return None;
        }
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_fields() {
        let p = Payload { run_id: 42, frame_id: 9001, gen_ts_ns: 1_234_567_890 };
        assert_eq!(Payload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn zero_values_roundtrip() {
        let p = Payload { run_id: 0, frame_id: 0, gen_ts_ns: 0 };
        assert_eq!(Payload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        let p = Payload { run_id: 1, frame_id: 2, gen_ts_ns: 3 };
        let mut s = p.encode();
        // Flip the last CRC digit.
        let last = s.pop().unwrap();
        let flipped = if last == '0' { '1' } else { '0' };
        s.push(flipped);
        assert_eq!(Payload::decode(&s), None);
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(Payload::decode("hello world"), None);
        assert_eq!(Payload::decode("P1.2.3"), None); // too few parts
        assert_eq!(Payload::decode(""), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail (compile error first)**

Run: `cargo test --features probe payload`
Expected: until this file compiles cleanly it FAILS to build; once it does, all four tests must PASS. (The module is new code + its tests together — RED here means "not yet compiling / a test asserts wrong".)

- [ ] **Step 3: (impl already in Step 1) confirm tests pass**

Run: `cargo test --features probe payload`
Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add src/probe/payload.rs
git commit -m "feat(probe): QR payload encode/decode with CRC"
```

---

## Task 3: Luma extraction (pure)

**Files:**
- Create: `src/probe/luma.rs`

- [ ] **Step 1: Write the failing tests + implementation**

Create `src/probe/luma.rs`:

```rust
//! Extract a grayscale (luma) image from captured frames for QR decoding.

use image::GrayImage;

/// UYVY 4:2:2 → luma. Layout per 2 px: [U, Y0, V, Y1]; luma is every odd byte.
pub fn uyvy_to_luma(data: &[u8], width: u32, height: u32) -> GrayImage {
    let n = (width as usize) * (height as usize);
    let mut buf = vec![0u8; n];
    for (i, px) in buf.iter_mut().enumerate() {
        let idx = i * 2 + 1;
        if idx < data.len() {
            *px = data[idx];
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

/// BGRA → luma via BT.601 integer weights.
pub fn bgra_to_luma(data: &[u8], width: u32, height: u32) -> GrayImage {
    let n = (width as usize) * (height as usize);
    let mut buf = vec![0u8; n];
    for (i, px) in buf.iter_mut().enumerate() {
        let o = i * 4;
        if o + 2 < data.len() {
            let b = data[o] as u32;
            let g = data[o + 1] as u32;
            let r = data[o + 2] as u32;
            *px = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
        }
    }
    GrayImage::from_raw(width, height, buf).expect("buffer sized w*h")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uyvy_picks_luma_bytes() {
        // 2x1 image: [U,Y0,V,Y1] = [10, 200, 20, 100]
        let img = uyvy_to_luma(&[10, 200, 20, 100], 2, 1);
        assert_eq!(img.get_pixel(0, 0)[0], 200);
        assert_eq!(img.get_pixel(1, 0)[0], 100);
    }

    #[test]
    fn bgra_white_and_black() {
        // px0 white (255,255,255), px1 black (0,0,0)
        let data = [255, 255, 255, 255, 0, 0, 0, 255];
        let img = bgra_to_luma(&data, 2, 1);
        assert_eq!(img.get_pixel(0, 0)[0], 255);
        assert_eq!(img.get_pixel(1, 0)[0], 0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --features probe luma`
Expected: PASS (2 tests).

- [ ] **Step 3: Commit**

```bash
git add src/probe/luma.rs
git commit -m "feat(probe): UYVY/BGRA to luma extraction"
```

---

## Task 4: QR render + decode (pure, with degraded-path test)

**Files:**
- Create: `src/probe/qr.rs`

- [ ] **Step 1: Write the failing tests + implementation**

Create `src/probe/qr.rs`:

```rust
//! Render a payload to a centered QR on a white BGRA canvas, and decode a payload
//! from a grayscale image.

use crate::probe::payload::Payload;
use image::{GrayImage, Luma};
use qrcode::{EcLevel, QrCode};

/// Render `payload` as a QR (EC level H), centered on a white BGRA canvas.
/// Returns a `canvas_w * canvas_h * 4` BGRA byte buffer.
pub fn render_qr_bgra(payload: &Payload, canvas_w: u32, canvas_h: u32, qr_size: u32) -> Vec<u8> {
    let s = payload.encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H)
        .expect("payload is small, encodes within QR capacity");
    let qr: GrayImage = code
        .render::<Luma<u8>>()
        .min_dimensions(qr_size, qr_size)
        .max_dimensions(qr_size, qr_size)
        .quiet_zone(true)
        .build();

    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize]; // white BGRA
    let (qw, qh) = (qr.width().min(canvas_w), qr.height().min(canvas_h));
    let ox = (canvas_w - qw) / 2;
    let oy = (canvas_h - qh) / 2;
    for y in 0..qh {
        for x in 0..qw {
            let lum = qr.get_pixel(x, y)[0];
            let ci = (((oy + y) * canvas_w + (ox + x)) * 4) as usize;
            canvas[ci] = lum;
            canvas[ci + 1] = lum;
            canvas[ci + 2] = lum;
            canvas[ci + 3] = 255;
        }
    }
    canvas
}

/// Decode the first QR found in a grayscale image into a Payload, or None.
pub fn decode_qr_luma(img: GrayImage) -> Option<Payload> {
    let mut prepared = rqrr::PreparedImage::prepare(img);
    for grid in prepared.detect_grids() {
        if let Ok((_meta, content)) = grid.decode() {
            if let Some(p) = Payload::decode(&content) {
                return Some(p);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::luma::bgra_to_luma;
    use image::imageops::{resize, FilterType};

    fn sample() -> Payload {
        Payload { run_id: 7, frame_id: 12345, gen_ts_ns: 9_876_543_210 }
    }

    #[test]
    fn clean_roundtrip() {
        let p = sample();
        let bgra = render_qr_bgra(&p, 1280, 720, 600);
        let luma = bgra_to_luma(&bgra, 1280, 720);
        assert_eq!(decode_qr_luma(luma), Some(p));
    }

    #[test]
    fn survives_downscale_and_noise() {
        // Simulate the lossy capture path: render large, halve resolution and back,
        // add a deterministic noise pattern, then decode.
        let p = sample();
        let bgra = render_qr_bgra(&p, 1920, 1080, 700);
        let full = bgra_to_luma(&bgra, 1920, 1080);

        let small = resize(&full, 960, 540, FilterType::Triangle);
        let mut back = resize(&small, 1920, 1080, FilterType::Triangle);

        // Deterministic ±6 luma dither (no RNG, stable test).
        for (i, px) in back.iter_mut().enumerate() {
            let d: i16 = if i % 3 == 0 { 6 } else { -6 };
            *px = (*px as i16 + d).clamp(0, 255) as u8;
        }

        assert_eq!(decode_qr_luma(back), Some(p));
    }

    #[test]
    fn blank_image_decodes_to_none() {
        let blank = GrayImage::from_raw(640, 480, vec![255u8; 640 * 480]).unwrap();
        assert_eq!(decode_qr_luma(blank), None);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --features probe qr`
Expected: PASS (3 tests). If `survives_downscale_and_noise` fails, increase `qr_size` (e.g. 800) — a bigger QR survives more degradation. If `render`/`decode` don't compile, reconcile the `qrcode`/`rqrr`/`image` versions per Task 1 Step 4.

- [ ] **Step 3: Commit**

```bash
git add src/probe/qr.rs
git commit -m "feat(probe): QR render to BGRA and decode from luma (degraded-path tested)"
```

---

## Task 5: Analyzer — the regression-guard core (pure)

**Files:**
- Create: `src/probe/analyzer.rs`

- [ ] **Step 1: Write the failing tests + implementation**

Create `src/probe/analyzer.rs`:

```rust
//! Correlate emitted vs observed frame IDs → loss / freeze / reorder + latency.

use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaintMode {
    Coverage,
    FullRate,
}

/// One decoded frame, in capture order.
#[derive(Debug, Clone, Copy)]
pub struct Observed {
    pub frame_id: u32,
    pub gen_ts_ns: i64,
    pub recv_ts_ns: i64,
}

pub struct AnalysisInput {
    pub mode: PaintMode,
    /// Every painted frame_id, in paint order.
    pub emitted_ids: Vec<u32>,
    /// Decoded frames, in capture order.
    pub observed: Vec<Observed>,
    pub capture_fps: f64,
    /// Freeze threshold, in capture periods (e.g. 3.0).
    pub freeze_periods: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyStats {
    pub samples: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Freeze {
    pub frame_id: u32,
    pub repeat_count: usize,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisReport {
    pub mode: PaintMode,
    pub emitted_count: usize,
    pub observed_count: usize,
    pub unique_observed: usize,
    pub missing_ids: Vec<u32>,
    pub reorders: Vec<(u32, u32)>,
    pub freezes: Vec<Freeze>,
    pub latency: Option<LatencyStats>,
    pub verdict_pass: bool,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    // Nearest-rank.
    let rank = (q * (sorted.len() as f64)).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

pub fn analyze(input: AnalysisInput) -> AnalysisReport {
    let emitted_set: HashSet<u32> = input.emitted_ids.iter().copied().collect();
    let observed_set: HashSet<u32> = input.observed.iter().map(|o| o.frame_id).collect();

    // Missing = emitted but never observed.
    let mut missing_ids: Vec<u32> = input
        .emitted_ids
        .iter()
        .copied()
        .filter(|id| !observed_set.contains(id))
        .collect();
    missing_ids.dedup();

    // Reorders = backward jumps in the observed stream.
    let mut reorders = Vec::new();
    for w in input.observed.windows(2) {
        if w[1].frame_id < w[0].frame_id {
            reorders.push((w[0].frame_id, w[1].frame_id));
        }
    }

    // Freezes = runs of identical consecutive IDs longer than the threshold.
    let period_ms = 1000.0 / input.capture_fps;
    let mut freezes = Vec::new();
    let mut i = 0;
    while i < input.observed.len() {
        let id = input.observed[i].frame_id;
        let mut j = i + 1;
        while j < input.observed.len() && input.observed[j].frame_id == id {
            j += 1;
        }
        let run = j - i;
        if (run as f64) > input.freeze_periods {
            freezes.push(Freeze {
                frame_id: id,
                repeat_count: run,
                duration_ms: run as f64 * period_ms,
            });
        }
        i = j;
    }

    // Latency = recv - gen for the FIRST observation of each id.
    let mut seen = HashSet::new();
    let mut lat_ms: Vec<f64> = Vec::new();
    for o in &input.observed {
        if seen.insert(o.frame_id) {
            lat_ms.push((o.recv_ts_ns - o.gen_ts_ns) as f64 / 1_000_000.0);
        }
    }
    let latency = if lat_ms.is_empty() {
        None
    } else {
        let mut sorted = lat_ms.clone();
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
    };

    // Verdict. Coverage: zero loss AND zero reorder. FullRate: report-only except reorder.
    let verdict_pass = match input.mode {
        PaintMode::Coverage => missing_ids.is_empty() && reorders.is_empty(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(frame_id: u32, gen: i64, recv: i64) -> Observed {
        Observed { frame_id, gen_ts_ns: gen, recv_ts_ns: recv }
    }

    fn input(mode: PaintMode, emitted: Vec<u32>, observed: Vec<Observed>) -> AnalysisInput {
        AnalysisInput { mode, emitted_ids: emitted, observed, capture_fps: 30.0, freeze_periods: 3.0 }
    }

    #[test]
    fn healthy_coverage_passes() {
        // ids 0..5, each seen once or twice, monotonic, ~10ms latency.
        let emitted = vec![0, 1, 2, 3, 4];
        let observed = vec![
            obs(0, 0, 10_000_000),
            obs(1, 33_000_000, 43_000_000),
            obs(1, 33_000_000, 43_000_000), // expected dup
            obs(2, 66_000_000, 76_000_000),
            obs(3, 99_000_000, 109_000_000),
            obs(4, 132_000_000, 142_000_000),
        ];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(r.verdict_pass);
        assert!(r.missing_ids.is_empty());
        assert!(r.reorders.is_empty());
        assert!(r.freezes.is_empty());
        let lat = r.latency.unwrap();
        assert_eq!(lat.samples, 5);
        assert!((lat.mean_ms - 10.0).abs() < 0.001);
    }

    #[test]
    fn missing_frame_fails_coverage() {
        let emitted = vec![0, 1, 2, 3];
        let observed = vec![obs(0, 0, 1), obs(1, 1, 2), obs(3, 3, 4)]; // 2 lost
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert!(!r.verdict_pass);
        assert_eq!(r.missing_ids, vec![2]);
    }

    #[test]
    fn freeze_is_detected_but_not_gated() {
        // id 1 captured 5x (run 5 > 3) = freeze; no loss, no reorder.
        let emitted = vec![0, 1, 2];
        let observed = vec![
            obs(0, 0, 1),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(1, 10, 11),
            obs(2, 20, 21),
        ];
        let r = analyze(input(PaintMode::Coverage, emitted, observed));
        assert_eq!(r.freezes.len(), 1);
        assert_eq!(r.freezes[0].frame_id, 1);
        assert_eq!(r.freezes[0].repeat_count, 5);
        assert!(r.verdict_pass); // freeze is reported, not gated in Phase 1
    }

    #[test]
    fn reorder_fails_both_modes() {
        let emitted = vec![0, 1, 2];
        let observed = vec![obs(0, 0, 1), obs(2, 2, 3), obs(1, 1, 4)]; // 2 then 1 = backward
        let cov = analyze(input(PaintMode::Coverage, emitted.clone(), observed.clone()));
        let full = analyze(input(PaintMode::FullRate, emitted, observed));
        assert!(!cov.verdict_pass);
        assert!(!full.verdict_pass);
        assert_eq!(cov.reorders, vec![(2, 1)]);
    }

    #[test]
    fn fullrate_missing_is_report_only() {
        // FullRate tolerates gaps (beat); only reorder gates.
        let emitted = vec![0, 1, 2, 3];
        let observed = vec![obs(0, 0, 1), obs(2, 2, 3), obs(3, 3, 4)]; // id 1 gap
        let r = analyze(input(PaintMode::FullRate, emitted, observed));
        assert!(r.verdict_pass);
        assert_eq!(r.missing_ids, vec![1]);
    }

    #[test]
    fn percentile_nearest_rank() {
        let s = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        assert_eq!(percentile(&s, 0.50), 5.0);
        assert_eq!(percentile(&s, 0.95), 10.0);
        assert_eq!(percentile(&s, 0.99), 10.0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test --features probe analyzer`
Expected: PASS (6 tests).

- [ ] **Step 3: Commit**

```bash
git add src/probe/analyzer.rs
git commit -m "feat(probe): analyzer — loss/freeze/reorder classification + latency stats"
```

---

## Task 6: Painter (hardware glue)

**Files:**
- Create: `src/probe/painter.rs`

- [ ] **Step 1: Implement (no unit test — hardware; logic is in tested modules)**

Create `src/probe/painter.rs`:

```rust
//! Painter thread: draw QR frames to /dev/fb0, paced, recording emitted IDs.

use crate::display::FramebufferDisplay;
use crate::probe::payload::Payload;
use crate::probe::qr::render_qr_bgra;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct PaintParams {
    pub run_id: u32,
    pub fb_device: String,
    pub paint_fps: f64,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub qr_size: u32,
}

/// Paint until `stop` is set. Records `(frame_id, gen_ts_ns)` of every emitted frame.
pub fn run_painter(
    params: PaintParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    emitted: Arc<Mutex<Vec<(u32, i64)>>>,
) -> Result<()> {
    let mut fb = FramebufferDisplay::open(&params.fb_device)?;
    let bgra_fourcc = u32::from_le_bytes(*b"BGRA");
    let period = Duration::from_secs_f64(1.0 / params.paint_fps);
    let mut frame_id: u32 = 0;
    let mut next = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        let gen_ts_ns = start.elapsed().as_nanos() as i64;
        let payload = Payload { run_id: params.run_id, frame_id, gen_ts_ns };
        let bgra = render_qr_bgra(&payload, params.canvas_w, params.canvas_h, params.qr_size);
        fb.display_frame(&bgra, params.canvas_w, params.canvas_h, bgra_fourcc)?;
        emitted.lock().unwrap().push((frame_id, gen_ts_ns));

        frame_id = frame_id.wrapping_add(1);
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now; // fell behind; don't accumulate debt
        }
    }
    tracing::info!("painter: emitted {} frames", frame_id);
    Ok(())
}
```

- [ ] **Step 2: Verify it compiles (clippy clean)**

Run: `cargo clippy --features probe --all-targets -- -D warnings`
Expected: PASS (may also flag later files; address painter-related warnings now).

- [ ] **Step 3: Commit**

```bash
git add src/probe/painter.rs
git commit -m "feat(probe): painter thread — QR to framebuffer, paced"
```

---

## Task 7: Reader (hardware glue)

**Files:**
- Create: `src/probe/reader.rs`

- [ ] **Step 1: Implement (no unit test — hardware)**

Create `src/probe/reader.rs`:

```rust
//! Reader thread: receive NDI, decode QR, record observed frames.

use crate::ndi::NdiReceiver;
use crate::probe::analyzer::Observed;
use crate::probe::luma::{bgra_to_luma, uyvy_to_luma};
use crate::probe::qr::decode_qr_luma;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct ReadParams {
    pub run_id: u32,
    pub source: String,
    pub connect_timeout_secs: u32,
}

/// Receive until `stop` is set. Records every decoded frame whose run_id matches.
pub fn run_reader(
    params: ReadParams,
    start: Instant,
    stop: Arc<AtomicBool>,
    observed: Arc<Mutex<Vec<Observed>>>,
) -> Result<()> {
    let mut rx = NdiReceiver::connect(&params.source, params.connect_timeout_secs)?;

    while !stop.load(Ordering::Relaxed) {
        let frame = match rx.capture_frame(100)? {
            Some(f) => f,
            None => continue,
        };
        let recv_ts_ns = start.elapsed().as_nanos() as i64;
        let img = match &frame.fourcc.to_le_bytes() {
            b"BGRA" | b"BGRX" => bgra_to_luma(&frame.data, frame.width, frame.height),
            _ => uyvy_to_luma(&frame.data, frame.width, frame.height),
        };
        if let Some(p) = decode_qr_luma(img) {
            if p.run_id == params.run_id {
                observed.lock().unwrap().push(Observed {
                    frame_id: p.frame_id,
                    gen_ts_ns: p.gen_ts_ns,
                    recv_ts_ns,
                });
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Verify clippy clean**

Run: `cargo clippy --features probe --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/probe/reader.rs
git commit -m "feat(probe): reader thread — NDI receive, QR decode, record"
```

---

## Task 8: Run orchestrator (hardware glue)

**Files:**
- Create: `src/probe/run.rs`

- [ ] **Step 1: Implement**

Create `src/probe/run.rs`:

```rust
//! Orchestrate painter + reader for a fixed duration, then analyze.

use crate::probe::analyzer::{analyze, AnalysisInput, AnalysisReport, Observed, PaintMode};
use crate::probe::painter::{run_painter, PaintParams};
use crate::probe::reader::{run_reader, ReadParams};
use anyhow::Result;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct RunConfig {
    pub mode: PaintMode,
    pub run_id: u32,
    pub source: String,
    pub fb_device: String,
    pub duration: Duration,
    pub paint_fps: f64,
    pub capture_fps: f64,
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub qr_size: u32,
    pub freeze_periods: f64,
    pub connect_timeout_secs: u32,
}

pub fn run(cfg: RunConfig) -> Result<AnalysisReport> {
    let start = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let emitted: Arc<Mutex<Vec<(u32, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let observed: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));

    // Reader connects to the always-on NDI source (camera-box is already sending).
    let reader_handle = {
        let stop = stop.clone();
        let observed = observed.clone();
        let params = ReadParams {
            run_id: cfg.run_id,
            source: cfg.source.clone(),
            connect_timeout_secs: cfg.connect_timeout_secs,
        };
        std::thread::spawn(move || run_reader(params, start, stop, observed))
    };

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

    std::thread::sleep(cfg.duration);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);

    painter_handle.join().expect("painter panicked")?;
    reader_handle.join().expect("reader panicked")?;

    let emitted_ids: Vec<u32> = emitted.lock().unwrap().iter().map(|(id, _)| *id).collect();
    let observed_vec = observed.lock().unwrap().clone();

    Ok(analyze(AnalysisInput {
        mode: cfg.mode,
        emitted_ids,
        observed: observed_vec,
        capture_fps: cfg.capture_fps,
        freeze_periods: cfg.freeze_periods,
    }))
}
```

- [ ] **Step 2: Verify clippy clean**

Run: `cargo clippy --features probe --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add src/probe/run.rs
git commit -m "feat(probe): run orchestrator — painter+reader threads → report"
```

---

## Task 9: `frame-probe` binary (CLI + artifact)

**Files:**
- Create: `src/bin/frame-probe.rs`

- [ ] **Step 1: Implement**

Create `src/bin/frame-probe.rs`:

```rust
//! frame-probe: cam2 HDMI-loopback frame-loss/latency probe (Phase 1).

use anyhow::Result;
use camera_box::probe::analyzer::PaintMode;
use camera_box::probe::run::{run, RunConfig};
use clap::Parser;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(about = "QR HDMI-loopback frame-loss/latency probe")]
struct Args {
    /// coverage = clean zero-loss gate; full-rate = realistic stress
    #[arg(long, default_value = "coverage")]
    mode: String,
    /// NDI source substring to receive (e.g. "usb (CAM2)")
    #[arg(long, default_value = "usb (CAM2)")]
    source: String,
    /// Framebuffer device (HDMI out)
    #[arg(long, default_value = "/dev/fb0")]
    fb_device: String,
    /// Run duration in seconds
    #[arg(long, default_value_t = 300)]
    duration_secs: u64,
    /// Painter rate (defaults: coverage 24, full-rate 30)
    #[arg(long)]
    paint_fps: Option<f64>,
    /// Expected capture rate
    #[arg(long, default_value_t = 30.0)]
    capture_fps: f64,
    /// QR pixel size on the canvas
    #[arg(long, default_value_t = 700)]
    qr_size: u32,
    /// Freeze threshold in capture periods
    #[arg(long, default_value_t = 3.0)]
    freeze_periods: f64,
    /// NDI connect timeout (seconds)
    #[arg(long, default_value_t = 30)]
    connect_timeout_secs: u32,
    /// JSON artifact output path
    #[arg(long, default_value = "/tmp/frame-probe.json")]
    out: String,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let mode = match args.mode.as_str() {
        "coverage" => PaintMode::Coverage,
        "full-rate" | "fullrate" => PaintMode::FullRate,
        other => anyhow::bail!("unknown mode '{}' (use coverage|full-rate)", other),
    };
    let paint_fps = args.paint_fps.unwrap_or(match mode {
        PaintMode::Coverage => 24.0,
        PaintMode::FullRate => 30.0,
    });
    let run_id = (SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() & 0xFFFF_FFFF) as u32;

    tracing::info!(
        "frame-probe start: mode={:?} run_id={} source={:?} paint_fps={} dur={}s",
        mode, run_id, args.source, paint_fps, args.duration_secs
    );

    let report = run(RunConfig {
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
    })?;

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
}
```

- [ ] **Step 2: Build the binary**

Run: `cargo build --features probe --bin frame-probe`
Expected: PASS. Run `cargo build` (no feature) — Expected: still PASS, frame-probe not built.

- [ ] **Step 3: Full local gate**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features -- -D warnings && cargo test --features probe`
Expected: all PASS (15 unit tests across payload/luma/qr/analyzer).

- [ ] **Step 4: Commit**

```bash
git add src/bin/frame-probe.rs
git commit -m "feat(probe): frame-probe CLI + JSON artifact + verdict exit code"
```

---

## Task 10: CI — run probe unit tests; exclude hardware glue from coverage

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Run probe tests in the `test` job**

In `.github/workflows/ci.yml`, change the test step (currently `cargo test --verbose`) to include the feature:

```yaml
      - name: Run tests
        run: cargo test --all-features --verbose
```

- [ ] **Step 2: Keep coverage honest — exclude untestable hardware glue**

In the `coverage` job, change the coverage step to ignore the hardware-only files (which have no unit tests by design) so the threshold reflects testable logic:

```yaml
      - name: Generate coverage report
        run: cargo llvm-cov --all-features --lcov --output-path lcov.info --ignore-filename-regex 'src/probe/(painter|reader|run)\.rs|src/bin/frame-probe\.rs' --fail-under-lines ${{ vars.COVERAGE_THRESHOLD || '47' }}
```

- [ ] **Step 3: Verify coverage locally does not regress**

Run: `cargo llvm-cov --all-features --ignore-filename-regex 'src/probe/(painter|reader|run)\.rs|src/bin/frame-probe\.rs' --fail-under-lines 47`
Expected: PASS (≥ 47%). The pure probe modules (payload/luma/qr/analyzer) are well covered and should hold or raise the number. If it dips, add assertions to the pure modules — never lower the threshold.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run probe unit tests; exclude probe hardware glue from coverage"
```

---

## Task 11: dev1 → cam2 orchestration script + hardware evidence run

**Files:**
- Create: `scripts/loopback-e2e.sh`

- [ ] **Step 1: Write the orchestration script**

Create `scripts/loopback-e2e.sh`:

```bash
#!/usr/bin/env bash
# Phase-1 NDI frame-loss/latency loopback E2E.
# Builds frame-probe on dev1, deploys to cam2 (the rig), runs the loopback,
# pulls the JSON artifact, and exits non-zero on a failing verdict.
set -euo pipefail

CAM_IP="${CAM_IP:-10.77.9.62}"
CAM_PASS="${CAM_PASS:-newlevel}"
SOURCE="${SOURCE:-usb (CAM2)}"
MODE="${MODE:-coverage}"
DURATION_SECS="${DURATION_SECS:-300}"
REMOTE_BIN="/tmp/frame-probe"
REMOTE_OUT="/tmp/frame-probe.json"
LOCAL_OUT="${LOCAL_OUT:-./frame-probe-${MODE}.json}"

ssh_cam() { sshpass -p "$CAM_PASS" ssh -o StrictHostKeyChecking=no -o ConnectTimeout=10 "root@${CAM_IP}" "$@"; }
scp_to_cam() { sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no "$1" "root@${CAM_IP}:$2"; }
scp_from_cam() { sshpass -p "$CAM_PASS" scp -o StrictHostKeyChecking=no "root@${CAM_IP}:$1" "$2"; }

echo ">> Building frame-probe (release, --features probe)"
cargo build --release --features probe --bin frame-probe

echo ">> Deploying to ${CAM_IP}"
scp_to_cam target/release/frame-probe "$REMOTE_BIN"
ssh_cam "chmod +x ${REMOTE_BIN}"

echo ">> Best-effort: stop console blanking/cursor on fb0 (ignore failures)"
ssh_cam "setterm --blank 0 --powerdown 0 >/dev/null 2>&1 || true; printf '\033[?25l' > /dev/tty1 2>/dev/null || true"

echo ">> Running ${MODE} loopback for ${DURATION_SECS}s on ${CAM_IP}"
echo "   (NDI source: '${SOURCE}') — this paints QR on cam2 HDMI-out; camera-box keeps sending NDI"
ssh_cam "NDI_RUNTIME_DIR_V6=/usr/lib/ndi ${REMOTE_BIN} --mode '${MODE}' --source '${SOURCE}' --duration-secs ${DURATION_SECS} --out ${REMOTE_OUT}" \
  && VERDICT_RC=0 || VERDICT_RC=$?

echo ">> Pulling artifact"
scp_from_cam "$REMOTE_OUT" "$LOCAL_OUT"
echo ">> Artifact saved to ${LOCAL_OUT}"
cat "$LOCAL_OUT"

if [ "$VERDICT_RC" -ne 0 ]; then
  echo "!! FRAME-PROBE FAILED (rc=${VERDICT_RC}) — see ${LOCAL_OUT}"
  exit "$VERDICT_RC"
fi
echo ">> PASS"
```

- [ ] **Step 2: Make executable + commit (script ships before first run, per script-failure-policy)**

```bash
chmod +x scripts/loopback-e2e.sh
git add scripts/loopback-e2e.sh
git commit -m "feat(probe): dev1->cam2 loopback E2E orchestration script"
```

- [ ] **Step 3: Smoke run — prove the loop decodes ≥1 frame (10s)**

Run: `DURATION_SECS=10 ./scripts/loopback-e2e.sh`
Expected: artifact shows `observed_count > 0` and a non-empty `latency`. If `observed_count == 0`:
  - the HDMI-out → HDMI-in cable may not be connected, OR
  - the NDI source name differs — list sources on cam2 and adjust `SOURCE`, OR
  - camera-box owns `/dev/fb0` (a `[display]` config). Verify the loop functionally: stop camera-box briefly (rig is off-air-freely), paint a frame, read `/dev/video0`. Fix root cause; do not loosen the test.

- [ ] **Step 4: Full evidence run — the Phase-1 gate (5 min coverage)**

Run: `DURATION_SECS=300 MODE=coverage ./scripts/loopback-e2e.sh`
Expected: `VERDICT=PASS`, `missing=0`, `reorders=0`; latency stats recorded. This is the zero-loss evidence artifact. If it FAILS with real losses, that is a genuine finding — investigate the camera-box path (V4L2 overrun, NDI send stall, genlock pacing); do not adjust the gate.

- [ ] **Step 5: Baseline the stress path (5 min full-rate)**

Run: `DURATION_SECS=300 MODE=full-rate ./scripts/loopback-e2e.sh`
Expected: artifact with latency percentiles + any freezes — the baseline from which a hard latency/freeze bound will be set in a follow-up. (Full-rate gates only on reorders in Phase 1.)

- [ ] **Step 6: Record the baseline numbers in the spec**

Append the observed p50/p95/p99 latency and freeze counts (both modes) as a short "Phase-1 baseline (YYYY-MM-DD)" note at the bottom of `docs/superpowers/specs/2026-06-08-ndi-frame-loss-e2e-design.md`, then commit:

```bash
git add docs/superpowers/specs/2026-06-08-ndi-frame-loss-e2e-design.md
git commit -m "docs: record Phase-1 loopback latency baseline"
```

---

## Final verification (before PR)

- [ ] `cargo fmt --all --check` — PASS
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` — PASS
- [ ] `cargo test --all-features` — PASS (all probe unit tests)
- [ ] `cargo build` (no feature) — production binary unaffected
- [ ] Coverage ≥ 47% with the probe hardware-glue exclusion
- [ ] Coverage run on cam2: `VERDICT=PASS`, `missing=0`, `reorders=0`, artifact captured
- [ ] Full-rate baseline captured
- [ ] Spec acceptance criteria (§13) all checked

## Notes / risks (from spec §12)

- **Loop cable presence** is the top unknown — Task 11 Step 3 surfaces it immediately.
- **fb0/fbcon contention**: the script best-effort disables console blanking; coverage mode holds each frame stable, defeating tearing. Escalate to DRM page-flip only if decode proves unreliable.
- **Artifact location**: written to `/tmp` on cam2 (avoids rootfs remount). If `/tmp` is `noexec`/`nowrite`, switch to `/run` or deploy to `/usr/local/bin` via the remount pattern.
- **`cargo audit`** now covers the new deps — keep them current if an advisory fires.
- **run_id** uses `SystemTime` (a normal binary, allowed); it scopes a run so stale frames from a prior run are ignored.
```
