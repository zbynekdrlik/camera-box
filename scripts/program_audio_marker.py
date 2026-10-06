#!/usr/bin/env python3
"""issue 1404 -- the program-audio guard's QPSK marker decoder: a ctypes binding to the shim.

WHY a shim and not Python: the marker decoder already exists twice -- the Rust reference
(`src/qpsk_marker.rs`) and its byte-for-byte C++ port in the av-sync dock
(`vendor/av-sync-dock/src/camera-box-marker-scan.hpp`), pinned to each other by a parity gate. A
third copy in Python would need its own parity proof and could drift (design: issue 1404 comment
6026235697, Approach 1). So `scripts/qpsk_guard_shim.cpp` wraps the C++ port in a C ABI and
`scripts/build-qpsk-guard-shim.sh` compiles it with g++ on dev1 (no cargo, Tier-0).

This module only DECODES: it returns every CRC-valid word per channel. Which of those are real
markers -- the timecode chain -- is decided in scripts/program_audio.py.

A library that is missing, unloadable or of another ABI raises `DecoderUnavailable`; the sampler then
writes UNKNOWN and exits (fail closed, never a spectral-only MEASUREMENT). A library built from other
sources than this checkout's (`sources_stale`) still loads -- it is the same decoder family -- and
the sampler logs a WARNING asking for a rebuild.
"""
from __future__ import annotations

import ctypes
import hashlib
import os
import pathlib

import numpy as np

SHIM_ENV = "QPSK_GUARD_SHIM"
DEFAULT_SHIM_PATH = os.path.join(os.path.expanduser("~"), ".local", "lib", "camera-box",
                                 "libqpsk-guard-shim.so")
SHIM_ABI = 2
_ROOT = pathlib.Path(__file__).resolve().parents[1]
# The decoder sources, in the order build-qpsk-guard-shim.sh hashes them (pinned by a test).
SOURCES = (
    "scripts/qpsk_guard_shim.cpp",
    "vendor/av-sync-dock/src/camera-box-audio.hpp",
    "vendor/av-sync-dock/src/camera-box-marker-scan.hpp",
)


class DecoderUnavailable(RuntimeError):
    """The shim cannot be used: missing, unloadable, or another ABI."""


class DecodeError(RuntimeError):
    """The shim refused one buffer (a negative return)."""


def default_shim_path() -> str:
    return os.environ.get(SHIM_ENV) or DEFAULT_SHIM_PATH


def sources_sha256(root: pathlib.Path = _ROOT) -> str:
    """sha256 over the decoder sources concatenated in SOURCES order (= the build script's hash)."""
    h = hashlib.sha256()
    for rel in SOURCES:
        h.update((root / rel).read_bytes())
    return h.hexdigest()


class MarkerDecoder:
    """The loaded shim. `decode(samples, sample_rate)` -> one list per channel of
    (start_s, index) CRC-valid words, in time order."""

    def __init__(self, path: str | None = None):
        self.path = path or default_shim_path()
        if not os.path.isfile(self.path):
            raise DecoderUnavailable(
                f"QPSK marker decoder shim {self.path} is missing -- build it with "
                "scripts/build-qpsk-guard-shim.sh")
        try:
            lib = ctypes.CDLL(self.path)
            lib.qpsk_guard_abi.restype = ctypes.c_int
            lib.qpsk_guard_abi.argtypes = []
            abi = int(lib.qpsk_guard_abi())
        except (OSError, AttributeError) as exc:
            raise DecoderUnavailable(f"QPSK marker decoder shim {self.path} is not loadable: {exc}") from exc
        if abi != SHIM_ABI:
            raise DecoderUnavailable(
                f"QPSK marker decoder shim {self.path} has ABI {abi}, this sampler needs {SHIM_ABI} -- "
                "rebuild it with scripts/build-qpsk-guard-shim.sh")
        lib.qpsk_guard_params.restype = None
        lib.qpsk_guard_params.argtypes = [ctypes.POINTER(ctypes.c_uint32)] * 3 + [ctypes.POINTER(ctypes.c_double)]
        lib.qpsk_guard_source_sha256.restype = ctypes.c_char_p
        lib.qpsk_guard_source_sha256.argtypes = []
        lib.qpsk_guard_decode_channel.restype = ctypes.c_int
        lib.qpsk_guard_decode_channel.argtypes = [
            ctypes.POINTER(ctypes.c_float), ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
            ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(ctypes.c_uint8), ctypes.c_int,
        ]
        self._lib = lib
        sr, carrier, c = ctypes.c_uint32(), ctypes.c_uint32(), ctypes.c_uint32()
        thr = ctypes.c_double()
        lib.qpsk_guard_params(ctypes.byref(sr), ctypes.byref(carrier), ctypes.byref(c), ctypes.byref(thr))
        self.params = {"sample_rate": int(sr.value), "carrier_hz": int(carrier.value), "c": int(c.value),
                       "threshold": float(thr.value)}
        self.built_sha256 = (lib.qpsk_guard_source_sha256() or b"").decode("ascii", "replace")

    def sources_stale(self, root: pathlib.Path = _ROOT) -> bool:
        """True when this library was built from other decoder sources than the checkout's."""
        return self.built_sha256 != sources_sha256(root)

    def _decode_channel(self, buf: np.ndarray, channel: int, sample_rate: int) -> list[tuple[int, int]]:
        frames, channels = buf.shape
        ptr = buf.ctypes.data_as(ctypes.POINTER(ctypes.c_float))
        cap = frames // 32 + 8  # a word is >= 32 samples long at any usable rate; retried below if not
        for _ in range(2):
            starts = (ctypes.c_int64 * cap)()
            idx = (ctypes.c_uint8 * cap)()
            n = self._lib.qpsk_guard_decode_channel(ptr, frames, channels, channel, int(sample_rate),
                                                    starts, idx, cap)
            if n < 0:
                raise DecodeError(f"QPSK marker decoder refused the buffer (code {n}: frames={frames} "
                                  f"channels={channels} sample_rate={sample_rate})")
            if n <= cap:
                return [(int(starts[k]), int(idx[k])) for k in range(n)]
            cap = n
        raise DecodeError(f"QPSK marker decoder returned more words than it reported ({n} > {cap})")

    def decode(self, samples, sample_rate: int) -> list[list[tuple[float, int]]]:
        """Every CRC-valid word per channel as (start_s, index); `samples` (n,) or (n, channels)."""
        buf = np.asarray(samples, dtype=np.float32)
        if buf.ndim == 1:
            buf = buf[:, None]
        if buf.ndim != 2 or buf.shape[1] < 1:
            raise DecodeError(f"decode: need (n,) or (n, channels), got shape {buf.shape}")
        buf = np.ascontiguousarray(buf)
        sr = float(sample_rate)
        return [[(start / sr, index) for start, index in self._decode_channel(buf, c, sample_rate)]
                for c in range(buf.shape[1])]
