"""#917 — unit tests for scripts/av_sync_measure.py's SyncNet per-shift distance-curve extraction:
_mean_shift_curve(), dist_curve_for_track(), _load_dist_tracks(), and measure()'s wiring of them.

Covers, with NO real syncnet_python / ffmpeg / torch / numpy dependency (pure Python fixtures --
a plain list-of-lists stands in for the numpy array the real pickle would contain; unpickling a
real numpy array is exercised only implicitly on the live box, never required for these tests):

  a. _mean_shift_curve() -- averages a (nframes, win_size) raw track over the frame axis.
  b. dist_curve_for_track() -- happy path (V-shaped curve, argmin matches reported offset),
     mismatch guard (derived argmin disagrees with SyncNet's own reported offset -- must not
     guess), and the two edge cases (argmin at either boundary -- no neighbor to interpolate).
  c. _load_dist_tracks() -- missing file, malformed (non-list) pickle, and a real round-tripped
     pickle all degrade/return correctly.
  d. measure() end-to-end (subprocess.run + the pickle read both monkeypatched) -- proves the
     wiring: a real activesd.pckl-shaped fixture produces a populated dist_curve for a track whose
     derived offset matches the regex-parsed "AV offset", and a track-count MISMATCH between the
     pickle and the parsed offsets falls back to dist_curve=None for every track (never guesses).
"""
import pathlib
import pickle
import sys
import types


_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_measure  # noqa: E402


def _v_shaped_track(win: int = 7, nframes: int = 5, minidx: int = 3, base: float = 10.0):
    """Build a fake (nframes, win) raw per-frame distance track whose per-shift MEAN is a clean
    V-shape with its minimum at `minidx` -- mirrors the real SyncNet mean-distance curve shape
    verified live on the stream box (2026-08-01, real diag.mp4 tracks)."""
    row = [base + abs(i - minidx) for i in range(win)]
    return [list(row) for _ in range(nframes)]  # identical rows -> mean == row, exactly


class TestMeanShiftCurve:
    def test_averages_over_frame_axis(self):
        track = [[1, 2, 3], [3, 2, 1], [2, 2, 2]]
        assert av_sync_measure._mean_shift_curve(track) == [2.0, 2.0, 2.0]

    def test_empty_track_returns_empty_curve(self):
        assert av_sync_measure._mean_shift_curve([]) == []

    def test_returns_plain_python_floats_not_numpy(self):
        # #917: must be JSON-serializable -- a numpy.float64 leaking through would blow up
        # log_calibration_window()'s json.dumps().
        track = [[1, 2, 3]]
        curve = av_sync_measure._mean_shift_curve(track)
        assert all(type(x) is float for x in curve)


class TestDistCurveForTrack:
    def test_happy_path_v_shape_matches_reported_offset(self):
        # vshift = (7-1)//2 = 3; minidx=2 -> derived_offset = 3-2 = 1
        track = _v_shaped_track(win=7, minidx=2)
        curve = av_sync_measure.dist_curve_for_track(track, offset_frames=1)
        mean = av_sync_measure._mean_shift_curve(track)
        assert curve == [mean[1], mean[2], mean[3]]
        # genuine V-shape: center strictly lower than both neighbors
        assert curve[1] < curve[0]
        assert curve[1] < curve[2]

    def test_mismatched_offset_returns_none_never_guesses(self):
        track = _v_shaped_track(win=7, minidx=2)  # derives to offset 1
        assert av_sync_measure.dist_curve_for_track(track, offset_frames=99) is None

    def test_argmin_at_left_edge_returns_none(self):
        track = _v_shaped_track(win=7, minidx=0)  # vshift=3 -> derived offset = 3
        assert av_sync_measure.dist_curve_for_track(track, offset_frames=3) is None

    def test_argmin_at_right_edge_returns_none(self):
        track = _v_shaped_track(win=7, minidx=6)  # vshift=3 -> derived offset = -3
        assert av_sync_measure.dist_curve_for_track(track, offset_frames=-3) is None

    def test_too_short_curve_returns_none(self):
        track = [[1.0, 2.0]]  # win=2 < 3 -- no 3-point window possible
        assert av_sync_measure.dist_curve_for_track(track, offset_frames=0) is None


class TestLoadDistTracks:
    def test_missing_file_returns_none(self, tmp_path):
        assert av_sync_measure._load_dist_tracks(tmp_path, "m") is None

    def test_malformed_non_list_pickle_returns_none(self, tmp_path):
        p = tmp_path / "pywork" / "m"
        p.mkdir(parents=True)
        with open(p / "activesd.pckl", "wb") as f:
            pickle.dump({"not": "a list"}, f)
        assert av_sync_measure._load_dist_tracks(tmp_path, "m") is None

    def test_real_pickle_round_trips(self, tmp_path):
        p = tmp_path / "pywork" / "m"
        p.mkdir(parents=True)
        tracks = [_v_shaped_track(minidx=2), _v_shaped_track(minidx=4)]
        with open(p / "activesd.pckl", "wb") as f:
            pickle.dump(tracks, f)
        loaded = av_sync_measure._load_dist_tracks(tmp_path, "m")
        assert loaded == tracks


class TestMeasureWiresDistCurve:
    """measure() end-to-end: subprocess.run() is monkeypatched (module-level `run`); the pickle
    is written to the real workdir path measure() reads from (no monkeypatch needed for that
    part -- it exercises the real _load_dist_tracks() against a real temp file)."""

    def _fake_run(self, offsets_and_confs):
        lines = "".join(
            f"AV offset: \t{off}\nConfidence: \t{conf}\n" for off, conf in offsets_and_confs
        )
        return lambda cmd, **kw: types.SimpleNamespace(returncode=0, stdout=lines, stderr="")

    def test_matching_track_count_produces_curve_for_matching_track(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "run", self._fake_run([(1, 8.0), (3, 5.0)]))
        pywork = tmp_path / "pywork" / "m"
        pywork.mkdir(parents=True)
        track0 = _v_shaped_track(win=7, minidx=2)  # derives to offset 1 -> matches
        track1 = _v_shaped_track(win=7, minidx=0)  # derives to offset 3, but argmin at edge -> None
        with open(pywork / "activesd.pckl", "wb") as f:
            pickle.dump([track0, track1], f)

        tracks = av_sync_measure.measure(pathlib.Path("unused-repo"), tmp_path / "clip.mp4", tmp_path)

        assert tracks[0][0] == 1 and tracks[0][1] == 8.0
        assert tracks[0][2] is not None and len(tracks[0][2]) == 3
        assert tracks[1][0] == 3 and tracks[1][1] == 5.0
        assert tracks[1][2] is None  # edge-argmin track -- no curve, never guessed

    def test_track_count_mismatch_falls_back_to_none_for_all(self, monkeypatch, tmp_path):
        # Regex parses 2 offsets, but the pickle only has 1 track -- must not misalign.
        monkeypatch.setattr(av_sync_measure, "run", self._fake_run([(1, 8.0), (3, 5.0)]))
        pywork = tmp_path / "pywork" / "m"
        pywork.mkdir(parents=True)
        with open(pywork / "activesd.pckl", "wb") as f:
            pickle.dump([_v_shaped_track(win=7, minidx=2)], f)

        tracks = av_sync_measure.measure(pathlib.Path("unused-repo"), tmp_path / "clip.mp4", tmp_path)

        assert [t[2] for t in tracks] == [None, None]

    def test_missing_pickle_falls_back_to_none_for_all(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "run", self._fake_run([(2, 7.0)]))
        tracks = av_sync_measure.measure(pathlib.Path("unused-repo"), tmp_path / "clip.mp4", tmp_path)
        assert tracks == [(2, 7.0, None)]
