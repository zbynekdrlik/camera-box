#!/usr/bin/env python3
"""issue 1404 -- a thin ctypes NDI AUDIO receiver over dev1's libndi (`/usr/lib/ndi/libndi.so.6`).

WHY ctypes (framework-first, evidence): the only in-repo NDI receive path, `camera_box::ndi::
NdiReceiver` (src/ndi.rs, used by src/bin/ndi-recv-probe.rs), is VIDEO-only -- it passes a NULL
audio frame to `NDIlib_recv_capture_v3` -- and a Rust change there would ship only through a CI
artifact with no local verification (Tier-0). dev1 has no Python NDI binding (no `NDIlib` /
`cyndilib`); `ndi-python` would bundle its own libndi next to the fleet's 6.3.2. So: ctypes over
the libndi the fleet already uses, with the struct layouts copied from the vendored SDK headers
(vendor/distroav/lib/ndi/Processing.NDI.structs.h, .Recv.h, .Lib.h) and pinned by
tests/python/test_program_audio_1404.py.

The receiver:
  * is created BY NAME (`source_to_connect_to.p_ndi_name`, no URL): the SDK runs its own finder and
    reconnects by itself when the sender restarts;
  * asks for `NDIlib_recv_bandwidth_audio_only` -- no video ever crosses the network;
  * discovers by mDNS only (no extra IPs), so while the rig is away at an event behind tailscale it
    simply finds nothing and pulls nothing over the mobile link.

Only FLTP (planar 32-bit float, the SDK's receive format) is accepted; each frame is COPIED into a
numpy array `(no_samples, no_channels)` before the SDK frame is freed. The block also carries the
frame's SDK `timestamp` (100 ns, the moment the sender submitted it; NDIlib_recv_timestamp_undefined
when the SDK has none): the sampler judges receive continuity on that sender timeline, never on its
own arrival time (program_audio.frame_continues, design issue 1404 comment 6030385284).
"""
from __future__ import annotations

import ctypes
import ctypes.util
import os
from typing import NamedTuple

import numpy as np

from program_audio import NDI_TIMESTAMP_UNDEFINED

# -- SDK enum values (Processing.NDI.structs.h / Processing.NDI.Recv.h) --
FRAME_TYPE_NONE = 0
FRAME_TYPE_AUDIO = 2
FRAME_TYPE_ERROR = 4
BANDWIDTH_AUDIO_ONLY = 10
COLOR_FORMAT_BGRX_BGRA = 0
FOURCC_FLTP = ord("F") | ord("L") << 8 | ord("T") << 16 | ord("p") << 24

LIB_ENV = "NDI_LIB_PATH"
# /usr/lib/ndi = dev1; /usr/local/lib = strih-lx (its NDI 6.3.2 runtime, setup-strih step 4b; the
# sampler moved there, issue 1404 ROZHODNUTÉ 6039368611).
DEFAULT_LIB_CANDIDATES = ("/usr/lib/ndi/libndi.so.6", "/usr/lib/ndi/libndi.so", "/usr/local/lib/libndi.so.6",
                          "libndi.so.6")
RECEIVER_NAME = "camera-box program-audio sampler"


class NDIlib_source_t(ctypes.Structure):
    _fields_ = [("p_ndi_name", ctypes.c_char_p), ("p_url_address", ctypes.c_char_p)]


class NDIlib_recv_create_v3_t(ctypes.Structure):
    _fields_ = [
        ("source_to_connect_to", NDIlib_source_t),
        ("color_format", ctypes.c_int),
        ("bandwidth", ctypes.c_int),
        ("allow_video_fields", ctypes.c_bool),
        ("p_ndi_recv_name", ctypes.c_char_p),
    ]


class NDIlib_audio_frame_v3_t(ctypes.Structure):
    _fields_ = [
        ("sample_rate", ctypes.c_int),
        ("no_channels", ctypes.c_int),
        ("no_samples", ctypes.c_int),
        ("timecode", ctypes.c_int64),
        ("FourCC", ctypes.c_int),
        ("p_data", ctypes.c_void_p),
        ("channel_stride_in_bytes", ctypes.c_int),  # union with data_size_in_bytes
        ("p_metadata", ctypes.c_char_p),
        ("timestamp", ctypes.c_int64),
    ]


class AudioBlock(NamedTuple):
    sample_rate: int
    samples: np.ndarray  # float32, shape (no_samples, no_channels)
    timestamp: int = NDI_TIMESTAMP_UNDEFINED  # the frame's SDK timestamp (100 ns, sender submission)


def frame_to_array(frame: NDIlib_audio_frame_v3_t) -> np.ndarray:
    """Copy one FLTP frame into a float32 array (no_samples, no_channels). Planes may be padded:
    each channel starts `channel_stride_in_bytes` after the previous one."""
    if frame.FourCC != FOURCC_FLTP:
        raise ValueError(f"NDI audio FourCC {frame.FourCC:#x} is not FLTP")
    n, ch, stride = frame.no_samples, frame.no_channels, frame.channel_stride_in_bytes
    if n <= 0 or ch <= 0:
        return np.zeros((0, max(ch, 0)), dtype=np.float32)
    if stride < n * 4 or not frame.p_data:
        raise ValueError(f"NDI audio frame: bad layout (samples={n} channels={ch} stride={stride})")
    raw = ctypes.string_at(frame.p_data, stride * (ch - 1) + n * 4)
    out = np.empty((n, ch), dtype=np.float32)
    for c in range(ch):
        out[:, c] = np.frombuffer(raw, dtype=np.float32, count=n, offset=c * stride)
    return out


def _load_library(lib_path: str | None) -> ctypes.CDLL:
    """An explicit `lib_path` is the ONLY candidate (no silent fallback to another libndi); else
    $NDI_LIB_PATH, the fleet's /usr/lib/ndi paths, then the loader's own search."""
    if lib_path:
        candidates = [lib_path]
    else:
        candidates = [os.environ[LIB_ENV]] if os.environ.get(LIB_ENV) else []
        candidates.extend(DEFAULT_LIB_CANDIDATES)
        found = ctypes.util.find_library("ndi")
        if found:
            candidates.append(found)
    errors = []
    for cand in candidates:
        try:
            return ctypes.CDLL(cand)
        except OSError as exc:
            errors.append(f"{cand}: {exc}")
    raise OSError("libndi not loadable -- tried " + "; ".join(errors))


class NdiAudioReceiver:
    """An audio-only NDI receiver for one named source. `capture()` returns an AudioBlock, or None
    when nothing arrived within the timeout (or a non-audio frame type came back)."""

    def __init__(self, source_name: str, lib_path: str | None = None,
                 receiver_name: str = RECEIVER_NAME):
        self.source_name = source_name
        lib = _load_library(lib_path)
        lib.NDIlib_initialize.restype = ctypes.c_bool
        lib.NDIlib_initialize.argtypes = []
        lib.NDIlib_destroy.restype = None
        lib.NDIlib_destroy.argtypes = []
        lib.NDIlib_recv_create_v3.restype = ctypes.c_void_p
        lib.NDIlib_recv_create_v3.argtypes = [ctypes.POINTER(NDIlib_recv_create_v3_t)]
        lib.NDIlib_recv_destroy.restype = None
        lib.NDIlib_recv_destroy.argtypes = [ctypes.c_void_p]
        lib.NDIlib_recv_capture_v3.restype = ctypes.c_int
        lib.NDIlib_recv_capture_v3.argtypes = [
            ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(NDIlib_audio_frame_v3_t),
            ctypes.c_void_p, ctypes.c_uint32,
        ]
        lib.NDIlib_recv_free_audio_v3.restype = None
        lib.NDIlib_recv_free_audio_v3.argtypes = [ctypes.c_void_p, ctypes.POINTER(NDIlib_audio_frame_v3_t)]
        lib.NDIlib_recv_get_no_connections.restype = ctypes.c_int
        lib.NDIlib_recv_get_no_connections.argtypes = [ctypes.c_void_p]
        if not lib.NDIlib_initialize():
            raise RuntimeError("NDIlib_initialize() returned false (CPU not supported?)")
        self._lib = lib
        # Keep the encoded strings alive for the receiver's lifetime.
        self._name_b = source_name.encode("utf-8")
        self._recv_name_b = receiver_name.encode("utf-8")
        settings = NDIlib_recv_create_v3_t()
        settings.source_to_connect_to = NDIlib_source_t(self._name_b, None)
        settings.color_format = COLOR_FORMAT_BGRX_BGRA
        settings.bandwidth = BANDWIDTH_AUDIO_ONLY
        settings.allow_video_fields = True
        settings.p_ndi_recv_name = self._recv_name_b
        self._recv = lib.NDIlib_recv_create_v3(ctypes.byref(settings))
        if not self._recv:
            lib.NDIlib_destroy()
            raise RuntimeError(f"NDIlib_recv_create_v3 failed for source {source_name!r}")

    def capture(self, timeout_ms: int) -> AudioBlock | None:
        frame = NDIlib_audio_frame_v3_t()
        kind = self._lib.NDIlib_recv_capture_v3(self._recv, None, ctypes.byref(frame), None,
                                                int(timeout_ms))
        if kind == FRAME_TYPE_ERROR:
            raise ConnectionError(f"NDI receive error from {self.source_name!r} (connection lost)")
        if kind != FRAME_TYPE_AUDIO:
            return None
        try:
            return AudioBlock(int(frame.sample_rate), frame_to_array(frame), int(frame.timestamp))
        finally:
            self._lib.NDIlib_recv_free_audio_v3(self._recv, ctypes.byref(frame))

    def connections(self) -> int:
        return int(self._lib.NDIlib_recv_get_no_connections(self._recv))

    def close(self) -> None:
        if self._recv:
            self._lib.NDIlib_recv_destroy(self._recv)
            self._recv = None
            self._lib.NDIlib_destroy()
