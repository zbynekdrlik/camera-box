---
paths:
  - "src/jxs_link.rs"
  - "src/jxs_link/**"
  - "src/jxs_send.rs"
  - "src/jxs_recv.rs"
  - "src/bin/jxs-recv-probe.rs"
  - "jxs/**"
  - "vendor/svt-jpeg-xs/**"
  - "tests/jxs_loopback_1387.rs"
  - "tests/jxs_wiring_1387.rs"
---

# The cambox JPEG XS link (issue 1387, slice 1)

A higher-quality camera picture than NDI's SpeedHQ, BESIDE NDI (never instead of it, owner
decision 6488036928). Slice 1 = the cambox sender + a receive/measure tool; the strih OBS source is
slice 2, strih -> stream slice 3. imag stays on NDI.

## The owner's binding choices (do not re-open them)

- Codec: JPEG XS, SVT-JPEG-XS, ~4 bpp (~500 Mb/s at 1080p60), latency profile, decomp_v 2
  (measured on the N100: comments 6489509200, 6489539657; approved 6489868243).
- Transport (6489239017): our own UDP, 16-byte header, NO RTP, no retransmission, no jitter
  buffer, each slice on the wire the moment it is encoded, at line rate, UDP GSO. Pacing at
  500 Mb/s measured ~24.5 ms vs 15.6 ms at line rate: never pace.
- No hardware change: the camboxes stay on 1 GbE.

## Where things are

- **Wire format: `src/jxs_link.rs` module doc is THE definition.** 16 bytes LE: magic `0x4A`,
  version 1, camera id (`CAM<N>`), timecode (56 bits, 100 ns since the epoch, the SAME genlock
  timecode the NDI frame carries), slice index (0 = the picture header packet), slice count, byte
  offset in the slice (u16), slice length (u16). Datagrams <= 1472 bytes (no IP fragmentation).
  A 1080p frame = 69 packets (`packets_per_frame`: header + one per 16-line slice); the
  encoder's `slice_height` is set to the SAME 16 (`svt_jpegxs_sys::SLICE_HEIGHT_LINES`, pinned
  equal by `tests/jxs_loopback_1387.rs`).
- **Codec: `jxs/sys`** (crate `svt-jpegxs-sys`). `build.rs` builds `vendor/svt-jpeg-xs` (pinned
  commit in its `CAMERA-BOX-VENDOR.md`) with cmake, static, Release, no LTO, no -march=native,
  output in `OUT_DIR`. `csrc/jxs_shim.c` includes the real headers and exposes scalars only, so
  Rust never re-declares an SVT struct. `Encoder::encode_frame` returns after the frame's last
  packet: one frame in flight.
- **Sender: `src/jxs_send.rs`.** `JxsSender::start_from_env` reads `CAMERA_BOX_JXS_TARGET`;
  unset = `None`, nothing exists. main.rs calls `jxs.offer(...)` right AFTER
  `emit_one(capture_timecode_100ns)` (NDI timing untouched): one memcpy into the one buffer, or a
  counted `dropped_busy` while the encoder is busy. The `jxs-send` thread pins itself with
  `pin_off_capture_core` BEFORE it opens the encoder, so SVT's threads inherit the display cores.
  `UDP_SEGMENT` (GSO) is a socket option: one `send` per slice (up to 40 datagrams per send).
  A GSO send that fails with EIO (no checksum offload) or EINVAL switches the socket to one send
  per datagram for good, logged once. IPv4 only: the 1472-byte datagram is the IPv4 budget of a
  1500 MTU (an IPv6 target is refused at start). `encode_ms` = picture handed to the encoder ->
  its last packet sent (the slice sends happen inside it); the YUYV conversion is not in it.
- **Receive: `src/jxs_recv.rs` + `src/jxs_link/assembler.rs`.** `recvmmsg` + `UDP_GRO` +
  `SO_TIMESTAMPNS`. The assembler hands each packet on as soon as everything before it in its
  frame is complete; it finalizes a frame (missing packets = lost) when the NEXT frame is complete
  or a third frame arrives. `DecodeSink` replaces a lost packet with the SAME index of the
  previous frame (JPEG XS slices are independently coded), so loss costs a stale stripe, never a
  stall. SVT's decoder init needs one WHOLE frame (the header packet alone is "too short"), so the
  decoder opens on the first complete frame. A timecode more than 50 ms behind the last finished
  frame is a clock step (the nightly dantesync date step is ~200 ms): the open frames are ended
  (missing packets as lost) and the stream starts over. One assembler = one sender: a datagram
  whose camera id differs from its frame's is refused, and the probe takes one camera
  (`--camera N`, else the first heard; the rest count as `foreign=`).
- **Never wait on the SVT decoder without a bound.** In packet mode the library answers a corrupt
  slice by emitting an (error) frame at once and returning OK; every further packet of that frame
  queues another one until its pools (3 decoder instances) run dry and `send_packet` blocks for
  good. So `DecodeSink` polls `poll_frame()` after every non-last packet: anything out early =
  the frame failed, stop feeding it, reopen the decoder from the next whole frame. The last packet
  must answer `Fed::Taken` (`NeedMore` there = a corrupt frame), and `finish_frame` takes a
  timeout (200 ms). Pinned by `a_garbled_slice_is_seen_before_the_frame_ends_and_never_blocks`
  (jxs/sys) and `a_garbled_slice_costs_that_frame_and_the_next_one_decodes` (loopback), both run on
  a thread with a deadline so a regression fails instead of wedging the suite.
- `Yuv422Planes` fields are private and `Encoder::encode_frame` re-checks the planes against the
  opened geometry before the FFI call (the C side reads by that geometry). `Decoder::open`
  refuses a declared picture over 7680x4320 (the size comes from the network).
- **Probe: `jxs-recv-probe`** (`--features jxs`, CI uploads it in `probe-tools-linux-amd64`).

## Build gotchas

- The `jxs` feature needs cmake >= 3.10 and **nasm >= 2.14**. CI installs nasm in lint, test,
  coverage, build and the on-demand mutants job, which builds `--features probe,jxs`
  (`tests/jxs_wiring_1387.rs` pins it). dev1 has no system nasm: point
  `ASM_NASM` at one (an extracted `nasm_*.deb` works) for any `--all-features` / `--features jxs`
  cargo run. A default `cargo build` needs neither.
- The deployed `camera-box-linux-amd64` is built `--features jxs` (ci.yml `build`, release.yml).
- The upstream CMake writes its outputs into `<source>/Bin` unless `CMAKE_OUTPUT_DIRECTORY` is
  overridden: `jxs/sys/csrc/CMakeLists.txt` does, so the vendored tree stays clean.
- A worktree lane cannot type a path containing the word `Source` into a Bash command (the
  isolation guard reads it as `source`): copy/list vendor files from a python script file.

## Live acceptance (supervisor; slice 1, one SNV cambox, rig TEST mode)

1. On strih-snv (or dev1): `jxs-recv-probe --listen 0.0.0.0:7000 --window-s 5`.
2. On the cambox: a RUNTIME drop-in (gone at reboot), then the deploy's restart:
   `systemctl edit --runtime camera-box` -> `[Service]` `Environment=CAMERA_BOX_JXS_TARGET=<strih ip>:7000`.
   Journal: `jxs send: link ON to ... (UDP GSO on)`, `encoder open 1920x1080`, then every 5 s
   `jxs send: frames= slices= bytes= dropped_busy= encode_ms_avg/max= send_errors= (issue 1387)`.
3. Grade: the `Streaming:` line still 60.0 sent with no capture drop; `frames` ~300 per 5 s and
   `dropped_busy` ~0; `send_errors=0`; ~500 Mb/s on the wire; camera-box CPU; the probe's
   `lost_slices=0` over 10 min, `latency_ms_p50/p99/max`, `spread_ms`; `--dump-png 3 --dump-skip
   300` vs the same frame over NDI (detail metric + a crop).
4. Remove the drop-in and restart: the link is off, the NDI path is byte-identical.

## Known limits (slice 1)

- The hand-off follows the NDI send, so the measured latency includes the NDI send call
  (~2.7 ms on a Ryzen, more on the N100).
- YUYV -> planar 4:2:2 is a scalar loop on the `jxs-send` thread, before the first slice.
- One frame in flight: a frame whose encode passes ~16.7 ms makes the next one `dropped_busy`.
- One receiver per cambox; only YUYV captures are sent (another format logs one WARN).
- A frame whose slice count differs from the previous one restarts the decoder (a geometry change).
- In TEST mode with the cam1 QR burn active, NDI carries the burned frame and JPEG XS the
  unburned capture: a pixel comparison of the two during an E2E differs by the burn.
- `SO_SNDBUF` asks for 4 MB; the kernel caps it at `net.core.wmem_max` (the "link ON" line logs
  what was granted). A frame is ~1.04 MB, so a small grant makes `send` block the `jxs-send`
  thread (never the capture thread) until the NIC drains it.
- The PNG dump is a stored-deflate writer in `src/jxs_link/png.rs` (no compression, no
  dependency): the probe needs only the `jxs` feature, not the probe feature's `image` crate.
