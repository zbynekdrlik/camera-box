# A/V-sync offset measurement (#188/#145) — cam2 QPSK marker → recording-verdict --av-sync

Measures the video↔audio offset of the stream chain: cam2 paints the dual-QR AND emits a
QPSK audio marker (norihiro-compatible, 442 Hz, CRC-4 payload) on its HDMI audio out; the
marker is mic'd into the stream OBS "mbc" input; `recording-verdict --av-sync` decodes a
stream recording and reports the offset. Convention: `av_offset_ms = video − audio`;
**negative = video LEADS audio** → ADD `latency_adjust_ms` (= −offset) to the video
source's genlock latency (DistroAV "Latency (ms)" on the program NDI input, hot-apply).

## Run recipe (proven 2026-07-02, measured −70.2 ms ±10 @ NDI 2ME PGM latency 1000 ms)

1. cam2 into TEST mode (fb0 free, capture+emit alive — `rig-mode.sh test` / the `/run` systemd
   drop-in). Painter (fresh CI `probe-tools-linux` frame-probe):
   ```
   frame-probe --paint-only --dual-qr --qr-size 700 --paint-fps 60 --duration-secs 300 \
     --audio-marker --audio-marker-device hw:CARD=PCH,DEV=3 \
     --audio-marker-cadence-ticks 180 --marker-log /run/rig-qpsk-markers.csv
   ```
   cadence 180 ≈ 3 s → ~96 markers/300 s. HDMI audio dev is `hw:CARD=PCH,DEV=3` (card0 = intercom,
   no speaker). Verify the run's QR reaches stream program first (`GetSourceScreenshot` on the
   program scene → cv2 QR decode shows `P<run_id>...`).
2. Confirm the mbc input is UNMUTED in stream OBS, then record ~150 s over WS
   (StartRecord → sleep → StopRecord).
3. Painter writes the marker log AT EXIT (`markers=N` log line) — wait for it, then copy the CSV
   to the stream box (`C:\camera-box\`).
4. Decode ON the stream box (has ffmpeg; detached + poll, ~5 min/150 s @1080p):
   ```
   recording-verdict.exe --av-sync <rec.mp4> --av-marker-log <markers.csv> --av-audio-track 0
   ```
   JSON: `av_offset_ms`, `mad_ms` (should be ≤ ~15), `matched` (cluster size; expect ~⅓ of
   emitted markers at 30 fps recording), `latency_adjust_ms`.

## The two measurement-bias gotchas (both cost a full rig cycle — DO NOT reintroduce)

- **ALSA ring bias (124 ms measured!):** the continuous-feed emitter keeps the 8192-frame ring
  nearly full, so a marker SOUNDS ~130–170 ms after enqueue. `playout_frame_id` (qpsk_marker.rs)
  compensates the logged frame_id by `pcm.delay()` converted to painter frames. Raw
  enqueue-instant pairing measured −194.5 ms where the true offset was −70.2 ms.
- **Per-stream origins:** video `frame_index/fps` and audio `sample/48k` are rebased onto the
  container origin via ffprobe `start_time` per stream (mux edit list / encoder priming shift).

## Decode robustness — expect a false-decode FLOOD

CRC-4 is 4 bits → a music-laden mbc mix passes ~1/16 of preamble candidates: 150 s decoded
1691–2521 audio "markers" for 96 real. This is protocol physics, not a decoder bug. The pairing
is therefore `av_offset_candidates` + `cluster_offset_ms` (densest ±60 ms window of
video−audio candidates); NEVER a sequential/lockstep walk (desyncs on one early false hit — the
live 0-paired failure). `--av-cluster-tol-ms` tunes the window.

## Interpreting the number

- The measurement is a clap-test analog: QR pixels on the cam2 monitor vs marker leaving its
  speakers, through the real mic → Dante/ASIO path. Monitor-internal skew is part of the chain
  (as for a real clap); acoustic flight to the mic ≈ 1 ms / 34 cm.
- The measured offset is valid AT the video source's genlock latency at record time — always
  read `genlock_latency_ms_src` (GetInputSettings) of the program NDI input alongside the
  measurement. Target latency = current + `latency_adjust_ms`.
- Emit params ride in the marker-log `#` header (`# qpsk-params sr=48000 carrier=442 c=1 q=2
  vr=60/1`); the decoder must demodulate with the same params (`AudioParams::rig60()`).
