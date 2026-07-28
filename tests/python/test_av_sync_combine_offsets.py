"""#856 -- unit tests for scripts/av_sync_combine_offsets.py: combining `all_cambox_av_sync`'s
per-camera measurements into ONE rig-wide A/V correction to feed `av_sync_calibrate.py --apply`.

See that script's own module docstring for the design rationale (recorded on the #856 issue
BEFORE this code was written): median across camera entries whose verdict is EXACTLY
"measured" (never "derived"/"unknown"/"excluded"), fail-closed when fewer than
`MIN_MEASURED_CAMS` qualify or the qualifying offsets span more than `MAX_SPREAD_MS` (a spread
that wide no longer looks like one shared-path rig-wide constant).

Covers, with NO live OBS/network:
  a. measured_offsets() -- extracts ONLY verdict=="measured" (camera, offset_ms) pairs, ignoring
     "derived"/"unknown"/"excluded" cameras and the block's own meta keys.
  b. combine() -- the real #856 issue's own 2026-07-28 fused-run numbers produce the expected
     median; too few measured cameras refuses; too-wide a spread refuses even with enough
     cameras; a stray non-null av_offset_ms on a non-measured verdict never leaks in.
  c. CLI -- prints the offset and exits 0 on success; exits 2 with no `all_cambox_av_sync`
     object at all, and on a refusal (never prints a guessed number).
"""
import json
import pathlib
import sys

import pytest

# av_sync_combine_offsets.py has no OBS dependency of its own, but keep the same sys.path
# convention as every sibling test in this directory (test_av_sync_calibrate.py etc.).
_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_combine_offsets as combiner  # noqa: E402


def _entry(offset_ms, verdict="measured"):
    return {"verdict": verdict, "av_offset_ms": offset_ms}


# ---------------------------------------------------------------------------
# (a) measured_offsets -- only verdict=="measured" entries, never the meta keys
# ---------------------------------------------------------------------------

class TestMeasuredOffsets:
    def test_extracts_only_measured_cameras(self):
        av = {
            "cam1": _entry(-269.35),
            "cam2": _entry(-287.67),
            "cam3": _entry(None, verdict="derived"),
            "cam5": _entry(None, verdict="excluded"),
            "expected_ms": 0.0,
            "gate_tolerance_ms": 20.0,
            "gate_pass": False,
            "gate": "enforced",
        }
        pairs = combiner.measured_offsets(av)
        assert dict(pairs) == {"cam1": -269.35, "cam2": -287.67}

    def test_ignores_non_dict_meta_keys_without_crashing(self):
        av = {"expected_ms": 0.0, "gate_pass": False, "gate": "enforced"}
        assert combiner.measured_offsets(av) == []

    def test_a_measured_entry_missing_av_offset_ms_is_skipped(self):
        # defensive: verdict=="measured" is SUPPOSED to always carry a real av_offset_ms
        # (recording-verdict's own invariant), but never trust that blindly downstream.
        av = {"cam1": {"verdict": "measured"}}
        assert combiner.measured_offsets(av) == []


# ---------------------------------------------------------------------------
# (b) combine -- the real decision, fail-closed on degenerate measurement
# ---------------------------------------------------------------------------

class TestCombine:
    def test_real_run_measurement_produces_the_median(self):
        # #856 issue's own 2026-07-28 fused-run numbers (run 30397575734, verdict
        # /tmp/recording-e2e-1290405279/verdict-1290405279.json).
        av = {
            "cam1": _entry(-269.35),
            "cam2": _entry(-287.67),
            "cam3": _entry(-277.21),
            "cam4": _entry(-297.56),
            "cam5": _entry(None, verdict="excluded"),
            "cam6": _entry(None, verdict="excluded"),
            "cam7": _entry(None, verdict="excluded"),
        }
        offset, cams_used = combiner.combine(av)
        # sorted: -297.56, -287.67, -277.21, -269.35 -- median of 4 = mean of the two middle
        assert offset == pytest.approx((-287.67 + -277.21) / 2, abs=0.001)
        assert cams_used == ["cam1", "cam2", "cam3", "cam4"]

    def test_too_few_measured_cameras_refuses(self):
        av = {"cam1": _entry(-269.35), "cam2": _entry(None, verdict="unknown")}
        offset, reasons = combiner.combine(av)
        assert offset is None
        assert any("measured" in r for r in reasons)

    def test_zero_measured_cameras_refuses(self):
        av = {"cam5": _entry(None, verdict="excluded")}
        offset, reasons = combiner.combine(av)
        assert offset is None
        assert reasons

    def test_wide_spread_refuses_even_with_enough_cameras(self):
        av = {"cam1": _entry(-10.0), "cam2": _entry(200.0), "cam3": _entry(-5.0)}
        offset, reasons = combiner.combine(av)
        assert offset is None
        assert any("spread" in r for r in reasons)

    def test_derived_and_excluded_never_contribute_even_if_offset_present(self):
        # a stray non-null av_offset_ms on a non-"measured" verdict must never leak in --
        # #714's derived estimate is built FROM cam2's own measured offset, so folding it back
        # in would double-count one real measurement (see the #856 design rationale).
        av = {
            "cam1": _entry(-269.35),
            "cam2": _entry(-287.67),
            "cam5": {"verdict": "excluded", "av_offset_ms": None},
            "cam6": {"verdict": "derived", "av_offset_ms": -999.0, "derived_offset_ms": -999.0},
        }
        offset, cams_used = combiner.combine(av)
        assert cams_used == ["cam1", "cam2"]
        assert offset == pytest.approx((-269.35 + -287.67) / 2, abs=0.001)

    def test_custom_min_cams_and_spread_are_honored(self):
        av = {"cam1": _entry(-10.0), "cam2": _entry(10.0)}
        # default min_cams=2 passes; raising it to 3 must refuse.
        offset, _ = combiner.combine(av, min_cams=3)
        assert offset is None
        # default max_spread_ms=100 passes (spread=20); tightening it must refuse.
        offset, _ = combiner.combine(av, max_spread_ms=10.0)
        assert offset is None


# ---------------------------------------------------------------------------
# (c) CLI -- reads a real verdict JSON file, prints the offset or refuses loudly
# ---------------------------------------------------------------------------

class TestCLI:
    def test_prints_offset_and_exits_cleanly_on_success(self, tmp_path, capsys, monkeypatch):
        verdict = {
            "all_cambox_av_sync": {
                "cam1": _entry(-269.35),
                "cam2": _entry(-287.67),
                "cam3": _entry(-277.21),
                "cam4": _entry(-297.56),
            }
        }
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps(verdict))
        monkeypatch.setattr(sys, "argv", ["av_sync_combine_offsets.py", "--verdict-json", str(p)])
        combiner.main()
        out = capsys.readouterr().out.strip()
        assert float(out) < 0

    def test_missing_all_cambox_av_sync_exits_2(self, tmp_path, monkeypatch):
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps({"some_other_key": 1}))
        monkeypatch.setattr(sys, "argv", ["av_sync_combine_offsets.py", "--verdict-json", str(p)])
        with pytest.raises(SystemExit) as exc:
            combiner.main()
        assert exc.value.code == 2

    def test_refusal_exits_2_and_prints_nothing_on_stdout(self, tmp_path, capsys, monkeypatch):
        verdict = {"all_cambox_av_sync": {"cam1": _entry(-10.0)}}  # only 1 measured cam
        p = tmp_path / "verdict.json"
        p.write_text(json.dumps(verdict))
        monkeypatch.setattr(sys, "argv", ["av_sync_combine_offsets.py", "--verdict-json", str(p)])
        with pytest.raises(SystemExit) as exc:
            combiner.main()
        assert exc.value.code == 2
        assert capsys.readouterr().out.strip() == ""
