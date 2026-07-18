#!/usr/bin/env python3
"""#801 — AI A/V-sync meter: SyncNet offset of the stream-OBS program output, zero management.

Wraps syncnet_python (pretrained SyncNet + S3FD): measures the audio-visual offset of a clip
of the PROGRAM output and maps it onto the one knob the operator actually turns — the
'NDI 2ME PGM' genlock latency. Confidence gating makes it self-managing: windows without a
usable face/lip correlation (band wide shots, graphics) report as unmeasurable and are simply
skipped — nobody has to tell it "now there is speech".

Sign convention (VALIDATED 2026-07-19 on synthetic shifts, exact to the frame):
  SyncNet offset +N frames  = audio LEADS video by N*40 ms (video late)   -> LOWER the latency knob
  SyncNet offset -N frames  = audio LAGS  video by N*40 ms (video early)  -> RAISE the latency knob
  => knob_delta_ms = -offset_frames * 40

Usage:
  av_sync_measure.py --media clip.mp4 [--repo DIR] [--webhook URL] [--threshold-ms 60]
  av_sync_measure.py --grab srt://127.0.0.1:9998 --secs 20 [...]        # one-shot from live tap
  av_sync_measure.py --grab ... --loop 300 [...]                        # daemon: measure every 300 s
Exit codes: 0 measured, 2 unmeasurable window (low confidence), 1 error.
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

CONF_MIN = 4.0  # below this SyncNet's own confidence, the window is unusable (no face / no lips)
FRAME_MS = 40  # syncnet pipeline is fixed 25 fps


def run(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, **kw)


def grab_clip(url: str, secs: int, out: Path) -> None:
    r = run(["ffmpeg", "-v", "error", "-y", "-i", url, "-t", str(secs),
             "-vf", "scale=960:-2,fps=25", "-c:v", "libx264", "-preset", "veryfast", "-crf", "26",
             "-c:a", "aac", "-ar", "16000", "-ac", "1", str(out)])
    if r.returncode != 0 or not out.exists():
        sys.exit(f"ERROR: grab failed: {r.stderr.strip()[:300]}")


def measure(repo: Path, media: Path, workdir: Path):
    """Run syncnet_python's two stages; return list of (offset_frames, confidence) per track."""
    py = sys.executable
    ref = "m"
    for stage in ("run_pipeline.py", "run_syncnet.py"):
        r = run([py, str(repo / stage), "--videofile", str(media), "--reference", ref,
                 "--data_dir", str(workdir)], cwd=str(repo))
        if stage == "run_pipeline.py" and r.returncode != 0:
            sys.exit(f"ERROR: {stage} failed: {(r.stderr or r.stdout).strip()[-300:]}")
        out = (r.stdout or "") + (r.stderr or "")
    tracks = []
    # SyncNetInstance logs per track: "AV offset:  N" then "Confidence: C"
    offsets = [int(m) for m in re.findall(r"AV offset:\s*(-?\d+)", out)]
    confs = [float(m) for m in re.findall(r"Confidence:\s*([\d.]+)", out)]
    tracks = list(zip(offsets, confs))
    return tracks


def notify_discord(webhook: str, text: str) -> None:
    data = json.dumps({"content": text}).encode()
    req = urllib.request.Request(webhook, data=data, headers={"Content-Type": "application/json"})
    try:
        urllib.request.urlopen(req, timeout=15).read()
    except OSError as exc:
        print(f"WARN: discord webhook failed: {exc}")


def one_measurement(args, repo: Path) -> int:
    with tempfile.TemporaryDirectory(prefix="avsync-") as td:
        workdir = Path(td)
        if args.grab:
            media = workdir / "grab.mp4"
            grab_clip(args.grab, args.secs, media)
        else:
            media = Path(args.media).resolve()
            if not media.exists():
                sys.exit(f"ERROR: no such file: {media}")
        tracks = measure(repo, media, workdir)

    usable = [t for t in tracks if t[1] >= CONF_MIN]
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    if not usable:
        best = max(tracks, key=lambda t: t[1], default=(0, 0.0))
        print(f"[{stamp}] UNMEASURABLE window (best confidence {best[1]:.1f} < {CONF_MIN}"
              f" — no usable face/lips; band/graphics segments are expected to skip)")
        return 2

    offset_frames, conf = max(usable, key=lambda t: t[1])
    offset_ms = offset_frames * FRAME_MS
    knob = -offset_ms
    if offset_ms > 0:
        verdict = f"audio predbieha video o ~{offset_ms} ms -> ZNIZ '2ME PGM' latency o {abs(knob)}"
    elif offset_ms < 0:
        verdict = f"video predbieha audio o ~{abs(offset_ms)} ms -> ZVYS '2ME PGM' latency o {abs(knob)}"
    else:
        verdict = "A/V sync OK (offset 0 ms)"
    line = f"[{stamp}] AV offset {offset_frames:+d} fr ({offset_ms:+d} ms) conf {conf:.1f} :: {verdict}"
    print(line)
    if args.webhook and abs(offset_ms) >= args.threshold_ms:
        notify_discord(args.webhook, f"🎯 AV-sync watchdog: {verdict} (conf {conf:.1f})")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser()
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--media", help="measure an existing clip file")
    src.add_argument("--grab", help="grab N secs from this ffmpeg-readable URL (srt://, rtmp://...)")
    ap.add_argument("--secs", type=int, default=20)
    ap.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent / "syncnet_python"),
                    help="path to syncnet_python checkout (with data/ + s3fd weights)")
    ap.add_argument("--webhook", help="Discord webhook for |offset| >= threshold alerts")
    ap.add_argument("--threshold-ms", type=int, default=60)
    ap.add_argument("--loop", type=int, metavar="SECS",
                    help="daemon mode: repeat every SECS (grab mode only)")
    args = ap.parse_args()

    repo = Path(args.repo).resolve()
    if not (repo / "run_syncnet.py").exists():
        sys.exit(f"ERROR: syncnet_python repo not found at {repo}")
    if not shutil.which("ffmpeg"):
        sys.exit("ERROR: ffmpeg not on PATH")

    if args.loop:
        if not args.grab:
            sys.exit("ERROR: --loop needs --grab")
        while True:
            t0 = time.time()
            try:
                one_measurement(args, repo)
            except SystemExit as exc:
                print(f"WARN: round failed: {exc}")
            time.sleep(max(10, args.loop - (time.time() - t0)))
    return one_measurement(args, repo)


if __name__ == "__main__":
    sys.exit(main())
