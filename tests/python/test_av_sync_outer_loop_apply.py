"""#806 — unit tests for scripts/av_sync_measure.py's outer-loop wiring: the state persistence
helpers, apply_outer_bias()'s verify+rollback over the SetAsrcOuterBiasPpm/GetAsrcOuterBiasPpm
obs-websocket requests, and one_measurement()/run_outer_loop()'s end-to-end call.

Covers, with NO real syncnet_python / ffmpeg / obs-websocket (measure() and _conn/_rpc are
monkeypatched, mirroring tests/python/test_av_sync_calibrate.py's FakeObs pattern):
  a. default_outer_loop_state_path() -- PROGRAMDATA vs home fallback.
  b. load_outer_loop_guard() -- missing/corrupt file starts fresh (bias 0); a valid file restores
     the persisted bias_ppm.
  c. save_outer_loop_state() -- atomic write, round-trips through load_outer_loop_guard().
  d. apply_outer_bias() happy path -- sets + verifies, exactly one SetAsrcOuterBiasPpm call.
  e. apply_outer_bias() verify-failure -- rolls back + fails loud, never half-set (#358 pattern).
  f. run_outer_loop() -- a sub-threshold measurement makes NO obs-websocket call and does not
     touch the state file; a sustained correction applies over WS, persists the new bias, and
     Discord-reports it.
  g. one_measurement() only invokes the outer loop when args.outer_loop is set.
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
from av_sync_outer_loop_guard import OuterLoopGuard, WINDOW_N, STEP_PPM  # noqa: E402


# ---------------------------------------------------------------------------
# (a) default_outer_loop_state_path
# ---------------------------------------------------------------------------

class TestDefaultOuterLoopStatePath:
    def test_uses_programdata_when_set(self, monkeypatch):
        monkeypatch.setenv("PROGRAMDATA", r"C:\ProgramData")
        p = av_sync_measure.default_outer_loop_state_path()
        assert str(p) == str(pathlib.Path(r"C:\ProgramData") / "camera-box" / "asrc-outer-loop-state.json")

    def test_falls_back_to_home_when_unset(self, monkeypatch):
        monkeypatch.delenv("PROGRAMDATA", raising=False)
        p = av_sync_measure.default_outer_loop_state_path()
        assert p == pathlib.Path.home() / ".camera-box" / "asrc-outer-loop-state.json"


# ---------------------------------------------------------------------------
# (b)/(c) load/save state
# ---------------------------------------------------------------------------

class TestLoadSaveOuterLoopState:
    def test_missing_file_starts_fresh(self, tmp_path):
        guard = av_sync_measure.load_outer_loop_guard(tmp_path / "nope.json")
        assert guard.bias_ppm == 0.0

    def test_corrupt_file_starts_fresh(self, tmp_path):
        p = tmp_path / "state.json"
        p.write_text("not json{{{")
        guard = av_sync_measure.load_outer_loop_guard(p)
        assert guard.bias_ppm == 0.0

    def test_save_then_load_round_trips_bias(self, tmp_path):
        p = tmp_path / "state.json"
        guard = OuterLoopGuard.from_bias_ppm(4.5)
        av_sync_measure.save_outer_loop_state(p, guard)
        assert json.loads(p.read_text())["bias_ppm"] == 4.5

        reloaded = av_sync_measure.load_outer_loop_guard(p)
        assert reloaded.bias_ppm == 4.5
        # The window is NOT persisted -- a fresh reload starts with an empty window.
        assert reloaded.observe(1000.0) is None


# ---------------------------------------------------------------------------
# fake OBS-websocket RPC layer (mirrors tests/python/test_av_sync_calibrate.py's FakeObs)
# ---------------------------------------------------------------------------

class FakeAsrcObs:
    """Minimal in-memory OBS-WebSocket stand-in for asrc_outer_bias_ppm on one source."""

    def __init__(self, *, bias_ppm=0.0, readback_override=None):
        self.bias_ppm = bias_ppm
        self._readback_override = readback_override
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False, timeout_s=None):
        self.calls.append((method, dict(params or {})))
        if method == "GetAsrcOuterBiasPpm":
            reported = self._readback_override if self._readback_override is not None else self.bias_ppm
            return {"biasPpm": reported}
        if method == "SetAsrcOuterBiasPpm":
            self.bias_ppm = params["biasPpm"]
            return {}
        return {}

    def set_calls(self):
        return [(m, p) for (m, p) in self.calls if m == "SetAsrcOuterBiasPpm"]


# ---------------------------------------------------------------------------
# (d) apply_outer_bias happy path
# ---------------------------------------------------------------------------

class TestApplyOuterBiasHappyPath:
    def test_sets_and_verifies(self, monkeypatch):
        fake = FakeAsrcObs(bias_ppm=0.0)
        monkeypatch.setattr(av_sync_measure, "_rpc", fake.rpc)
        actual = av_sync_measure.apply_outer_bias(None, "mbc", 0.0, 1.0)
        assert actual == 1.0

        sets = fake.set_calls()
        bias_sets = [p for _, p in sets if p.get("inputName") == "mbc"]
        assert len(bias_sets) == 1, f"expected exactly one apply (no rollback), got {sets}"
        assert bias_sets[0]["biasPpm"] == 1.0


# ---------------------------------------------------------------------------
# (e) apply_outer_bias verify-failure -- rollback + fail loud, never half-set
# ---------------------------------------------------------------------------

class TestApplyOuterBiasRollback:
    def test_readback_mismatch_rolls_back_and_raises(self, monkeypatch):
        fake = FakeAsrcObs(bias_ppm=0.0, readback_override=0.0)
        monkeypatch.setattr(av_sync_measure, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            av_sync_measure.apply_outer_bias(None, "mbc", 0.0, 1.0)

        sets = fake.set_calls()
        bias_sets = [p for _, p in sets if p.get("inputName") == "mbc"]
        assert len(bias_sets) == 2, f"expected apply + rollback, got {sets}"
        assert bias_sets[0]["biasPpm"] == 1.0
        assert bias_sets[1]["biasPpm"] == 0.0, (
            "verify-failure MUST roll back to the pre-change value -- never leave the source "
            "half-set"
        )

    def test_rollback_failure_still_raises_with_warning(self, monkeypatch, capsys):
        # readback_override=99.0 matches NEITHER the new value (1.0) NOR the rollback target
        # (0.0) -- so both the initial verify AND the rollback verify mismatch.
        fake = FakeAsrcObs(bias_ppm=0.0, readback_override=99.0)
        monkeypatch.setattr(av_sync_measure, "_rpc", fake.rpc)

        with pytest.raises(SystemExit):
            av_sync_measure.apply_outer_bias(None, "mbc", 0.0, 1.0)

        captured = capsys.readouterr()
        combined = (captured.out + captured.err).lower()
        assert "warn" in combined or "manual check" in combined


# ---------------------------------------------------------------------------
# (f)/(g) run_outer_loop + one_measurement wiring
# ---------------------------------------------------------------------------

def _args(tmp_path, *, outer_loop=True, state_path=None, offsets_frames=None):
    media = tmp_path / "clip.mp4"
    media.write_bytes(b"fake")
    return types.SimpleNamespace(
        media=str(media), grab=None, secs=20, webhook="https://discord.example/webhook",
        threshold_ms=999999,  # never fires the unrelated --threshold-ms alert in these tests
        calibration_log=None,
        outer_loop=outer_loop,
        outer_loop_state=str(state_path) if state_path else None,
        outer_loop_source="mbc",
        ws_host="10.77.9.204",
        ws_password="",
    )


class TestRunOuterLoopWiring:
    def test_sub_threshold_measurement_makes_no_ws_call_and_no_state_write(self, monkeypatch, tmp_path):
        fake = FakeAsrcObs()
        monkeypatch.setattr(av_sync_measure, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_measure, "_conn", lambda host, password: object())
        state_path = tmp_path / "state.json"

        av_sync_measure.run_outer_loop(_args(tmp_path, state_path=state_path), 10.0)

        assert fake.calls == []
        assert not state_path.exists()

    def test_sustained_correction_applies_persists_and_reports(self, monkeypatch, tmp_path):
        fake = FakeAsrcObs(bias_ppm=0.0)
        monkeypatch.setattr(av_sync_measure, "_rpc", fake.rpc)
        connected = []
        monkeypatch.setattr(av_sync_measure, "_conn", lambda host, password: connected.append((host, password)) or types.SimpleNamespace(close=lambda: None))
        reported = []
        monkeypatch.setattr(av_sync_measure, "notify_discord", lambda webhook, text: reported.append((webhook, text)))

        state_path = tmp_path / "state.json"
        args = _args(tmp_path, state_path=state_path)
        for _ in range(WINDOW_N):
            av_sync_measure.run_outer_loop(args, 60.0)

        assert fake.set_calls(), "expected the sustained 60ms residual to apply a correction"
        assert fake.bias_ppm == STEP_PPM
        assert json.loads(state_path.read_text())["bias_ppm"] == STEP_PPM
        assert connected == [("10.77.9.204", "")]
        assert len(reported) == 1
        assert "outer-loop" in reported[0][1]

    def test_one_measurement_only_runs_outer_loop_when_flag_set(self, monkeypatch, tmp_path):
        monkeypatch.setattr(av_sync_measure, "measure", lambda repo, media, workdir: [(2, 9.0)])
        called = []
        monkeypatch.setattr(av_sync_measure, "run_outer_loop", lambda args, offset_ms: called.append(offset_ms))

        av_sync_measure.one_measurement(_args(tmp_path, outer_loop=False), tmp_path)
        assert called == []

        av_sync_measure.one_measurement(_args(tmp_path, outer_loop=True), tmp_path)
        assert called == [80.0]  # 2 frames * 40ms
