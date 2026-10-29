---
paths:
  - "src/yuyv_to_p216.rs"
  - "benches/format_conversions.rs"
---

# NDI send format: UYVY default, P216 behind one switch (issue 1387)

- The standard NDI SDK 6 sender has NO quality or bitrate setting (`NDIlib_send_create_t` is name,
  groups and the two clock flags). The only sender lever is the pixel FORMAT. A fixed SpeedHQ
  quality needs the Advanced SDK and its licence (an owner decision).
- `CAMERA_BOX_NDI_FORMAT=p216` (read ONCE in `NdiSender::new`, start line
  `ndi send format: <uyvy|p216> (issue 1387)`, one line per sender, so the cam1-burn sender logs a
  second one) sends a YUYV capture as `P216`. Every other input (UYVY passthrough, NV12, MJPG,
  BGRA) keeps UYVY and WARNs once. Unset / empty / `uyvy` = the pre-1387 path, byte-identical.
  An unknown value WARNs and uses uyvy.
- P216 layout (`vendor/distroav/lib/ndi/Processing.NDI.structs.h:70-75`): one buffer, the u16
  luma plane, then IMMEDIATELY the interleaved u16 Cb,Cr plane. `line_stride_in_bytes`
  (`:237-239`) is the luma stride `width * 2`; the CbCr plane has the same stride. A padded V4L2
  `bytesperline` is the SOURCE stride only, never the P216 stride (`p216_send_params`).
- Read as little-endian u16 lanes a YUYV row is `Y | C << 8` with C already in Cb,Cr order, so the
  AVX2 kernel is one shift (luma) and one and-not (chroma), no shuffle. Keep it bit-exact with `scalar_row`; the tests
  assert AVX2 is present on x86_64 (a missing AVX2 fails, never skips).
- Each 8-bit code becomes `v << 8` (main decision 29.10.2026): the capture is limited range
  (issue 1430), so black 16 / white 235 reach a 10-bit reader as 64 / 940. Bit replication
  (`v * 257`, the first cut) was dropped: it adds a 1/256 gain (235 -> 943 in 10 bits). An 8-bit
  reader gets `v` back exactly either way.
- Cost (dev1 Ryzen 7 5800X, `cargo bench --bench format_conversions -- yuyv_to_p216`, reused
  buffer): AVX2 ~137 us / 1080p frame, scalar ~1.05 ms. The N100/N150 camboxes are slower; the
  real budget is read on the box (camera-box CPU from `/proc/<pid>/stat`, tx from
  `/sys/class/net/<if>/statistics/tx_bytes`), comparing the same splitter picture UYVY vs P216.
