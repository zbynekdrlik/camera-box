"""Issue 1404 -- shared helpers for the YouTube-leg tool tests (not a test module).

Loads scripts/youtube_leg_verdict.py once (it re-exports the tick, timeline and audio parts), and
builds synthetic painter frames / videos: real QR codes carrying CRC-valid painter payloads
(src/probe/payload.rs), the left QR optionally in equal-gray colours only the blue channel reads.
"""
import importlib.util
import pathlib
import subprocess
import sys
import zlib

import numpy as np

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "youtube_leg_verdict.py"
FIX = ROOT / "tests" / "fixtures" / "youtube_leg_1404"


def _load():
    mod = sys.modules.get("youtube_leg_verdict")
    if mod is None:
        spec = importlib.util.spec_from_file_location("youtube_leg_verdict", SCRIPT)
        mod = importlib.util.module_from_spec(spec)
        sys.modules["youtube_leg_verdict"] = mod
        spec.loader.exec_module(mod)
    return mod


ylv = _load()

# BGR module colours of EQUAL gray (226): gray sees no QR at all, the blue channel sees 0 vs 255.
# A stand-in for the mid-transition colour pattern the camera captures on the half just repainted.
COLOUR_DARK, COLOUR_LIGHT = (0, 255, 255), (255, 255, 158)
FPS = 30
SECONDS = 8
NOISE = "anoisesrc=d={d}:c=pink:r=48000:a=0.05:seed=7"


def payload(tick, run=123456, gen=17):
    """A CRC-valid painter payload P{run}.{tick}.{gen}.{crc32 of the body}."""
    body = f"{run}.{tick}.{gen}"
    return f"P{body}.{zlib.crc32(body.encode())}"


def qr_frame(left, right, w=960, h=540, size=280, left_colours=((0, 0, 0), (255, 255, 255))):
    """The painter's two QRs on white; `left_colours` = (dark, light) module colours of the left QR."""
    import cv2

    enc = cv2.QRCodeEncoder.create()
    f = np.full((h, w, 3), 255, np.uint8)
    for k, text in enumerate((left, right)):
        if text is None:
            continue
        q = cv2.resize(enc.encode(text), (size, size), interpolation=cv2.INTER_NEAREST)
        x0 = (w // 2) * k + (w // 2 - size) // 2
        dark, light = left_colours if k == 0 else ((0, 0, 0), (255, 255, 255))
        block = np.empty((size, size, 3), np.uint8)
        block[:] = light
        block[q < 128] = dark
        f[20:20 + size, x0:x0 + size] = block
    return f


def write_video(path, ticks, right_only=(), colour_left=(), even_phase=False):
    """A lossless (FFV1, RGB) 960x540 30 fps clip with the painter's two QRs (left = even tick,
    right = tick + 1, or tick - 1 when captured on even painter ticks) and a deterministic
    pink-noise audio track (identical in every generated file). Frames in `right_only` have no left
    QR, frames in `colour_left` a left QR only the blue channel reads."""
    ff = subprocess.Popen(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgr24",
                           "-s", "960x540", "-r", str(FPS), "-i", "-", "-f", "lavfi", "-i",
                           NOISE.format(d=SECONDS + 1), "-map", "0:v:0", "-map", "1:a:0", "-c:v", "ffv1",
                           "-pix_fmt", "bgr0", "-c:a", "aac", "-b:a", "192k", "-shortest", str(path)],
                          stdin=subprocess.PIPE)
    for k, t in enumerate(ticks):
        left = None if k in right_only else payload(t)
        colours = (COLOUR_DARK, COLOUR_LIGHT) if k in colour_left else ((0, 0, 0), (255, 255, 255))
        ff.stdin.write(qr_frame(left, payload(t - 1 if even_phase else t + 1), left_colours=colours).tobytes())
    ff.stdin.close()
    assert ff.wait() == 0
