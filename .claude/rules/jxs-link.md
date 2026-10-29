---
paths:
  - "src/jxs_link.rs"
  - "src/jxs_link/**"
  - "src/jxs_send.rs"
  - "src/jxs_send/**"
  - "src/jxs_recv.rs"
  - "src/jxs_recv/**"
  - "vendor/distroav/src/jxs-source.cpp"
  - "vendor/distroav/src/jxs-source-decide.h"
  - "tests/jxs_receiver_1387.rs"
  - "tests/distroav_jxs_source_1387.rs"
  - "tests/jxs_strih_build_1387.rs"
  - "src/bin/jxs-recv-probe.rs"
  - "src/bin/jxs-encode-bench.rs"
  - "src/jxs_auto_bpp.rs"
  - "src/jxs_bench.rs"
  - "jxs/**"
  - "vendor/svt-jpeg-xs/**"
  - "tests/jxs_loopback_1387.rs"
  - "tests/jxs_wiring_1387.rs"
  - "vendor/distroav/src/jxs-output.cpp"
  - "vendor/distroav/src/jxs-output-decide.h"
  - "vendor/distroav/src/genlock-emit-timecode.h"
  - "tests/distroav_jxs_output_1387.rs"
---

# The cambox JPEG XS link (issue 1387, slices 1, 1b, 2 + 3)

A higher-quality camera picture than NDI's SpeedHQ, BESIDE NDI (never instead of it, owner
decision 6488036928). Slice 1 = the cambox sender + a receive/measure tool; slice 2 = the strih OBS
source `camera_box_jxs_source` (section "Slice 2" below); slice 3 = the strih PROGRAM to stream-snv
through the DistroAV output `camera_box_jxs_output` (section "Slice 3" below). imag stays on NDI.

## The owner's binding choices (do not re-open them)

- Codec: JPEG XS, SVT-JPEG-XS, ~4 bpp (~500 Mb/s at 1080p60), latency profile, decomp_v 2
  (measured on the N100: comments 6489509200, 6489539657; approved 6489868243).
- Transport (6489239017): our own UDP, 16-byte header, NO RTP, no retransmission, no jitter
  buffer, each slice on the wire the moment it is encoded, at line rate, UDP GSO. Pacing at
  500 Mb/s measured ~24.5 ms vs 15.6 ms at line rate: never pace a CAMBOX (1 GbE into the strih's
  10 GbE, no port steps the burst down; `CAMERA_BOX_JXS_PACE_MBPS` defaults to 0). The ONE
  exception is a link that steps DOWN on the way: the strih program output (10 GbE) to stream-snv
  (1 GbE) loses slices at line rate, so it paces (section "Send pacing" below; decided by the
  main session in the slice-3 fix item of comment 6507194583 and its lane brief, 29.10.2026; the
  cambox default stays line rate, so the owner's choice above is unchanged for the cameras).
- No hardware change: the camboxes stay on 1 GbE.
- **30 fps, the strih grid** (owner decision 29.10.2026, slice 1b): only the frames on the strih
  30 fps grid go over JPEG XS; NDI stays at 60 fps, unchanged. The encode budget is 33.3 ms.
- **bpp is automatic** (owner decision 6495938345: the highest quality the box encodes in time
  AND the strih takes, low overhead): `CAMERA_BOX_JXS_BPP_MIN` / `_MAX` (default 4..=8, ~500 Mb/s
  at 30 fps = the owner's 1 GbE budget); `CAMERA_BOX_JXS_BPP` set alone pins a fixed bpp (auto
  off). See "The automatic bpp" below.

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
    The bpp mode comes from `parse_bpp_mode` (`CAMERA_BOX_JXS_BPP` = fixed, a whole number
    1..=12, a refused value keeps a FIXED 4; else auto between `_MIN`/`_MAX`, each refused bound
    keeps its default, a floor above the ceiling keeps 4..=8; every refusal WARNs). 12 is the most
    a 16-line 1080p slice fits in the 16-bit slice length. `CAMERA_BOX_JXS_FIFO_PRIO`
    (`parse_fifo_prio`, 1..=89, unset/0/off = SCHED_OTHER) is the opt-in encoder SCHED_FIFO.
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
  - The status line every 5 s: `jxs send: fps= bpp= auto= bpp_reason= frames= slices= bytes=
    dropped_busy= in_flight=max/cap convert_ms_avg/max= handoff_age_ms_avg/min/max=
    capture_age_ms_avg/min/max= first_slice_ms_avg/max= encode_ms_avg/max= send_errors=
    encode_errors= report_age_s= reports= pace_mbps= wire_ms_avg/max= (issue 1387)`
    (`pace_mbps` 0 = line rate; `wire_ms` = a whole frame's first send -> its last send returned,
    see "Send pacing").
  - **`handoff_age_ms` / `capture_age_ms`** (coordinator request 29.10, the cam5 sawtooth: the
    receiver's first arrival after the timecode wandered 21-39 ms, ~+3.5 ms per 5 s then a reset,
    while the capture phase tracker was locked at +18 ppm): the wall clock (`CLOCK_REALTIME`, the
    dantesync-disciplined clock the genlock timecode is stamped on, read with
    `jxs_recv::realtime_100ns`) minus the frame's TIMECODE / minus its RAW capture instant
    (`JxsFrame::capture_realtime_100ns` = `genlock_stamp::capture_realtime_100ns(V4L2 stamp,
    offset)`, built in the hand-off literal; NOT main.rs's `capture_realtime_100ns` variable,
    which is the phase tracker's SLOT MIDDLE while `phase_lock=stamp`; 0 = unknown = no capture
    sample), at the moment the capture thread hands a grid frame to `JxsHandoff::offer` (a
    busy-dropped one too), avg/min/max per window, `-/-/-` with none. The timecode is the
    emit-grid boundary AT OR BEFORE the stamp instant (`genlock_emit_timecode_100ns` floors), so
    `handoff_age - capture_age` = the raw capture's phase against its stamped slot (0..16.7 ms at
    60 fps; a little outside it near an edge under the tracker's 500 us hysteresis), and
    `capture_age` alone = capture -> hand-off (V4L2 queue residence, the emit gate). An 18 ms
    sawtooth in `handoff_age` with a flat `capture_age` is the slot phase sliding; one in
    `capture_age` sits in the queue/gate; both flat = after the hand-off (encode, send, network,
    receiver). Pure stat `jxs_link::HandoffAge` + `handoff_age_100ns`; the capture thread only
    `try_lock`s the one `Mutex<[HandoffAge; 2]>` (a sample is skipped, never waited for; no
    allocation).
  - `bpp` = the bpp of the session whose
    frames went out last; `bpp_reason` the last change's reason (`-` = none yet);
    `report_age_s` seconds since the last receiver report (`-` = never one: the auto bpp holds).
    `in_flight` counts a frame from its submit to its last packet; `first_slice_ms` = the converted
    frame queued -> its first packet sent; `encode_ms` = submit -> last packet SENT, so it
    INCLUDES a send that blocks on a full socket/qdisc (the 29.10 8 bpp "55.8 ms" was that, not
    the codec: see "Encoder cost" below).
  - **Wedge alarm:** the status line lives on `jxs-send`, the thread a hung `get_packet` would
    stop. So `jxs-send` stamps a progress clock every loop turn and every packet, and `jxs-feed`
    checks it every 500 ms: a frame in flight with no packet for 2 s (`encoder_wedged`,
    `WEDGE_MS`) logs ONE `CRITICAL jxs send: encoder wedged -- N frame(s) in flight, no packet for
    X ms` line (and one when packets flow again). Alert-only, never an exit: NDI is untouched.
    Dropping the hand-off waits at most 2 s for its threads, then leaves them detached.
  - **`SO_MARK 0x1387` (`JXS_SOCKET_MARK`, issue 1387, the egress-pacing exemption):** the cambox
    egress pacing (issue 1242) caps every flow at 400 Mb/s in an `fq`. That capped this link too (live
    29.10: a 23 ms slice spread at 4 bpp, 33 ms at 6 bpp, a 162 ms backlog at 8 bpp). The pacing's
    root is now a `prio` whose `fw` filter on this mark sends the flow to an unpaced class at line
    rate (`.claude/rules/cambox-egress-pacing.md`). The mark is set in `JxsSocket::connect`, so it
    exists only while the link is on. It needs CAP_NET_ADMIN or CAP_NET_RAW (camera-box.service
    runs as root); a refused mark logs one WARN with the errno, and the "link ON" line reads
    `egress mark NONE: paced with NDI`. The unit test reads the capability from `CapEff`, and an
    unprivileged run must see EPERM (proof the setsockopt was attempted).
  - `UDP_SEGMENT` (GSO) is a socket option: one `send` per slice (up to 40 datagrams per send).
    A GSO send that fails with EIO (no checksum offload) or EINVAL switches the socket to one send
    per datagram for good (and clears the option), logged once. IPv4 only: the 1472-byte datagram
    is the IPv4 budget of a 1500 MTU (an IPv6 target is refused at start). `encode_errors=` counts
    frames not encoded or not sent whole; a geometry the encoder refuses at a bpp is logged once
    and never re-opened per frame at that bpp (another bpp tries it again); the packet thread is
    told only when a step was pending (it cancels the step, no back-off), else the feeder's
    ERROR line is the whole report.
- **Send pacing (`src/jxs_link/pace.rs` pure schedule, `src/jxs_send/pacing.rs` clock;
  slice-3 live finding, comment 6507194583).** `SenderOptions::pace_mbps` (0 = line rate,
  today's behaviour), else 100..=100000 Mb/s (`check_pace_mbps`).
  - **Why app-level:** `SO_MAX_PACING_RATE` is enforced only by the `fq` qdisc (and TCP's own
    pacing, which UDP lacks); strih-snv's root qdisc is `fq_codel`, which ignores it. `SO_TXTIME`
    needs an `etf` qdisc. So `JxsSocket::send_slice_paced` asks the schedule for each send's
    departure and waits for it with an absolute `clock_nanosleep(CLOCK_MONOTONIC)`, on the
    `jxs-send` thread only. The encoder never waits (its packets are already coded into the
    frame's own bitstream), nor does the caller (`offer` / `cbjxs_send_frame` copy and return; a
    pace too low for the frame rate shows as `dropped_busy`). The thread's timer slack goes to
    1 ns while it paces (`PR_SET_TIMERSLACK`; the default 50 us is a quarter of one send).
  - **The schedule (`PaceSchedule`):** counted in WIRE bytes, each datagram's payload + 66
    (UDP/IPv4/Ethernet/FCS/preamble/IFG), so `pace_mbps` compares with the port speed. Exact:
    `anchor + ceil(bits x 1000 / mbps)`, nothing rounded per send, so no drift over a frame or
    a day. A send ready later than due keeps at most HALF a burst of its lateness as credit (a
    50 us late wake catches up fully, pinned by `a_thread_that_wakes_late_every_time_keeps_the_rate`);
    more than half a burst moves the schedule later; more than one burst (`tolerance_ns`) = the
    link was idle and the schedule restarts at that instant.
  - **Bounded bursts:** while pacing one GSO send carries at most `PACE_BURST_SEGMENTS` (16)
    datagrams (24608 wire bytes, ~0.22 ms at 900 Mb/s); a longer slice is split into such
    groups (`send_ranges`). With the credit, at most 1.5 groups (~37 KB) ever reach the switch
    faster than the pace (`after_a_late_send_at_most_half_a_burst_follows_back_to_back`; review
    round 1: a full-tolerance credit let two whole groups go back to back). Line rate keeps the
    40-datagram sends.
  - **Budget:** a 1080p frame at 900 Mb/s takes 9.8 ms at 4 bpp, 19.7 at 8, 29.4 at 12 (pinned
    by `a_frame_at_900_mbps_finishes_within_the_30_fps_budget_up_to_12_bpp`), under 33.3 ms.
  - **The status line's `encode_ms` includes the paced wire time** (it always included the
    send), but the AUTO BPP does not see it: `send_slice_paced` reports the time the schedule held
    the sends back (`SliceSent::paced_ns`) and the controller gets `pace::codec_ms` = encode minus
    that. Otherwise the climb rule (p99 < 60 % of 33.3 ms, judged at the level BELOW the step)
    would cap the bpp by the pace: at 800 Mb/s 7 bpp alone is ~19 ms of wire (review round 1). A
    pace too slow for the frames backs them up into `dropped_busy`, which steps the bpp down on its
    own. `wire_ms_avg/max` = first send -> last send returned per whole frame (`wire_frames`
    divides); at line rate it is the encoder's slice span, not the wire.
  - Settings: the cambox `CAMERA_BOX_JXS_PACE_MBPS` (default 0, a refused value WARNs and keeps
    line rate), the C ABI's `cbjxs_send_config.pace_mbps`, the strih output's `jxs_pace_mbps`
    (default 900, below). The "link ON" line names it (`send line rate (no pacing)` / `send paced
    at 900 Mb/s of wire, at most 16 datagrams per send`).
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
- **A bpp change reopens the decoder at the changed picture header** (live 29.10.2026: every auto
  step 4 -> 5 cost an undecodable frame, `the frame ended before its last packet`, the receiver
  reported it as a loss about the new bpp and the controller stepped straight back, so auto bpp
  never climbed). A bpp change keeps the geometry and the packet count (69 at 1080p at EVERY
  bpp), so a count-only restart never fires. The header packet (index 0, 110 bytes) carries the
  codestream length `Lcod` (bytes 12..16: 172800 at 4 bpp, 216000 at 5, 345600 at 8 for
  320x1080); SVT's packet-mode decoder sizes its frame from the header it was opened with, so a
  frame of another size ends early (up) or waits past its end (down). `DecodeSink` keeps the
  LAST header packet that arrived (`last_header`) and compares every arriving one with it,
  whether a decoder is open or not (review round 1: checked only while open, a bpp change right
  after a frame the decoder gave up on went unseen, and a lost slice in the first new-bpp frame
  was concealed with an old-bpp slice: that frame DECODED from a mix of two bpps, a garbled
  picture reported as good). A changed header drops an open decoder AND the concealment source
  (the old bpp's slices have other sizes); the decoder opens from that whole frame, the same
  bounded path as the first frame (that one frame decodes after its last packet instead of
  slice by slice). `header_changes` counts every header change; `jxs-recv-probe` prints
  `decoder reopened at N new picture header(s) (a sender bpp change)` once per window.
  A slice lost in the FIRST frame at a new bpp has no concealment source: that frame is
  undecodable and reported (a real loss). A header packet lost in exactly that frame is concealed
  with the old header and the frame fails (counted); the next header resets.
  **"A steady stream never reopens" rests on a constant header per bpp:** the default rate
  control (1) codes every frame to exactly `Lcod`, so the header packet is byte-identical frame to
  frame (dumped from the real encoder, ramp content). A codec setting that let `Lcod` vary per
  frame would reopen EVERY frame (decode after the last packet, no concealment); the probe's
  reopen line would show it climbing with the frame count. Pinned by
  `a_bpp_change_mid_stream_decodes_every_frame_with_no_loss` (4 -> 5 -> 8 -> 6 -> 4, every frame
  decodes, 0 lost, 0 undecodable),
  `after_a_bpp_change_every_picture_is_its_own_and_a_loss_is_concealed_from_the_new_bpp` (one
  reopen, every picture its own, the hole in the 2nd new-bpp frame shows the 1st one's decode),
  `a_loss_in_the_first_frame_at_a_new_bpp_is_never_concealed_from_the_old_bpp` and
  `a_bpp_change_after_a_failed_frame_never_conceals_from_the_old_bpp`; mutating out the
  concealment reset or the decoder drop fails two of them each.
- **Every SVT init is serialized** (a mutex in the shim): the library fills its run-time CPU
  dispatch tables at the first init through a plain static flag, and two inits at once race on it
  (stdout `Error: ... Pointer "..." is set before!`, seen with parallel tests).
- `Yuv422Planes` fields are private and `Encoder::encode_frame` re-checks the planes against the
  opened geometry before the FFI call (the C side reads by that geometry). `Decoder::open`
  refuses a declared picture over 7680x4320 (the size comes from the network).
- **Probe: `jxs-recv-probe`** (`--features jxs`, CI uploads it in `probe-tools-linux-amd64`). Since
  slice 2 it is a thin caller of `jxs_recv::JxsReceiver` (below); its output lines are unchanged.

## Slice 2: the strih OBS source (design 6500573353)

### The one receive loop: `jxs_recv::JxsReceiver` (`src/jxs_recv/receiver.rs`, feature `jxs`)

- socket -> assembler -> `DecodeSink` -> decoded frames, the per-window statistics, and the receiver
  report sent once a second to the address the slices came from. `jxs-recv-probe` and the OBS source
  both run it, so the probe's numbers describe what OBS receives.
- `poll(timeout)` returns the next decoded frame, borrowed until the next call; a zero timeout
  still receives once. Frames queue up to `MAX_READY_FRAMES` (3); past that the OLDEST is dropped and
  counted (`dropped_unread`), so a slow caller gets the newest pictures. The socket is read only
  inside `poll`: between two polls the kernel buffer holds what arrives (the OBS source logs the
  granted size). A frame the decoder gave up on never reaches the caller: it is counted and reported
  to the sender as a loss.
- `set_max_bpp` changes only the report's cap. A reopen would drop the decoder AND restart the
  report `seq` at 0, which the sender's `AutoBpp` reads as reordered reports for up to
  `REORDER_WINDOW` of them (review round 1 of slice 2).
- `take_last_error` hands out the newest decoder error and the newest failed report send (`report
  to <addr>: ...`), each once; the probe prints it with its window line (`jxs recv: last error:`).
- The decoder callback writes into an `Arc<Mutex<Outbox>>` (uncontended, one thread) only so the
  receiver is `Send` (a compile-time assertion in receiver.rs): the OBS source may open it on one
  thread and poll on another.

### The C ABI: `jxs/recv-ffi` (workspace member, `staticlib` + `rlib`)

- `include/camera_box_jxs.h` is written by hand. `tests/abi.rs` compiles it with `cc` and pins every
  struct size + field offset against the Rust `#[repr(C)]` twins, and pins that the header declares
  exactly the functions `src/lib.rs` exports (each `#[no_mangle]`).
- Calls: `cbjxs_open(port, camera, max_bpp, err, err_len)` (0.0.0.0; camera 0 = the first heard;
  max_bpp 0 = no limit, sent in every report), `cbjxs_next_frame(rx, timeout_ms, &frame)` (1 / 0 /
  -1 socket error / -2 NULL or a panic), `cbjxs_set_max_bpp` (the report's cap, no reopen),
  `cbjxs_recv_buffer_bytes`, `cbjxs_get_stats`, `cbjxs_take_error`, `cbjxs_local_port`,
  `cbjxs_abi_version` (`CBJXS_ABI_VERSION` 2 since slice 3's `cbjxs_frame.chroma`; bump it on any
  struct or signature change), and
  `cbjxs_close`. The stats call is `cbjxs_get_stats`: C cannot have a function named like the
  `cbjxs_stats` typedef. The frame's planes stay valid until the next `next_frame` / `close`.
- No panic crosses into C (`catch_unwind`). A panic poisons the receiver: `-2` from then on, close it.
- **GOTCHA -- SVT's nasm objects carry no `.note.GNU-stack`.** A shared object linking them asks for
  an EXECUTABLE stack (`readelf -lW` GNU_STACK `RWE`), and glibc 2.41+ (the Ubuntu 26.04 strih box)
  refuses to dlopen that. The DistroAV link therefore carries `-z noexecstack`, plus
  `--exclude-libs` so no Rust or SVT symbol is exported from the plugin. `tests/abi.rs` links the
  static library into a `-z defs` shared object with those flags and asserts GNU_STACK `RW` and an
  `nm -D` export list of only the test's own symbol. The deployed `camera-box` BINARY links the same
  objects and also has an executable stack (an executable, so it still runs; a hardening follow-up).
- The static library's native libraries (`rustc --print native-static-libs`, measured 29.10.2026):
  `-lpthread -lm -ldl -lasound -lgcc_s -lutil -lrt -lc`. `-lasound` comes from the camera-box crate's
  Linux deps; with `--as-needed` nothing of it reaches the plugin's NEEDED list.

### The DistroAV source: `vendor/distroav/src/jxs-source.cpp`, kind `camera_box_jxs_source`

- A receiver thread (`jxs-source`) opens the FFI, polls it every 100 ms (`JXS_POLL_MS`) and hands each
  frame to `obs_source_output_video` as `VIDEO_FORMAT_I422`, BT.709 limited range, with
  `timestamp = timecode_100ns x 100` (`cb_jxs_timestamp_ns`): the stamp the NDI source hands over, so
  `genlock_is_wallclock_ts` engages the genlock FIFO and the per-second grid, and libobs writes the
  input's own `genlock-fifo audit '<name>'` line (the existing `jitter_audit` parser reads it, pinned).
- Settings: `jxs_port` (default 7000), `jxs_camera` (0 = the first heard), `jxs_max_bpp` (0 = no
  limit, max 12; a change only moves the report's cap), and the genlock whitelist keys of ndi-source
  that apply: `genlock_fifo` (default ON), `genlock_latency_ms_src` (3..2000, default 3). They reach
  libobs through the same runtime-resolved exports (`obs_source_set_genlock_fifo` / `_latency_ms`).
  None of the NDI-only forced keys (`ndi_sync`, `ndi_bw_mode`, ...) exist here.
- **No `genlock_burn` on this source** (review round 1): the burn sweeps (`obs_burn_filter.py`
  sweep-check / sweep-off, the EVENT contract, the leak guards) enumerate `ndi_source` inputs only,
  so a burn left on here would escape all of them and burn on the strih program. A measurement burn
  on the JPEG XS input needs the sweeps extended first.
- `obs_source_set_genlock_connected` follows the frames on EVERY path (no frame for 3 s = not
  connected, also while the port stays taken or the socket fails). The thread starts from `true`
  (libobs' default), so a source no sender has reached is pushed to "not connected" at once.
- `update()` runs on the GRAPHICS thread (an async video source, the issue-1320 lesson): it only
  stores the settings and bumps a generation counter; a changed port / camera reopens the receiver
  on its own thread, a changed max bpp goes into the next report (`cbjxs_set_max_bpp`). `destroy()` stops and joins the thread (at most one poll + one decode).
  A failed open (port taken) logs once and retries every second; a socket error reopens.
- Logs (`[distroav] ` prefixed):
  - `jxs-source '<name>': listening on UDP P (camera C, 0 = the first heard; max bpp M, 0 = no limit; receive buffer B bytes) -- JPEG XS on the genlock FIFO, the receiver report back to the sender (ABI 2) (issue 1387)`
    (B below ~4 MB on strih-snv = raise `net.core.rmem_max`: a 1080p frame is ~2 MB at 8 bpp)
  - every 5 s: `jxs-source '<name>': fps= frames= lost_slices= concealed= undecodable= reopens=
    decoder_reopens= bpp= reports= report_errors= dropped_unread= foreign= camera= (issue 1387)`
    (`bpp` = the window's payload bits per pixel, the bpp the camera sends; `reopens` = socket
    reopens; `decoder_reopens` = picture-header changes, one per sender bpp step)
  - `frames arriving` / `not connected -- no frame for 3 s (or none yet)`, `max bpp A -> B (0 = no
    limit), sent in the next report`, `cannot listen on UDP P: ...`, `receiver failed (rc): ... --
    reopening`, `last error: ...`.
  The marker `jxs-source '` is not a substring of any `genlock-` line (pinned: the parser reads None).
- **Built ONLY by the strih Linux variant** (`linux-genlock.yml` `linux-genlock-build-strih`): CMake
  option `CAMERA_BOX_JXS_SOURCE` (OFF by default) + `CAMERA_BOX_JXS_FFI_LIB` / `_INCLUDE` /
  `_NATIVE_LIBS`; the registration in `plugin-main.cpp` is under `#ifdef CAMERA_BOX_JXS_SOURCE`.
  Windows DistroAV, the imag full build and the compile-check compile it out: the receiver is
  Linux-only (`recvmmsg`, GRO), the Windows strih is retired, and those builds have no Rust. The job
  installs `nasm libclang-dev` + `dtolnay/rust-toolchain@stable`, runs
  `cargo rustc --release -p jxs-recv-ffi --crate-type staticlib -- --print native-static-libs`, hands
  the list to CMake through `$GITHUB_ENV`, and refuses a `distroav.so` without `camera_box_jxs_source`
  or with GNU_STACK other than `RW`. A change to `jxs/**`, `vendor/svt-jpeg-xs/**`, `src/jxs_recv*`,
  `src/jxs_link*`, `Cargo.toml` or `src/lib.rs` re-runs the strih build. ci.yml lints and tests
  `jxs-recv-ffi`.
- `jxs_resolve` repeats ndi-source.cpp's `resolve_obs_export` (dlsym) on purpose: that one is
  `static` in ndi-source.cpp, carries the `_WIN32` branch and sits under the windows-genlock*.yml
  pwsh anchors; sharing it would touch the Windows builds for a Linux-only source.
- Tests: `tests/jxs_receiver_1387.rs` (the receiver over real UDP), `jxs/recv-ffi/tests/{abi,loopback}.rs`
  (the ABI pins; the real encoder -> the C ABI, timecode + PSNR + the report reaching the sender),
  `tests/distroav_jxs_source_1387.rs` (the decide header's truth table compiled as shipped, a g++
  `-fsyntax-only -Werror` of the whole source against the vendored libobs headers + a stub
  `obsconfig.h`, the anchors), `tests/jxs_strih_build_1387.rs` (the CI wiring).

### Live acceptance (supervisor runbook; strih-snv, cam5, rig TEST mode)

1. CI green, then the FULL strih bundle deploy on strih-snv (`deploy-genlock-fleet.sh`, strih-snv
   arm). The OBS log must show `Plugin features registered: camera-box JPEG XS source (issue 1387)`.
   If OBS refuses to load `distroav.so` with `cannot enable executable stack`, the noexecstack link
   flag did not apply: stop and read the job's GNU_STACK check.
2. Stop any `jxs-recv-probe` still listening on the port (the source cannot bind a taken port; it
   logs `cannot listen on UDP 7000: ... Address already in use` and retries every second).
3. Add the source on strih-snv over OBS-WS (no auth on :4455), from dev1, with the repo's client
   (`obs_phase2._conn` + `_rpc`, so the production-scene guard applies). Put it in a NEW scene, never
   in an operator scene:
   ```python
   import sys; sys.path.insert(0, "scripts"); import obs_phase2 as o
   ws = o._conn("10.77.9.202")
   o._rpc(ws, "CreateScene", {"sceneName": "JXS cam5"})
   o._rpc(ws, "CreateInput", {"sceneName": "JXS cam5", "inputName": "JXS cam5 src",
          "inputKind": "camera_box_jxs_source",
          "inputSettings": {"jxs_port": 7000, "jxs_camera": 5, "jxs_max_bpp": 0,
                            "genlock_fifo": True, "genlock_latency_ms_src": 3}})
   o._rpc(ws, "GetInputSettings", {"inputName": "JXS cam5 src"})   # read the keys back (an input cannot share its scene's name: code 601)
   ```
   The OBS log shows the `listening on UDP 7000` line.
4. On cam5, the slice-1 runtime drop-in with `CAMERA_BOX_JXS_TARGET=10.77.9.202:7000` (rule section
   "Live acceptance" above, step 2) and the deploy's restart.
5. Program: in Studio Mode put `JXS cam5` on the preview, then cut it to the strih program (never
   the stream box, never `PRO`). The OBS log must show `frames arriving`.
6. Grade:
   - `jxs-source 'JXS cam5 src': fps=30.00` every 5 s, `lost_slices=0 undecodable=0 dropped_unread=0`;
   - `genlock-fifo audit 'JXS cam5 src'` clean for 10 min: no `underruns` / `relocks` / `late_holds` /
     `backward_steps` growth (`genlock-jitter-report` reads the log). The JPEG XS input is a 30 fps
     source on the 30 fps strih canvas, so it should take the N==1 pin-derived-depth path (unverified until this run)
     (`genlock-n1-pin-derived-depth.md`), not the N>=2 grid conveyor the 60 fps NDI camera inputs
     take: read `n1_grows=` and the depth it settles on;
   - the cambox's `jxs send: bpp A -> B` lines climb 4 -> 8 within ~1 min while the source reports
     (`reports=5` per window on the source, `report_age_s` < 3 on the sender), and the source's
     `bpp=` reads 8.00; then `jxs_max_bpp` 6 over WS must step the sender down to 6 (`receiver-cap`);
   - a strih screenshot (`GetSourceScreenshot` of `JXS cam5 src` and of `NDI cam5`, same moment, PNG)
     compared with the detail metric + a crop for the owner (the slice-0 comparison method).
7. Leave nothing behind unasked: the `JXS cam5` scene stays only if the owner wants it on the
   program; remove the cambox drop-in (step 4 of the slice-1 runbook) when the run is over.

### Slice 2 known limits

- **Not proven before the strih build ran:** that DistroAV links with the static library on the
  ubuntu-26.04 runner (the native-library list there, `libclang-dev` for bindgen), that OBS loads the
  plugin, and the whole live acceptance above. The gate proves the decisions, the syntax against the
  vendored headers, the ABI, the static library's PIC + noexecstack link, and the FFI end to end.
- The source sets genlock "connected" from frames only; it has no sender-presence signal like NDI's
  connection count.
- One source = one camera = one UDP port. A second camera needs a second source on another port and
  that camera's `CAMERA_BOX_JXS_TARGET` pointing at it.

## Slice 3: the strih program -> stream over JPEG XS (design 6504997031)

The strih PROGRAM goes to stream-snv at JPEG XS quality BESIDE `NDI 2ME PGM` (owner decision 5:
NDI stays). One sender, one receiver: the cambox sender and the slice-2 source, nothing new on the
wire.

### The codec takes 4:2:0 and 4:4:4 (shared code, not a copy)

- `jxs/sys`: `ChromaFormat` (`Yuv420` / `Yuv422` / `Yuv444`); `Yuv422Planes` became `YuvPlanes`
  (`new` = 4:2:2, `with_chroma`). The shim's `cbjxs_enc_settings.chroma` (`CBJXS_CHROMA_*`, the
  shim's own codes, never the library's enum values) picks the SVT colour format; the encoder's
  chroma rows halve at 4:2:0; `cbjxs_chroma_of_format` maps a decoded stream's format, and the
  decoder opens 4:2:0 / 4:2:2 / 4:4:4 (anything else is refused). `EncoderSettings::with_chroma`;
  a geometry the sampling cannot take (odd height at 4:2:0) is refused before the library sees it.
- **The packet count does not change:** a 4:2:0 frame is the header + one packet per 16 LUMA lines
  too (69 at 1080p), measured with the real encoder (`a_420_or_444_picture_encodes_and_decodes_as_itself`,
  the 1080p loopback).
- The receiver's plane pool is chroma-aware (a 4:2:0 frame never lands in a pooled 4:2:2 buffer).

### The sender: a raw planar intake beside the YUYV one (`src/jxs_send/raw.rs` + `handoff.rs`)

- `JxsHandoff::offer_raw(&RawFrame)`: `RawLayout` I420 / I422 / I444 / NV12 (NV12 = 4:2:0 with U V
  interleaved, split on the copy). The caller's thread copies the planes into a free picture
  (`jxs_link::copy_plane` / `split_interleaved`, row padding dropped) and queues it. Never blocks:
  `Offered::Busy` (dropped_busy), `OffGrid` (the 30 fps grid of a faster caller), `Refused` (a
  geometry or stride that does not fit, `encode_errors`). The YUYV `offer` is unchanged.
- The feeder's sessions are keyed on (width, height, chroma): a new sampling opens a new session.
- `JxsSender::start_link(LinkConfig)`: a link with a given stream id, cores (empty = every online
  core, `affinity::online_cores`) and NO egress mark (`JxsSocket::connect_with(_, false)`: only a
  cambox has the issue-1242 egress pacing it exempts the flow from; the "link ON" line says
  `egress mark not used`).
- The status line gained `reports=` (receiver reports read in the window, at the end before
  `(issue 1387)`); the packet thread also publishes each finished window (`take_window`, once per
  window) for a caller with its own log.

### The C ABI: `jxs/send-ffi` (an rlib, carried INSIDE `libjxs_recv_ffi.a`)

- `include/camera_box_jxs_send.h` (by hand; `tests/abi.rs` pins every size, offset, constant and
  exported name): `cbjxs_send_open(cfg)` (target, cores, emit_fps, stream_id, bpp_min/max: 0 = the
  sender's 4 / 8, min == max = fixed; ABI 2: `pace_mbps`, 0 = line rate, a value outside
  100..=100000 refuses the open by name), `cbjxs_send_frame` (1 taken / 0 busy or off-grid / -1 refused
  / -2 NULL or a panic), `cbjxs_send_get_stats` (1 = a window not handed out before),
  `cbjxs_send_take_error`, `cbjxs_send_close` (bounded ~2 s), `cbjxs_send_abi_version` (2 since the send pacing: the
  config's `pace_mbps` + `reserved2`, the stats' `pace_mbps`, `wire_ms_avg`, `wire_ms_max`),
  `cbjxs_send_set_log` (the sender's tracing lines, INFO and above, one line per call, into the
  caller's log; a process-global subscriber, installed once).
- **ONE Rust archive in DistroAV.** `jxs-recv-ffi` depends on `jxs-send-ffi` (`pub use
  jxs_send_ffi;`), so `libjxs_recv_ffi.a` defines both ABIs. Two Rust static libraries in one
  shared object would each carry std, the camera-box crate and a second SVT-JPEG-XS (duplicate
  symbols, two codec init states). The recv-ffi `tests/abi.rs` links a `-z defs` shared object that
  calls every `cbjxs_send_*` from that one archive; the strih CI job `nm`s the archive for
  `cbjxs_send_open` & co. before CMake runs.
- The receive ABI went to 2: `cbjxs_frame.chroma` (`CBJXS_CHROMA_420` 1 / `_422` 2 / `_444` 3, the
  same numbers as `CBJXS_SEND_I420` / `_I422` / `_I444`; the byte came from `reserved`, the layout is
  unchanged). The source maps it to `VIDEO_FORMAT_I420` / `I422` / `I444` and never hands libobs an
  unknown sampling (`jxs_video_format`).
- Tests: `jxs/send-ffi/tests/{abi,log}.rs`, `jxs/recv-ffi/tests/send_loopback.rs` (1080p NV12 + I420
  through BOTH C ABIs and the real encoder: every frame arrives as 4:2:0, timecode carried, 0 lost
  slices, PSNR >= 40 dB per plane, the receiver's reports counted in the sender's window).

### The DistroAV output: `vendor/distroav/src/jxs-output.cpp`, kind `camera_box_jxs_output`

- `raw_video` on libobs' video output thread, the program canvas in its own format (NV12 / I420 /
  I422 / I444; another canvas format refuses to start, named), and only a BT.709 limited-range
  canvas (what the receiving source shows; an sRGB canvas counts, libobs converts it with the 709
  matrix; `cb_jxs_out_colour_ok`, refused by name otherwise).
- **The stamp:** each frame gets `genlock_emit_timecode_100ns(o->framerate)`, the NDI output's ONE
  stamp FUNCTION (it lost its `static` in ndi-output.cpp and is declared in
  `genlock-emit-timecode.h`; the #78 / #1009 anchored lines stay byte-identical, and the genlock-grid
  parity gate still lifts `genlock_floor_boundary_100ns` from ndi-output.cpp). It is read at this
  output's callback, which libobs runs after the NDI output's on the same thread, so near a slot
  edge the JPEG XS copy can carry the next slot's stamp: the same function, not always the same
  value as `NDI 2ME PGM`'s frame. Two callbacks inside one slot (libobs catching up after a lag)
  would carry the same stamp, which the receiver drops as a duplicate unseen; the output skips the
  second and counts it (`stamp_dup=`, `cb_jxs_out_stamp_fresh`). Only a frame the sender TOOK claims
  its slot, so after a busy drop the slot's next frame may still go. A stamp BEHIND the last one (a
  wall-clock step back, the nightly date step) still goes: the receiver resyncs on it.
- It hands the frame to `cbjxs_send_frame` (one copy, never a wait); with both pictures in use the
  frame is dropped and counted.
- It registers as a wall-stamping genlock sender (`obs_output_set_genlock_wall_stamping`) while it
  runs. A failed capture start closes the sender (the #1185 leak class). `stop` ends the data
  capture, then closes the sender. libobs ends the capture on ITS OWN thread
  (`end_data_capture_thread`), so a `raw_video` can still run after `obs_output_end_data_capture`
  returns: `tx_mutex` + `tx = nullptr` are what keep it off a closed sender (keep both); destroy is
  safe because `obs_output_destroy` joins that thread.
- Settings: `jxs_target` (host:port, IPv4), `jxs_stream_id` (1..255, default 100: outside the CAM
  range), `jxs_pace_mbps` (**default 900**: strih-snv sends on 10 GbE and stream-snv, the only
  target today, receives on a 1 GbE port whose switch buffer dropped the tail of every line-rate
  slice burst, `lost_slices` 1-15 in 10 of 21 windows of the slice-3 run, so the auto bpp held at
  4; 900 carries even 12 bpp in time (29.4 ms); the port's other traffic, `NDI 2ME PGM` and the
  rest, read 108 Mb/s on stream-snv's enp56s0 (rx_bytes over 10 s, 29.10.2026, JPEG XS off), so
  while a frame is on the wire the port is offered ~1008 Mb/s: ~8 Mb/s over for ~20 ms at 8 bpp,
  a ~20 KB queue the switch absorbs; if `lost_slices` stays > 0, 850 or 800 is the next step; 0 = line rate, the
  `cb_jxs_out_pace_mbps` clamp keeps any other value inside 100..100000, so a start never fails on
  it; a 10 GbE receiver can take 0 or a higher pace), `jxs_bpp_min` / `jxs_bpp_max` (0 = 4 / 8;
  equal = fixed), `jxs_cores` (a cpulist, empty =
  every core). They apply at the next start.
- **Started from a settings file, driven over obs-websocket.** obs-websocket 5 can `StartOutput` /
  `StopOutput` / `SetOutputSettings` / `GetOutputStatus` an EXISTING output, but has no request
  that creates one. So the plugin creates the named output `camera-box JPEG XS program` at
  FINISHED_LOADING (after the NDI outputs) from `<module config>/jxs-output.json` =
  `~/.config/obs-studio/plugin_config/distroav/jxs-output.json`, and starts it only when that file
  says `"enabled": true` (no file = created with defaults, not started). It is stopped and released
  at OBS_FRONTEND_EVENT_EXIT and PROFILE_CHANGING (a video reset refuses active outputs), and
  re-created at PROFILE_CHANGED. Only the strih Linux build has it (`#ifdef CAMERA_BOX_JXS_SOURCE`,
  the same CMake option as the source, plus `CAMERA_BOX_JXS_SEND_FFI_INCLUDE`).
- Logs (`[distroav] ` prefixed):
  - `jxs-output: 'camera-box JPEG XS program' created from <path> (read|absent: defaults; enabled=, target '...')`
  - `jxs-output 'camera-box JPEG XS program': started -- the program WxH NV12 at 30.00 fps to <target> (stream id 100, bpp 0..0, ...)`
  - every 5 s: `jxs-output '<name>': fps= frames= dropped_busy= offered= taken= not_taken= refused=
    stamp_dup= encode_ms_avg/max= first_slice_ms_avg= copy_ms_avg/max= bpp= auto= reports=
    report_age_s= send_errors= encode_errors= pace_mbps= wire_ms_avg/max= (issue 1387)` (`copy_ms` =
    the copy on libobs' video
    thread; `report_age_s` -1.0 = never a report; the window's raw_video counters reset when the
    sender publishes one)
  - the sender's own lines prefixed `jxs-output: ` (`jxs send: link ON ...`, `encoder open 1920x1080
    4:2:0, ...`, every `bpp A -> B`, the wedge CRITICAL); its own 5 s `jxs send: fps=` line is NOT
    forwarded (`cb_jxs_out_forward_line`), the output's line replaces it.
- Tests: `tests/distroav_jxs_output_1387.rs` (the decide header's truth table, a g++
  `-fsyntax-only -Werror` of the output AND the source against the vendored libobs + both FFI
  headers, the anchors: the one stamp, the format map, start / stop order, the settings-file start,
  the frontend lifecycle, the CMake block, the CI wiring, the source's format map).

### Live acceptance (supervisor runbook; strih-snv -> stream-snv, rig TEST mode)

1. CI green, then the FULL strih bundle deploy on BOTH Linux OBS boxes (strih-snv and stream-snv run
   the same bundle). Each OBS log must show `Plugin features registered: camera-box JPEG XS program
   output (issue 1387)` and the `jxs-output: '...' created from ...` line.
2. **Measure strih's encode cost first** (the design's precondition): on strih-snv, from /dev/shm,
   `jxs-encode-bench` at 1080p 30 fps on the cores the output will use (it cannot take NV12; its
   4:2:2 figure is an upper bound of the 4:2:0 one), next to the running OBS. Pick `jxs_cores` from
   it only if the default (every core) shows contention with OBS's render/encode threads.
3. **The receiver on stream-snv** (over OBS-WS from dev1 with the repo's client, never a scene the
   stream program shows; Studio Mode, never `PRO` on program):
   ```python
   import sys; sys.path.insert(0, "scripts"); import obs_phase2 as o
   ws = o._conn("10.77.9.204")
   o._rpc(ws, "CreateScene", {"sceneName": "JXS PGM"})
   o._rpc(ws, "CreateInput", {"sceneName": "JXS PGM", "inputName": "JXS PGM src",
          "inputKind": "camera_box_jxs_source",
          "inputSettings": {"jxs_port": 7100, "jxs_camera": 100, "jxs_max_bpp": 0,
                            "genlock_fifo": True, "genlock_latency_ms_src": <the NDI 2ME PGM pin>}})
   ```
   Its log: `jxs-source 'JXS PGM src': listening on UDP 7100 (camera 100, ...; receive buffer B bytes)`
   (B is 128 MiB on a box with the OBS baseline's `net.core.rmem_max`). The genlock latency: start
   from the `NDI 2ME PGM` input's `genlock_latency_ms_src` (read it with `GetInputSettings`), so both
   copies of the program sit at the same depth.
4. **The output on strih-snv:** write the settings file (ssh, as the OBS user), then start it over
   WS without an OBS restart:
   ```bash
   ssh newlevel@10.77.9.202 'mkdir -p ~/.config/obs-studio/plugin_config/distroav && cat > ~/.config/obs-studio/plugin_config/distroav/jxs-output.json' <<'JSON'
   {"enabled": true, "jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100, "jxs_bpp_min": 0, "jxs_bpp_max": 0, "jxs_cores": "", "jxs_pace_mbps": 900}
   JSON
   ```
   ```python
   ws = o._conn("10.77.9.202")
   o._rpc(ws, "SetOutputSettings", {"outputName": "camera-box JPEG XS program",
          "outputSettings": {"jxs_target": "10.77.9.204:7100", "jxs_stream_id": 100}})
   o._rpc(ws, "StartOutput", {"outputName": "camera-box JPEG XS program"})
   o._rpc(ws, "GetOutputStatus", {"outputName": "camera-box JPEG XS program"})   # outputActive: true
   ```
   **`jxs_pace_mbps`:** 900 for stream-snv's 1 GbE port (a file without the key gets the same
   default). Change it in the file AND with `SetOutputSettings {"jxs_pace_mbps": N}`, then
   `StopOutput` + `StartOutput` (settings apply at the next start). The started line and the sender's
   `link ON` line name the pace; every status line carries `pace_mbps=900 wire_ms_avg/max=`. If
   stream still shows `lost_slices` > 0 at 900, step down (800, 700) before suspecting anything
   else; `wire_ms_max` must stay under ~30 ms at the bpp the link climbs to.
   The file makes it start by itself at every later OBS start; `"enabled": false` (or deleting the
   file) and `StopOutput` turn it off. **The file is the only persistent store:** the plugin
   re-creates the output from it at every OBS start and every profile change, so a setting changed
   only over WS (`SetOutputSettings`) is lost then. Write the file first, then the WS call. strih's log: `started -- the program 1920x1080 NV12 at 30.00
   fps to 10.77.9.204:7100`, then `jxs-output: jxs send: link ON ...` and `encoder open 1920x1080 4:2:0`.
5. Put `JXS PGM` on stream's PREVIEW (Studio Mode) and read it; never cut it to the stream program
   unasked.
6. **Grade** (10 min):
   - strih: `jxs-output '...': fps=30.00` every 5 s, `dropped_busy=0`, `refused=0`, `encode_ms_avg`
     well under 33.3, `copy_ms_avg/max` (the cost on libobs' video thread: well under 1 ms at 1080p
     NV12), `reports=5` per window, `report_age_s` < 3; the `jxs send: bpp A -> B` lines climb 4 -> 8;
     strih-snv CPU (`top -H -p <obs pid>` for the `jxs-feed` / `jxs-send` / SVT worker threads, and
     the box total) before and after the start; the `genlock-ndi-output audit` lines of the NDI
     outputs unchanged (`dropped=0`, `max_send_wait_ms` as before);
   - the pacing: strih `pace_mbps=900`, `wire_ms_avg` ~10 ms at 4 bpp / ~20 ms at 8 bpp; on stream
     `lost_slices=0` in EVERY window (the slice-3 line-rate run: 10 of 21 windows lost 1-15) and the
     auto bpp climbing to 8;
   - stream: `jxs-source 'JXS PGM src': fps=30.00`, `lost_slices=0 undecodable=0 dropped_unread=0`,
     `bpp=8.00` once the auto bpp has climbed, `decoder_reopens` = one per sender bpp step;
   - `genlock-fifo audit 'JXS PGM src'` clean for 10 min (no `underruns` / `relocks` /
     `late_holds` / `backward_steps` growth, `genlock-jitter-report`), and its `ts_head_skew_ms`
     close to `NDI 2ME PGM`'s;
   - a `GetSourceScreenshot` of `JXS PGM src` and of `NDI 2ME PGM` at the same moment (PNG), the
     detail metric + a crop for the owner (the slice-0 comparison method).
7. Leave nothing behind unasked: `StopOutput` + `"enabled": false` in the file, and remove the `JXS
   PGM` scene on stream, unless the owner wants the link to stay up.

### Slice 3 known limits

- **Not proven before the strih build ran:** the DistroAV link of the output (the archive's sender
  symbols are checked by `nm` in CI and by the recv-ffi shared-object test), that OBS creates and
  starts it, the strih encode cost, and the whole live acceptance above.
- The copy into the picture runs on libobs' video output thread (one 1080p NV12 frame: ~3 MB read,
  the chroma split), like the NDI output's own work there; `copy_ms` on the status line measures it.
- The stamp is taken when libobs hands the frame to the output, like the NDI output's, not at the
  render tick: near a slot edge the two copies of one program frame can carry stamps one slot apart
  (above). Stamping from the frame's own render time (`video_data.timestamp` mapped to the wall
  clock) would make them equal; it would also stop sharing the NDI output's stamp function.
- The colour space is checked at start only (a canvas change needs a profile change, which
  re-creates the output).
- `jxs-encode-bench` codes 4:2:2 only; the strih cost of 4:2:0 is read from the live status line.
- One output, one target. A second receiver needs a second output (not built).

## The automatic bpp (issue 1387, owner decision 6495938345)

- **The back channel: ONE 32-byte receiver report per second** (`jxs_link::ReceiverReport`,
  layout in the `src/jxs_link.rs` module doc next to the slice datagram: magic `'R'`, version 1,
  camera id, max bpp, seq, frames, lost slices, late frames, decode p99 us, last timecode). The
  receiver sends it from its LISTENING socket to the address the slices came from
  (`JxsRecvSocket::recv_batch_from` names it, `send_report` sends it); the sender's socket is
  connected, so only that receiver reaches it (`JxsSocket::take_report`: non-blocking, newest of
  this camera, garbage and queued ICMP errors skipped, at most 64 datagrams per call).
  `jxs-recv-probe` sends it (`--max-bpp`, `--late-ms` default 100, `--no-report`; `reports=` on its
  status line). The slice-2 OBS source must send the same report, or the auto bpp holds at its floor.
- **The controller is pure: `src/jxs_auto_bpp.rs` `AutoBpp`** (crate root, default features,
  every rule unit-tested), ticked once per second on `jxs-send` (`handoff.rs` `BppControl`) with
  the second's frames, encode p99 (submit -> last packet sent), `dropped_busy` (a separate
  per-tick counter, `Stats::dropped_tick`) and encode errors. `AutoBppConfig::new(min, max)`:
  - DOWN one step AT ONCE: a dropped frame, encode p99 > 90 % of 33.3 ms, or a report of lost
    slices / late frames about the CURRENT bpp. "About the current bpp" = the report's newest
    timecode >= the timecode of the first frame sent at it (`applied`), so a report about frames
    of a bpp already left steps nothing; one report acts once (its seq). A receiver cap below the
    current bpp steps straight to it (clamped to the floor).
  - UP one step after 10 clean seconds in a row: frames sent, encode p99 < 60 % of the budget,
    nothing dropped, no encode error, a FRESH (<= 3 s) clean report about the current bpp. Never
    above `max` or the receiver's cap. One unclean second, or a second whose report is not yet
    about the current bpp, restarts the count. One report counts for ONE clean second (its seq
    must move on; a receiver reporting slower than the tick is not credited twice).
  - A report with no frame says nothing about the bpp (no timecode to tie a loss to): the
    receiver counts an undecodable frame as a frame with its timecode and its loss, so a link
    losing whole frames still steps down. A report whose seq is at most `REORDER_WINDOW` (16)
    below the newest is a late (reordered) datagram and is dropped; further below = a restarted
    receiver, which clears the acted/counted seqs.
  - NO report for 3 s: hold, both ways (never climb blind; a silent receiver also never steps
    the sender down).
  - Back-off: every step down from level L doubles the clean run needed to climb back to L
    (10, 20, 40 ... capped at 300 s); 1800 clean REPORTS below it (30 min from a receiver
    reporting every second) halve it again. A session refused AT that bpp counts as a failure of
    it (`Job::Refused { penalize: true }`); a refused frame geometry only cancels the pending
    step (`penalize: false`, `AutoBpp::cancelled`). A receiver CAP adds no back-off (it is the receiver's
    policy, not a failure of the levels above it). `jxs_auto_bpp::LEVELS` (33) bounds the tracked
    levels; `MAX_BPP` (12) is const-asserted below it.
  - Reading the reports consumes the ICMP "port unreachable" a dead receiver queues on the
    connected socket; `JxsSocket::take_read_errors` adds those to `send_errors=`, so a receiver
    that is not listening still shows there.
  - While a step is being applied nothing else is decided (`Hold::Pending`).
- **How a step reaches the encoder: a NEW session, swapped at a frame boundary.** SVT-JPEG-XS has
  no call that changes the bpp of a live session (`svt_jpeg_xs_encoder_init` reads it once). The
  packet thread writes the wanted bpp into an `AtomicU32`; the feeder (`Sessions::prepare`) opens
  a session at it BETWEEN two frames (after a submit, or on its 500 ms idle turn), and
  `Sessions::ensure` swaps it in before the next submit: a `Job::Session { output, bpp }` goes
  into the ordered job queue, so every frame of the old session is sent before the packet thread
  switches, and the old session closes then. Measured on cam5: open 0.2-0.7 ms, close 0.1 ms.
  A refused open sends `Job::Refused`, the controller stays and backs off. Pinned end to end by
  `the_auto_bpp_climbs_on_clean_reports_and_falls_on_a_loss_without_losing_a_frame` (the real
  encoder, a fake receiver: 4 -> 5 on clean reports, back to 4 on a loss, every handed-over frame
  arrives whole).
- Each change logs ONE INFO line: `jxs send: bpp 6 -> 7 (clean-10s; second: frames=30
  encode_p99_ms=14.80 dropped_busy=0 encode_errors=0) (issue 1387)`. The "link ON" line names the
  mode (`auto bpp 4..=8 (starts at 4, ...)` or `6 bpp (fixed by CAMERA_BOX_JXS_BPP, auto off)`).

## Encoder cost on the N150 (29.10.2026, comment 6496253608)

`jxs-encode-bench` (`--features jxs`, `src/bin/jxs-encode-bench.rs`) runs the sender's own
pipelined session with NO socket, paced at 30 fps or saturated, and prints encode p50/p99/max per
bpp plus the session open/close cost. It touches no device; on a cambox run it from /dev/shm next
to the running camera-box (never stop camera-box for it). Its content is synthetic: `noise`
(every pixel detailed, the entropy coder's worst case) or `smooth`, or `--yuv422p FILE`.

| cam5 N150, cores 0-1, 2 slice threads | 4 bpp | 6 | 8 | 10 | 12 |
|---|---|---|---|---|---|
| noise, 30 fps, p50/p99 ms | 11.5/12.3 | 13.0/14.0 | 13.8/14.8 | 13.4/14.4 | 13.1/13.9 |
| smooth, 30 fps, p50/p99 ms | 9.9/10.4 | 10.2/11.0 | 10.1/10.8 | 10.2/11.2 | 10.1/10.9 |
| noise, saturated fps | 80.8 | 72.8 | 69.5 | - | 71.0 |

- 8 bpp on ONE core: 24.2/25.3 ms. So bpp is NOT the N150's CPU limit up to 12; the 1 GbE wire
  is (8 bpp = ~500 Mb/s, 12 = ~750 Mb/s plus NDI).
- **The 29.10 live "8 bpp = 55.8 ms, 23 fps" was the 400 Mb/s egress pacing**: the live wire read
  381 Mb/s = 23 x 2.07 MB x 8, the send blocked on the paced queue and `encode_ms` includes the
  send. The `SO_MARK 0x1387` exemption removes it.
- The library's knobs (`EncoderTuning`, `None` = its default; the shim passes them through and
  `the_coding_knobs_reach_the_library` proves a bad value is refused by the library): rate
  control 0/2/3, vertical prediction 1/2 and uniform quantization each cost 0.3-1.3 ms more;
  sign handling fast/full doubles the cost; significance coding off saves ~1 ms but codes less
  efficiently. The defaults (rc 1, signs 0, sigf 1, vpred 0, deadzone) stay.
- SCHED_FIFO 50 on an idle core pair: 13.5/14.0 ms vs 13.8/14.8 SCHED_OTHER.
- **The encoder SCHED_FIFO is OPT-IN** (`CAMERA_BOX_JXS_FIFO_PRIO`, `affinity::JXS_ENCODER_FIFO_PRIORITY`
  = 50 is the suggested value, below the capture's 90). The feeder raises itself with
  `affinity::set_current_thread_fifo_inherited` (FIFO WITHOUT reset-on-fork) right before it opens
  a session, so the library's worker threads (created inside the open) inherit it: the ONE
  deliberate exception to the reset-on-fork rule, documented in affinity.rs. `jxs-send` takes the
  same priority (`raise_fifo`) WITH reset-on-fork (`affinity::set_current_thread_fifo_reset_on_fork`,
  it spawns nothing to pass it to): a FIFO encoder beside a SCHED_OTHER packet thread on the same
  cores would delay the previous frame's slices. Why opt-in: (1) the measured gain on an idle
  pair is ~0.8 ms at p99 and the budget has 2x headroom; (2) it puts 6 more SCHED_FIFO threads
  into camera-box (`jxs-feed`, `jxs-send`, 2 slice + init + final), and while a prepared session
  waits for its swap its 4 workers double that, all over verify-device `(ah)`'s ceiling of 4, so
  making it the default needs `(ah)` to count the `jxs-feed`/`jxs-send`-named threads apart (the
  workers carry the feeder's name, inherited) -- a live A/B with the HDMI preview running decides
  it first.
- **The HDMI preview cannot move off the encoder cores without touching capture:** on the 4-core
  cambox core 3 is capture, core 2 the reserved capture-IRQ core that must stay quiet (issues
  1198 / 1367 measured drops with the preview there); 0-1 are the only general cores. Read live
  on cam5 (JXS off, no monitor): capture + every `ndis:*` NDI send thread run on core 3, only four
  light camera-box threads on 0-1, and the preview skips rendering without a monitor. With a
  monitor plugged in, its NDI receive + scale share 0-1 with the encoder; the FIFO opt-in is the
  lever, measured live.

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

## Testing gotchas (auto bpp)

- **A new `JxsFrame` field goes AFTER `emit_fps` in main.rs's hand-off literal.**
  `tests/jxs_wiring_1387.rs` reads only the 400 bytes after `jxs.offer(` for the timecode and
  `emit_fps: send_fps`; a field inserted before them pushed `emit_fps` out of the window.
- **main.rs's `capture_realtime_100ns` is not the capture instant while `phase_lock=stamp`**: it
  is the phase tracker's slot middle (`capture_phase::stamp_instant_100ns`). Anything that needs
  the real capture time builds it from `info.capture_monotonic_100ns` with
  `genlock_stamp::capture_realtime_100ns` (the round-3 review 🔴 of `capture_age_ms`).
- **The live `encode_ms` includes the send.** A paced or full socket shows up there as "encode
  time"; measure the codec alone with `jxs-encode-bench` before calling a bpp a CPU limit.

## Testing gotchas (slice 2)

- **A test build never uplifts the static library.** `cargo test -p jxs-recv-ffi` leaves it at
  `target/<profile>/deps/libjxs_recv_ffi-<hash>.a`, not `target/<profile>/libjxs_recv_ffi.a`;
  `tests/abi.rs` takes the newest `deps/libjxs_recv_ffi-*.a` next to its own binary.
  `cargo rustc --release -p jxs-recv-ffi --crate-type staticlib -- --print native-static-libs` (the
  strih job's command) does write `target/release/libjxs_recv_ffi.a` and prints the native libs.
- **The DistroAV source gates read the source files at RUN time.** Editing `jxs-source.cpp` or
  `jxs-source-decide.h` while a full `cargo test` is still running fails those tests for the wrong
  reason. Re-run the full suite after the edits.
- The g++ `-fsyntax-only` of `jxs-source.cpp` needs only a two-define stub `obsconfig.h` plus the
  vendored libobs, `vendor/distroav/src` and `jxs/recv-ffi/include` (it uses `plugin-support.h`'s
  `obs_log`, never the generated `.c`), so it runs in `cargo test` on every push.

## Testing gotchas (slice 1b)

- **A debug test races a RELEASE-built codec.** `build.rs` always builds SVT-JPEG-XS Release, while
  the Rust side (the AVX2 conversion included) runs unoptimised under `cargo test`. A 1080p frame
  can then finish encoding before the next one is converted, so "two frames in flight" never
  happens. The in-flight test converts both frames into an `unstarted()` hand-off FIRST, then calls
  `start_threads`: the feeder submits them microseconds apart whatever the build.
- **A loopback receiver drops a whole-frame burst** with the default receive buffer (two 1080p
  frames are ~2 MB). Set `SO_RCVBUF` to 4 MB (dev1's `net.core.rmem_max`) or use smaller frames.
- **Benching:** a throwaway `examples/` file in the worktree, run as
  `cargo build --release --features jxs --example <name>` with `CAMERA_BOX_JXS_CORES=0-1` so the
  encoder gets the cambox's two cores; delete it before committing and before clippy
  `--all-targets`.

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
   Optional in the same drop-in: `Environment=CAMERA_BOX_JXS_BPP=8` (pins 8, auto off) OR
   `Environment=CAMERA_BOX_JXS_BPP_MAX=10` (auto, default 4..=8), `Environment=CAMERA_BOX_JXS_FIFO_PRIO=50`
   (the encoder FIFO A/B) and `Environment=CAMERA_BOX_JXS_CORES=0-1` (must lie inside the
   off-capture cores).
   Journal: `jxs send: link ON to ... (UDP GSO on, egress mark 0x1387: line rate past the NDI
   pacing, ...) -- JPEG XS auto bpp 4..=8 (starts at 4, ...) at 30 fps ...,
   encoder on cores [0, 1] with 2 slice threads (SCHED_OTHER) (capture core Some(3), capture-IRQ
   core Some(2) untouched)`, `encoder open 1920x1080, <bpp> bpp, ..., opened in X ms`, then every
   5 s the status line above, and one `jxs send: bpp A -> B (<reason>; ...)` line per change.
   The auto bpp climbs one step per 10 clean s only while the probe reports (`report_age_s` < 3).
3. Grade: the `Streaming:` line still 60.0 sent with no capture drop; `fps=30.0`, `frames` ~150
   per 5 s, `dropped_busy=0`, `in_flight` max 1-2 of 2, `encode_ms_avg` well under 33.3,
   `first_slice_ms_avg` under ~1 ms; `send_errors=0`; ~250 Mb/s at 4 bpp (~500 at 8) on the
   wire; camera-box CPU; the #1242 stagger line (`max per-frame work` now includes the
   conversion); the probe's `fps=30.00 lost_slices=0` over 10 min, `latency_ms_p50/p99/max`
   (target: near the encode time, slice 1 was 50 ms), `spread_ms`; `--dump-png 3 --dump-skip 300`
   vs the same frame over NDI (detail metric + a crop). Repeat at 6 and 8 bpp. Auto: start the
   probe, read the `bpp A -> B` lines climb 4 -> 8 within ~1 min, then `bpp=8 auto=on`, `fps=30.0`,
   `dropped_busy=0`; `--no-report` on the probe must leave it holding; `--max-bpp 6` must step it
   down to 6 at once (`receiver-cap`).
4. Remove the drop-in and restart: the link is off, the NDI path is byte-identical.

**Before the issue-1387 egress rerun:** the cambox must carry the classful egress tree. Bring it there
with the live apply program in `.claude/rules/cambox-egress-pacing.md` (runbook step 4: one rw window
closed by the verified ro close, then the unit restart and a graded read-back). Read it back on the
box while the link runs:
- `tc filter show dev <if>` -> `... fw ... handle 0x1387 classid 1:2`;
- `tc -s qdisc show dev <if>` -> the `pfifo 20: parent 1:2` `Sent` bytes grow ~250 Mb/s at 4 bpp,
  while the `fq 10: parent 1:1` still counts `throttled` (NDI paced as before);
- the journal's "link ON" line says `egress mark 0x1387`.

Grade the rerun at 4 and 6 bpp. The probe's `spread_ms_p50` should drop from 23 / 33 ms to the
line-rate ~8-12 ms. `first_arrival_ms` should stop wandering. The latency p50 should approach
~15-20 ms. If the spread stays at the paced value, the mark is missing: check the WARN line, then the
`1:2` counters.

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
- A frame whose slice count (a geometry change) or picture header packet (a bpp change) differs
  from the decoder's restarts it; that frame decodes after its last packet, not slice by slice.
- In TEST mode with the cam1 QR burn active, NDI carries the burned frame and JPEG XS the
  unburned capture: a pixel comparison of the two during an E2E differs by the burn.
- `SO_SNDBUF` asks for 4 MB; the kernel caps it at `net.core.wmem_max` (the "link ON" line logs
  what was granted). A frame is ~1.04 MB, so a small grant makes `send` block the `jxs-send`
  thread (never the capture thread) until the NIC drains it.
- The PNG dump is a stored-deflate writer in `src/jxs_link/png.rs` (no compression, no
  dependency): the probe needs only the `jxs` feature, not the probe feature's `image` crate.
