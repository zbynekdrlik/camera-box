"""issue 1404 -- test helpers for the program-audio guard's QPSK marker decoder.

`build_shim(out_dir)` builds the real shim with the real build script (g++, no cargo), so every test
that decodes runs the dock's own decoder exactly as the dev1 sampler loads it. `NoMarkers` and
`FixedChain` are decoder fakes for sampler tests whose subject is not the decode.
"""
from __future__ import annotations

import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]
BUILD_SCRIPT = ROOT / "scripts" / "build-qpsk-guard-shim.sh"


def build_shim(out_dir: pathlib.Path) -> str:
    out = pathlib.Path(out_dir) / "libqpsk-guard-shim.so"
    r = subprocess.run(["bash", str(BUILD_SCRIPT), str(out)], capture_output=True, text=True,
                       timeout=300)
    assert r.returncode == 0, f"build-qpsk-guard-shim.sh failed:\n{r.stdout}\n{r.stderr}"
    assert out.is_file(), r.stdout
    return str(out)


def real_marker_words(n: int, start_s: float = 0.1, start_index: int = 17, gap_s: float = 0.5,
                      frames_per_marker: int = 30) -> list[tuple[float, int]]:
    """A real emitter cadence: one word every `gap_s`, the index advancing `frames_per_marker`."""
    return [(start_s + k * gap_s, (start_index + k * frames_per_marker) % 256) for k in range(n)]


class NoMarkers:
    """A decoder that finds nothing on any channel."""

    def decode(self, samples, sample_rate):
        ch = 1 if getattr(samples, "ndim", 1) == 1 else samples.shape[1]
        return [[] for _ in range(ch)]


class FixedChain:
    """A decoder that finds a full real chain (8 markers) on channel 0 of every span."""

    def decode(self, samples, sample_rate):
        ch = 1 if getattr(samples, "ndim", 1) == 1 else samples.shape[1]
        return [real_marker_words(8)] + [[] for _ in range(ch - 1)]
