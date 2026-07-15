"""#757 -- unit tests for scripts/prerecord_phase_calibrate.py, the pre-record phase auto-pin
reconstructor: turns a `genlock-jitter-report --json` snapshot into per-camera measured cam->
strih latencies, re-keyed under each target host's own source-name template.

Covers, with NO live OBS/rig:
  a. measured_by_camera() -- reconstructs latency_ms + mean_head_skew_ms per "NDI cam<N>"
     source; skips non-strih-shaped names, missing/non-numeric fields, and non-dict values.
  b. source_names_by_template() -- pure re-keying under a host's naming convention.
  c. main() CLI wiring -- writes the strih file always; writes imag-main/imag-mv files only
     when requested; exits 1 (writes nothing) when the jitter JSON has no usable cameras.
"""
import json
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import prerecord_phase_calibrate as ppc  # noqa: E402


# ---------------------------------------------------------------------------
# measured_by_camera -- pure
# ---------------------------------------------------------------------------

class TestMeasuredByCamera:
    def test_reconstructs_latency_plus_signed_skew_per_camera(self):
        jitter = {
            "NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 12.5, "samples": 4},
            "NDI cam3": {"latency_ms": 18, "mean_head_skew_ms": -2.0, "samples": 2},
        }
        out = ppc.measured_by_camera(jitter)
        assert out == {1: 15.5, 3: 16.0}

    def test_skips_a_source_name_that_is_not_ndi_cam_n(self):
        jitter = {
            "NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 1.0},
            "MV NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 99.0},  # not the main name
            "NDI 2ME PGM": {"latency_ms": 500, "mean_head_skew_ms": 5.0},  # stream, not a cam
            "garbage": {"latency_ms": 1, "mean_head_skew_ms": 1},
        }
        out = ppc.measured_by_camera(jitter)
        assert out == {1: 4.0}

    def test_skips_a_source_missing_latency_ms_or_mean_head_skew_ms(self):
        jitter = {
            "NDI cam1": {"mean_head_skew_ms": 1.0},  # no latency_ms
            "NDI cam2": {"latency_ms": 5},  # no mean_head_skew_ms
            "NDI cam3": {"latency_ms": 5, "mean_head_skew_ms": None},  # non-numeric
            "NDI cam4": {"latency_ms": "5", "mean_head_skew_ms": 1.0},  # non-numeric string
            "NDI cam5": {"latency_ms": 5, "mean_head_skew_ms": 1.0},  # the only valid one
        }
        out = ppc.measured_by_camera(jitter)
        assert out == {5: 6.0}

    def test_skips_a_non_dict_value(self):
        jitter = {"NDI cam1": "not-a-dict", "NDI cam2": {"latency_ms": 3, "mean_head_skew_ms": 0.0}}
        assert ppc.measured_by_camera(jitter) == {2: 3.0}

    def test_rejects_a_boolean_masquerading_as_numeric(self):
        # bool is a subclass of int in Python -- must not silently pass isinstance(..., (int, float))
        jitter = {"NDI cam1": {"latency_ms": True, "mean_head_skew_ms": 1.0}}
        assert ppc.measured_by_camera(jitter) == {}

    def test_empty_or_malformed_input_returns_empty_dict_never_raises(self):
        assert ppc.measured_by_camera({}) == {}
        assert ppc.measured_by_camera(None) == {}
        assert ppc.measured_by_camera([1, 2, 3]) == {}

    def test_handles_all_seven_cameras(self):
        jitter = {
            f"NDI cam{n}": {"latency_ms": n, "mean_head_skew_ms": 0.5}
            for n in range(1, 8)
        }
        out = ppc.measured_by_camera(jitter)
        assert out == {n: n + 0.5 for n in range(1, 8)}


# ---------------------------------------------------------------------------
# source_names_by_template -- pure
# ---------------------------------------------------------------------------

class TestSourceNamesByTemplate:
    def test_strih_main_template(self):
        assert ppc.source_names_by_template({1: 15.5, 3: 16.0}, "NDI cam{n}") == {
            "NDI cam1": 15.5,
            "NDI cam3": 16.0,
        }

    def test_imag_main_and_mv_templates_differ_by_key_only(self):
        by_cam = {2: 20.0}
        assert ppc.source_names_by_template(by_cam, "NDI CAM{n}") == {"NDI CAM2": 20.0}
        assert ppc.source_names_by_template(by_cam, "MV CAM{n}") == {"MV CAM2": 20.0}

    def test_empty_input_produces_empty_output(self):
        assert ppc.source_names_by_template({}, "NDI cam{n}") == {}


# ---------------------------------------------------------------------------
# main() -- CLI wiring
# ---------------------------------------------------------------------------

class TestMainCli:
    def test_writes_the_strih_file(self, tmp_path):
        jitter_path = tmp_path / "jitter.json"
        jitter_path.write_text(json.dumps({
            "NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 1.0},
            "NDI cam4": {"latency_ms": 47, "mean_head_skew_ms": -3.0},
        }))
        out_path = tmp_path / "strih-measured.json"
        rc = ppc.main(["--jitter-json", str(jitter_path), "--out", str(out_path)])
        assert rc == 0
        written = json.loads(out_path.read_text())
        assert written == {"NDI cam1": 4.0, "NDI cam4": 44.0}

    def test_writes_imag_main_and_mv_only_when_requested(self, tmp_path):
        jitter_path = tmp_path / "jitter.json"
        jitter_path.write_text(json.dumps({
            "NDI cam2": {"latency_ms": 14, "mean_head_skew_ms": 0.0},
        }))
        out_path = tmp_path / "strih.json"
        imag_main = tmp_path / "imag-main.json"
        imag_mv = tmp_path / "imag-mv.json"
        rc = ppc.main([
            "--jitter-json", str(jitter_path), "--out", str(out_path),
            "--imag-main-out", str(imag_main), "--imag-mv-out", str(imag_mv),
        ])
        assert rc == 0
        assert json.loads(imag_main.read_text()) == {"NDI CAM2": 14.0}
        assert json.loads(imag_mv.read_text()) == {"MV CAM2": 14.0}

    def test_imag_outputs_are_not_written_when_not_requested(self, tmp_path):
        jitter_path = tmp_path / "jitter.json"
        jitter_path.write_text(json.dumps({"NDI cam1": {"latency_ms": 3, "mean_head_skew_ms": 0.0}}))
        out_path = tmp_path / "strih.json"
        imag_main = tmp_path / "imag-main.json"
        rc = ppc.main(["--jitter-json", str(jitter_path), "--out", str(out_path)])
        assert rc == 0
        assert not imag_main.exists()

    def test_no_usable_cameras_returns_1_and_writes_nothing(self, tmp_path, capsys):
        jitter_path = tmp_path / "jitter.json"
        jitter_path.write_text(json.dumps({"NDI 2ME PGM": {"latency_ms": 500, "mean_head_skew_ms": 1.0}}))
        out_path = tmp_path / "strih.json"
        rc = ppc.main(["--jitter-json", str(jitter_path), "--out", str(out_path)])
        assert rc == 1
        assert not out_path.exists()
        captured = capsys.readouterr()
        assert "no usable" in captured.err
