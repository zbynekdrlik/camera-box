"""issue 1404 Task 2 -- the stream program-audio guard's classifier + sampler.

The guard protects the YouTube channel (owner amendment, issue 1404 comment 6016489928): only
measurement content (the QPSK marker + its room) may reach YouTube. A level bar cannot tell the
loud healthy marker from music, so the verdict is spectral: the share of energy outside the marker
band (200-800 Hz).

* the classifier on the REAL measurement clips (Task 1's fixtures cut from the session recordings
  and the YouTube VODs) -> MEASUREMENT in every 2 s window;
* on synthetic broadband (white / pink) and speech-shaped noise -> FOREIGN, also pink noise mixed
  at the measurement's own level under a real measurement window;
* on silence (digital zero, a -80 dBFS noise floor, the real VOD window before the stream began)
  -> SILENT;
* the constants are single-sourced in scripts/program_audio.py and pinned here;
* the sampler's window accumulator and run loop driven by a fake NDI receiver;
* the ctypes struct layout pinned to the vendored NDI SDK headers (x86_64).
"""
from __future__ import annotations

import ctypes
import json
import pathlib
import subprocess
import sys

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import program_audio as pa  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402

FIX = _ROOT / "tests" / "fixtures" / "youtube_leg_1404"
SR = 48000


def _load_flac(path: pathlib.Path):
    """Decode a FLAC fixture with ffmpeg (the CI python-tests job installs it). Returns
    (samples float64 (n,), sample_rate)."""
    probe = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "a:0", "-show_entries", "stream=sample_rate",
         "-of", "default=nw=1:nk=1", str(path)],
        capture_output=True, text=True, check=True,
    )
    sr = int(probe.stdout.strip())
    raw = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-ac", "1", "-"],
        capture_output=True, check=True,
    ).stdout
    return np.frombuffer(raw, dtype="<f4").astype(np.float64), sr


def _windows(x, sr, window_s=2.0):
    n = int(sr * window_s)
    return [x[i * n:(i + 1) * n] for i in range(len(x) // n)]


def _scale_to_dbfs(x, dbfs):
    ms = float(np.mean(np.asarray(x, dtype=np.float64) ** 2))
    return x * (10 ** (dbfs / 20.0) / np.sqrt(ms))


def _pink(n, rng):
    s = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1.0 / SR)
    s[1:] /= np.sqrt(f[1:])
    s[0] = 0
    return np.fft.irfft(s, n)


def _speech_shaped(n, rng):
    """Long-term-average-speech-spectrum-shaped noise: high-passed at 100 Hz, rising to 500 Hz,
    -6 dB/octave above, band-limited at 8 kHz, with a 4 Hz syllabic envelope."""
    s = np.fft.rfft(rng.standard_normal(n))
    f = np.fft.rfftfreq(n, 1.0 / SR)
    g = np.zeros_like(f)
    lo = (f >= 100) & (f < 500)
    g[lo] = np.sqrt(f[lo] / 500.0)
    hi = (f >= 500) & (f <= 8000)
    g[hi] = 500.0 / f[hi]
    x = np.fft.irfft(s * g, n)
    t = np.arange(n) / SR
    return x * (0.5 * (1 + np.sin(2 * np.pi * 4 * t))) ** 2


# ---------------------------------------------------------------------------------------------
# constants -- single-sourced, pinned
# ---------------------------------------------------------------------------------------------


def test_the_calibrated_constants_are_pinned():
    assert pa.BAND_LO_HZ == 200.0
    assert pa.BAND_HI_HZ == 800.0
    assert pa.SPECTRUM_FLOOR_HZ == 20.0
    assert pa.WINDOW_S == 2.0
    assert pa.SILENT_RMS_DBFS == -60.0
    assert pa.FOREIGN_OUTSIDE_BAND_PCT == 30.0
    assert pa.VERDICTS == ("MEASUREMENT", "FOREIGN", "SILENT", "UNKNOWN")


# ---------------------------------------------------------------------------------------------
# the classifier on real measurement audio
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("clip", ["s2-R-rec", "base-R-rec", "s3-A-rec", "s2-R-vod", "base-R-vod"])
def test_real_measurement_audio_is_measurement_in_every_window(clip):
    x, sr = _load_flac(FIX / f"{clip}.flac")
    wins = _windows(x, sr)
    assert len(wins) == 10
    for w in wins:
        rms, outside = pa.analyse(w, sr)
        assert -40.0 < rms < -30.0
        assert outside < pa.FOREIGN_OUTSIDE_BAND_PCT
        assert pa.classify(rms, outside) == "MEASUREMENT"


def test_the_real_vod_window_before_the_stream_began_is_silent():
    """s3-A-vod starts before YouTube had content (the clamped window of session 3): its first
    2 s are digital silence."""
    x, sr = _load_flac(FIX / "s3-A-vod.flac")
    rms, outside = pa.analyse(_windows(x, sr)[0], sr)
    assert pa.classify(rms, outside) == "SILENT"


# ---------------------------------------------------------------------------------------------
# synthetic foreign content
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("kind", ["white", "pink", "speech"])
def test_broadband_and_speech_like_audio_is_foreign(kind):
    rng = np.random.default_rng(1404)
    n = 2 * SR
    gen = {"white": lambda: rng.standard_normal(n), "pink": lambda: _pink(n, rng),
           "speech": lambda: _speech_shaped(n, rng)}[kind]
    x = _scale_to_dbfs(gen(), -20.0)
    rms, outside = pa.analyse(np.stack([x, x], axis=1), SR)
    assert abs(rms - (-20.0)) < 0.5
    assert outside > pa.FOREIGN_OUTSIDE_BAND_PCT
    assert pa.classify(rms, outside) == "FOREIGN"


def test_pink_noise_at_the_measurement_level_mixed_under_a_real_window_is_foreign():
    x, sr = _load_flac(FIX / "s2-R-rec.flac")
    meas = _windows(x, sr)[3]
    rng = np.random.default_rng(7)
    mrms = 10 * np.log10(np.mean(meas ** 2))
    noise = _scale_to_dbfs(_pink(len(meas), rng), mrms)
    rms, outside = pa.analyse(meas + noise, sr)
    assert pa.classify(rms, outside) == "FOREIGN"


# ---------------------------------------------------------------------------------------------
# silence + edge cases
# ---------------------------------------------------------------------------------------------


def test_digital_silence_is_silent_with_no_spectral_share():
    rms, outside = pa.analyse(np.zeros((2 * SR, 2), dtype=np.float32), SR)
    assert rms == pa.DIGITAL_SILENCE_DBFS
    assert outside is None
    assert pa.classify(rms, outside) == "SILENT"


def test_a_minus_80_dbfs_noise_floor_is_silent_not_foreign():
    rng = np.random.default_rng(3)
    x = _scale_to_dbfs(rng.standard_normal(2 * SR), -80.0)
    rms, outside = pa.analyse(x, SR)
    assert outside is not None and outside > 90.0  # white noise: broadband
    assert pa.classify(rms, outside) == "SILENT"


def test_classify_rejects_a_missing_measurement_as_unknown():
    assert pa.classify(None, None) == "UNKNOWN"
    assert pa.classify(float("nan"), 10.0) == "UNKNOWN"
    assert pa.classify(-30.0, None) == "UNKNOWN"
    assert pa.classify(-30.0, float("nan")) == "UNKNOWN"


def test_the_classifier_boundaries():
    assert pa.classify(pa.SILENT_RMS_DBFS - 0.1, 99.0) == "SILENT"
    assert pa.classify(pa.SILENT_RMS_DBFS, 10.0) == "MEASUREMENT"
    assert pa.classify(-35.0, pa.FOREIGN_OUTSIDE_BAND_PCT) == "FOREIGN"
    assert pa.classify(-35.0, pa.FOREIGN_OUTSIDE_BAND_PCT - 0.1) == "MEASUREMENT"


def test_stereo_channels_are_summed_in_power_so_an_inter_channel_delay_cannot_cancel():
    """The measurement track carries the marker on L and R about 10 ms apart: a mono downmix
    comb-filters it. Per-channel power summed: delaying one channel leaves the share unchanged."""
    x, sr = _load_flac(FIX / "s2-R-rec.flac")
    w = _windows(x, sr)[2]
    d = int(0.01017 * sr)
    delayed = np.concatenate([np.zeros(d), w[:-d]])
    _r1, same = pa.analyse(np.stack([w, w], axis=1), sr)
    _r2, shifted = pa.analyse(np.stack([w, delayed], axis=1), sr)
    assert abs(same - shifted) < 1.0


def test_analyse_accepts_mono_and_float32():
    x = _scale_to_dbfs(np.sin(2 * np.pi * 442 * np.arange(2 * SR) / SR), -30.0).astype(np.float32)
    rms, outside = pa.analyse(x, SR)
    assert abs(rms + 30.0) < 0.2
    assert outside < 1.0  # a 442 Hz carrier is all in band
    assert pa.classify(rms, outside) == "MEASUREMENT"


# ---------------------------------------------------------------------------------------------
# payload + atomic write
# ---------------------------------------------------------------------------------------------


def test_build_payload_has_the_contract_fields_rounded():
    from datetime import datetime, timezone

    now = datetime(2026, 10, 6, 19, 30, 2, 123456, tzinfo=timezone.utc)
    p = pa.build_payload("MEASUREMENT", -35.6449, 16.8449, now=now, window_s=2.0,
                         source="STREAM-SNV (stream)")
    assert p == {
        "schema": 1, "ts_utc": "2026-10-06T19:30:02.123Z", "age_s": 0.0, "verdict": "MEASUREMENT",
        "rms_dbfs": -35.6, "outside_band_pct": 16.8, "window_s": 2.0, "source": "STREAM-SNV (stream)",
    }
    u = pa.build_payload("UNKNOWN", None, None, now=now, window_s=2.0, source="X", reason="no audio")
    assert u["rms_dbfs"] is None and u["outside_band_pct"] is None and u["reason"] == "no audio"


def test_write_payload_replaces_the_file_atomically(tmp_path):
    from datetime import datetime, timezone

    p = pa.build_payload("SILENT", -90.0, None, now=datetime.now(timezone.utc), window_s=2.0, source="X")
    target = tmp_path / rsf.PROGRAM_AUDIO_NAME
    target.write_text("{}", encoding="utf-8")
    with open(target, "rb") as reader:
        pa.write_payload(str(tmp_path), p)
        assert reader.read() == b"{}"
    assert json.loads(target.read_text(encoding="utf-8")) == p
    assert sorted(x.name for x in tmp_path.iterdir()) == [rsf.PROGRAM_AUDIO_NAME]


# ---------------------------------------------------------------------------------------------
# the sampler: window accumulator + run loop with a fake receiver
# ---------------------------------------------------------------------------------------------


def test_window_accumulator_emits_exact_windows_from_ndi_sized_blocks():
    acc = pas.WindowAccumulator(2.0)
    out = []
    block = np.ones((1600, 2), dtype=np.float32)
    for _ in range(130):  # 130 * 1600 = 208000 samples = 2 windows + 16000
        out.extend(acc.push(block, 48000))
    assert len(out) == 2
    for w, sr in out:
        assert sr == 48000 and w.shape == (96000, 2)


def test_window_accumulator_restarts_on_a_format_change():
    acc = pas.WindowAccumulator(2.0)
    assert acc.push(np.ones((90000, 2), dtype=np.float32), 48000) == []
    # a sample-rate change drops the 90000-sample partial (90000 + 80000 would already be a full
    # 48 kHz window): a window never mixes two formats
    assert acc.push(np.ones((80000, 2), dtype=np.float32), 44100) == []
    out = acc.push(np.ones((10000, 2), dtype=np.float32), 44100)
    assert len(out) == 1 and out[0][1] == 44100 and out[0][0].shape == (88200, 2)
    acc2 = pas.WindowAccumulator(2.0)
    acc2.push(np.ones((90000, 2), dtype=np.float32), 48000)
    assert acc2.push(np.ones((10000, 1), dtype=np.float32), 48000) == []


class _FakeReceiver:
    def __init__(self, blocks, connections=1):
        self._blocks = list(blocks)
        self.closed = False
        self._connections = connections

    def capture(self, timeout_ms):
        if self._blocks:
            return self._blocks.pop(0)
        return None

    def connections(self):
        return self._connections

    def close(self):
        self.closed = True


class _Clock:
    def __init__(self):
        self.t = 1_000.0

    def mono(self):
        return self.t

    def advance(self, s):
        self.t += s


def _read(serve):
    return json.loads((serve / rsf.PROGRAM_AUDIO_NAME).read_text(encoding="utf-8"))


def test_run_writes_unknown_at_start_then_a_measurement_verdict(tmp_path):
    x, sr = _load_flac(FIX / "s2-R-rec.flac")
    stereo = np.stack([x, x], axis=1).astype(np.float32)
    blocks = [pan.AudioBlock(sr, stereo[i:i + 1600]) for i in range(0, 2 * sr * 2, 1600)]
    clock = _Clock()
    seen = []

    def on_write(payload):
        seen.append(payload["verdict"])

    rx = _FakeReceiver(blocks)
    pas.run(rx, str(tmp_path), source="STREAM-SNV (stream)", mono=clock.mono, max_loops=len(blocks),
            on_write=on_write, log=lambda m: None)
    assert seen[0] == "UNKNOWN"
    assert seen[1:] == ["MEASUREMENT", "MEASUREMENT"]
    final = _read(tmp_path)
    assert final["verdict"] == "MEASUREMENT"
    assert final["source"] == "STREAM-SNV (stream)"
    assert final["window_s"] == 2.0


def test_run_reports_unknown_when_no_audio_arrives(tmp_path):
    clock = _Clock()

    class _Ticking(_FakeReceiver):
        def capture(self, timeout_ms):
            clock.advance(timeout_ms / 1000.0)
            return None

    rx = _Ticking([], connections=0)
    pas.run(rx, str(tmp_path), source="STREAM-SNV (stream)", mono=clock.mono, max_loops=40,
            log=lambda m: None)
    final = _read(tmp_path)
    assert final["verdict"] == "UNKNOWN"
    assert "no audio" in final["reason"]
    assert "connections=0" in final["reason"]
    assert final["rms_dbfs"] is None


def test_run_never_writes_a_stale_verdict_as_fresh_after_audio_stops(tmp_path):
    x, sr = _load_flac(FIX / "s2-R-rec.flac")
    stereo = np.stack([x, x], axis=1).astype(np.float32)
    blocks = [pan.AudioBlock(sr, stereo[i:i + 1600]) for i in range(0, 2 * sr, 1600)]
    clock = _Clock()

    class _ThenSilence(_FakeReceiver):
        def capture(self, timeout_ms):
            if self._blocks:
                return self._blocks.pop(0)
            clock.advance(timeout_ms / 1000.0)
            return None

    rx = _ThenSilence(blocks)
    pas.run(rx, str(tmp_path), source="S", mono=clock.mono, max_loops=len(blocks) + 30,
            log=lambda m: None)
    assert _read(tmp_path)["verdict"] == "UNKNOWN"


# ---------------------------------------------------------------------------------------------
# the ctypes binding: layout pinned to the vendored SDK headers
# ---------------------------------------------------------------------------------------------


def test_ndi_struct_layout_matches_the_sdk_headers():
    assert ctypes.sizeof(ctypes.c_void_p) == 8
    assert ctypes.sizeof(pan.NDIlib_source_t) == 16
    assert ctypes.sizeof(pan.NDIlib_recv_create_v3_t) == 40
    assert pan.NDIlib_recv_create_v3_t.bandwidth.offset == 20
    assert pan.NDIlib_recv_create_v3_t.p_ndi_recv_name.offset == 32
    f = pan.NDIlib_audio_frame_v3_t
    assert ctypes.sizeof(f) == 64
    assert (f.sample_rate.offset, f.no_channels.offset, f.no_samples.offset) == (0, 4, 8)
    assert f.timecode.offset == 16
    assert f.FourCC.offset == 24
    assert f.p_data.offset == 32
    assert f.channel_stride_in_bytes.offset == 40
    assert f.p_metadata.offset == 48
    assert f.timestamp.offset == 56


def test_ndi_enum_values_match_the_sdk_headers():
    assert pan.BANDWIDTH_AUDIO_ONLY == 10
    assert pan.FRAME_TYPE_AUDIO == 2
    assert pan.FRAME_TYPE_ERROR == 4
    assert pan.FOURCC_FLTP == (ord("F") | ord("L") << 8 | ord("T") << 16 | ord("p") << 24)


def test_frame_to_array_deinterleaves_planar_float_with_a_stride():
    n, ch, stride_samples = 5, 2, 8  # padded planes: stride > no_samples
    planes = np.zeros((ch, stride_samples), dtype=np.float32)
    planes[0, :n] = [1, 2, 3, 4, 5]
    planes[1, :n] = [-1, -2, -3, -4, -5]
    buf = planes.tobytes()
    cbuf = ctypes.create_string_buffer(buf, len(buf))
    frame = pan.NDIlib_audio_frame_v3_t()
    frame.sample_rate = 48000
    frame.no_channels = ch
    frame.no_samples = n
    frame.FourCC = pan.FOURCC_FLTP
    frame.p_data = ctypes.cast(cbuf, ctypes.c_void_p)
    frame.channel_stride_in_bytes = stride_samples * 4
    out = pan.frame_to_array(frame)
    assert out.shape == (n, ch) and out.dtype == np.float32
    assert out[:, 0].tolist() == [1, 2, 3, 4, 5]
    assert out[:, 1].tolist() == [-1, -2, -3, -4, -5]
    # a copy: freeing the SDK frame later must not change it
    cbuf[0:4] = b"\x00\x00\x00\x00"
    assert out[0, 0] == 1


def test_frame_to_array_refuses_a_non_float_format():
    frame = pan.NDIlib_audio_frame_v3_t()
    frame.no_channels = 2
    frame.no_samples = 4
    frame.FourCC = 0
    with pytest.raises(ValueError):
        pan.frame_to_array(frame)


def test_the_sampler_defaults_name_the_live_stream_program_sender():
    assert pas.DEFAULT_SOURCE == "STREAM-SNV (stream)"
    assert pas.NO_AUDIO_TIMEOUT_S == 5.0


# ---------------------------------------------------------------------------------------------
# the sampler process: a receiver it cannot create leaves UNKNOWN behind, never a stale verdict
# ---------------------------------------------------------------------------------------------


def test_main_with_an_unloadable_libndi_writes_unknown_and_fails(tmp_path):
    serve = tmp_path / "serve"
    from datetime import datetime, timezone

    old = pa.build_payload("MEASUREMENT", -35.0, 15.0, now=datetime.now(timezone.utc), window_s=2.0,
                           source="X")
    serve.mkdir()
    pa.write_payload(str(serve), old)
    rc = pas.main(["--serve-dir", str(serve), "--lib", str(tmp_path / "no-libndi.so"),
                   "--source", "STREAM-SNV (stream)"])
    assert rc == 1
    got = _read(serve)
    assert got["verdict"] == "UNKNOWN"
    assert "cannot receive" in got["reason"]
    assert "no-libndi.so" in got["reason"]


def test_an_explicit_lib_path_is_the_only_candidate(tmp_path):
    with pytest.raises(OSError) as exc:
        pan._load_library(str(tmp_path / "no-libndi.so"))
    assert "/usr/lib/ndi" not in str(exc.value)


# ---------------------------------------------------------------------------------------------
# the sampler unit -- long-running, restarts on failure, shipped disabled
# ---------------------------------------------------------------------------------------------


def test_the_sampler_unit_runs_the_sampler_and_restarts_on_failure():
    import re

    s = (_ROOT / "systemd" / "program-audio-sampler.service").read_text(encoding="utf-8")
    assert re.search(r"^Type=simple$", s, re.M)
    assert re.search(
        r"^ExecStart=/usr/bin/python3 %h/devel/camera-box/scripts/program_audio_sampler.py$", s, re.M)
    assert re.search(r"^Restart=on-failure$", s, re.M)
    assert re.search(r"^RestartSec=\d+$", s, re.M)
    assert re.search(r"^WantedBy=default.target$", s, re.M)


def test_the_sampler_refuses_a_serve_dir_inside_the_lease_dir(tmp_path, monkeypatch):
    lease = tmp_path / "rig-lease"
    monkeypatch.setenv("RIG_LEASE_DIR", str(lease))
    rc = pas.main(["--serve-dir", str(lease / "serve"), "--lib", str(tmp_path / "no-libndi.so")])
    assert rc == 2
    assert not lease.exists()
