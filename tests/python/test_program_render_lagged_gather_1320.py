"""#1320 — unit tests for the PROGRAM-render freeze facet in scripts/bundle_state_gather.py.

`program_render_lagged_from_log(text)` scans the #1222 bounded TAIL once and returns
`(max_lagged_str, age_s_str)`: the MAX `program-render-audit lagged` over the tail + the in-log age
(whole seconds) of the MOST RECENT window achieving that max, `("", "")` when there is no
`program-render-audit:` line at all (absent -> UNKNOWN downstream, never a fabricated 0).

Root cause it surfaces (live strih logs 15.9.2026): a scene-switch-coincident DistroAV reattach whose
blocking NDIlib_recv_destroy ran on the OBS graphics thread froze the PROGRAM render ~7.5 s
(`program-render-audit lagged=228 avg_frame_ms=782` — the ONLY lagged>0 window in a 95 min session)
-> 2ME PGM starved -> stream FIFO underrun -> relock storm. In-log RELATIVE recency (no clock
injection), so these are pure fixture tests. Same "source the PURE parser" split as
test_audio_lag_gather_1231.py.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import bundle_state_gather as bsg  # noqa: E402

# The exact 15.9.2026 12:51 signature (real values): a healthy 0-lagged window, then the freeze.
FREEZE_LOG = (
    "13:14:35.881: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=17.9 lagged=0 total=151\n"
    "13:14:33.324: [distroav] 'NDI cam1' ndi_source_thread: ndiLib->recv_destroy(ndi_receiver)\n"
    "13:14:40.881: [distroav] 'NDI cam1' ndi_source_thread: Reset NDI Receiver\n"
    "13:14:40.889: program-render-audit: render_fps=29.9 target_fps=30.0 avg_frame_ms=295.06 lagged=228 total=233\n"
    "13:14:45.900: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=17.5 lagged=0 total=151\n"
)

# A healthy tail: every window lagged=0 (render telemetry live, no freeze).
HEALTHY_LOG = (
    "18:05:21.271: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=20.92 lagged=0 total=151\n"
    "18:05:26.305: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=18.23 lagged=0 total=151\n"
)


def test_freeze_reports_max_lagged_and_recency():
    lagged, age = bsg.program_render_lagged_from_log(FREEZE_LOG)
    assert lagged == "228", lagged
    # the freeze window (13:14:40.889) sits ~5.0 s behind the log head (13:14:45.900).
    assert age == "5", age


def test_healthy_tail_reports_zero_not_absent():
    # "0" is a truthy string KEPT by build_bundle_state (render telemetry live, no freeze) —
    # distinct from "" (no program-render-audit line at all).
    lagged, age = bsg.program_render_lagged_from_log(HEALTHY_LOG)
    assert lagged == "0", lagged
    assert age == "0", age  # the newest 0-lagged window IS the log head


def test_no_program_render_line_is_absent():
    assert bsg.program_render_lagged_from_log("18:00:00.000: [obs] some unrelated line\n") == ("", "")
    assert bsg.program_render_lagged_from_log("") == ("", "")


def test_most_recent_of_two_equal_max_windows_wins_the_age():
    # Two lagged=228 windows; the age must reflect the MORE RECENT one (fresher freeze).
    log = (
        "12:00:00.000: program-render-audit: render_fps=29.9 target_fps=30.0 avg_frame_ms=300.0 lagged=228 total=233\n"
        "12:30:00.000: program-render-audit: render_fps=29.9 target_fps=30.0 avg_frame_ms=300.0 lagged=228 total=233\n"
        "12:30:10.000: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=17.0 lagged=0 total=151\n"
    )
    lagged, age = bsg.program_render_lagged_from_log(log)
    assert lagged == "228", lagged
    assert age == "10", age  # 12:30:10 head - 12:30:00 recent max


def test_only_tail_slice_after_the_bounded_separator_is_read():
    # A freeze surviving only in the HEAD (before the #1222 separator) must NOT be reported.
    log = (
        "09:00:00.000: program-render-audit: render_fps=29.9 target_fps=30.0 avg_frame_ms=800.0 lagged=228 total=233\n"
        + bsg.LOG_BOUNDED_READ_SEPARATOR
        + "18:05:21.271: program-render-audit: render_fps=30.0 target_fps=30.0 avg_frame_ms=20.9 lagged=0 total=151\n"
    )
    lagged, age = bsg.program_render_lagged_from_log(log)
    assert lagged == "0", lagged  # only the tail's healthy window is seen, not the head freeze


def test_build_bundle_state_omits_empty_but_keeps_zero():
    keep = bsg.build_bundle_state(program_render_lagged="228", program_render_lagged_age_s="5")
    assert keep["program_render_lagged"] == "228"
    assert keep["program_render_lagged_age_s"] == "5"

    zero = bsg.build_bundle_state(program_render_lagged="0", program_render_lagged_age_s="0")
    assert zero["program_render_lagged"] == "0"  # "0" is truthy -> KEPT (render live, no freeze)

    absent = bsg.build_bundle_state()
    assert "program_render_lagged" not in absent  # "" -> dropped (UNKNOWN downstream)
    assert "program_render_lagged_age_s" not in absent
