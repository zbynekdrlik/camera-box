#!/usr/bin/env python3
"""issue 1404 Task 5 part a -- the camera-box MEASUREMENT CLIP (`measurement-clip-v1.mp4`).

WHY: nothing copyrighted may reach YouTube (owner amendment, issue 1404 comment 6016489928: "nech to
je normalne qr meracie video aj s zvukovym meracim generatorom"). SongPlayer's test item and the cg
OBS test scene play THIS file instead of music, so the CG segments of the release E2E carry the same
instrument as the camera chain: a per-frame painter-format QR and the cam2 QPSK marker.

What the clip IS: a 30 fps recording of the cam2 painter, synthesized. Every parameter that a
decoder reads is the painter's, so the existing decoders read the clip with the painter's rules (the
YouTube-leg tick decoder reads the reserved id 911016 only when asked: `runs=CLIP_RUNS`):
  picture  1920x1080, 30 fps, H.264 yuv420p. Frame f shows the painter's dual-QR Vernier for the
           60 Hz tick T = 2f (src/probe/painter.rs `vernier_ids`): LEFT = the latest even tick (T),
           RIGHT = the latest odd tick (T - 1), each `P911016.{tick}.{pts_ns}.{crc32}`
           (src/probe/payload.rs; pts_ns = the tick's content time), EC-H with a 4-module quiet
           zone, the module size floor(700 / modules) of the qrcode crate's renderer, centred in its
           half, top at 24 px (`--qr-size 700`, `TOP_MARGIN_PX`). A seven-segment frame counter sits
           in the central gap between the two QRs (where the painter puts its colour column).
  sound    48 kHz stereo, L == R. The QPSK marker (src/qpsk_marker.rs: 20-bit word = preamble 0xF,
           zero nibble, 8-bit index, CRC-4; carrier 442 Hz, c = 1, raised-cosine edges, amplitude
           0.8) every 30 ticks (0.5 s), index = tick & 0xFF (`frame_id_to_index`), over a
           -30 dBFS tone bed at `program_audio.MEASUREMENT_TONE_LINES_HZ` (imported: the guard
           removes exactly that line, any other bed frequency reads FOREIGN). Quantized to int16
           like the painter's `to_stereo_i16`.
  log      `<clip>.markers.csv` in the painter's marker-log format (`# qpsk-params ...`, then
           `index,frame_id,emit_ts_ns`), emit_ts_ns = the marker's content time in the clip.

The 60 Hz tick (not the 30 fps frame number) and the painter's index are what the YouTube-leg
timeline (`continuity(step=2)`, `PAINTER_HZ = 60`) and the program-audio guard's timecode chain
(`MARKER_INDEX_RATE_HZ = 60`) need. Design-question on issue 1404 (comment 6046375956): the plan's
literal "QR frame number + index = frame / 15" reads UNKNOWN on the timeline and FOREIGN on the guard;
the tick rule lives in TICK_HZ / TICKS_PER_FRAME / MARKER_EVERY_TICKS below.

ONE parameter set: TICK_HZ and the tone line come from program_audio (the guard); the run id from
youtube_leg_ticks (the tick decoder); the QPSK and geometry constants are pinned to the Rust and C++
sources and to the decoder shim's compiled-in parameters by tests/python/test_gen_measurement_clip_1404.py.

Deterministic on one machine: the same generator version with the same ffmpeg and numpy builds
gives the same bytes (x264 threads pinned, bitexact muxing, no timestamps in the metadata). Another
CPU or build may take other float paths (AAC, sin/cos), so the published sha256 is the build box's.
The CLI prints the sha256. The encoded file must hold exactly seconds x 30 frames (ffprobe), or no
clip is written.

  python3 scripts/gen_measurement_clip.py --out measurement-clip-v1.mp4 [--seconds 128]
"""
from __future__ import annotations

import argparse
import hashlib
import math
import os
import signal
import subprocess
import sys
import tempfile
import threading
import zlib

import numpy as np

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

import program_audio as pa  # noqa: E402  (the guard: tone line + marker index rate)
import youtube_leg_ticks as ylt  # noqa: E402  (the tick decoder: the clip's run id)

# -- the ONE parameter block -------------------------------------------------------------------
CLIP_VERSION = 1
RUN_ID = ylt.MEASUREMENT_CLIP_RUN_ID      # 911016: an origin like SongPlayer's 911014, never a camera
WIDTH, HEIGHT = 1920, 1080                # the painter canvas (frame-probe --canvas-w / --canvas-h)
FPS = 30
SECONDS = 128  # issue 1404 ROZHODNUTE 6048179415: 7680 ticks = 30 index wraps, a loop keeps the marker line
TICK_HZ = int(pa.MARKER_INDEX_RATE_HZ)    # 60: the painter's tick rate = the guard's index rate
TICKS_PER_FRAME = TICK_HZ // FPS          # 2: a 30 fps recording of the painter sees every 2nd tick
QR_SIZE = 700                             # cam2-painter.service --qr-size
TOP_MARGIN_PX = 24                        # src/colour_scale.rs TOP_MARGIN_PX
QR_BORDER_MODULES = 4                     # the qrcode crate's quiet zone (render().quiet_zone(true))
MARKER_EVERY_TICKS = 30                   # frame-probe --audio-marker-cadence-ticks default (0.5 s)
INDEX_MODULUS = pa.MARKER_INDEX_MODULUS   # 256: the 8-bit index = tick & 0xFF
SAMPLE_RATE = 48_000                      # src/qpsk_marker.rs AUDIO_SAMPLE_RATE_HZ
CHANNELS = 2
CARRIER_HZ = 442                          # CARRIER_HZ_DEFAULT
CYCLES_PER_SYMBOL = 1                     # c = auto_c(q=2, 442, 60/1)
N_PAYLOAD_BITS = 20
N_SYMBOLS = N_PAYLOAD_BITS // 2
PREAMBLE_NIBBLE = 0xF
CRC4_POLY = 0x13
MARKER_AMPLITUDE = 0.8                    # AMPLITUDE
CONTINUOUS_CYCLES = 0.25                  # AUDIO_CONTINUOUS_CYCLES (raised-cosine edges)
QPSK_Q = 2                                # Q_FRAMES (only in the `# qpsk-params` header)
TONE_LINES_HZ = pa.MEASUREMENT_TONE_LINES_HZ
TONE_BED_HZ = TONE_LINES_HZ[0]
TONE_BED_DBFS = -30.0                     # as program_audio.analyse measures it: 10*log10(mean x^2)
X264_THREADS = 4                          # pinned: x264's bitstream depends on the thread count
# seven-segment frame counter, black on white in the gap between the two QRs
DIGITS = 4
DIGIT_W, DIGIT_H, DIGIT_STROKE, DIGIT_GAP = 48, 88, 10, 12
SEGMENTS = {"0": "abcdef", "1": "bc", "2": "abged", "3": "abgcd", "4": "fgbc",
            "5": "afgcd", "6": "afgedc", "7": "abc", "8": "abcdefg", "9": "abcdfg"}


# -- picture -------------------------------------------------------------------------------------

def vernier_ids(tick: int) -> tuple[int, int]:
    """src/probe/painter.rs `vernier_ids`: (latest even tick <= tick, latest odd tick <= tick)."""
    left = tick & ~1
    right = 0 if tick == 0 else min((tick - 1) | 1, tick)
    return left, right


def tick_pts_ns(tick: int) -> int:
    """The content time of a 60 Hz tick in ns, rounded half up (tick 2f = frame f's pts)."""
    return (2 * tick * 1_000_000_000 + TICK_HZ) // (2 * TICK_HZ)


def qr_payload(run_id: int, tick: int, gen_ts_ns: int) -> str:
    """src/probe/payload.rs `Payload::encode`: P{run}.{frame_id}.{gen_ts_ns}.{crc32 of the body}."""
    body = f"{run_id}.{tick}.{gen_ts_ns}"
    return f"P{body}.{zlib.crc32(body.encode('ascii'))}"


def frame_payloads(frame: int) -> tuple[str, str]:
    """(left, right) QR texts of clip frame `frame`: the painter's pair for tick TICKS_PER_FRAME * frame.
    A side's gen_ts_ns is the time it was last painted fresh, i.e. its own tick's time (painter #854)."""
    left, right = vernier_ids(TICKS_PER_FRAME * frame)
    return qr_payload(RUN_ID, left, tick_pts_ns(left)), qr_payload(RUN_ID, right, tick_pts_ns(right))


def qr_modules(text: str) -> np.ndarray:
    """The QR (EC level H, quiet zone included) of `text` as a bool module matrix, True = dark."""
    import qrcode

    q = qrcode.QRCode(version=None, error_correction=qrcode.constants.ERROR_CORRECT_H, box_size=1,
                      border=QR_BORDER_MODULES)
    q.add_data(text)
    q.make(fit=True)
    return np.asarray(q.get_matrix(), dtype=bool)


def qr_placement(n_modules: int, right_half: bool) -> tuple[int, int, int]:
    """(x, y, module_px) of an n-module QR (quiet zone included) in its half of the canvas: the
    qrcode crate's renderer with min = max = QR_SIZE gives module_px = QR_SIZE // n, the painter's
    blit centres it in the half (`render_qr_dual_bgra`) and anchors it to the top (`VAnchor::Top`)."""
    module = QR_SIZE // n_modules
    if module < 1:
        raise ValueError(f"a {n_modules}-module QR does not fit {QR_SIZE} px")
    size = module * n_modules
    half = WIDTH // 2
    band_x, band_w = (half, WIDTH - half) if right_half else (0, half)
    return band_x + (band_w - size) // 2, min(TOP_MARGIN_PX, HEIGHT - size), module


def _blit_qr(canvas: np.ndarray, text: str, right_half: bool) -> None:
    m = qr_modules(text)
    x, y, module = qr_placement(m.shape[0], right_half)
    img = np.where(np.repeat(np.repeat(m, module, axis=0), module, axis=1), 0, 255).astype(np.uint8)
    canvas[y:y + img.shape[0], x:x + img.shape[1]] = img


def counter_box() -> tuple[int, int, int, int]:
    """(x, y, w, h) of the frame counter: centred on the canvas centre line, at the QR band's middle."""
    w = DIGITS * DIGIT_W + (DIGITS - 1) * DIGIT_GAP
    return (WIDTH - w) // 2, TOP_MARGIN_PX + QR_SIZE // 2 - DIGIT_H // 2, w, DIGIT_H


def _segment_rects(x: int, y: int) -> dict[str, tuple[int, int, int, int]]:
    w, h, t = DIGIT_W, DIGIT_H, DIGIT_STROKE
    mid = y + h // 2
    return {"a": (x + t, y, w - 2 * t, t), "b": (x + w - t, y + t, t, h // 2 - t),
            "c": (x + w - t, mid, t, h // 2 - t), "d": (x + t, y + h - t, w - 2 * t, t),
            "e": (x, mid, t, h // 2 - t), "f": (x, y + t, t, h // 2 - t),
            "g": (x + t, mid - t // 2, w - 2 * t, t)}


def draw_counter(canvas: np.ndarray, frame: int) -> None:
    """The frame number as DIGITS seven-segment digits (black on the white canvas)."""
    text = f"{frame:0{DIGITS}d}"
    if len(text) > DIGITS:
        raise ValueError(f"frame {frame} needs more than {DIGITS} counter digits")
    x0, y0, _, _ = counter_box()
    for k, ch in enumerate(text):
        rects = _segment_rects(x0 + k * (DIGIT_W + DIGIT_GAP), y0)
        for seg in SEGMENTS[ch]:
            x, y, w, h = rects[seg]
            canvas[y:y + h, x:x + w] = 0


def render_frame(frame: int) -> np.ndarray:
    """Clip frame `frame` as a (HEIGHT, WIDTH) uint8 gray image."""
    canvas = np.full((HEIGHT, WIDTH), 255, dtype=np.uint8)
    left, right = frame_payloads(frame)
    _blit_qr(canvas, left, right_half=False)
    _blit_qr(canvas, right, right_half=True)
    draw_counter(canvas, frame)
    return canvas


# -- sound ---------------------------------------------------------------------------------------

def crc4(data: int, size: int) -> int:
    """src/qpsk_marker.rs `crc4` (vendor/av-sync-dock/tool/videogen.py)."""
    data <<= 4
    p = CRC4_POLY << (size - 1)
    s = size
    while s > 0:
        if data & (0x8 << s):
            data ^= p
        s -= 1
        p >>= 1
    return data


def payload_word(index: int) -> int:
    """The 20-bit word: preamble 0xF, zero nibble, 8-bit index, CRC-4 (`payload_word`)."""
    data16 = (PREAMBLE_NIBBLE << 12) | (index & 0xFF)
    return (data16 << 4) | crc4(data16, 16)


def symbols(word20: int) -> list[int]:
    """The 10 QPSK symbols of a word, MSB first (`symbols`)."""
    return [(word20 >> (N_PAYLOAD_BITS - 2 - 2 * i)) & 0b11 for i in range(N_SYMBOLS)]


def signal_len() -> int:
    return N_SYMBOLS * CYCLES_PER_SYMBOL * SAMPLE_RATE // CARRIER_HZ


def marker_signal(index: int) -> np.ndarray:
    """`marker_signal_for_word(payload_word(index))`: the 10-symbol waveform, amplitude 1.0, float32.
    Symbol mapping 0 sin, 1 cos, 2 -cos, 3 -sin; a raised-cosine ramp of CONTINUOUS_CYCLES carrier
    cycles where a symbol changes (and at the first rising and the last falling edge)."""
    s = np.asarray(symbols(payload_word(index)), dtype=np.int64)
    n = signal_len()
    i = np.arange(n, dtype=np.int64)
    period = SAMPLE_RATE * CYCLES_PER_SYMBOL
    phase = i.astype(np.float64) * 2.0 * math.pi * CARRIER_HZ / SAMPLE_RATE
    k = np.minimum((i * CARRIER_HZ) // period, N_SYMBOLS - 1)
    sym = s[k]
    v = np.select([sym == 0, sym == 1, sym == 2, sym == 3],
                  [np.sin(phase), np.cos(phase), -np.cos(phase), -np.sin(phase)])
    f_sym = ((i * CARRIER_HZ) % period).astype(np.float64) / SAMPLE_RATE
    prev = np.where(k > 0, s[np.maximum(k - 1, 0)], -1)
    nxt = np.where(k + 1 < N_SYMBOLS, s[np.minimum(k + 1, N_SYMBOLS - 1)], -1)
    rise = (f_sym < CONTINUOUS_CYCLES) & (sym != prev)
    fall = ~rise & ((CYCLES_PER_SYMBOL - f_sym) < CONTINUOUS_CYCLES) & (sym != nxt)
    v = np.where(rise, v * (0.5 - np.cos(f_sym / CONTINUOUS_CYCLES * math.pi) * 0.5), v)
    v = np.where(fall, v * (0.5 - np.cos((CYCLES_PER_SYMBOL - f_sym) / CONTINUOUS_CYCLES * math.pi) * 0.5), v)
    return v.astype(np.float32)


def marker_schedule(frames: int) -> list[tuple[int, int, int]]:
    """(frame, tick, index) of every marker of a `frames`-long clip: one every MARKER_EVERY_TICKS
    ticks from the first cadence point (the painter fires at tick 30, never at 0), index = tick &
    0xFF. A marker starts on a frame inside the clip and its signal (22.6 ms) is shorter than one
    frame (pinned by a test), so every marker ends inside the clip."""
    every = MARKER_EVERY_TICKS // TICKS_PER_FRAME
    return [(f, TICKS_PER_FRAME * f, (TICKS_PER_FRAME * f) % INDEX_MODULUS) for f in range(every, frames, every)]


def tone_bed(n: int) -> np.ndarray:
    """The bed as program_audio measures TONE_BED_DBFS: a sine of peak sqrt(2) * 10^(dBFS/20), built
    from ONE exact period (TONE_BED_HZ must divide the sample rate), so every period is bit-identical."""
    period = SAMPLE_RATE / TONE_BED_HZ
    if period != int(period):
        raise ValueError(f"the tone bed {TONE_BED_HZ} Hz has no whole period at {SAMPLE_RATE} Hz")
    p = int(period)
    one = math.sqrt(2.0) * 10.0 ** (TONE_BED_DBFS / 20.0) * np.sin(2.0 * math.pi * np.arange(p) / p)
    return np.tile(one, n // p + 1)[:n]


def _round_half_away(x: np.ndarray) -> np.ndarray:
    return np.where(x >= 0, np.floor(x + 0.5), np.ceil(x - 0.5))


def render_audio(frames: int) -> np.ndarray:
    """The clip's sound: int16 (n, CHANNELS), L == R, bed + MARKER_AMPLITUDE * marker."""
    n = frames * SAMPLE_RATE // FPS
    mono = tone_bed(n)
    cache: dict[int, np.ndarray] = {}
    for f, _, index in marker_schedule(frames):
        sig = cache.setdefault(index, marker_signal(index).astype(np.float64))
        start = f * SAMPLE_RATE // FPS
        mono[start:start + sig.shape[0]] += MARKER_AMPLITUDE * sig
    pcm = np.clip(_round_half_away(mono * 32767.0), -32768, 32767).astype("<i2")
    return np.repeat(pcm[:, None], CHANNELS, axis=1)


def qpsk_params_line() -> str:
    """The painter's marker-log header (src/qpsk_marker.rs `qpsk_marker_log_header`)."""
    return (f"# qpsk-params sr={SAMPLE_RATE} carrier={CARRIER_HZ} c={CYCLES_PER_SYMBOL} q={QPSK_Q} "
            f"vr={TICK_HZ}/1")


def marker_log_text(frames: int) -> str:
    rows = "".join(f"{index},{tick},{tick_pts_ns(tick)}\n" for _, tick, index in marker_schedule(frames))
    return f"{qpsk_params_line()}\nindex,frame_id,emit_ts_ns\n{rows}"


# -- encode --------------------------------------------------------------------------------------

def ffmpeg_cmd(audio_path: str, out_path: str) -> list[str]:
    """Video from stdin (gray rawvideo), sound from the s16le file; pinned for byte-identical output."""
    return ["ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "gray", "-video_size", f"{WIDTH}x{HEIGHT}",
            "-framerate", str(FPS), "-i", "pipe:0",
            "-f", "s16le", "-ar", str(SAMPLE_RATE), "-ac", str(CHANNELS), "-i", audio_path,
            "-map", "0:v:0", "-map", "1:a:0",
            "-c:v", "libx264", "-preset", "medium", "-crf", "18", "-g", str(FPS), "-keyint_min", str(FPS),
            "-sc_threshold", "0", "-threads", str(X264_THREADS), "-pix_fmt", "yuv420p",
            "-color_range", "tv", "-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709",
            "-c:a", "aac", "-b:a", "192k",
            "-fflags", "+bitexact", "-flags:v", "+bitexact", "-flags:a", "+bitexact",
            "-map_metadata", "-1", "-metadata", f"title=camera-box measurement clip v{CLIP_VERSION}",
            "-movflags", "+faststart", "-f", "mp4", out_path]


def _kill_group(pid: int) -> None:
    """SIGKILL ffmpeg's whole process group (its own session); a group already gone is logged."""
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        print(f"gen_measurement_clip: ffmpeg group {pid} already exited", file=sys.stderr)


def _remove_part(part: str) -> None:
    if os.path.exists(part):
        os.remove(part)


def _feed_ffmpeg(proc: subprocess.Popen, frames: int) -> bool:
    """Write every frame to ffmpeg's stdin. False when ffmpeg stopped reading (a broken pipe):
    a clip it did not read in full is a failure whatever its exit code says."""
    try:
        for f in range(frames):
            proc.stdin.write(render_frame(f).tobytes())
        proc.stdin.close()
        return True
    except BrokenPipeError:
        print("gen_measurement_clip: ffmpeg stopped reading its video input", file=sys.stderr)
        try:
            proc.stdin.close()
        except BrokenPipeError as exc:
            print(f"gen_measurement_clip: unflushed video input dropped ({exc})", file=sys.stderr)
        return False


def _atomic_write_text(path: str, text: str) -> None:
    tmp = f"{path}.part"
    with open(tmp, "w", encoding="ascii", newline="\n") as f:
        f.write(text)
    os.replace(tmp, path)


def write_clip(out_path: str, seconds: int = SECONDS, marker_log_path: str | None = None,
               timeout_s: float = 3600.0) -> str:
    """Encode the clip to `out_path` (through a `.part` file, renamed only after ffmpeg succeeded and
    the file holds exactly seconds x FPS frames) and its marker log next to it (default
    `<out>.markers.csv`). Returns the marker log path. Every failure is a RuntimeError naming the
    cause (ffmpeg's stderr tail), never a partial clip. `timeout_s` bounds the whole encode, the
    frame feed included: a watchdog kills ffmpeg's process group when it runs out."""
    if seconds <= 0:
        raise ValueError(f"seconds must be positive, got {seconds}")
    frames = int(seconds) * FPS
    if frames > 10 ** DIGITS:
        raise ValueError(f"{seconds} s = {frames} frames does not fit the {DIGITS}-digit frame counter")
    log_path = marker_log_path or f"{out_path}.markers.csv"
    part = f"{out_path}.part"
    with tempfile.TemporaryDirectory(prefix="measurement-clip-") as tmp:
        audio_path = os.path.join(tmp, "audio.s16le")
        render_audio(frames).tofile(audio_path)
        err_path = os.path.join(tmp, "ffmpeg.err")
        with open(err_path, "w+b") as err:
            proc = subprocess.Popen(ffmpeg_cmd(audio_path, part), stdin=subprocess.PIPE,
                                    stdout=subprocess.DEVNULL, stderr=err, start_new_session=True)
            expired = threading.Event()

            def _expire(pid=proc.pid):
                expired.set()
                _kill_group(pid)

            watchdog = threading.Timer(timeout_s, _expire)
            watchdog.start()
            try:
                fed = _feed_ffmpeg(proc, frames)
                rc = proc.wait(timeout=timeout_s)
            except BaseException as exc:
                _kill_group(proc.pid)
                proc.wait()
                _remove_part(part)
                if isinstance(exc, subprocess.TimeoutExpired):
                    raise RuntimeError(f"ffmpeg did not finish {out_path} in {timeout_s:.0f} s") from exc
                raise
            finally:
                watchdog.cancel()
            err.seek(0)
            tail = err.read().decode("utf-8", "replace")[-800:]
        if rc != 0 or not fed:
            _remove_part(part)
            why = f"the {timeout_s:.0f} s bound ran out" if expired.is_set() else f"exit {rc}"
            raise RuntimeError(f"ffmpeg failed ({why}{'' if fed else ', input not read in full'}) "
                               f"writing {out_path}: {tail.strip()}")
    try:
        try:
            got = ylt.container_frames(part)
        except (RuntimeError, ValueError, OSError, subprocess.SubprocessError) as exc:
            raise RuntimeError(f"{out_path}: the encoded file cannot be probed: {exc}") from exc
        if got != frames:
            raise RuntimeError(f"{out_path}: the encoded file holds {got} frames, expected {frames}")
        os.replace(part, out_path)
    except BaseException:  # a failed probe, a missing ffprobe, a stop: never a partial clip
        _remove_part(part)
        raise
    _atomic_write_text(log_path, marker_log_text(frames))
    return log_path


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def _stop_on_sigterm(signum, _frame):
    print(f"gen_measurement_clip: stopped by signal {signum}; cleaning up", file=sys.stderr)
    raise SystemExit(128 + signum)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="issue 1404 -- write the camera-box measurement clip")
    ap.add_argument("--out", required=True, help="the clip path (mp4), e.g. measurement-clip-v1.mp4")
    ap.add_argument("--seconds", type=int, default=SECONDS, help=f"length in seconds (default {SECONDS})")
    ap.add_argument("--marker-log", default=None, help="the marker log path (default <out>.markers.csv)")
    args = ap.parse_args(argv)
    # A stopped run (SIGTERM, a cancelled job) unwinds like Ctrl-C: write_clip kills ffmpeg's group
    # and removes the `.part`, and the temp audio dir is cleaned. Without it a killed generator left
    # an orphaned ffmpeg encoding and its 64 MB temp audio behind (seen in the lane's RED runs).
    signal.signal(signal.SIGTERM, _stop_on_sigterm)
    try:
        log_path = write_clip(args.out, args.seconds, args.marker_log)
    except (RuntimeError, ValueError, OSError) as exc:
        print(f"gen_measurement_clip: {exc}", file=sys.stderr)
        return 1
    print(f"{sha256_file(args.out)}  {os.path.getsize(args.out)} bytes  {args.out}")
    print(f"{sha256_file(log_path)}  {os.path.getsize(log_path)} bytes  {log_path}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
