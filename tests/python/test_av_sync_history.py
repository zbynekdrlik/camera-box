"""#1265 -- unit tests for scripts/av_sync_history.py, the append-only per-run A/V-controller
history log the #856 loop-gain lane starts collecting for the (rejected-for-now) Prístup 2 adaptive
slope estimator. It records ONE JSON object per run to ~/.camera-box/av-sync-history.jsonl:
run_id, ts, pin_at_measure, residual_median_ms, residual_spread_ms, proposed_offset_ms, loop_gain,
combined_offset_ms_raw, and EITHER applied_pin (a proceed) OR held+hold_reason (a HOLD).

It reads the two dev1 state files the guard lib already writes (av-sync-residual-last.json for the
measured state, av-sync-last.json for the applied pin) so the record is complete, and appends
atomically. Runs inside the cleanup() EXIT trap so it must NEVER truncate an existing log, must be
tolerant of a missing dir/file, and must never crash. Pure + tmp-file testable, fully Tier-0.
"""
import json
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_history  # noqa: E402


def _residual_last(run_id="616", pin=913.0, resid=-61.35, spread=36.7, ts=1788390000.0):
    return {
        "run_id": run_id, "ts": ts, "residual_median_ms": resid,
        "residual_spread_ms": spread, "pin_at_measure": pin,
    }


# ---------------------------------------------------------------------------
# build_record -- the pure record builder
# ---------------------------------------------------------------------------

class TestBuildRecord:
    def test_proceed_run_records_applied_pin(self):
        rec = av_sync_history.build_record(
            residual_last=_residual_last(run_id="616", pin=913.0, resid=-61.35),
            last_applied={"applied_latency_ms": 938, "offset_ms": -24.54},
            run_id="616", proposed_offset_ms="-24.54", hold_reason="",
            loop_gain="0.4", combined_offset_ms_raw="-61.35",
        )
        assert rec is not None
        assert rec["run_id"] == "616"
        assert rec["pin_at_measure"] == 913.0
        assert rec["residual_median_ms"] == pytest.approx(-61.35)
        assert rec["residual_spread_ms"] == pytest.approx(36.7)
        assert rec["proposed_offset_ms"] == pytest.approx(-24.54)
        assert rec["loop_gain"] == pytest.approx(0.4)
        assert rec["combined_offset_ms_raw"] == pytest.approx(-61.35)
        assert rec["applied_pin"] == 938
        assert rec["held"] is False
        assert "hold_reason" not in rec
        assert "ts" in rec

    def test_held_run_records_reason_not_applied_pin(self):
        rec = av_sync_history.build_record(
            residual_last=_residual_last(run_id="615", pin=913.0, resid=-61.35),
            last_applied={"applied_latency_ms": 913, "offset_ms": 47.41},
            run_id="615", proposed_offset_ms="-24.54",
            hold_reason="run residual median -61.4ms exceeds the +/-60ms sanity band -- HOLDING",
            loop_gain="0.4", combined_offset_ms_raw="-61.35",
        )
        assert rec is not None
        assert rec["held"] is True
        assert "HOLDING" in rec["hold_reason"]
        assert "applied_pin" not in rec
        # a held run still records what WAS proposed (so the estimator sees the intent)
        assert rec["proposed_offset_ms"] == pytest.approx(-24.54)

    def test_run_id_mismatch_returns_none(self):
        # residual-last.json is a PRIOR run's (this run had no residual -> persist_residual no-op'd);
        # never record a stale line for a run that produced no measurement.
        rec = av_sync_history.build_record(
            residual_last=_residual_last(run_id="OLD"),
            last_applied={"applied_latency_ms": 913},
            run_id="616", proposed_offset_ms="", hold_reason="",
            loop_gain="", combined_offset_ms_raw="",
        )
        assert rec is None

    def test_residual_last_none_returns_none(self):
        rec = av_sync_history.build_record(
            residual_last=None, last_applied={"applied_latency_ms": 913},
            run_id="616", proposed_offset_ms="", hold_reason="",
            loop_gain="", combined_offset_ms_raw="",
        )
        assert rec is None

    def test_no_correction_run_omits_proposed_and_applied(self):
        # combiner refused -> no proposed, not held -> a bare measurement line.
        rec = av_sync_history.build_record(
            residual_last=_residual_last(run_id="616"),
            last_applied=None,
            run_id="616", proposed_offset_ms="", hold_reason="",
            loop_gain="", combined_offset_ms_raw="",
        )
        assert rec is not None
        assert "proposed_offset_ms" not in rec
        assert "applied_pin" not in rec
        assert rec["held"] is False

    def test_missing_spread_is_omitted(self):
        rl = _residual_last()
        del rl["residual_spread_ms"]
        rec = av_sync_history.build_record(
            residual_last=rl, last_applied={"applied_latency_ms": 938},
            run_id="616", proposed_offset_ms="-24.54", hold_reason="",
            loop_gain="0.4", combined_offset_ms_raw="-61.35",
        )
        assert "residual_spread_ms" not in rec


# ---------------------------------------------------------------------------
# append_history -- append-only, one object per line, dir/file tolerant
# ---------------------------------------------------------------------------

class TestAppendHistory:
    def test_creates_missing_dir_and_file(self, tmp_path):
        dest = tmp_path / "does" / "not" / "exist" / "av-sync-history.jsonl"
        av_sync_history.append_history(str(dest), {"run_id": "1", "x": 1})
        assert dest.exists()
        assert dest.read_text().count("\n") == 1

    def test_appends_never_truncates(self, tmp_path):
        dest = tmp_path / "av-sync-history.jsonl"
        av_sync_history.append_history(str(dest), {"run_id": "a"})
        av_sync_history.append_history(str(dest), {"run_id": "b"})
        av_sync_history.append_history(str(dest), {"run_id": "c"})
        lines = dest.read_text().splitlines()
        assert len(lines) == 3
        assert [json.loads(x)["run_id"] for x in lines] == ["a", "b", "c"]

    def test_one_json_object_per_line(self, tmp_path):
        dest = tmp_path / "av-sync-history.jsonl"
        av_sync_history.append_history(str(dest), {"run_id": "616", "pin_at_measure": 913.0})
        line = dest.read_text().strip()
        assert "\n" not in line
        assert json.loads(line)["pin_at_measure"] == 913.0

    def test_none_record_is_a_noop(self, tmp_path):
        dest = tmp_path / "av-sync-history.jsonl"
        av_sync_history.append_history(str(dest), None)
        assert not dest.exists()

    def test_preserves_a_preexisting_line(self, tmp_path):
        dest = tmp_path / "av-sync-history.jsonl"
        dest.write_text(json.dumps({"run_id": "old"}) + "\n")
        av_sync_history.append_history(str(dest), {"run_id": "new"})
        lines = dest.read_text().splitlines()
        assert [json.loads(x)["run_id"] for x in lines] == ["old", "new"]


# ---------------------------------------------------------------------------
# CLI -- reads the two state files, builds + appends, always exits 0
# ---------------------------------------------------------------------------

class TestCli:
    def _write(self, path, obj):
        path.write_text(json.dumps(obj))

    def test_append_from_state_files(self, tmp_path):
        resid = tmp_path / "residual-last.json"
        last = tmp_path / "last.json"
        dest = tmp_path / "history.jsonl"
        self._write(resid, _residual_last(run_id="616", pin=913.0, resid=-61.35))
        self._write(last, {"applied_latency_ms": 938, "offset_ms": -24.54})
        rc = av_sync_history.main([
            "append", "--run-id", "616", "--proposed-offset-ms", "-24.54",
            "--hold-reason", "", "--loop-gain", "0.4", "--combined-offset-ms-raw", "-61.35",
            "--residual-last", str(resid), "--last-applied", str(last), "--dest", str(dest),
        ])
        assert rc == 0
        line = dest.read_text().strip()
        rec = json.loads(line)
        assert rec["run_id"] == "616"
        assert rec["applied_pin"] == 938
        assert rec["loop_gain"] == pytest.approx(0.4)

    def test_append_is_appendonly_across_calls(self, tmp_path):
        resid = tmp_path / "residual-last.json"
        last = tmp_path / "last.json"
        dest = tmp_path / "history.jsonl"
        self._write(last, {"applied_latency_ms": 938})
        for rid in ("616", "617"):
            self._write(resid, _residual_last(run_id=rid))
            av_sync_history.main([
                "append", "--run-id", rid, "--proposed-offset-ms", "-24.54",
                "--hold-reason", "", "--loop-gain", "0.4", "--combined-offset-ms-raw", "-61.35",
                "--residual-last", str(resid), "--last-applied", str(last), "--dest", str(dest),
            ])
        assert len(dest.read_text().splitlines()) == 2

    def test_missing_state_files_never_crash_and_exit_zero(self, tmp_path):
        dest = tmp_path / "history.jsonl"
        rc = av_sync_history.main([
            "append", "--run-id", "616", "--proposed-offset-ms", "",
            "--hold-reason", "", "--loop-gain", "", "--combined-offset-ms-raw", "",
            "--residual-last", str(tmp_path / "nope.json"),
            "--last-applied", str(tmp_path / "nope2.json"), "--dest", str(dest),
        ])
        assert rc == 0
        # no residual-last this run -> no line written
        assert not dest.exists()
