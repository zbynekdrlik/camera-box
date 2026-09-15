"""#1168 -- the arrival-floor sufficiency predicate + `--floor-samples-ok` CLI mode.

Root cause: `arrival_floors_from_jitter` DROPS a source whose explicit `samples < MIN_FLOOR_SAMPLES`
(a phantom floor one source-frame off), and align() then falls back to the budget-UNCHECKED plan when
a FASTER camera lacks a floor. Run 34973535496: cam4 had only 2 audit samples in ONE 12 s window --
a TRANSIENT thin window, not a genuinely-missing floor -- so the budget check was skipped exactly on
the run that needed it. qr-align.sh must re-fetch the post-reset audit ONCE more before letting that
fallback fire; `floor_samples_sufficient` is the pure predicate that decides whether a re-fetch is
still needed (Tier-0: pytest, no rig).
"""
import json
import pathlib
import subprocess
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

SRC = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]


def _jitter(samples_by_src):
    """A genlock-jitter-report --json dict carrying the given per-source sample counts (a None value
    OMITS the samples key = an older/partial report; absence from the dict = the source never
    appeared)."""
    out = {}
    for s, n in samples_by_src.items():
        entry = {"latency_ms": 3, "mean_head_skew_ms": 60.0}
        if n is not None:
            entry["samples"] = n
        out[s] = entry
    return out


class TestFloorSamplesSufficient:
    def test_all_sources_well_sampled_is_sufficient(self):
        jj = _jitter({s: 3 for s in SRC})
        assert qa.floor_samples_sufficient(jj, SRC) is True

    def test_a_source_below_min_samples_is_insufficient(self):
        # the run 34973535496 shape: cam4 with only 2 audit samples -> its floor is a phantom qa
        # drops -> the budget-unchecked fallback. NOT sufficient -> re-fetch.
        jj = _jitter({"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3, "NDI cam4": 2})
        assert qa.floor_samples_sufficient(jj, SRC) is False

    def test_a_missing_source_is_insufficient(self):
        jj = _jitter({"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3})  # cam4 absent
        assert qa.floor_samples_sufficient(jj, SRC) is False

    def test_a_missing_samples_count_is_trusted_sufficient(self):
        # a MISSING samples count is trusted (only an explicit low count is the known phantom),
        # mirroring arrival_floors_from_jitter's own semantics -- so it is sufficient, no re-fetch.
        jj = _jitter({"NDI cam1": 3, "NDI cam2": None, "NDI cam3": 3, "NDI cam4": 3})
        assert qa.floor_samples_sufficient(jj, SRC) is True


class TestFloorSamplesOkCli:
    def _run(self, tmp_path, jj):
        p = tmp_path / "jitter.json"
        p.write_text(json.dumps(jj), encoding="utf-8")
        return subprocess.run(
            [sys.executable, str(_SCRIPTS / "qr_align_pins.py"), "--floor-samples-ok",
             "--host", "x", "--sources", ",".join(SRC), "--jitter-json", str(p)],
            capture_output=True, text=True)

    def test_cli_exit_0_when_sufficient(self, tmp_path):
        r = self._run(tmp_path, _jitter({s: 3 for s in SRC}))
        assert r.returncode == 0, r.stderr

    def test_cli_exit_1_when_a_source_is_short(self, tmp_path):
        r = self._run(tmp_path, _jitter({"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 3, "NDI cam4": 2}))
        assert r.returncode == 1, r.stderr
