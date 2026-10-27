"""Issue 1482: the `[cam2->SOURCE]` capture-drop count covers the RECORDING window only.

camera-box writes the capture-stats sidecar once, on shutdown of the burn instance, so its
`v4l2_dropped` counts the whole burn life (deploy, the painter relaunch, teardown). In the direct
feed (cam2's HDMI straight into the splitter) every painter start/stop drops a few frames on the
Cam Link: run 869228708 counted 24, all outside 02:16-02:21 UTC, 0 inside. The fixture lines below
are that run's real burn-log shape (ANSI colours included).
"""

import importlib.util
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
SCRIPT = os.path.join(REPO, "scripts", "capture_drop_window.py")
E2E = os.path.join(REPO, "scripts", "recording-e2e.sh")


def _load():
    spec = importlib.util.spec_from_file_location("capture_drop_window", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _drop(ts, n, total):
    return (
        f"\x1b[2m{ts}\x1b[0m \x1b[33m WARN\x1b[0m \x1b[2mcamera_box::capture\x1b[0m\x1b[2m:\x1b[0m "
        f"capture device dropped {n} frame(s): v4l2 sequence 1 -> {n + 2} (total dropped {total})\n"
    )


# 2026-10-27T02:16:00Z = 1793067360 (the window start), 02:21:20Z = 1793067680 (the end).
START = 1793067360
END = 1793067680


def _log(lines):
    noise = "\x1b[2m2026-10-27T02:14:00.000000Z\x1b[0m  INFO camera_box: Streaming: fps 60\n"
    return noise + "".join(lines)


OUTSIDE_ONLY = _log(
    [
        _drop("2026-10-27T02:12:40.747503Z", 1, 1),
        _drop("2026-10-27T02:13:10.878450Z", 2, 3),
        _drop("2026-10-27T02:21:26.646763Z", 1, 4),
    ]
)


def test_drops_outside_the_recording_window_are_not_counted_1482():
    m = _load()
    assert m.window_drops(OUTSIDE_ONLY, START, END) == (0, 4)


def test_drops_inside_the_window_are_counted_with_their_frame_count_1482():
    m = _load()
    text = _log(
        [
            _drop("2026-10-27T02:12:40.747503Z", 1, 1),
            _drop("2026-10-27T02:16:00.000001Z", 2, 3),  # first second of the window
            _drop("2026-10-27T02:18:13.500000Z", 1, 4),
            _drop("2026-10-27T02:21:20.999999Z", 3, 7),  # last second of the window
            _drop("2026-10-27T02:21:21.000000Z", 1, 8),  # one second past it
        ]
    )
    assert m.window_drops(text, START, END) == (6, 8)


def test_the_sidecar_is_rewritten_to_the_window_count_and_keeps_the_whole_run_1482(tmp_path):
    m = _load()
    sidecar = tmp_path / "cam1-capture-stats.txt"
    sidecar.write_text("v4l2_dropped=4\nframes_captured=32926\n")
    log = tmp_path / "cam1-cbox-burn-1.log"
    log.write_text(OUTSIDE_ONLY)
    rc = subprocess.run(
        [sys.executable, SCRIPT, "--sidecar", str(sidecar), "--burn-log", str(log),
         "--start", str(START), "--end", str(END)],
        capture_output=True, text=True,
    )
    assert rc.returncode == 0, rc.stderr
    kv = dict(l.split("=", 1) for l in sidecar.read_text().split())
    assert kv["v4l2_dropped"] == "0"
    assert kv["frames_captured"] == "32926"
    assert kv["v4l2_dropped_whole_run"] == "4"
    assert kv["v4l2_drop_window"] == f"{START}..{END}"
    assert "4 of them outside the recording window" in rc.stdout


@pytest.mark.parametrize(
    "case",
    ["no_log", "total_mismatch", "no_drop_lines_but_sidecar_counts_drops", "bad_window"],
)
def test_every_unprovable_case_keeps_the_whole_run_count_1482(tmp_path, case):
    sidecar = tmp_path / "cam1-capture-stats.txt"
    original = "v4l2_dropped=4\nframes_captured=32926\n"
    sidecar.write_text(original)
    log = tmp_path / "burn.log"
    start, end = START, END
    if case == "total_mismatch":
        log.write_text(OUTSIDE_ONLY.replace("(total dropped 4)", "(total dropped 3)"))
    elif case == "no_drop_lines_but_sidecar_counts_drops":
        log.write_text(_log([]))
    elif case == "bad_window":
        log.write_text(OUTSIDE_ONLY)
        start, end = END, START
    rc = subprocess.run(
        [sys.executable, SCRIPT, "--sidecar", str(sidecar), "--burn-log", str(log),
         "--start", str(start), "--end", str(end)],
        capture_output=True, text=True,
    )
    assert rc.returncode == 0, rc.stderr
    assert sidecar.read_text() == original
    assert "WARNING" in rc.stderr and "whole-run count kept" in rc.stderr


def test_a_clean_run_with_no_drops_stays_zero_1482(tmp_path):
    sidecar = tmp_path / "s.txt"
    sidecar.write_text("v4l2_dropped=0\nframes_captured=100\n")
    log = tmp_path / "b.log"
    log.write_text(_log([]))
    rc = subprocess.run(
        [sys.executable, SCRIPT, "--sidecar", str(sidecar), "--burn-log", str(log),
         "--start", str(START), "--end", str(END)],
        capture_output=True, text=True,
    )
    assert rc.returncode == 0, rc.stderr
    assert "v4l2_dropped=0" in sidecar.read_text()


def test_recording_e2e_scopes_the_sidecar_after_persisting_the_burn_log_1482():
    s = open(E2E, encoding="utf-8").read()
    call = 'python3 "$HERE/capture_drop_window.py"'
    assert s.count(call) == 1, "issue 1482: the scope step must be wired exactly once"
    pos = s.find(call)
    assert s.find("#716: persist each cam-box burn-run fps log") < pos
    block = s[pos : pos + 600]
    assert '--sidecar "$CAM1_CAPTURE_STATS"' in block
    assert '--start "$CAPTURE_RATE_WINDOW_START_EPOCH"' in block
    assert '--end "$CAPTURE_RATE_WINDOW_END_EPOCH"' in block
    assert "cbox_burn_log_dest_name cam1" in block
    # the verdict reads the scoped sidecar: the scope step runs before VERDICT_ARGS takes it
    assert pos < s.find("VERDICT_ARGS+=(--cam1-capture-stats")
