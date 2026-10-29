---
paths:
  - "src/capture_range.rs"
  - "src/yuyv_to_uyvy.rs"
  - "src/ndi.rs"
  - "benches/format_conversions.rs"
  - "tests/harness_capture_range_wiring_1430.rs"
---

# Capture range: a full-range grabber leaves as limited-range UYVY (issue 1430)

- The Cam Link 4K delivers FULL-range YUYV (luma p1 0 / p99 255). NDI UYVY is BT.709 LIMITED and
  every receiver reads it so (the genlock OBS forces `yuv_range=partial`). UVC carries no range
  flag, so V4L2 cannot tell; the decision is `capture_range::resolve_capture_range`: a valid
  `CAMERA_BOX_CAPTURE_RANGE=full|limited` wins, else the detected V4L2 card (Cam Link = full,
  anything else or an unread card = limited passthrough, never a guess).
- `yuyv_to_uyvy::convert_into` is the ONE send-path conversion (`NdiSender` and the `ndi.rs`
  standalone twins call it). Full range: Y' = (219 Y + 4207) / 255, C' = (224 C + 4095) / 255,
  = round(16 + Y*219/255), round(128 + (C-128)*224/255); no ties (255 is odd). AVX2: reorder,
  zero-extend to (chroma, luma) 16-bit lanes, mullo + add (max 61 215, fits u16), exact /255 as
  `mulhi_epu16(n, 0x8081) >> 7`, packus. Limited = the plain reorder, byte-identical to pre-1430.
- One journal line at start: `capture range: <full->limited|limited passthrough> (issue 1430) source=...`.
- The `capture luma:` / `capture chroma:` / `rough=` samplers read the RAW capture buffer; keep
  them there (pinned by `tests/harness_capture_range_wiring_1430.rs`), they diagnose the grabber.
- **Cost:** the conversion is memory-bound; the arithmetic costs ~40 us per 1080p frame on dev1
  (passthrough 0.07 ms, full 0.11 ms). The old kernel's `resize(total, 0)` zero-fill was 0.3 ms
  of every frame by itself: write every byte and `set_len`, never zero-fill a buffer you overwrite.
  `cargo bench --bench format_conversions -- yuyv_to_uyvy` measures it.
- **Downstream levels moved:** OBS now maps Y 16..235 to RGB 0..255 instead of crushing; the colour
  patches' RGB chroma drops ~12 % (224/255), so `colour_verify::GRAYSCALE_CHROMA_MIN` (24) has
  less headroom on red/blue: re-read `--colour-gate` numbers after a fleet deploy before tuning.
