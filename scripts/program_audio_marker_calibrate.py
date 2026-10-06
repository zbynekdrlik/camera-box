#!/usr/bin/env python3
"""issue 1404 -- re-check the program-audio guard's marker bars on real and synthetic audio.

The two bars of ROZHODNUTÉ issue 1404 comment 6026577906 (the rule: 6026826572), computed through
the REAL sampler loop (program_audio_sampler.run, a fake receiver feeding the audio in NDI-sized
blocks, the real marker decoder shim) -- so a bar is checked on exactly what the dev1 service does:

  (a) REAL measurement audio: no window reads FOREIGN, and every window with a full marker span
      (non-silent) has a chain >= MARKER_CHAIN_MIN + 2 -- a margin of two missed decodes;
  (b) SYNTHETIC in-band content with no marker (held chords, tremolo chords, 200-800 Hz
      band-limited noise, a random-note melody), stereo with R 10.17 ms behind L like the
      measurement track: in every trial, every 3 consecutive judged windows hold a FOREIGN.

The tests (tests/python/test_program_audio_marker_1404.py) run it on the committed fixtures and a
few synthetic trials. The full calibration -- the four session recordings (1851 windows) in
~/.claude/work-products/issue-1404/audio/ and 50 trials per class -- is this CLI:

  python3 scripts/program_audio_marker_calibrate.py --shim <lib.so> \\
      --real rec2.flac rec3a.flac rec3b.flac session.flac --synthetic-trials 50

It prints one line per file and per class and exits 1 when a bar fails (0 when all hold).
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from typing import NamedTuple

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import program_audio as pa  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_sampler as pas  # noqa: E402

BLOCK_FRAMES = 1024          # an OBS audio tick, the size NDI delivers
STEREO_DELAY_S = 0.01017     # the measurement track's R-behind-L delay
SYNTH_SR = 48000
SYNTH_CLASSES = ("chord", "tremolo", "bandnoise", "melody")
SYNTH_LEVELS_DBFS = (-30.0, -15.0)
RUN_OF = 3                   # bar (b): a FOREIGN in every RUN_OF consecutive judged windows


class _Block(NamedTuple):
    sample_rate: int
    samples: np.ndarray


class _FileReceiver:
    """Feeds one buffer in NDI-sized blocks, then nothing."""

    def __init__(self, samples: np.ndarray, sample_rate: int):
        self._blocks = [_Block(sample_rate, samples[i:i + BLOCK_FRAMES])
                        for i in range(0, samples.shape[0], BLOCK_FRAMES)]

    def __len__(self):
        return len(self._blocks)

    def capture(self, _timeout_ms):
        return self._blocks.pop(0) if self._blocks else None

    def connections(self):
        return 1


def load_audio(path: str) -> tuple[np.ndarray, int]:
    """Decode an audio file with ffmpeg: (float32 (n, channels), sample_rate)."""
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries",
         "stream=sample_rate,channels", "-of", "csv=p=0", path],
        capture_output=True, text=True, check=True)
    sr, channels = (int(v) for v in probe.stdout.strip().split(",")[:2])
    raw = subprocess.run(["ffmpeg", "-v", "error", "-i", path, "-f", "f32le", "-"],
                         capture_output=True, check=True).stdout
    return np.frombuffer(raw, dtype="<f4").reshape(-1, channels).copy(), sr


def evaluate(samples: np.ndarray, sample_rate: int, decoder) -> list[dict]:
    """The window payloads the sampler writes for this audio (the start UNKNOWN dropped)."""
    x = np.asarray(samples, dtype=np.float32)
    if x.ndim == 1:
        x = x[:, None]
    rx = _FileReceiver(np.ascontiguousarray(x), sample_rate)
    payloads: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="program-audio-calibrate-") as serve:
        pas.run(rx, serve, source="calibration", decoder=decoder, mono=lambda: 0.0,
                max_loops=len(rx), on_write=payloads.append, log=lambda _m: None)
    return payloads[1:]


def judged(payloads: list[dict]) -> list[dict]:
    """The windows with a full marker span (their verdict depends on the chain)."""
    return [p for p in payloads if p["marker_chain"] is not None]


def bar_real(payloads: list[dict]) -> tuple[bool, dict]:
    """Bar (a): no FOREIGN, and every judged non-silent window's chain >= MIN + 2."""
    j = [p for p in judged(payloads) if p["verdict"] != "SILENT"]
    chains = [p["marker_chain"] for p in j]
    foreign = sum(1 for p in payloads if p["verdict"] == "FOREIGN")
    need = pa.MARKER_CHAIN_MIN + 2
    ok = bool(j) and foreign == 0 and min(chains) >= need and all(p["verdict"] == "MEASUREMENT" for p in j)
    return ok, {"windows": len(payloads), "judged": len(j), "foreign": foreign,
                "min_chain": min(chains) if chains else None, "need": need}


def bar_synthetic(per_trial: list[list[dict]]) -> tuple[bool, dict]:
    """Bar (b): in every trial every RUN_OF consecutive judged windows hold a FOREIGN. Also the
    worst chain a run held (the bar needs it below MARKER_CHAIN_MIN)."""
    ok = True
    worst = 0
    judged_n = 0
    for payloads in per_trial:
        j = judged(payloads)
        judged_n += len(j)
        if len(j) < RUN_OF:
            ok = False
        for k in range(len(j) - RUN_OF + 1):
            run = j[k:k + RUN_OF]
            worst = max(worst, min(p["marker_chain"] for p in run))
            if not any(p["verdict"] == "FOREIGN" for p in run):
                ok = False
    return ok, {"trials": len(per_trial), "judged": judged_n, "worst_run_chain": worst}


# ---------------------------------------------------------------------------------------------
# synthetic in-band content (generated, never downloaded music)
# ---------------------------------------------------------------------------------------------


def _held_chord(rng, n, sr):
    t = np.arange(n) / sr
    partials = rng.uniform(200.0, 800.0, size=int(rng.integers(2, 6)))
    return sum(np.sin(2 * np.pi * f * t + rng.uniform(0, 2 * np.pi)) for f in partials)


def _tremolo_chord(rng, n, sr):
    t = np.arange(n) / sr
    return _held_chord(rng, n, sr) * (0.6 + 0.4 * np.sin(2 * np.pi * rng.uniform(2.0, 8.0) * t))


def _band_noise(rng, n, sr):
    f = np.fft.rfftfreq(n, 1.0 / sr)
    return np.fft.irfft(np.fft.rfft(rng.standard_normal(n)) * ((f >= 200.0) & (f <= 800.0)), n)


def _melody(rng, n, sr):
    out = np.zeros(n)
    i = 0
    while i < n:
        d = int(rng.uniform(0.1, 0.5) * sr)
        f0 = rng.uniform(200.0, 800.0)
        t = np.arange(min(d, n - i)) / sr
        env = np.minimum(1.0, t / 0.01) * np.exp(-t * rng.uniform(1.0, 6.0))
        note = sum((0.5 ** k) * np.sin(2 * np.pi * f0 * (k + 1) * t + rng.uniform(0, 2 * np.pi))
                   for k in range(3) if f0 * (k + 1) <= 800.0)
        out[i:i + t.shape[0]] += env * note
        i += d
    return out


_GENERATORS = {"chord": _held_chord, "tremolo": _tremolo_chord, "bandnoise": _band_noise, "melody": _melody}


def synthetic_stream(kind: str, level_dbfs: float, seconds: float, rng, sr: int = SYNTH_SR) -> np.ndarray:
    """One continuous stereo stream of `kind` at `level_dbfs` RMS, R STEREO_DELAY_S behind L."""
    n = int(round(seconds * sr))
    x = _GENERATORS[kind](rng, n, sr)
    x = x * (10 ** (level_dbfs / 20.0) / np.sqrt(np.mean(x * x)))
    d = int(STEREO_DELAY_S * sr)
    return np.stack([x, np.concatenate([np.zeros(d), x[:-d]])], axis=1).astype(np.float32)


def synthetic_trials(kind: str, level_dbfs: float, trials: int, windows: int, decoder,
                     seed: int = 1404) -> list[list[dict]]:
    rng = np.random.default_rng([seed, SYNTH_CLASSES.index(kind), int(abs(level_dbfs))])
    return [evaluate(synthetic_stream(kind, level_dbfs, windows * pa.WINDOW_S, rng), SYNTH_SR, decoder)
            for _ in range(trials)]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description="issue 1404 -- re-check the program-audio marker bars")
    ap.add_argument("--shim", default=None, help="the decoder shim (default as the sampler's)")
    ap.add_argument("--real", nargs="*", default=[], help="real measurement audio files (bar a)")
    ap.add_argument("--synthetic-trials", type=int, default=0, help="trials per class and level (bar b)")
    ap.add_argument("--synthetic-windows", type=int, default=10, help="2 s windows per trial")
    ap.add_argument("--classes", default=",".join(SYNTH_CLASSES),
                    help=f"comma-separated synthetic classes (default all: {','.join(SYNTH_CLASSES)})")
    ap.add_argument("--json", action="store_true", help="one JSON object per line")
    args = ap.parse_args(argv)
    decoder = pam.MarkerDecoder(args.shim)
    all_ok = True

    def emit(kind, name, ok, detail):
        rec = {"bar": kind, "name": name, "ok": ok, **detail}
        print(json.dumps(rec) if args.json else
              f"bar {kind} {name}: {'OK' if ok else 'FAIL'} " + " ".join(f"{k}={v}" for k, v in detail.items()),
              flush=True)

    for path in args.real:
        samples, sr = load_audio(path)
        ok, detail = bar_real(evaluate(samples, sr, decoder))
        all_ok &= ok
        emit("a", os.path.basename(path), ok, detail)
    classes = [c for c in args.classes.split(",") if c]
    unknown = [c for c in classes if c not in SYNTH_CLASSES]
    if unknown:
        ap.error(f"unknown synthetic class(es) {unknown}; known: {SYNTH_CLASSES}")
    if args.synthetic_trials > 0:
        for kind in classes:
            for level in SYNTH_LEVELS_DBFS:
                ok, detail = bar_synthetic(synthetic_trials(kind, level, args.synthetic_trials,
                                                            args.synthetic_windows, decoder))
                all_ok &= ok
                emit("b", f"{kind}@{level:g}dBFS", ok, detail)
    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
