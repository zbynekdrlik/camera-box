"""#805 — unit tests for scripts/av_sync_measure.py's `--calibration-log` addition: the JSONL
producer that feeds `av_sync_calibrate.py --calibrate`'s N-window baseline aggregation.

Covers, with NO real syncnet_python / ffmpeg (measure() is monkeypatched):
  a. log_calibration_window() -- appends one JSON line per call (JSONL, not a JSON array).
  b. one_measurement() usable window -- logs {offset_frames, confidence, usable: True}.
  c. one_measurement() unmeasurable window -- logs the best track with usable: False (still
     recorded so the operator can see how many windows were skipped, even though
     aggregate_syncnet_windows() will filter it out by confidence anyway).
  d. no --calibration-log -- writes nothing (default: unchanged prior behavior).
  e. multiple calls append, never overwrite.

#917: `measure()`'s contract is now 3-tuples `(offset_frames, confidence, dist_curve)` -- every
monkeypatched `measure()` below returns `dist_curve=None` (today's behavior when SyncNet's own
per-shift curve is unavailable/not modeled here); see
`test_av_sync_measure_dist_curve_917.py` for the dist_curve extraction + pass-through coverage.
"""
import json
import pathlib
import sys
import types

import pytest


_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_measure  # noqa: E402


@pytest.fixture(autouse=True)
def _stub_alert_delivery(monkeypatch):
    """#1207: one_measurement() now DELIVERS a `🎯 AV-sync watchdog` alert on the default (no
    --webhook) path via airuleset notify whenever `|offset| >= threshold_ms`. Several tests here
    use `webhook=None` with a suprathreshold offset (e.g. 3 frames = 120ms >= the 60ms threshold),
    so without this stub they would fire a REAL notify subprocess. These tests exercise the JSONL
    calibration log ONLY, so stub the delivery seam to a no-op — delivery has its own coverage in
    tests/python/test_av_sync_measure_notify_dedup_1207.py."""
    monkeypatch.setattr(av_sync_measure, "deliver_alert", lambda *a, **k: None)


def _args(tmp_path, calibration_log=None):
    media = tmp_path / "clip.mp4"
    media.write_bytes(b"fake")
    return types.SimpleNamespace(
        media=str(media), grab=None, secs=20, webhook=None, threshold_ms=60,
        calibration_log=calibration_log,
    )


class TestLogCalibrationWindow:
    def test_appends_jsonl_not_a_json_array(self, tmp_path):
        p = tmp_path / "calib.jsonl"
        av_sync_measure.log_calibration_window(str(p), {"a": 1})
        av_sync_measure.log_calibration_window(str(p), {"a": 2})
        lines = p.read_text().strip().splitlines()
        assert len(lines) == 2
        assert json.loads(lines[0]) == {"a": 1}
        assert json.loads(lines[1]) == {"a": 2}


class TestOneMeasurementCalibrationLogging:
    def test_usable_window_appends_record(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(3, 8.5, None)])
        log_path = tmp_path / "calib.jsonl"
        rc = av_sync_measure.one_measurement(_args(tmp_path, str(log_path)), tmp_path)
        assert rc == 0

        rec = json.loads(log_path.read_text().strip())
        assert rec["offset_frames"] == 3
        assert rec["confidence"] == 8.5
        assert rec["usable"] is True
        assert "ts" in rec
        assert "dist_curve" not in rec  # #917: absent when measure() has no real curve

    def test_unmeasurable_window_appends_record_marked_unusable(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(1, 1.0, None)])
        log_path = tmp_path / "calib.jsonl"
        rc = av_sync_measure.one_measurement(_args(tmp_path, str(log_path)), tmp_path)
        assert rc == 2

        rec = json.loads(log_path.read_text().strip())
        assert rec["usable"] is False
        assert rec["confidence"] == 1.0

    def test_no_calibration_log_flag_writes_nothing(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(3, 8.5, None)])
        av_sync_measure.one_measurement(_args(tmp_path, None), tmp_path)
        assert not (tmp_path / "calib.jsonl").exists()

    def test_appends_across_multiple_calls(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(2, 9.0, None)])
        log_path = tmp_path / "calib.jsonl"
        av_sync_measure.one_measurement(_args(tmp_path, str(log_path)), tmp_path)
        av_sync_measure.one_measurement(_args(tmp_path, str(log_path)), tmp_path)
        assert len(log_path.read_text().strip().splitlines()) == 2

    def test_usable_window_with_dist_curve_includes_it_in_record(self, monkeypatch, tmp_path):
        """#917: when measure() returns a real dist_curve, it lands in the calibration record
        under the exact key av_sync_calibrate.py's window_offset_ms() reads (record.get)."""
        curve = [8.482, 7.813, 8.523]
        monkeypatch.setattr(
            av_sync_measure, "measure", lambda repo, media, workdir: [(3, 8.5, curve)]
        )
        log_path = tmp_path / "calib.jsonl"
        rc = av_sync_measure.one_measurement(_args(tmp_path, str(log_path)), tmp_path)
        assert rc == 0

        rec = json.loads(log_path.read_text().strip())
        assert rec["dist_curve"] == curve
