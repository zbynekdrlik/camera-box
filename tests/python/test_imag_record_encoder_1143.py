"""#1143 -- imag E2E recording must not perturb the measurement: the pure record-encoder
decision + OBS-log parsing.

Root cause (#1130): the imag E2E records the OBS program with SOFTWARE x264, which under the
imag-nb 30W PL1 clamp pushes the graphics thread past its 16.67ms budget -> ~18.4% OBS "lagged"
frames -> the recording repeats ~19.5% of frames (observer effect). VAAPI-tex (FFmpeg VAAPI H.264,
texture/zero-copy) was LIVE-PROVEN this session to record valid H.264 High 1080p60 with render held
at ~4ms / ~0% lagged. #847 already live-proved the OTHER Intel HW path -- QSV (obs_qsv11_v2) -- is
BROKEN on this box (libmfx MFX_ERR_UNSUPPORTED at Init()), so QSV must NEVER be chosen; x264 stays
the graceful fallback only when VAAPI is genuinely unavailable.
"""
import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parents[2]


def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def _mod():
    return _load(REPO / "scripts" / "imag_record_encoder.py", "imag_record_encoder_under_test")


# The three encoder ids OBS advertises on the imag-nb Intel-iGPU box (live "Available Encoders").
AVAIL_INTEL = {"ffmpeg_vaapi_tex", "hevc_ffmpeg_vaapi_tex", "obs_qsv11_v2", "obs_qsv11_hevc", "obs_x264"}
AVAIL_NO_VAAPI = {"obs_qsv11_v2", "obs_x264"}  # a hypothetical build lacking VAAPI


# ---- choose_record_encoder -------------------------------------------------
def test_no_dgpu_prefers_vaapi_tex_when_available():
    m = _mod()
    assert m.choose_record_encoder(False, AVAIL_INTEL) == "ffmpeg_vaapi_tex"


def test_no_dgpu_falls_back_to_x264_when_vaapi_absent():
    """#847: x264 is the safe fallback -- NEVER QSV, even though it is 'available' (live-broken)."""
    m = _mod()
    assert m.choose_record_encoder(False, AVAIL_NO_VAAPI) == "obs_x264"


def test_no_dgpu_none_available_trusts_the_intel_bundle_vaapi_default():
    """None = 'availability not probed' -> trust the vendored Intel bundle, which always ships
    ffmpeg_vaapi_tex (the #1143 default). seed_profile calls this shape."""
    m = _mod()
    assert m.choose_record_encoder(False, None) == "ffmpeg_vaapi_tex"


def test_dgpu_keeps_nvenc():
    m = _mod()
    assert m.choose_record_encoder(True, {"obs_nvenc_h264_tex", "obs_x264"}) == "obs_nvenc_h264_tex"
    assert m.choose_record_encoder(True, None) == "obs_nvenc_h264_tex"


def test_never_returns_qsv():
    """Explicit negative guard -- QSV is live-proven unreliable on this box (#847), never picked."""
    m = _mod()
    for has_dgpu in (True, False):
        for avail in (AVAIL_INTEL, AVAIL_NO_VAAPI, {"obs_qsv11_v2"}, None):
            assert "qsv" not in m.choose_record_encoder(has_dgpu, avail).lower()


# ---- vaapi_record_encoder_settings ----------------------------------------
def test_vaapi_settings_are_cqp_on_the_render_node():
    """CQP (not CBR) -- the consult: EncSliceLP CBR/VBR need HuC firmware; CQP sidesteps it AND
    preserves QR/burn detail (disk covered by #1122 retention)."""
    m = _mod()
    s = m.vaapi_record_encoder_settings()
    assert s["rate_control"] == "CQP"
    assert 18 <= s["qp"] <= 22
    assert s["vaapi_device"] == "/dev/dri/renderD128"


def test_vaapi_settings_qp_is_overridable():
    m = _mod()
    assert m.vaapi_record_encoder_settings(qp=19)["qp"] == 19


# ---- parse_available_encoders ---------------------------------------------
AVAIL_BLOCK = (
    "23:37:34.811: Available Encoders:\n"
    "23:37:34.811:   Video Encoders:\n"
    "23:37:34.811: \t- ffmpeg_vaapi_tex (FFmpeg VAAPI H.264)\n"
    "23:37:34.811: \t- hevc_ffmpeg_vaapi_tex (FFmpeg VAAPI HEVC)\n"
    "23:37:34.811: \t- obs_qsv11_v2 (QuickSync H.264)\n"
    "23:37:34.811: \t- obs_x264 (x264)\n"
    "23:37:34.811:   Audio Encoders:\n"
    "23:37:34.811: \t- ffmpeg_aac (FFmpeg AAC)\n"
)


def test_parse_available_encoders_reads_the_video_encoder_ids():
    m = _mod()
    got = m.parse_available_encoders(AVAIL_BLOCK)
    assert "ffmpeg_vaapi_tex" in got
    assert "obs_qsv11_v2" in got
    assert "obs_x264" in got
    # audio encoder ids are NOT record-video encoders
    assert "ffmpeg_aac" not in got


def test_parse_available_encoders_empty_on_no_block():
    m = _mod()
    assert m.parse_available_encoders("no encoders here") == set()


# ---- parse_obs_record_stats -----------------------------------------------
# x264 shape: an explicit lagged line (the observer-effect state).
X264_STATS = (
    "20:07:44.498: program-render-audit: render_fps=58.8 target_fps=60.0 avg_frame_ms=14.34 lagged=10 total=294\n"
    "20:07:49.502: program-render-audit: render_fps=58.2 target_fps=60.0 avg_frame_ms=15.13 lagged=6 total=291\n"
    "20:07:54.671: [ffmpeg muxer: 'adv_file_output'] Output of file '/home/newlevel/rec.mkv' stopped\n"
    "20:07:54.672: Output 'adv_file_output': Total frames output: 840\n"
    "20:07:54.672: Output 'adv_file_output': Total drawn frames: 15740 (19297 attempted)\n"
    "20:07:54.672: Output 'adv_file_output': Number of lagged frames due to rendering lag/stalls: 3557 (18.4%)\n"
)
# VAAPI-clean shape: OBS omits the lagged line entirely when there are ~no lagged frames.
VAAPI_STATS = (
    "23:37:49.679: program-render-audit: render_fps=60.0 target_fps=60.0 avg_frame_ms=3.96 lagged=0 total=300\n"
    "23:37:54.679: program-render-audit: render_fps=60.0 target_fps=60.0 avg_frame_ms=4.83 lagged=0 total=300\n"
    "23:38:01.253: Output 'adv_file_output': Total frames output: 958\n"
    "23:38:01.253: Output 'adv_file_output': Total drawn frames: 964\n"
    "23:38:01.254: ==== Recording Stop ================================================\n"
)


def test_parse_x264_stats_reads_drawn_attempted_lagged():
    m = _mod()
    s = m.parse_obs_record_stats(X264_STATS)
    assert s["drawn_frames"] == 15740
    assert s["attempted_frames"] == 19297
    assert s["lagged_frames"] == 3557
    assert abs(s["lagged_pct"] - 18.4) < 0.05
    # Task 4: max in-record render frame time is captured too
    assert abs(s["max_render_ms"] - 15.13) < 0.01


def test_parse_vaapi_clean_stats_no_lagged_line_means_zero():
    """The VAAPI-fixed state: no 'Number of lagged frames' line -> lagged 0, pct 0.0. attempted
    defaults to drawn when OBS omits the '(N attempted)' suffix."""
    m = _mod()
    s = m.parse_obs_record_stats(VAAPI_STATS)
    assert s["drawn_frames"] == 964
    assert s["attempted_frames"] == 964
    assert s["lagged_frames"] == 0
    assert s["lagged_pct"] == 0.0
    assert abs(s["max_render_ms"] - 4.83) < 0.01


def test_parse_returns_none_without_stop_stats():
    m = _mod()
    assert m.parse_obs_record_stats("just some log lines\nnothing useful\n") is None


# ---- record_encoder_apply_plan (the conditional make-it-live decision) ------
def test_apply_plan_ok_when_already_vaapi_and_json_present():
    """Disk is already the VAAPI target AND recordEncoder.json matches -> OBS booted into VAAPI,
    no restart needed (a no-op verify)."""
    m = _mod()
    assert m.record_encoder_apply_plan("ffmpeg_vaapi_tex", "ffmpeg_vaapi_tex", True) == "ok"


def test_apply_plan_apply_when_encoder_still_x264():
    """The first run after deploy: disk RecEncoder is still x264 -> write VAAPI + restart."""
    m = _mod()
    assert m.record_encoder_apply_plan("obs_x264", "ffmpeg_vaapi_tex", False) == "apply"


def test_apply_plan_apply_when_encoder_matches_but_json_missing():
    """RecEncoder says VAAPI but recordEncoder.json is absent/wrong -> OBS would use VAAPI defaults
    (CBR2500), not the CQP settings -> apply to fix the on-disk json + restart."""
    m = _mod()
    assert m.record_encoder_apply_plan("ffmpeg_vaapi_tex", "ffmpeg_vaapi_tex", False) == "apply"


def test_apply_plan_ok_when_target_is_x264_and_running_x264():
    """A no-dGPU box WITHOUT VAAPI available (target x264): already x264, no restart, no json."""
    m = _mod()
    assert m.record_encoder_apply_plan("obs_x264", "obs_x264", True) == "ok"
