"""issue 1404 Task 5 part a -- the camera-box measurement clip (scripts/gen_measurement_clip.py).

The clip replaces music in the CG segments of the release E2E (owner amendment, comment 6016489928):
a synthesized 30 fps recording of the cam2 painter. Pinned here on a real generated 10 s clip,
decoded by the REAL consumers, never by a re-implementation:
  - the YouTube-leg tick decoder (scripts/youtube_leg_ticks.py), asked for the clip's id
    (`runs=CLIP_RUNS`), reads the painter's 60 Hz tick of every frame (>= 99 %) and the timeline
    proves every frame's cadence; its default decode still refuses the reserved id;
  - the dock's own QPSK decoder (the shim built by scripts/build-qpsk-guard-shim.sh) finds every
    marker at its time and index on BOTH channels, and nothing else;
  - the program-audio guard (the real sampler loop + the shim) reads every window after the warm-up
    as MEASUREMENT: the tone bed is the guard's own tone line (imported), the marker index sits on
    the guard's 60 Hz timecode line;
  - two runs give the same bytes.
The parameter block is pinned to the Rust painter / marker sources, the dock header and the shim's
compiled-in parameters (one parameter set), and the waveform to the vendored norihiro encoder
(vendor/av-sync-dock/tool/videogen.py) for all 256 indices. `recording-verdict --av-sync` on the clip is CI/live only
(the probe feature never compiles on dev1, Tier-0).
"""
import hashlib
import math
import pathlib
import re
import struct
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

import gen_measurement_clip as gen  # noqa: E402
import program_audio as pa  # noqa: E402
import program_audio_marker as pam  # noqa: E402
import program_audio_marker_calibrate as cal  # noqa: E402
import rig_marker_mirror as rmm  # noqa: E402
import youtube_leg_ticks as ylt  # noqa: E402
from qpsk_guard_shim_1404 import build_shim  # noqa: E402

CLIP_SECONDS = 10


def _src(rel):
    return (_ROOT / rel).read_text(encoding="utf-8")


def _rust_const(text, name):
    m = re.search(rf"pub const {name}: \w+ = ([0-9_.xXa-fA-F]+);", text)
    assert m, f"`pub const {name}` is gone from the source"
    v = m.group(1).replace("_", "")
    return int(v, 16) if v.lower().startswith("0x") else (float(v) if "." in v else int(v))


@pytest.fixture(scope="module")
def clip(tmp_path_factory):
    out = tmp_path_factory.mktemp("measurement-clip") / "clip.mp4"
    log = gen.write_clip(str(out), CLIP_SECONDS)
    return out, pathlib.Path(log)


@pytest.fixture(scope="module")
def decoder(tmp_path_factory):
    return pam.MarkerDecoder(build_shim(tmp_path_factory.mktemp("qpsk-guard-shim")))


@pytest.fixture(scope="module")
def clip_audio(clip):
    return cal.load_audio(str(clip[0]))


def _log_rows(path):
    rows = []
    for line in pathlib.Path(path).read_text(encoding="ascii").splitlines():
        if line.startswith("#") or line.startswith("index") or not line.strip():
            continue
        idx, fid, ts = (int(v) for v in line.split(","))
        rows.append((idx, fid, ts))
    return rows


# ---------------------------------------------------------------------------------------------
# the real consumers on a real generated clip
# ---------------------------------------------------------------------------------------------


def test_the_tick_decoder_reads_the_painters_tick_on_every_frame(clip):
    import youtube_leg_timeline as ytl

    rows = ylt.decode_ticks(str(clip[0]), workers=4, runs=ylt.CLIP_RUNS)
    assert len(rows) == CLIP_SECONDS * gen.FPS == ylt.container_frames(str(clip[0]))
    right = sum(1 for r in rows if r[2] == 2 * r[0])  # the painter's 60 Hz tick: 2 per 30 fps frame
    assert right >= 0.99 * len(rows), f"only {right}/{len(rows)} frames read their tick"
    assert all(r[2] in (None, 2 * r[0]) for r in rows), "a frame read a WRONG tick"
    cont = ytl.continuity([r[:3] for r in rows])  # the YouTube-leg timeline's own cadence proof
    assert (cont["cadence_proven"], cont["events"]) == (len(rows), 0), cont


def test_the_default_tick_decode_still_refuses_the_clip_id():
    """A default decode (camera windows, restreamer's gate) never reads the clip's tick: the clip
    restarts its tick every play, so a clip tick there would read as replay dups."""
    import cv2

    det = cv2.QRCodeDetector()
    for f in (1, 151):
        frame = cv2.cvtColor(gen.render_frame(f), cv2.COLOR_GRAY2BGR)
        assert ylt.half_ticks(frame, det) == (None, None)
        assert ylt.half_ticks(frame, det, runs=ylt.CLIP_RUNS) == (2 * f, 2 * f - 1)


def test_the_dock_decoder_finds_every_marker_on_both_channels_and_nothing_else(clip, clip_audio, decoder):
    samples, sr = clip_audio
    assert sr == gen.SAMPLE_RATE and samples.shape[1] == gen.CHANNELS
    expected = [(fid / gen.TICK_HZ, idx) for idx, fid, _ in _log_rows(clip[1])]
    assert len(expected) == len(gen.marker_schedule(CLIP_SECONDS * gen.FPS)) == 19
    words = decoder.decode(samples, sr)
    assert len(words) == 2
    for ch, found in enumerate(words):
        assert len(found) == len(expected), f"channel {ch}: {len(found)} words, {len(expected)} markers"
        for (t, idx), (ts, wi) in zip(expected, found):
            assert wi == idx, f"channel {ch}: index {wi} at {ts:.4f} s, expected {idx}"
            assert abs(ts - t) <= 0.002, f"channel {ch}: marker {idx} at {ts:.4f} s, expected {t:.4f} s"


def test_the_program_audio_guard_reads_every_window_after_the_warm_up_as_measurement(clip_audio, decoder):
    samples, sr = clip_audio
    payloads = cal.evaluate(samples, sr, decoder)
    verdicts = [p["verdict"] for p in payloads]
    first = next((i for i, p in enumerate(payloads) if p["marker_chain"] is not None), None)
    assert first is not None, f"no window had a full marker span: {verdicts}"
    assert all(v == "UNKNOWN" for v in verdicts[:first]), f"the warm-up read {verdicts[:first]}"
    assert verdicts[first:] == ["MEASUREMENT"] * (len(payloads) - first), verdicts
    assert len(payloads) - first >= CLIP_SECONDS / pa.WINDOW_S - 2
    ok, detail = cal.bar_real(payloads)  # the real-audio bar: no FOREIGN, chain >= MARKER_CHAIN_MIN + 2
    assert ok, detail
    assert all(p["outside_band_pct"] < pa.FOREIGN_OUTSIDE_BAND_PCT for p in payloads)


def test_two_runs_give_the_same_bytes(tmp_path):
    a, b = tmp_path / "a.mp4", tmp_path / "b.mp4"
    log_a = gen.write_clip(str(a), 2)
    log_b = gen.write_clip(str(b), 2)
    assert gen.sha256_file(str(a)) == gen.sha256_file(str(b))
    assert pathlib.Path(log_a).read_bytes() == pathlib.Path(log_b).read_bytes()


def test_the_cli_prints_the_sha256_and_size_and_leaves_no_part_file(tmp_path):
    out = tmp_path / "c.mp4"
    r = subprocess.run([sys.executable, str(_SCRIPTS / "gen_measurement_clip.py"), "--out", str(out),
                        "--seconds", "1"], capture_output=True, text=True, timeout=300)
    assert r.returncode == 0, r.stderr
    first = r.stdout.splitlines()[0].split()
    assert first[0] == hashlib.sha256(out.read_bytes()).hexdigest()
    assert int(first[1]) == out.stat().st_size and first[3] == str(out)
    assert sorted(p.name for p in tmp_path.iterdir()) == ["c.mp4", "c.mp4.markers.csv"]


def test_a_failed_encode_fails_loud_and_leaves_nothing(tmp_path, monkeypatch):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "ffmpeg"
    fake.write_text("#!/bin/sh\necho 'fake ffmpeg: no encoder' >&2\nexit 3\n")
    fake.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}:/usr/bin:/bin")
    out = tmp_path / "d.mp4"
    with pytest.raises(RuntimeError, match="exit 3.*no encoder"):
        gen.write_clip(str(out), 1)
    assert not out.exists() and not (tmp_path / "d.mp4.part").exists()
    assert not (tmp_path / "d.mp4.markers.csv").exists()
    assert gen.main(["--out", str(out), "--seconds", "1"]) == 1


def _fake_ffmpeg(tmp_path, monkeypatch, body):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "ffmpeg"
    fake.write_text("#!/bin/sh\n" + body)
    fake.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}:/usr/bin:/bin:/usr/local/bin")


def test_an_ffmpeg_that_exits_0_without_reading_is_a_failure(tmp_path, monkeypatch):
    _fake_ffmpeg(tmp_path, monkeypatch, "exit 0\n")
    out = tmp_path / "e.mp4"
    with pytest.raises(RuntimeError, match="input not read in full"):
        gen.write_clip(str(out), 1)
    assert sorted(p.name for p in tmp_path.iterdir()) == ["bin"]


def test_a_hung_ffmpeg_is_killed_by_the_bound_and_leaves_nothing(tmp_path, monkeypatch):
    _fake_ffmpeg(tmp_path, monkeypatch, "sleep 60\n")  # never reads its input
    out = tmp_path / "f.mp4"
    with pytest.raises(RuntimeError, match="bound ran out"):
        gen.write_clip(str(out), 1, timeout_s=2)
    assert sorted(p.name for p in tmp_path.iterdir()) == ["bin"]


def test_an_encode_without_the_right_frame_count_is_never_renamed(tmp_path, monkeypatch):
    # reads every frame, exits 0, but writes no real mp4 (ffprobe cannot count its frames)
    _fake_ffmpeg(tmp_path, monkeypatch, 'cat >/dev/null\nfor last; do :; done\necho junk > "$last"\n')
    out = tmp_path / "g.mp4"
    with pytest.raises(RuntimeError, match="cannot be probed|frames, expected"):
        gen.write_clip(str(out), 1)
    assert sorted(p.name for p in tmp_path.iterdir()) == ["bin"]


def test_a_stopped_generator_kills_ffmpeg_and_leaves_nothing(tmp_path, monkeypatch):
    import os
    import signal
    import time

    _fake_ffmpeg(tmp_path, monkeypatch, 'echo $$ > "$FAKE_FFMPEG_PID"\nsleep 60\n')  # never reads
    work = tmp_path / "tmp"
    work.mkdir()
    pid_file = tmp_path / "pid"
    env = dict(os.environ, TMPDIR=str(work), FAKE_FFMPEG_PID=str(pid_file))
    p = subprocess.Popen([sys.executable, str(_SCRIPTS / "gen_measurement_clip.py"), "--out",
                          str(tmp_path / "s.mp4"), "--seconds", "1"], env=env,
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    deadline = time.monotonic() + 60
    while not (pid_file.exists() and pid_file.read_text().strip()) and time.monotonic() < deadline:
        time.sleep(0.1)
    assert pid_file.exists(), "the fake ffmpeg never started"
    time.sleep(0.5)  # the generator is now blocked feeding frames
    p.send_signal(signal.SIGTERM)
    _, err = p.communicate(timeout=60)
    assert p.returncode == 128 + signal.SIGTERM, err
    assert not os.path.exists(f"/proc/{int(pid_file.read_text())}"), "ffmpeg survived the stop"
    assert not list(work.iterdir()), "the temp audio dir was left behind"
    assert sorted(x.name for x in tmp_path.iterdir()) == ["bin", "pid", "tmp"]


def test_a_clip_longer_than_the_counter_is_refused_before_any_encode(tmp_path):
    assert 10 ** gen.DIGITS // gen.FPS == 333
    with pytest.raises(ValueError, match="frame counter"):
        gen.write_clip(str(tmp_path / "h.mp4"), 334)
    assert not list(tmp_path.iterdir())


def test_the_committed_deliverable_frame_reads_its_ticks():
    """tests/fixtures/measurement-clip-1404/clip-v1-frame-1801.png is frame 1801 of the real
    deliverable (H.264, gray via ffmpeg). The Rust recording decode reads the same file
    (tests/measurement_clip_decode_1404.rs); its expected payloads are the generator's own."""
    import cv2

    img = cv2.imread(str(_ROOT / "tests" / "fixtures" / "measurement-clip-1404" / "clip-v1-frame-1801.png"))
    assert img is not None and img.shape[:2] == (gen.HEIGHT, gen.WIDTH)
    assert ylt.half_ticks(img, cv2.QRCodeDetector(), runs=ylt.CLIP_RUNS) == (3602, 3601)
    assert gen.frame_payloads(1801) == ("P911016.3602.60033333333.1636215862",
                                        "P911016.3601.60016666667.527692900")
    rs = _src("tests/measurement_clip_decode_1404.rs")
    assert "frame_id: 3602,\n            gen_ts_ns: 60_033_333_333," in rs
    assert "frame_id: 3601,\n            gen_ts_ns: 60_016_666_667," in rs


# ---------------------------------------------------------------------------------------------
# the picture
# ---------------------------------------------------------------------------------------------


def test_vernier_ids_are_the_painters():
    assert [gen.vernier_ids(t) for t in range(5)] == [(0, 0), (0, 1), (2, 1), (2, 3), (4, 3)]


def test_frame_payloads_are_crc_valid_painter_payloads_of_the_frames_tick():
    for f in (0, 1, 151, 1800, 3599):
        left, right = gen.frame_payloads(f)
        t = gen.TICKS_PER_FRAME * f
        assert ylt.painter_payload(left, ylt.CLIP_RUNS) == (gen.RUN_ID, t)
        assert ylt.painter_payload(right, ylt.CLIP_RUNS) == (gen.RUN_ID, max(t - 1, 0))
        assert left.split(".")[2] == str(gen.tick_pts_ns(t))
    assert gen.tick_pts_ns(2) == 33_333_333 and gen.tick_pts_ns(1) == 16_666_667
    assert gen.tick_pts_ns(2 * 30) == 1_000_000_000  # frame 30 = 1 s


def test_the_tick_decoder_reads_the_clip_id_only_when_asked_and_never_another_reserved_id():
    assert ylt.MEASUREMENT_CLIP_RUN_ID == gen.RUN_ID == 911016 and ylt.CLIP_RUNS == (911016,)
    assert ylt.painter_payload(gen.qr_payload(911016, 7, 0)) is None  # the default decode
    assert ylt.painter_payload(gen.qr_payload(911016, 7, 0), ylt.CLIP_RUNS) == (911016, 7)
    for run in (911001, 911013, 911014, 911015, 911017):
        assert ylt.painter_payload(gen.qr_payload(run, 7, 0), ylt.CLIP_RUNS) is None
    assert ylt.painter_tick([gen.qr_payload(911016, 9, 0), "junk"], ylt.CLIP_RUNS) == 9


def test_the_geometry_is_the_painters():
    assert (gen.WIDTH, gen.HEIGHT) == (1920, 1080)
    fp = _src("src/bin/frame-probe.rs")
    assert "#[arg(long, default_value_t = 1920)]\n    canvas_w: u32" in fp
    assert "#[arg(long, default_value_t = 1080)]\n    canvas_h: u32" in fp
    assert gen.TOP_MARGIN_PX == _rust_const(_src("src/colour_scale.rs"), "TOP_MARGIN_PX")
    assert f"--dual-qr --qr-size {gen.QR_SIZE} " in _src("systemd/cam2-painter.service")
    m = re.search(r"default_value_t = (\d+)\)\]\n    audio_marker_cadence_ticks", fp)
    assert m and int(m.group(1)) == gen.MARKER_EVERY_TICKS


def test_qr_placement_is_the_qrcode_crates_module_size_centred_in_its_half_at_the_top():
    for n in (29 + 8, 33 + 8, 37 + 8):  # versions 3..5 with the 4-module quiet zone
        module = gen.QR_SIZE // n
        size = module * n
        assert size <= gen.QR_SIZE
        assert gen.qr_placement(n, False) == ((960 - size) // 2, gen.TOP_MARGIN_PX, module)
        assert gen.qr_placement(n, True) == (960 + (960 - size) // 2, gen.TOP_MARGIN_PX, module)


def test_the_counter_stays_clear_of_both_qrs_and_the_bottom_burn_band():
    sizes = {gen.qr_modules(p).shape[0] for f in (0, 1, 499, 500, 1799, 3599) for p in gen.frame_payloads(f)}
    x, y, w, h = gen.counter_box()
    for n in sizes:
        lx, ly, module = gen.qr_placement(n, False)
        rx, _, _ = gen.qr_placement(n, True)
        assert lx + module * n <= x and x + w <= rx, f"counter overlaps a {n}-module QR image"
        assert ly <= y and y + h <= ly + module * n
    assert y + h < 736  # above every bottom burn slot (src/burn_regions.rs: the band starts at 736)


def test_a_rendered_frame_is_white_with_black_qrs_and_counter():
    img = gen.render_frame(151)
    assert img.shape == (gen.HEIGHT, gen.WIDTH) and img.dtype == np.uint8
    assert set(np.unique(img)) == {0, 255}
    assert img[gen.HEIGHT - 10:, :].min() == 255  # nothing below the QR band
    x, y, w, h = gen.counter_box()
    assert img[y:y + h, x:x + w].min() == 0


# ---------------------------------------------------------------------------------------------
# the sound
# ---------------------------------------------------------------------------------------------


def _crc4_residual(word, bits=20, poly=0b10011):
    for s in range(bits - 1, 3, -1):
        if word & (1 << s):
            word ^= poly << (s - 4)
    return word


def test_the_marker_word_is_preamble_zero_nibble_index_crc4():
    for index in range(256):
        w = gen.payload_word(index)
        assert w >> 20 == 0
        assert (w >> 16) & 0xF == 0xF and (w >> 12) & 0xF == 0 and (w >> 4) & 0xFF == index
        assert _crc4_residual(w) == 0, f"index {index}: CRC-4 residual"
        assert gen.symbols(w)[0] == 0b11 and gen.symbols(w)[1] == 0b11


def test_the_qpsk_parameters_are_the_rust_painters_the_docks_and_the_shims(decoder):
    rs = _src("src/qpsk_marker.rs")
    assert gen.SAMPLE_RATE == _rust_const(rs, "AUDIO_SAMPLE_RATE_HZ")
    assert gen.CARRIER_HZ == _rust_const(rs, "CARRIER_HZ_DEFAULT")
    assert gen.N_PAYLOAD_BITS == _rust_const(rs, "N_PAYLOAD_BITS")
    assert gen.PREAMBLE_NIBBLE == _rust_const(rs, "PREAMBLE_NIBBLE")
    assert gen.CRC4_POLY == _rust_const(rs, "CRC4_POLY")
    assert gen.MARKER_AMPLITUDE == _rust_const(rs, "AMPLITUDE")
    assert gen.CONTINUOUS_CYCLES == _rust_const(rs, "AUDIO_CONTINUOUS_CYCLES")
    assert gen.QPSK_Q == _rust_const(rs, "Q_FRAMES")
    # c = auto_c(q, f, 60, 1) = q * f // (60 * N_SYMBOLS)
    assert gen.CYCLES_PER_SYMBOL == gen.QPSK_Q * gen.CARRIER_HZ // (gen.TICK_HZ * gen.N_SYMBOLS)
    hpp = _src("vendor/av-sync-dock/src/camera-box-audio.hpp")
    for name, value in (("CB_AUDIO_SAMPLE_RATE", gen.SAMPLE_RATE), ("CB_AUDIO_CARRIER_HZ", gen.CARRIER_HZ),
                        ("CB_AUDIO_C", gen.CYCLES_PER_SYMBOL)):
        assert re.search(rf"static const uint32_t {name} = {value}u;", hpp), name
    assert (decoder.params["sample_rate"], decoder.params["carrier_hz"], decoder.params["c"]) == \
        (gen.SAMPLE_RATE, gen.CARRIER_HZ, gen.CYCLES_PER_SYMBOL)


def test_every_scheduled_marker_ends_inside_the_clip():
    assert gen.signal_len() < gen.SAMPLE_RATE // gen.FPS  # 1085 samples < one frame (1600)
    for frames in (60, 61, 74, 3600):
        last_f = gen.marker_schedule(frames)[-1][0]
        assert last_f < frames
        assert last_f * gen.SAMPLE_RATE // gen.FPS + gen.signal_len() <= frames * gen.SAMPLE_RATE // gen.FPS


def test_the_waveform_is_the_vendored_norihiro_encoders_for_every_index(tmp_path):
    """vendor/av-sync-dock/tool/videogen.py `Pattern.audio_frames` is the reference encoder the dock
    decodes: every marker of the generator matches it within one int16 step (float order)."""
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "videogen_1404", _ROOT / "vendor" / "av-sync-dock" / "tool" / "videogen.py")
    videogen = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(videogen)
    ctx = videogen.Context(str(tmp_path), str(gen.TICK_HZ), str(gen.SAMPLE_RATE))
    pat = videogen.Pattern(ctx, f"q={gen.QPSK_Q},f={gen.CARRIER_HZ}")
    assert (pat.q, pat.f, pat.c) == (gen.QPSK_Q, gen.CARRIER_HZ, gen.CYCLES_PER_SYMBOL)
    lead = ctx.ar * (pat.q * 2) * ctx.vr[1] // ctx.vr[0]  # skip the reference's lead silence
    worst = 0
    for index in range(256):
        pat.i = index
        raw = pat.audio_frames(start_offset=lead)
        ref = np.array(struct.unpack(f"<{len(raw) // 2}h", raw), dtype=np.int64)
        mine = np.round(gen.marker_signal(index).astype(np.float64) * 32767 * gen.MARKER_AMPLITUDE)
        assert ref.shape == mine.shape, (index, ref.shape, mine.shape)
        worst = max(worst, int(np.abs(ref - mine.astype(np.int64)).max()))
    assert worst <= 1, f"the waveform differs from videogen.py by {worst} int16 steps"


def test_the_marker_signal_is_ten_raised_cosine_symbols():
    sig = gen.marker_signal(0x5A)
    assert sig.dtype == np.float32 and sig.shape == (gen.signal_len(),) == (1085,)
    assert abs(float(sig[0])) < 1e-6  # the first rising edge starts on the ramp's zero
    assert float(np.abs(sig).max()) <= 1.0 and float(np.abs(sig).max()) > 0.99


def test_the_tone_bed_is_the_guards_tone_line_at_minus_30_dbfs():
    assert gen.TONE_LINES_HZ is pa.MEASUREMENT_TONE_LINES_HZ  # imported, never retyped
    assert gen.TONE_BED_HZ == pa.MEASUREMENT_TONE_LINES_HZ[0]
    body = _src("scripts/gen_measurement_clip.py")
    assert not re.search(r"TONE_BED_HZ\s*=\s*[0-9]", body), "the bed frequency must come from program_audio"
    bed = gen.tone_bed(gen.SAMPLE_RATE * 2)
    assert abs(10.0 * math.log10(float(np.mean(bed * bed))) - gen.TONE_BED_DBFS) < 0.01
    rms, _ = pa.analyse(bed, gen.SAMPLE_RATE)
    assert pa.classify(rms, None, None) == "SILENT"  # the guard takes the whole bed out


def test_the_marker_index_is_the_painters_and_sits_on_the_guards_timecode_line():
    import youtube_leg_timeline as ytl

    assert gen.TICK_HZ == pa.MARKER_INDEX_RATE_HZ == ytl.PAINTER_HZ
    sched = gen.marker_schedule(gen.SECONDS * gen.FPS)
    assert sched[0] == (15, 30, 30) and sched[1] == (30, 60, 60)
    assert all(idx == tick & 0xFF and tick == gen.TICKS_PER_FRAME * f for f, tick, idx in sched)
    assert len(sched) == 239  # every 0.5 s of 120 s, the first at 0.5 s
    words = [(tick / gen.TICK_HZ, idx) for _, tick, idx in sched]
    for start in (0.4, 30.0, 63.7, 115.5):
        span = [w for w in words if start <= w[0] < start + pa.MARKER_SPAN_S]
        assert pa.marker_chain(span) == len(span) == 8


def test_the_marker_log_is_the_painters_format():
    text = gen.marker_log_text(CLIP_SECONDS * gen.FPS)
    lines = text.splitlines()
    assert lines[0].encode().startswith(rmm.HEADER_PREFIX)
    assert lines[0] == "# qpsk-params sr=48000 carrier=442 c=1 q=2 vr=60/1"
    assert lines[1].encode() == rmm.COLUMN_HEADER
    assert lines[2] == "30,30,500000000" and lines[3] == "60,60,1000000000"
    assert text.endswith("\n") and len(lines) == 2 + 19


def test_the_audio_is_int16_stereo_with_identical_channels_and_headroom():
    pcm = gen.render_audio(2 * gen.FPS)
    assert pcm.dtype == np.dtype("<i2") and pcm.shape == (2 * gen.SAMPLE_RATE, 2)
    assert np.array_equal(pcm[:, 0], pcm[:, 1])
    assert int(np.abs(pcm.astype(np.int32)).max()) < 32767


# ---------------------------------------------------------------------------------------------
# 911016 is registered like the other reserved origin ids
# ---------------------------------------------------------------------------------------------


def test_911016_is_a_reserved_tick_excluded_origin_id_everywhere_the_ids_are_listed():
    import mv_skew_snapshot as mvs
    import qr_align_pins as qa

    assert gen.RUN_ID in qa.NODE_BURN_RUN_IDS
    assert gen.RUN_ID in mvs.RESERVED_RUN_IDS
    assert _rust_const(_src("src/probe/recording_latency.rs"), "MEASUREMENT_CLIP_RUN_ID") == gen.RUN_ID
    rec = _src("src/probe/recording.rs")
    block = rec[rec.index("pub const NODE_BURN_RUN_IDS"):]
    block = block[:block.index("];")]
    assert "recording_latency::MEASUREMENT_CLIP_RUN_ID" in block
    assert "911_016 =>" not in _src("src/burn_regions.rs")  # no overlay slot: never an echo-gated burn
