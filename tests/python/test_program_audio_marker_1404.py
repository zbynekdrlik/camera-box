"""issue 1404 -- the program-audio guard requires the QPSK marker itself before it says MEASUREMENT.

The spectral share alone passed tonal content inside 200-800 Hz as MEASUREMENT (Design-question
6026559236), and a raw count of CRC-valid words passed held chords and band-limited noise. The rule
(ROZHODNUTÉ 6026577906 + 6026826572), per channel over the trailing 4 s span:
  1. same-index words less than 0.25 s apart are one marker;
  2. an index that appears again 0.25 s or more away is dropped (a real index wraps every 4.27 s);
  3. the longest chain with idx_j - idx_i == round(60 * dt) (mod 256, +-2);
  4. MEASUREMENT = chain >= 4 AND the spectral condition on the current 2 s window.

Covered here:
  * the constants, and the span strictly shorter than one index wrap;
  * each step of the rule on hand-built words (merge, drop, a real index after the wrap, the +-2
    tolerance, both channels);
  * the shim (built with the real build script and g++) and its ctypes loader, its parameters
    against the Rust sources and the painter's `# qpsk-params` line;
  * bar (a) on real measurement audio: the Task 1 fixtures and the hardest recorded span;
  * bar (b) on synthetic in-band content (chords, tremolo chords, band-limited noise, melody);
  * the sampler: warm-up and receive gaps = UNKNOWN, a silent window restarts the span, a missing
    shim = UNKNOWN + exit 1.
"""
from __future__ import annotations

import ctypes
import ctypes.util
import json
import pathlib
import re
import shutil
import subprocess
import sys

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for _p in (str(_SCRIPTS), str(_HERE)):
    if _p not in sys.path:
        sys.path.insert(0, _p)

import program_audio as pa  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402
from qpsk_guard_shim_1404 import FixedChain, NoMarkers, build_shim, real_marker_words  # noqa: E402

FIX = _ROOT / "tests" / "fixtures" / "youtube_leg_1404"
REC_CLIP = _ROOT / "tests" / "fixtures" / "program_audio_1404" / "rec3b-290s-stereo-48k.flac"
TASK1_CLIPS = ("s2-R-rec", "base-R-rec", "s3-A-rec", "s2-R-vod", "base-R-vod", "s3-A-vod")


@pytest.fixture(scope="session")
def shim_path(tmp_path_factory):
    return build_shim(tmp_path_factory.mktemp("qpsk-guard-shim"))


@pytest.fixture(scope="session")
def decoder(shim_path):
    return pam.MarkerDecoder(shim_path)


# ---------------------------------------------------------------------------------------------
# constants
# ---------------------------------------------------------------------------------------------


def test_the_marker_constants_are_pinned():
    assert pa.MARKER_CHAIN_MIN == 4
    assert pa.MARKER_INDEX_TOL == 2
    assert pa.MARKER_MIN_SEP_S == 0.25
    assert pa.MARKER_INDEX_RATE_HZ == 60.0
    assert pa.MARKER_INDEX_MODULUS == 256
    assert pa.MARKER_SPAN_S == 4.0


def test_the_span_is_strictly_shorter_than_one_index_wrap():
    """Rule 2 drops a repeating index. That is only safe while a REAL index cannot repeat inside
    the span: the index wraps every 256 / 60 = 4.27 s, so the span must stay below that."""
    assert pa.MARKER_SPAN_S < pa.MARKER_INDEX_MODULUS / pa.MARKER_INDEX_RATE_HZ


def test_the_span_is_a_whole_number_of_windows():
    k = pa.MARKER_SPAN_S / pa.WINDOW_S
    assert k == int(k) and int(k) == 2
    with pytest.raises(ValueError):
        pas.MarkerSpan(window_s=2.0, span_s=3.0)


def test_the_index_rate_is_the_emitters_frame_rate():
    """The painter's marker log header (written by src/qpsk_marker.rs) declares vr=60/1: the index
    is frame_id mod 256 at 60 fps."""
    rs = (_ROOT / "src" / "qpsk_marker.rs").read_text(encoding="utf-8")
    m = re.search(r"# qpsk-params sr=(\d+) carrier=(\d+) c=(\d+) q=(\d+) vr=(\d+)/(\d+)", rs)
    assert m, "the pinned `# qpsk-params` line is gone from src/qpsk_marker.rs"
    vr_num, vr_den = int(m.group(5)), int(m.group(6))
    assert pa.MARKER_INDEX_RATE_HZ == vr_num / vr_den


# ---------------------------------------------------------------------------------------------
# the rule, step by step (pure)
# ---------------------------------------------------------------------------------------------


def test_rule1_same_index_words_under_the_separation_are_one_marker():
    assert pa.marker_candidates([(1.0, 50), (1.1, 50), (1.24, 50)]) == [(1.0, 50)]
    # a re-hit of every marker of a real chain leaves the chain as it was
    real = real_marker_words(7)
    rehit = real + [(t + 0.03, i) for t, i in real]
    assert pa.marker_chain(rehit) == pa.marker_chain(real) == 7


def test_rule2_an_index_that_repeats_inside_the_span_is_dropped():
    assert pa.marker_candidates([(0.5, 99), (2.5, 99)]) == []
    assert pa.marker_candidates([(0.5, 99), (0.75, 99)]) == []  # exactly the separation = a repeat


def test_rule2_never_chains_re_hits_so_a_dense_run_is_dropped_not_merged():
    """A steady tone decodes one index every ~25-70 ms. Each step is under 0.25 s, but the run
    spans more, so the index repeats and is dropped -- never collapsed into one marker."""
    run = [(0.1 + k * 0.07, 233) for k in range(40)]
    assert pa.marker_candidates(run) == []
    assert pa.marker_chain(run) == 0


def test_rule2_rejects_the_tremolo_chord_false_chain():
    """The measured failure of the plain chain (issue 1404, 6026817074): a held tremolo chord
    decodes a few indices in dense runs; each run crosses any 60/s line once, so the chain read
    one marker per index, four windows in a row. After the drop nothing is left."""
    words = sorted((0.05 + k * 0.071 + j * 0.017, idx)
                   for j, idx in enumerate((233, 109, 60, 185)) for k in range(55))
    assert pa.marker_chain(words) == 0


def test_rule2_keeps_a_real_chain_next_to_repeating_false_indices():
    real = real_marker_words(8, start_index=3)
    false = [(0.2 + k * 0.4, 200) for k in range(9)]  # index 200 is never on the real line here
    assert pa.marker_chain(real + false) == 8


def test_a_real_index_returns_only_after_the_wrap_outside_the_span():
    """A cadence of 32 frames brings index 17 back after 256 frames = 4.27 s. Inside a 4 s span
    the real chain is whole; a span longer than one wrap would hold both and drop them."""
    words = real_marker_words(9, start_s=0.1, start_index=17, gap_s=32 / 60, frames_per_marker=32)
    assert words[0][1] == words[8][1] == 17
    assert words[8][0] - words[0][0] > pa.MARKER_SPAN_S
    in_span = [w for w in words if w[0] - words[0][0] < pa.MARKER_SPAN_S]
    assert len(in_span) == 8
    assert pa.marker_chain(in_span) == 8
    assert pa.marker_chain(words) == 7  # index 17 twice = dropped: why the span must stay < 4.27 s


@pytest.mark.parametrize("shift,on_chain", [(-3, False), (-2, True), (2, True), (3, False)])
def test_rule3_tolerance_is_plus_minus_two_indices(shift, on_chain):
    words = real_marker_words(6, start_index=40)
    t, i = words[3]
    words[3] = (t, (i + shift) % 256)
    assert pa.marker_chain(words) == (6 if on_chain else 5)


def test_rule3_counts_distinct_markers_only():
    """A second word on the line within 0.25 s of a marker is not a second marker."""
    words = real_marker_words(5, start_index=10)
    t, i = words[2]
    words.append((t + 0.05, (i + 3) % 256))  # round(60 * 0.05) = 3: exactly on the line
    assert pa.marker_chain(words) == 5


def test_rule3_wraps_the_index_modulo_256():
    words = real_marker_words(8, start_index=200)  # 200, 230, 4, 34 ...
    assert any(i < 200 for _t, i in words)
    assert pa.marker_chain(words) == 8


def test_rule3_empty_and_single():
    assert pa.marker_chain([]) == 0
    assert pa.marker_chain([(1.0, 7)]) == 1


def test_a_non_finite_word_time_is_refused():
    with pytest.raises(ValueError):
        pa.marker_chain([(float("nan"), 3)])


def test_both_channels_are_read_on_their_own_and_the_best_counts():
    real = real_marker_words(6)
    assert pa.span_markers([[], real]) == (6, 6)
    assert pa.span_markers([real, []]) == (6, 6)
    noise = [(0.03 * k, (17 * k + 5) % 256) for k in range(10)]
    assert pa.span_markers([noise, real[:5]]) == (10, 5)
    # never merged across channels: half the markers on each channel is a chain of 3 each
    assert pa.span_markers([real[:3], real[3:]]) == (3, 3)
    with pytest.raises(ValueError):
        pa.span_markers([])


# ---------------------------------------------------------------------------------------------
# rule 4: the verdict
# ---------------------------------------------------------------------------------------------


def test_measurement_needs_the_chain_and_the_spectral_condition():
    assert pa.classify(-35.0, 15.0, pa.MARKER_CHAIN_MIN) == "MEASUREMENT"
    assert pa.classify(-35.0, 15.0, pa.MARKER_CHAIN_MIN - 1) == "FOREIGN"
    assert pa.classify(-35.0, 15.0, 0) == "FOREIGN"
    assert pa.classify(-35.0, 15.0, None) == "UNKNOWN"
    assert pa.classify(-35.0, 15.0, True) == "UNKNOWN"  # a bool is not a chain


def test_marker_plus_loud_broadband_is_foreign_whatever_the_chain():
    assert pa.classify(-15.0, 90.0, 8) == "FOREIGN"
    assert pa.classify(-15.0, 90.0, None) == "FOREIGN"


def test_silence_stays_silent_without_a_chain():
    assert pa.classify(-80.0, None, None) == "SILENT"
    assert pa.classify(pa.DIGITAL_SILENCE_DBFS, None, None) == "SILENT"


def test_the_payload_carries_the_marker_fields():
    from datetime import datetime, timezone

    now = datetime(2026, 10, 7, 1, 0, 0, tzinfo=timezone.utc)
    p = pa.build_payload("MEASUREMENT", -35.6, 16.8, now=now, window_s=2.0, source="S",
                         markers_decoded=9, marker_chain=7)
    assert p["markers_decoded"] == 9 and p["marker_chain"] == 7
    u = pa.build_payload("UNKNOWN", None, None, now=now, window_s=2.0, source="S", reason="r")
    assert "markers_decoded" in u and u["markers_decoded"] is None
    assert "marker_chain" in u and u["marker_chain"] is None
    with pytest.raises(ValueError):
        pa.build_payload("FOREIGN", -30.0, 10.0, now=now, window_s=2.0, source="S", marker_chain=-1)


# ---------------------------------------------------------------------------------------------
# the shim + its loader
# ---------------------------------------------------------------------------------------------


def test_the_build_script_and_the_loader_agree_on_the_default_path():
    r = subprocess.run(["bash", str(_SCRIPTS / "build-qpsk-guard-shim.sh"), "--print-default"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip() == pam.DEFAULT_SHIM_PATH


def test_the_shim_runs_the_dock_decoder_with_the_rig_parameters(decoder):
    assert decoder.params == {"sample_rate": 48000, "carrier_hz": 442, "c": 1, "threshold": 0.35}
    rs = (_ROOT / "src" / "qpsk_marker.rs").read_text(encoding="utf-8")
    assert re.search(r"pub const AUDIO_SAMPLE_RATE_HZ: u32 = 48_000;", rs)
    assert re.search(r"pub const CARRIER_HZ_DEFAULT: u32 = 442;", rs)
    m = re.search(r"# qpsk-params sr=(\d+) carrier=(\d+) c=(\d+)", rs)
    assert (int(m.group(1)), int(m.group(2)), int(m.group(3))) == (48000, 442, 1)
    dock = (_ROOT / "src" / "av_sync_dock.rs").read_text(encoding="utf-8")
    assert re.search(r"pub const DOCK_QPSK_THRESHOLD: f64 = 0\.35;", dock)


def test_the_shim_records_the_sources_it_was_built_from(decoder, tmp_path):
    assert decoder.built_sha256 == pam.sources_sha256()
    assert not decoder.sources_stale()
    for rel in pam.SOURCES:
        (tmp_path / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy(_ROOT / rel, tmp_path / rel)
    (tmp_path / pam.SOURCES[-1]).write_bytes((_ROOT / pam.SOURCES[-1]).read_bytes() + b"\n// newer\n")
    assert decoder.sources_stale(root=tmp_path)


def test_a_missing_shim_is_unavailable_and_names_the_build_script(tmp_path):
    with pytest.raises(pam.DecoderUnavailable) as exc:
        pam.MarkerDecoder(str(tmp_path / "nope.so"))
    assert "build-qpsk-guard-shim.sh" in str(exc.value)


def test_an_unloadable_shim_is_unavailable(tmp_path):
    bad = tmp_path / "libqpsk-guard-shim.so"
    bad.write_text("not a shared object\n", encoding="utf-8")
    with pytest.raises(pam.DecoderUnavailable):
        pam.MarkerDecoder(str(bad))


def test_a_library_without_the_shim_abi_is_unavailable():
    libm = ctypes.util.find_library("m")
    assert libm, "libm not found"
    with pytest.raises(pam.DecoderUnavailable):
        pam.MarkerDecoder(libm if libm.startswith("/") else _resolve_lib(libm))


def test_a_bare_shim_name_loads_the_file_that_was_checked(shim_path, tmp_path, monkeypatch):
    """dlopen searches the system library paths for a name without a slash, so a bare override
    (QPSK_GUARD_SHIM=libm.so.6) must load the file the existence check saw, not the system libm."""
    shutil.copy(shim_path, tmp_path / "libm.so.6")
    monkeypatch.chdir(tmp_path)
    d = pam.MarkerDecoder("libm.so.6")
    assert d.path == str(tmp_path / "libm.so.6")
    assert d.params["carrier_hz"] == 442


def _resolve_lib(name: str) -> str:
    for d in ("/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu", "/lib64", "/usr/lib64", "/lib", "/usr/lib"):
        p = pathlib.Path(d) / name
        if p.exists():
            return str(p)
    raise AssertionError(f"cannot resolve {name}")


def _fixture(name):
    return cal.load_audio(str(FIX / f"{name}.flac"))


def test_decode_returns_each_channels_words_in_time_order(decoder):
    x, sr = _fixture("s2-R-rec")
    span = x[: int(pa.MARKER_SPAN_S * sr)]
    words = decoder.decode(span, sr)
    assert len(words) == 1 and 6 <= len(words[0]) <= 10
    times = [t for t, _i in words[0]]
    assert times == sorted(times)
    assert all(0 <= i <= 255 for _t, i in words[0])
    stereo = np.concatenate([span, np.zeros_like(span)], axis=1)
    left, right = decoder.decode(stereo, sr)
    assert left == words[0] and right == []


def test_the_real_span_reads_a_full_chain(decoder):
    x, sr = _fixture("base-R-rec")
    markers, chain = pa.span_markers(decoder.decode(x[: int(pa.MARKER_SPAN_S * sr)], sr))
    assert chain >= pa.MARKER_CHAIN_MIN + 2
    assert markers >= chain


def test_decode_refuses_a_buffer_that_is_not_samples_by_channels(decoder):
    with pytest.raises(pam.DecodeError):
        decoder.decode(np.zeros((10, 2, 2), dtype=np.float32), 48000)


def test_the_c_abi_refuses_bad_arguments_and_reports_more_words_than_room(shim_path):
    lib = ctypes.CDLL(shim_path)
    f = lib.qpsk_guard_decode_channel
    f.restype = ctypes.c_int
    f.argtypes = [ctypes.POINTER(ctypes.c_float), ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                  ctypes.POINTER(ctypes.c_int64), ctypes.POINTER(ctypes.c_uint8), ctypes.c_int]
    x, sr = _fixture("s2-R-rec")
    buf = np.ascontiguousarray(x[: 4 * sr], dtype=np.float32)
    ptr = buf.ctypes.data_as(ctypes.POINTER(ctypes.c_float))
    assert f(ptr, buf.shape[0], 1, 1, sr, None, None, 0) < 0  # channel out of range
    assert f(ptr, buf.shape[0], 1, 0, 0, None, None, 0) < 0  # no sample rate
    assert f(None, buf.shape[0], 1, 0, sr, None, None, 0) < 0
    n = f(ptr, buf.shape[0], 1, 0, sr, None, None, 0)  # no room: still counts
    assert n >= 6


def test_a_non_finite_sample_never_crashes_the_decode(decoder):
    x, sr = _fixture("s2-R-rec")
    span = x[: 4 * sr].copy()
    span[1000] = np.nan
    span[2000] = np.inf
    words = decoder.decode(span, sr)
    assert len(words) == 1


# ---------------------------------------------------------------------------------------------
# bar (a): real measurement audio -- no FOREIGN, chain >= MARKER_CHAIN_MIN + 2 in every window
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("clip", TASK1_CLIPS)
def test_bar_a_on_every_task1_fixture(decoder, clip):
    x, sr = _fixture(clip)
    ok, detail = cal.bar_real(cal.evaluate(x, sr, decoder))
    assert ok, detail
    assert detail["min_chain"] >= pa.MARKER_CHAIN_MIN + 2
    assert detail["judged"] >= 6


def test_bar_a_on_the_hardest_recorded_span(decoder):
    """The one 4 s span of the 1851 recorded session windows whose chain is lowest (6, rec3b at
    292 s), cut as 10 s of the real 48 kHz stereo track."""
    x, sr = cal.load_audio(str(REC_CLIP))
    assert sr == 48000 and x.shape[1] == 2
    payloads = cal.evaluate(x, sr, decoder)
    ok, detail = cal.bar_real(payloads)
    assert ok, detail
    assert detail["min_chain"] == pa.MARKER_CHAIN_MIN + 2
    assert [p["verdict"] for p in payloads] == ["UNKNOWN"] + ["MEASUREMENT"] * 4


# ---------------------------------------------------------------------------------------------
# bar (b): synthetic in-band content -- a FOREIGN in every 3 consecutive windows, every trial
# ---------------------------------------------------------------------------------------------


@pytest.mark.parametrize("level", cal.SYNTH_LEVELS_DBFS)
@pytest.mark.parametrize("kind", cal.SYNTH_CLASSES)
def test_bar_b_on_synthetic_in_band_content(decoder, kind, level):
    trials = cal.synthetic_trials(kind, level, trials=3, windows=7, decoder=decoder)
    for payloads in trials:
        for p in cal.judged(payloads):
            # in band: the spectral share alone would have said MEASUREMENT
            assert p["outside_band_pct"] < pa.FOREIGN_OUTSIDE_BAND_PCT, p
    ok, detail = cal.bar_synthetic(trials)
    assert ok, detail
    assert detail["worst_run_chain"] < pa.MARKER_CHAIN_MIN


def test_the_calibrate_cli_exits_nonzero_when_a_bar_fails(shim_path, tmp_path):
    """Pure noise is not measurement audio: bar (a) must fail on it, and the CLI says so."""
    rng = np.random.default_rng(3)
    noise = (0.05 * rng.standard_normal((6 * 16000, 1))).astype("<f4")
    raw = tmp_path / "noise.f32"
    raw.write_bytes(noise.tobytes())
    flac = tmp_path / "noise.flac"
    subprocess.run(["ffmpeg", "-v", "error", "-f", "f32le", "-ar", "16000", "-ac", "1", "-i", str(raw),
                    str(flac)], check=True, timeout=60)
    r = subprocess.run([sys.executable, str(_SCRIPTS / "program_audio_marker_calibrate.py"),
                        "--shim", shim_path, "--real", str(flac), "--json"],
                       capture_output=True, text=True, timeout=120)
    assert r.returncode == 1, r.stdout + r.stderr
    rec = json.loads(r.stdout.strip().splitlines()[-1])
    assert rec["bar"] == "a" and rec["ok"] is False


# ---------------------------------------------------------------------------------------------
# the sampler
# ---------------------------------------------------------------------------------------------


def test_marker_span_warm_up_silence_and_format_change():
    span = pas.MarkerSpan()
    w = np.ones((10, 2), dtype=np.float32)
    assert span.push(w, 48000, silent=False) is None and not span.warm
    full = span.push(w, 48000, silent=False)
    assert span.warm and full is not None and full.shape == (20, 2)
    assert span.push(w, 48000, silent=True) is None and span.non_silent_s == 0.0 and span.warm
    assert span.push(w, 48000, silent=False) is None  # one window after a silence
    assert span.push(w, 48000, silent=False) is not None
    assert span.push(np.ones((10, 1), dtype=np.float32), 48000, silent=False) is None  # format change
    assert not span.warm
    span.reset()
    assert span.audio_s == 0.0 and not span.warm


class _Receiver:
    def __init__(self, blocks):
        self._blocks = list(blocks)

    def capture(self, _timeout_ms):
        return self._blocks.pop(0) if self._blocks else None

    def connections(self):
        return 1


class _Clock:
    def __init__(self):
        self.t = 100.0

    def mono(self):
        return self.t


def _blocks(stereo, sr, n=1600):
    return [pan.AudioBlock(sr, stereo[i:i + n]) for i in range(0, stereo.shape[0], n)]


def _stereo(x):
    return np.ascontiguousarray(np.concatenate([x, x], axis=1), dtype=np.float32)


def _run(blocks, tmp_path, decoder, clock=None, gap_before=None, gap_s=0.0):
    """Drive pas.run over `blocks`; the clock advances by each block's duration, plus `gap_s`
    before block index `gap_before`."""
    clock = clock or _Clock()
    payloads = []
    durations = [b.samples.shape[0] / b.sample_rate for b in blocks]
    state = {"i": 0}

    class Rx(_Receiver):
        def capture(self, _timeout_ms):
            i = state["i"]
            if i >= len(blocks):
                return None
            if gap_before is not None and i == gap_before:
                clock.t += gap_s
            clock.t += durations[i]
            state["i"] = i + 1
            return blocks[i]

    pas.run(Rx([]), str(tmp_path), source="S", decoder=decoder, mono=clock.mono,
            max_loops=len(blocks), on_write=payloads.append, log=lambda m: None)
    return payloads


def test_run_is_unknown_until_4_s_of_audio_then_measurement(decoder, tmp_path):
    x, sr = _fixture("s2-R-rec")
    payloads = _run(_blocks(_stereo(x[: 6 * sr]), sr), tmp_path, decoder)
    assert [p["verdict"] for p in payloads] == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT"]
    assert "warming up" in payloads[1]["reason"]
    assert payloads[1]["marker_chain"] is None
    for p in payloads[2:]:
        assert p["marker_chain"] >= pa.MARKER_CHAIN_MIN + 2
        assert p["markers_decoded"] >= p["marker_chain"]
    assert json.loads((tmp_path / rsf.PROGRAM_AUDIO_NAME).read_text())["marker_chain"] >= 6


def test_run_restarts_the_span_after_a_receive_gap_without_a_timestamp(decoder, tmp_path):
    """The arrival-gap FALLBACK: these blocks carry no sender timestamp (AudioBlock's default is
    NDIlib_recv_timestamp_undefined). With timestamps the sender timeline decides instead
    (tests/python/test_program_audio_timeline_1404.py)."""
    x, sr = _fixture("base-R-rec")
    stereo = _stereo(x[: 10 * sr])
    blocks = _blocks(stereo, sr)
    gap_at = len(blocks) // 10 * 6  # after 6 s of audio
    payloads = _run(blocks, tmp_path, decoder, gap_before=gap_at, gap_s=pas.RECEIVE_GAP_S + 0.5)
    verdicts = [p["verdict"] for p in payloads]
    assert verdicts == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT", "UNKNOWN", "MEASUREMENT"]
    assert "warming up" in payloads[4]["reason"]


def test_run_does_not_treat_a_short_delay_as_a_gap(decoder, tmp_path):
    x, sr = _fixture("base-R-rec")
    blocks = _blocks(_stereo(x[: 8 * sr]), sr)
    payloads = _run(blocks, tmp_path, decoder, gap_before=len(blocks) // 2,
                    gap_s=pas.RECEIVE_GAP_S - 0.5)
    assert [p["verdict"] for p in payloads] == ["UNKNOWN", "UNKNOWN", "MEASUREMENT", "MEASUREMENT",
                                                "MEASUREMENT"]


def test_run_after_silence_waits_for_a_full_span_before_judging_the_marker(decoder, tmp_path):
    x, sr = _fixture("s3-A-rec")
    stereo = np.concatenate([np.zeros((4 * sr, 2), dtype=np.float32), _stereo(x[: 6 * sr])])
    payloads = _run(_blocks(stereo, sr), tmp_path, decoder)
    verdicts = [p["verdict"] for p in payloads]
    assert verdicts == ["UNKNOWN", "UNKNOWN", "SILENT", "UNKNOWN", "MEASUREMENT", "MEASUREMENT"]
    assert "marker span" in payloads[3]["reason"]


def test_run_reads_in_band_content_without_the_marker_as_foreign(decoder, tmp_path):
    rng = np.random.default_rng([1404, 7])
    stream = cal.synthetic_stream("chord", -30.0, 8.0, rng)
    payloads = _run(_blocks(stream, cal.SYNTH_SR, n=1024), tmp_path, decoder)
    judged = [p for p in payloads if p["marker_chain"] is not None]
    assert judged and all(p["outside_band_pct"] < pa.FOREIGN_OUTSIDE_BAND_PCT for p in judged)
    assert any(p["verdict"] == "FOREIGN" for p in judged)
    assert payloads[-1]["last_foreign_ts_utc"] is not None


def test_run_never_says_measurement_from_the_spectrum_alone(tmp_path):
    x, sr = _fixture("s2-R-rec")
    payloads = _run(_blocks(_stereo(x[: 8 * sr]), sr), tmp_path, NoMarkers())
    verdicts = [p["verdict"] for p in payloads]
    assert "MEASUREMENT" not in verdicts
    assert verdicts[2:] == ["FOREIGN"] * 3


def test_run_with_a_fake_full_chain_reads_measurement(tmp_path):
    """The sampler trusts the decoder's chain: a full chain over measurement audio = MEASUREMENT."""
    x, sr = _fixture("s2-R-rec")
    payloads = _run(_blocks(_stereo(x[: 6 * sr]), sr), tmp_path, FixedChain())
    assert [p["verdict"] for p in payloads][2:] == ["MEASUREMENT", "MEASUREMENT"]


def test_run_reads_a_decode_failure_as_unknown(tmp_path):
    class Broken:
        def decode(self, samples, sample_rate):
            raise pam.DecodeError("refused")

    x, sr = _fixture("s2-R-rec")
    payloads = _run(_blocks(_stereo(x[: 6 * sr]), sr), tmp_path, Broken())
    assert [p["verdict"] for p in payloads][2:] == ["UNKNOWN", "UNKNOWN"]
    assert "marker decode failed" in payloads[2]["reason"]


def test_main_without_the_shim_writes_unknown_and_never_opens_ndi(tmp_path, monkeypatch):
    def _forbidden(*_a, **_k):
        raise AssertionError("the NDI receiver must not be created without a marker decoder")

    monkeypatch.setattr(pan, "NdiAudioReceiver", _forbidden)
    serve = tmp_path / "serve"
    rc = pas.main(["--serve-dir", str(serve), "--marker-shim", str(tmp_path / "missing.so")])
    assert rc == 1
    got = json.loads((serve / rsf.PROGRAM_AUDIO_NAME).read_text())
    assert got["verdict"] == "UNKNOWN"
    assert "missing.so" in got["reason"] and "build-qpsk-guard-shim.sh" in got["reason"]


def test_main_reads_the_shim_path_from_its_environment_variable(tmp_path, monkeypatch):
    monkeypatch.setenv(pam.SHIM_ENV, str(tmp_path / "from-env.so"))
    monkeypatch.setattr(pan, "NdiAudioReceiver", lambda *_a, **_k: (_ for _ in ()).throw(AssertionError()))
    serve = tmp_path / "serve"
    assert pas.main(["--serve-dir", str(serve)]) == 1
    assert "from-env.so" in json.loads((serve / rsf.PROGRAM_AUDIO_NAME).read_text())["reason"]
