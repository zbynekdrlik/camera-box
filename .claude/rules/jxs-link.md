---
paths:
  - "src/jxs_link.rs"
  - "src/jxs_link/**"
  - "src/jxs_send.rs"
  - "src/jxs_send/**"
  - "src/jxs_recv.rs"
  - "src/bin/jxs-recv-probe.rs"
  - "jxs/**"
  - "vendor/svt-jpeg-xs/**"
  - "tests/jxs_loopback_1387.rs"
  - "tests/jxs_wiring_1387.rs"
---

# The cambox JPEG XS link (issue 1387, slices 1 + 1b)

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
- **30 fps, the strih grid** (owner decision 29.10.2026, slice 1b): only the frames on the strih
  30 fps grid go over JPEG XS; NDI stays at 60 fps, unchanged. The encode budget is 33.3 ms.
- **bpp is a setting:** `CAMERA_BOX_JXS_BPP`, default 4; the owner's budget allows ~500 Mb/s,
  i.e. ~8 bpp at 30 fps.

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
  packet (one frame in flight: the tests and the probe); the sender uses the pipelined session
  (`session.rs`, below).
- **Sender: `src/jxs_send.rs` + `src/jxs_send/handoff.rs` (slice 1b).**
  - `JxsSender::start_from_env` reads `CAMERA_BOX_JXS_TARGET`; unset = `None`, nothing exists.
    `CAMERA_BOX_JXS_BPP` (`parse_bpp`: a whole number 1..=12, default 4; anything else WARNs and
    keeps 4; 12 is the most a 16-line 1080p slice fits in the 16-bit slice length).
  - **Which frames:** `jxs_link::on_jxs_grid(timecode, emit_fps)` floors the frame's genlock
    timecode on the per-second emit grid (`genlock_grid::per_second_floor`) and keeps it only when
    that stamp is also a point of the 30 fps grid: the EVEN 60 fps slots (`floor(2j/60 s)` ==
    `floor(j/30 s)`, the same integer). Never a frame counter: a dropped capture costs only its own
    frame and a second boundary never shifts the parity. Genlock off (emit 0) floors on an assumed
    60 fps grid; an emit rate at or below 30 sends every frame.
  - **Hand-off BEFORE the NDI send:** main.rs calls `jxs.offer(&JxsFrame { .., timecode_100ns:
    capture_timecode_100ns, emit_fps: send_fps })` right after the timecode is fixed
    (`note_emitted_stamp_100ns`) and BEFORE the #1242 stagger sleep and every `emit_one`
    (`tests/jxs_wiring_1387.rs` pins the order; slice 1 had it after the send, a deliberate
    change). The capture thread converts the YUYV frame straight into one of `FRAMES_IN_FLIGHT`
    (2) reusable planar `Picture`s (AVX2, no separate copy) and queues it; with both in use the
    frame is `dropped_busy`. Never blocks. Its cost lands inside the stagger offset (the sleep
    shrinks by it), so the NDI send time of every camera but CAM1 (offset 0) is unchanged; CAM1's
    NDI send moves by the conversion (~0.25 ms on dev1, more on the N150).
  - **AVX2 conversion** (`jxs_link::planar`): four 32-byte loads per 64 pixels, `packus` of the
    low bytes (Y) and the high bytes (chroma), split once more into U and V, `permute4x64(0,2,1,3)`
    restoring the lane order; the scalar loop does the tail; runtime-detected
    (`is_x86_feature_detected!`, std caches it). Bit-exact with the scalar one over every byte
    value on every YUYV position, every tail, padded strides, a guard byte past each plane.
  - **Two threads** pinned to the encoder's EXPLICIT core set: the off-capture cores of the #1198
    split (`affinity::off_capture_split`; a cambox: [0, 1], capture 3 and its IRQ core 2 never),
    or `CAMERA_BOX_JXS_CORES` (a cpulist) when it lies inside them (else WARN and the default).
    One slice-pack thread per core, 1..=4 (`slice_threads_for`). `jxs-feed` opens the encoder (its
    threads inherit the set) and submits each picture at once; `jxs-send` reads each frame's
    packets as they come out and sends every slice with GSO, then returns the picture.
  - **The pipelined session** (`jxs/sys/src/session.rs`, `open_pipelined`): `EncoderInput::submit`
    moves a picture INTO the session (the library reads its planes, writes its bitstream);
    `EncoderOutput::recv_frame` returns it after its last packet. The library allows it: a 10-deep
    input FIFO, the final stage re-orders output by frame number. `abandon` closes after a codec
    error and returns every held picture; the feeder re-opens and resubmits that frame once.
    `EncoderOutput` is `Send` but NOT `Sync` (a compile-time proof in session.rs): only one
    thread reads packets and may close the handle. `MAX_IN_FLIGHT` (8) stays below the library's
    10-deep input FIFO (const-asserted): `submit` holds the session lock across the BLOCKING
    `send_picture`, so a full FIFO would deadlock against `recv_frame`.
    **`threads_num` 0 meant ONE slice-pack thread**
    (latency profile: `pack_stage_threads_num = MAX(1, threads_num - 2)`): slice 1 packed all 68
    slices of a frame serially, the 16.9 ms live encode. `with_slice_threads(n)` sets n + 2.
  - The status line every 5 s: `jxs send: fps= bpp= frames= slices= bytes= dropped_busy=
    in_flight=max/cap convert_ms_avg/max= first_slice_ms_avg/max= encode_ms_avg/max= send_errors=
    encode_errors= (issue 1387)`. `in_flight` counts a frame from its submit to its last packet;
    `first_slice_ms` = the converted frame queued -> its first packet sent; `encode_ms` = submit ->
    last packet sent.
  - **Wedge alarm:** the status line lives on `jxs-send`, the thread a hung `get_packet` would
    stop. So `jxs-send` stamps a progress clock every loop turn and every packet, and `jxs-feed`
    checks it every 500 ms: a frame in flight with no packet for 2 s (`encoder_wedged`,
    `WEDGE_MS`) logs ONE `CRITICAL jxs send: encoder wedged -- N frame(s) in flight, no packet for
    X ms` line (and one when packets flow again). Alert-only, never an exit: NDI is untouched.
    Dropping the hand-off waits at most 2 s for its threads, then leaves them detached.
  - `UDP_SEGMENT` (GSO) is a socket option: one `send` per slice (up to 40 datagrams per send).
    A GSO send that fails with EIO (no checksum offload) or EINVAL switches the socket to one send
    per datagram for good (and clears the option), logged once. IPv4 only: the 1472-byte datagram
    is the IPv4 budget of a 1500 MTU (an IPv6 target is refused at start). `encode_errors=` counts
    frames not encoded or not sent whole; a geometry the encoder refuses is logged once and never
    re-opened per frame.
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
  good. The deterministic signal: before the last packet the library answers `Fed::NeedMore`,
  so `Fed::Taken` there means it gave the frame up; `DecodeSink` fails the frame on it at once,
  then also polls `poll_frame()` (anything out early = failed). A failed frame is never fed
  further, its packets are never a concealment source, and the decoder reopens from the next
  whole frame (a lost packet of that frame is concealed when the previous one is known). The last
  packet must answer `Fed::Taken` (`NeedMore` there = a corrupt frame), and `finish_frame` takes a
  timeout (200 ms, sleeping 100 us per look). Pinned by `a_garbled_slice_is_seen_before_the_frame_ends_and_never_blocks`
  (jxs/sys) and `a_garbled_slice_costs_that_frame_and_the_next_one_decodes` (loopback), both run on
  a thread with a deadline so a regression fails instead of wedging the suite.
- **Every SVT init is serialized** (a mutex in the shim): the library fills its run-time CPU
  dispatch tables at the first init through a plain static flag, and two inits at once race on it
  (stdout `Error: ... Pointer "..." is set before!`, seen with parallel tests).
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

## The dev1 budget (slice 1b, 29.10.2026, Ryzen 7 5800X, encoder pinned to cores 0-1)

A throwaway release bench drove the real `JxsSender` to a loopback receiver with 1080p frames.

| | result |
|---|---|
| YUYV -> planar, 1080p | scalar 1.92 ms, AVX2 0.095 ms in cache (0.23-0.27 ms in the live loop), memcpy 0.08 ms |
| 30 fps live shape, 4 / 6 / 8 bpp | fps 30.0, dropped_busy 0, encode 6.6-7.5 ms avg (max 15), first slice 0.32-0.40 ms avg after the hand-off |
| 8 bpp, ONE core | encode 13.8 ms avg (max 19.5), still 30.0 fps |
| saturation, 2 cores, 8 bpp | 154-161 fps, in_flight 2/2 |
| saturation, 1 core, 8 bpp | 80 fps, in_flight 2/2 |

The encode time hardly depends on bpp (4 -> 8: +0.5 ms). The N150 runs a core at roughly half a
5800X core (slice 0: 15.4 ms per frame on ONE slice thread on the N100), so expect ~14-16 ms on
its two cores against the 33.3 ms budget. The first `offer` allocates the picture (~2 ms, once).

## Live acceptance (supervisor; one SNV cambox, rig TEST mode)

1. On strih-snv (or dev1): `jxs-recv-probe --listen 0.0.0.0:7000 --window-s 5`.
2. On the cambox: a RUNTIME drop-in (gone at reboot), then the deploy's restart:
   `systemctl edit --runtime camera-box` -> `[Service]` `Environment=CAMERA_BOX_JXS_TARGET=<strih ip>:7000`.
   Optional in the same drop-in: `Environment=CAMERA_BOX_JXS_BPP=8` (default 4) and
   `Environment=CAMERA_BOX_JXS_CORES=0-1` (must lie inside the off-capture cores).
   Journal: `jxs send: link ON to ... (UDP GSO on, ...) -- JPEG XS <bpp> bpp at 30 fps ...,
   encoder on cores [0, 1] with 2 slice threads (capture core Some(3), capture-IRQ core Some(2)
   untouched)`, `encoder open 1920x1080, <bpp> bpp, ...`, then every 5 s the status line above.
3. Grade: the `Streaming:` line still 60.0 sent with no capture drop; `fps=30.0`, `frames` ~150
   per 5 s, `dropped_busy=0`, `in_flight` max 1-2 of 2, `encode_ms_avg` well under 33.3,
   `first_slice_ms_avg` under ~1 ms; `send_errors=0`; ~250 Mb/s at 4 bpp (~500 at 8) on the
   wire; camera-box CPU; the #1242 stagger line (`max per-frame work` now includes the
   conversion); the probe's `fps=30.00 lost_slices=0` over 10 min, `latency_ms_p50/p99/max`
   (target: near the encode time, slice 1 was 50 ms), `spread_ms`; `--dump-png 3 --dump-skip 300`
   vs the same frame over NDI (detail metric + a crop). Repeat at 6 and 8 bpp.
4. Remove the drop-in and restart: the link is off, the NDI path is byte-identical.

## Known limits (slices 1 + 1b)

- **The even slot is the strih's frame only at the production pin.** The strih N==2 conveyor
  presents `grid_floor(T - 50 ms - pin)` (`genlock-n2-grid-conveyor.md`): even 60 fps slots for a
  pin of 1..16 ms (production 3), ODD slots for 17..33 ms. A camera re-pinned past 16 ms by the
  `[4i/8align]` step would put the other frame of each pair on the strih program than the one
  JPEG XS carries; the slice-2 OBS source picks its own frame anyway.
- CAM1 (stagger offset 0) sends its NDI frame ~one conversion later than before (~0.25 ms on dev1).
- The first frame after a geometry change allocates its pictures on the capture thread (~2 ms,
  once).
- One receiver per cambox; only YUYV captures are sent (another format logs one WARN).
- A frame whose slice count differs from the previous one restarts the decoder (a geometry change).
- In TEST mode with the cam1 QR burn active, NDI carries the burned frame and JPEG XS the
  unburned capture: a pixel comparison of the two during an E2E differs by the burn.
- `SO_SNDBUF` asks for 4 MB; the kernel caps it at `net.core.wmem_max` (the "link ON" line logs
  what was granted). A frame is ~1.04 MB, so a small grant makes `send` block the `jxs-send`
  thread (never the capture thread) until the NIC drains it.
- The PNG dump is a stored-deflate writer in `src/jxs_link/png.rs` (no compression, no
  dependency): the probe needs only the `jxs` feature, not the probe feature's `image` crate.
