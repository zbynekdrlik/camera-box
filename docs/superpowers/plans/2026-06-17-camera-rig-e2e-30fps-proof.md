# Real-Camera-Rig E2E 30 fps Zero-Loss Proof — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove every 30 fps frame of the real camera rig (cam2 paint → monitor → camera → cam1 ShadowCast → strih OBS → stream OBS) reaches the stream program output 1:1, with a dual-QR anti-blur source, a visual graph report, and a ≥300 s honesty gate.

**Architecture:** Extend the existing Phase-2 multitap harness. Source tap moves to cam1; the painter shows two phase-offset QR regions so every camera frame is readable (CRC rejects the blurred one); the strict verdict is anchored at cam1 (TAP A) → stream PGM (TAP C); a Python report renders two graphs + a per-hop table delivered as a LAN URL.

**Tech Stack:** Rust (camera-box bins/libs, `qrcode`/`rqrr`/`image`, `serde`), Python 3 + matplotlib (report), bash + obs-websocket (orchestration), v4l2-ctl (capture rate), NDI/DistroAV.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-06-17-camera-rig-e2e-30fps-proof-design.md` (verbatim authority).
- Local builds: Tier 0 — run `cargo fmt --all --check`, `cargo check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --no-run` before push; never `cargo build --release` locally (CI builds the binary).
- Two-branch: work on `dev`. Bump version FIRST (Task 0) before any code.
- Anchor: strict zero-loss verdict = cam1 (TAP A) → stream PGM (TAP C). painter→cam1 is report-only (optical hop; 60 Hz logical vs 30 fps capture = expected downsampling, not loss).
- Duration: no zero-loss PASS may be emitted for a measured steady-state window < 300 s.
- Boxes: cam1 `10.77.9.61`, cam2 `10.77.9.62` (root/newlevel); strih `10.77.9.202`, stream `10.77.9.204`; dev1 `10.77.9.21`. NDI source cam1 = `CAM1 (usb)`.
- Do NOT edit production OBS scenes; use the dedicated PHASE2-PROBE scene only.
- Pure logic is unit-tested (RED→GREEN, test commit before impl commit); hardware/NDI glue (`multi_reader.rs`, painter present path, scripts) is excluded from unit tests and verified on the rig.

---

### Task 0: Version bump

**Files:**
- Modify: `Cargo.toml` (version)

- [ ] **Step 1: Bump version on dev**

```bash
git fetch origin && git merge --ff-only origin/dev 2>/dev/null || true
# current dev binary is 1.7.0-dev.47; bump the -dev suffix
sed -i 's/^version = "1.7.0-dev.47"/version = "1.7.0-dev.48"/' Cargo.toml
grep '^version' Cargo.toml
```

- [ ] **Step 2: Commit**

```bash
git add Cargo.toml
git commit -m "chore: bump to 1.7.0-dev.48 for camera-rig e2e 30fps proof"
```

---

### Task 1: Configurable native capture fps (true 30 fps, kill the forced 60)

**Files:**
- Modify: `src/capture.rs:126-141` (hard-coded `denominator = 60`)
- Test: `src/capture.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `pub fn requested_capture_denominator(override_fps: Option<u32>) -> u32` — the v4l2 interval denominator to request (frames/sec). `None`/`Some(0)` ⇒ 60 (unchanged default); `Some(f)` ⇒ `f`.
- Consumes (runtime): env `CAMERA_BOX_CAPTURE_FPS` parsed in `VideoCapture::open`.

- [ ] **Step 1: Write the failing test**

In `src/capture.rs` tests module:

```rust
#[test]
fn capture_denominator_defaults_to_60_and_honors_override() {
    assert_eq!(requested_capture_denominator(None), 60);
    assert_eq!(requested_capture_denominator(Some(0)), 60); // 0 is invalid -> default
    assert_eq!(requested_capture_denominator(Some(30)), 30);
    assert_eq!(requested_capture_denominator(Some(60)), 60);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib capture_denominator_defaults_to_60 -- --nocapture`
Expected: FAIL — `requested_capture_denominator` not found.

- [ ] **Step 3: Write minimal implementation**

Add near the top of `src/capture.rs` (after `frame_rate_from_interval`):

```rust
/// The v4l2 capture-interval denominator (frames/sec) to request. The rig runs a
/// true-30 fps chain (no 60→30 decimation), so `CAMERA_BOX_CAPTURE_FPS=30` lets the
/// device negotiate native 30; unset / 0 / invalid keeps the 60 fps default (#11).
pub fn requested_capture_denominator(override_fps: Option<u32>) -> u32 {
    override_fps.filter(|&f| f > 0).unwrap_or(60)
}
```

Then in `VideoCapture::open`, replace the hard-coded `params.interval.denominator = 60;` (around line 133) with:

```rust
                params.interval.numerator = 1;
                let req = requested_capture_denominator(
                    std::env::var("CAMERA_BOX_CAPTURE_FPS").ok().and_then(|s| s.parse().ok()),
                );
                params.interval.denominator = req;
```

(Leave the `Err(_) => frame_rate_from_interval(1, 60)` fallback as-is.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib capture_denominator_defaults_to_60`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/capture.rs
git commit -m "feat(capture): configurable native capture fps via CAMERA_BOX_CAPTURE_FPS (true 30fps)"
```

---

### Task 2: Dual-QR render — two QR regions side by side

**Files:**
- Modify: `src/probe/qr.rs:11-37` (refactor `render_qr_bgra` to reuse a blit helper; add dual render)
- Test: `src/probe/qr.rs` tests

**Interfaces:**
- Produces: `pub fn render_qr_dual_bgra(left: &Payload, right: &Payload, canvas_w: u32, canvas_h: u32, qr_size: u32) -> Vec<u8>` — LEFT payload centered in the left half `[0, canvas_w/2)`, RIGHT payload centered in the right half `[canvas_w/2, canvas_w)`, both on a white BGRA canvas.
- Consumes: existing `bgra_to_luma`, `crop_center` for the test decode.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dual_render_places_two_decodable_qrs_left_and_right() {
    let l = Payload { run_id: 7, frame_id: 100, gen_ts_ns: 1 };
    let r = Payload { run_id: 7, frame_id: 101, gen_ts_ns: 2 };
    let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
    let bgra = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    assert_eq!(bgra.len(), (cw * ch * 4) as usize);
    let full = bgra_to_luma(&bgra, cw, ch, cw * 4);
    // Left half image and right half image each decode to their own payload.
    let left_img = image::imageops::crop_imm(&full, 0, 0, cw / 2, ch).to_image();
    let right_img = image::imageops::crop_imm(&full, cw / 2, 0, cw / 2, ch).to_image();
    assert_eq!(decode_qr_luma(left_img), Some(l));
    assert_eq!(decode_qr_luma(right_img), Some(r));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib dual_render_places_two_decodable_qrs`
Expected: FAIL — `render_qr_dual_bgra` not found.

- [ ] **Step 3: Write minimal implementation**

Refactor `render_qr_bgra` to blit at an x-offset, then add the dual renderer:

```rust
/// Blit `payload`'s QR (EC-H), centered within the horizontal band
/// `[band_x, band_x + band_w)`, onto an existing white BGRA `canvas`.
fn blit_qr_bgra(canvas: &mut [u8], canvas_w: u32, canvas_h: u32, band_x: u32, band_w: u32, payload: &Payload, qr_size: u32) {
    let s = payload.encode();
    let code = QrCode::with_error_correction_level(s.as_bytes(), EcLevel::H)
        .expect("payload is small, encodes within QR capacity");
    let qr: GrayImage = code.render::<Luma<u8>>()
        .min_dimensions(qr_size, qr_size).max_dimensions(qr_size, qr_size)
        .quiet_zone(true).build();
    let (qw, qh) = (qr.width().min(band_w), qr.height().min(canvas_h));
    let ox = band_x + (band_w - qw) / 2;
    let oy = (canvas_h - qh) / 2;
    for y in 0..qh {
        for x in 0..qw {
            let lum = qr.get_pixel(x, y)[0];
            let ci = (((oy + y) * canvas_w + (ox + x)) * 4) as usize;
            canvas[ci] = lum; canvas[ci + 1] = lum; canvas[ci + 2] = lum; canvas[ci + 3] = 255;
        }
    }
}

/// Two QRs side by side: `left` centered in `[0, w/2)`, `right` in `[w/2, w)`.
pub fn render_qr_dual_bgra(left: &Payload, right: &Payload, canvas_w: u32, canvas_h: u32, qr_size: u32) -> Vec<u8> {
    let mut canvas = vec![255u8; (canvas_w * canvas_h * 4) as usize];
    let half = canvas_w / 2;
    blit_qr_bgra(&mut canvas, canvas_w, canvas_h, 0, half, left, qr_size);
    blit_qr_bgra(&mut canvas, canvas_w, canvas_h, half, canvas_w - half, right, qr_size);
    canvas
}
```

Rewrite the existing `render_qr_bgra` to call `blit_qr_bgra(&mut canvas, canvas_w, canvas_h, 0, canvas_w, payload, qr_size)` (keeps its centered-single behavior + its existing tests green).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib qr::` (run the whole qr module so the existing single-QR tests stay green)
Expected: PASS (new dual test + all existing render/decode tests).

- [ ] **Step 5: Commit**

```bash
git add src/probe/qr.rs
git commit -m "feat(qr): render two side-by-side QR regions (dual-QR Vernier source)"
```

---

### Task 3: Dual-ROI decode + CRC reconciliation

**Files:**
- Modify: `src/probe/qr.rs`
- Test: `src/probe/qr.rs` tests

**Interfaces:**
- Produces: `pub fn decode_capture_dual(fourcc: u32, data: &[u8], width: u32, height: u32, stride: u32, roi: u32) -> Option<Payload>` — decode the LEFT and RIGHT halves independently; of the CRC-valid payloads (a blurred QR fails CRC and is dropped), return the one with the **highest `frame_id`** (the freshest sharp region). `None` only when neither half decodes.
- Consumes: `bgra_to_luma`/`uyvy_to_luma`, `crop_center`, `decode_qr_luma`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dual_decode_returns_highest_frame_id_and_tolerates_one_blurred() {
    let l = Payload { run_id: 7, frame_id: 200, gen_ts_ns: 1 };
    let r = Payload { run_id: 7, frame_id: 201, gen_ts_ns: 2 };
    let (cw, ch, qs) = (1920u32, 1080u32, 520u32);
    let fourcc = u32::from_le_bytes(*b"BGRA");

    // Both sharp -> highest frame_id (201).
    let both = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    assert_eq!(decode_capture_dual(fourcc, &both, cw, ch, cw * 4, 620), Some(r));

    // Right region blanked (simulating an unreadable/blurred QR) -> falls back to left (200).
    let l_only = render_qr_dual_bgra(&l, &r, cw, ch, qs);
    let mut blanked = l_only.clone();
    let half = (cw / 2) as usize;
    for y in 0..ch as usize {
        for x in half..cw as usize {
            let i = (y * cw as usize + x) * 4;
            blanked[i] = 255; blanked[i + 1] = 255; blanked[i + 2] = 255; blanked[i + 3] = 255;
        }
    }
    assert_eq!(decode_capture_dual(fourcc, &blanked, cw, ch, cw * 4, 620), Some(l));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib dual_decode_returns_highest_frame_id`
Expected: FAIL — `decode_capture_dual` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Decode both QR regions of a dual-QR frame and reconcile. Each half is converted
/// to luma, the QR-ROI cropped, and decoded; a blurred (mid-transition) QR fails
/// CRC inside `Payload::decode` and is silently dropped. The frame's identity is
/// the CRC-valid payload with the highest `frame_id` (freshest sharp region); at
/// least one region is always sharp on the Vernier display, so this returns `Some`
/// for every well-framed capture. `None` only when neither half decodes.
pub fn decode_capture_dual(fourcc: u32, data: &[u8], width: u32, height: u32, stride: u32, roi: u32) -> Option<Payload> {
    let full = match &fourcc.to_le_bytes() {
        b"BGRA" | b"BGRX" => bgra_to_luma(data, width, height, stride),
        _ => uyvy_to_luma(data, width, height, stride),
    };
    let half = width / 2;
    let left = image::imageops::crop_imm(&full, 0, 0, half, height).to_image();
    let right = image::imageops::crop_imm(&full, half, 0, width - half, height).to_image();
    let roi = roi.min(half).min(height);
    let cand = [
        decode_qr_luma(crop_center(&left, roi, roi)),
        decode_qr_luma(crop_center(&right, roi, roi)),
    ];
    cand.into_iter().flatten().max_by_key(|p| p.frame_id)
}
```

(Confirm `crop_center(&GrayImage, w, h) -> GrayImage` signature in `src/probe/luma.rs`; if it takes the full image and centers, the half-images above are already cropped to a half so `crop_center` re-centers within the half — correct.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib qr::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/probe/qr.rs
git commit -m "feat(qr): decode_capture_dual — CRC-reconcile two ROIs to highest frame_id"
```

---

### Task 4: Painter Vernier phase logic + dual paint

**Files:**
- Modify: `src/probe/painter.rs`
- Test: `src/probe/painter.rs` tests

**Interfaces:**
- Produces: `pub fn vernier_ids(refresh_tick: u64) -> (u32, u32)` — `(left_id, right_id)` for refresh counter `refresh_tick`. LEFT holds the latest EVEN tick, RIGHT the latest ODD tick: `left = tick & !1`, `right = if tick == 0 { 0 } else { (tick - 1) | 1 }` capped so `right <= tick`. On an even tick LEFT is fresh; on odd, RIGHT is fresh. Both as `u32` (wrap via `as u32`).
- Consumes: `render_qr_dual_bgra` (Task 2).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn vernier_ids_interleave_even_left_odd_right() {
    assert_eq!(vernier_ids(0), (0, 0)); // tick 0: left fresh=0, no odd yet -> right 0
    assert_eq!(vernier_ids(1), (0, 1)); // right updates to 1
    assert_eq!(vernier_ids(2), (2, 1)); // left updates to 2
    assert_eq!(vernier_ids(3), (2, 3)); // right updates to 3
    assert_eq!(vernier_ids(4), (4, 3));
    // The fresh side equals the tick; the other is the previous parity -> the two
    // are never both freshly-changed on the same tick (the anti-blur guarantee).
    for t in 1..1000u64 {
        let (l, r) = vernier_ids(t);
        let fresh_is_left = t % 2 == 0;
        if fresh_is_left { assert_eq!(l as u64, t); } else { assert_eq!(r as u64, t); }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib vernier_ids_interleave`
Expected: FAIL — `vernier_ids` not found.

- [ ] **Step 3: Write minimal implementation**

```rust
/// Vernier dual-QR ids for refresh counter `tick`. LEFT carries the latest EVEN
/// tick, RIGHT the latest ODD tick, so exactly one region changes per refresh and
/// the two are never freshly-painted on the same refresh — at least one is settled
/// (sharp) when the camera fires (the anti-blur guarantee, spec §dual-QR).
pub fn vernier_ids(tick: u64) -> (u32, u32) {
    let left = tick & !1; // latest even <= tick
    let right = if tick == 0 { 0 } else { (tick - 1) | 1 }.min(tick); // latest odd <= tick
    (left as u32, right as u32)
}
```

Then add a dual-paint path to `run_painter` (gated by a new `PaintParams.dual_qr: bool`): when `dual_qr`, drive a `refresh_tick` (incremented every `present()`), compute `(l, r) = vernier_ids(tick)`, build payloads with `frame_id = max(l, r)` recorded into `emitted` as the logical id, render via `render_qr_dual_bgra`, present. Keep the single-QR path unchanged when `dual_qr == false`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib painter::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/probe/painter.rs
git commit -m "feat(painter): Vernier dual-QR phase logic + dual paint path"
```

---

### Task 5: frame-probe + readers use the dual paths

**Files:**
- Modify: `src/bin/frame-probe.rs` (add `--dual-qr` flag → `PaintParams.dual_qr`)
- Modify: `src/probe/multi_reader.rs:106` (use `decode_capture_dual` when dual mode)
- Modify: `src/probe/reader.rs` (same, single-box path) — only if it shares the decode
- Modify: `src/bin/multitap-probe.rs` (add `--dual-qr` flag → `TapSpec.dual`)

**Interfaces:**
- Consumes: `decode_capture_dual` (Task 3), `PaintParams.dual_qr` / `TapSpec.dual` (Task 4).

- [ ] **Step 1:** Add `dual: bool` to `TapSpec`; in `tap_loop` call `decode_capture_dual(...)` when `spec.dual`, else `decode_capture(...)`. (No unit test — `multi_reader` is hardware glue, excluded from coverage per its header.)
- [ ] **Step 2:** Add `--dual-qr` (`default_value_t = false`) to `multitap-probe` Args; thread into each `TapSpec.dual`.
- [ ] **Step 3:** Add `--dual-qr` to `frame-probe`; set `PaintParams.dual_qr`.
- [ ] **Step 4:** `cargo check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --no-run`
- [ ] **Step 5: Commit**

```bash
git add src/bin/frame-probe.rs src/bin/multitap-probe.rs src/probe/multi_reader.rs src/probe/reader.rs
git commit -m "feat(probe): wire dual-QR paint + dual-ROI decode behind --dual-qr"
```

---

### Task 6: Per-output 30 fps grid-continuity check

**Files:**
- Modify: `src/probe/differ.rs`
- Test: `src/probe/differ.rs` tests

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize)]
  pub struct GridContinuity {
      pub first_id: u32,
      pub last_id: u32,
      pub expected: usize,        // last-first+1 (logical grid slots in span)
      pub present: usize,         // distinct ids present
      pub missing_slots: Vec<u32>,// absent ids in span = output starvation
      pub repeated_ids: Vec<u32>, // ids appearing >1x = FIFO underrun repeats
  }
  pub fn grid_continuity(observed: &[Observed], stride: u32) -> GridContinuity
  ```
  `stride` = the logical-id step a single output advances per frame (1 for an OBS 30 fps output decoding a 30 fps id sequence; 2 if it decodes the 60 Hz dual-QR logical counter). A slot is "missing" only at multiples of `stride` within the span; an id seen more than once is a repeat. `is_clean()` = `missing_slots.is_empty() && repeated_ids.is_empty() && present >= 2`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn grid_continuity_flags_starvation_and_underrun() {
    // ids 10..=14 each once, stride 1 -> clean.
    let clean: Vec<Observed> = (10..=14).map(|i| o(i, i as i64)).collect();
    let g = grid_continuity(&clean, 1);
    assert!(g.missing_slots.is_empty() && g.repeated_ids.is_empty());
    assert!(g.is_clean());

    // id 12 missing (starvation), id 13 repeated (underrun).
    let bad = vec![o(10,0), o(11,1), o(13,2), o(13,3), o(14,4)];
    let g = grid_continuity(&bad, 1);
    assert_eq!(g.missing_slots, vec![12]);
    assert_eq!(g.repeated_ids, vec![13]);
    assert!(!g.is_clean());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib grid_continuity_flags`
Expected: FAIL — `grid_continuity` not found.

- [ ] **Step 3: Write minimal implementation** — add `GridContinuity` + `grid_continuity` to `differ.rs` (count occurrences per id over `[min..=max]` stepping by `stride`; missing = count 0, repeated = count > 1).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib differ::grid`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/probe/differ.rs
git commit -m "feat(differ): per-output 30fps grid-continuity (starvation + underrun) check"
```

---

### Task 7: multitap-probe — emit grid-continuity + per-frame series; duration honesty gate

**Files:**
- Modify: `src/bin/multitap-probe.rs`
- Test: `src/bin/multitap-probe.rs` tests (pure gate fn)

**Interfaces:**
- Produces: `pub fn zero_loss_window_ok(measured_window_secs: f64, min_secs: f64) -> bool` (`measured_window_secs >= min_secs`); a `--min-zero-loss-secs` Arg (default 300). When a run claims `verdict_pass` (zero loss) but the measured steady-state window < `--min-zero-loss-secs`, downgrade the printed claim to `INCONCLUSIVE (window < {min}s)` and exit non-zero.
- Add `grid: Vec<GridContinuity>` (one per tap) and the per-frame series to `MultiTapReport`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn zero_loss_requires_min_window() {
    assert!(zero_loss_window_ok(300.0, 300.0));   // exactly the floor passes
    assert!(zero_loss_window_ok(1800.0, 300.0));
    assert!(!zero_loss_window_ok(120.0, 300.0));   // 120s ad-hoc cannot claim zero-loss
    assert!(!zero_loss_window_ok(299.9, 300.0));
}
```

- [ ] **Step 2: Run test to verify it fails** — `cargo test --bin multitap-probe zero_loss_requires_min_window` → FAIL.
- [ ] **Step 3: Implement** `zero_loss_window_ok`; add `--min-zero-loss-secs` (default 300); compute the measured window = `duration - lead_discard - settle`; fold into the final verdict (a PASS with window < floor prints `INCONCLUSIVE` + exits 1). Compute `grid_continuity` per trimmed tap (stride 1) and push to the report; write a per-frame JSONL series `{tap, frame_id, recv_ts_ns, node_emit_tc_ns}` to `--out`.series.jsonl for the report graphs.
- [ ] **Step 4: Run test** — `cargo test --bin multitap-probe` → PASS.
- [ ] **Step 5: Commit**

```bash
git add src/bin/multitap-probe.rs
git commit -m "feat(multitap): grid-continuity per tap + per-frame series + >=300s zero-loss gate"
```

---

### Task 8: Report generator — two graphs + per-hop table → LAN URL

**Files:**
- Create: `scripts/e2e-report.py`

**Interfaces:**
- Consumes: the `multitap-probe` JSON (`--out`) + the per-frame series JSONL (Task 7).
- Produces: one PNG (`/tmp/e2e-report-<run_id>.png`) and prints the `airuleset.py share` URL.

- [ ] **Step 1:** Write `scripts/e2e-report.py` (argparse `--json`, `--series`, `--out`). Using matplotlib:
  - **Graph 1 (top), delivery continuity:** scatter, x = frame_id, four y-lanes (Painted / cam1 / strih PGM / stream PGM); one marker per delivered id per lane; gaps in a lane = dropped frames. Title shows per-lane delivered counts.
  - **Graph 2 (bottom), per-frame latency:** line per hop (cam1→strih, strih→stream emit-latency) + absolute (painter→stream); y = ms; annotate p50/p99.
  - **Footer table:** per hop unique up/down, dropped, %, p50/p99, verdict; + optical-hop readability % (painter vs cam1).
- [ ] **Step 2:** Save the PNG, then `python3 ~/devel/airuleset/airuleset.py share <png>` and print the URL (per `deliver-files-as-urls.md`).
- [ ] **Step 3:** Smoke-run against a synthetic JSON+series fixture committed under `scripts/testdata/` to prove it renders without a live rig:

```bash
python3 scripts/e2e-report.py --json scripts/testdata/sample-report.json --series scripts/testdata/sample-series.jsonl --out /tmp/e2e-report-test.png
test -s /tmp/e2e-report-test.png && echo "PNG rendered"
```

- [ ] **Step 4: Commit**

```bash
git add scripts/e2e-report.py scripts/testdata/sample-report.json scripts/testdata/sample-series.jsonl
git commit -m "feat(e2e): report.py — delivery-continuity + latency graphs + per-hop table -> LAN URL"
```

---

### Task 9: Orchestrator — cam1 source, true-30 capture, dedicated scene, ≥300 s, report

**Files:**
- Modify: `scripts/multitap-e2e.sh`
- Modify: `scripts/obs_phase2.py`

- [ ] **Step 1:** In `multitap-e2e.sh`: change the source tap from `cam="CAM2 (usb)"` to `cam="CAM1 (usb)"`; painter still runs on cam2 (`10.77.9.62`); add `--dual-qr` to both the `frame-probe` and `multitap-probe` invocations.
- [ ] **Step 2:** Before starting the painter, set cam1 to true 30 fps:

```bash
sshpass -p 'newlevel' ssh -o StrictHostKeyChecking=no root@10.77.9.61 \
  'systemctl set-environment CAMERA_BOX_CAPTURE_FPS=30; systemctl restart camera-box'
# (teardown restores: systemctl unset-environment CAMERA_BOX_CAPTURE_FPS; systemctl restart camera-box)
```

(If `camera-box.service` ignores `systemctl set-environment`, write a drop-in `Environment=CAMERA_BOX_CAPTURE_FPS=30` under `/etc/systemd/system/camera-box.service.d/` and `daemon-reload`; remove on teardown.)

- [ ] **Step 3:** Enforce the duration floor: default `--duration-secs 1800`, pass `--min-zero-loss-secs 300`; reject `--duration-secs < 300` in the script with a clear error.
- [ ] **Step 4:** In `obs_phase2.py`: keep the dedicated `PHASE2-PROBE` scene; before the run READ the production cam1 input's genlock settings (on strih) and the strih input's settings (on stream) and COPY them onto the probe input (do not modify the production scenes/inputs). Leave the probe scene in place on teardown (the user runs their own visual test on it).
- [ ] **Step 5:** After the probe exits, call `scripts/e2e-report.py` on the JSON + series and print the LAN URL.
- [ ] **Step 6:** `bash -n scripts/multitap-e2e.sh` (syntax check) and `python3 -m py_compile scripts/obs_phase2.py scripts/e2e-report.py`.
- [ ] **Step 7: Commit**

```bash
git add scripts/multitap-e2e.sh scripts/obs_phase2.py
git commit -m "feat(e2e): cam1 source + true-30 capture + dedicated scene + >=300s + report URL"
```

---

### Task 10: Rig verification run (the actual proof)

**Files:** none (runtime verification per `autonomous-verification.md`)

- [ ] **Step 1:** Confirm prerequisites on the rig: camera HDMI = 1080p30, short shutter set (user action — verify cam1 `camera-box` log shows `30.0 fps captured`, not 60). Confirm cam2 monitor live, cam1 NDI `CAM1 (usb)` present.
- [ ] **Step 2:** Run the full harness ≥300 s (target 1800 s) with `--dual-qr --wall-clock`:

```bash
./scripts/multitap-e2e.sh --duration-secs 1800
```

- [ ] **Step 3:** Read the report PNG via the printed LAN URL; confirm: Graph-1 cam1/strih/stream lanes continuous (no gaps), Graph-2 latency flat, per-hop table all `PASS`, full-span `ZERO-LOSS`, window ≥300 s. If ANY loss: stop early, keep the partial report, investigate the failing hop.
- [ ] **Step 4:** Deliver the report URL + a one-line verdict to the user (milestone ping).

---

## Self-Review

**Spec coverage:** dual-QR Vernier (Tasks 2-5) ✓; true-30 capture (Task 1, Task 9.2) ✓; cam1 anchor + per-output grid continuity (Tasks 6-7) ✓; dedicated scene, no prod edits (Task 9.4) ✓; graphs + LAN URL (Task 8) ✓; ≥300 s honesty gate (Task 7, 9.3) ✓; optical-hop report-only (Task 8 table, anchor in Global Constraints) ✓; rig verification ≥300 s (Task 10) ✓.

**Placeholder scan:** no TBD/“handle errors”; pure-unit tasks carry real test code; script/hardware tasks carry exact commands. `reader.rs` edit in Task 5 is conditional (“only if it shares the decode”) — the implementer confirms by reading the file.

**Type consistency:** `Payload` (run_id/frame_id/gen_ts_ns) used consistently; `decode_capture_dual` mirrors `decode_capture`'s signature; `Observed` fields match `analyzer.rs`; `GridContinuity` returned by `grid_continuity` and stored in `MultiTapReport`; `vernier_ids -> (u32,u32)` consumed by the painter dual path.
