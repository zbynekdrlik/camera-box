---
paths:
  - "src/jxs_link.rs"
  - "src/jxs_link/**"
  - "src/jxs_send.rs"
  - "src/jxs_send/**"
  - "src/jxs_recv.rs"
  - "src/bin/jxs-recv-probe.rs"
  - "src/bin/jxs-encode-bench.rs"
  - "src/jxs_auto_bpp.rs"
  - "src/jxs_bench.rs"
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
    encode_errors= report_age_s= (issue 1387)`.
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
- **Probe: `jxs-recv-probe`** (`--features jxs`, CI uploads it in `probe-tools-linux-amd64`).

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
