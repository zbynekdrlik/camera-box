"""#1333 (bod 4) — unit tests for the stream-box A/V correction SPLIT: a frame-quantized,
phase-snapped video pin + a continuous `mbc` audio sync offset (scripts/av_sync_calibrate.py).

Root cause (Nález 1-4 on the ticket): `required_delay_ms` wrote `genlock_latency_ms_src` as an
ARBITRARY integer ms; the vendored FIFO holds video frame-quantized (hold = ceil(pin/33.333)), so a
sub-frame pin with frac(pin/33.333) < 0.5 toggles the hold 29/30 frames (±33 ms limit cycle) and the
sub-frame A/V remainder has no actuator at all. The fix splits the correction:
  * whole frames -> the pin, ALWAYS phase-snapped (deterministic ceil-hold), step/hw clamps kept;
  * the sub-frame remainder -> the `mbc` audio sync offset (OBS SetInputAudioSyncOffset, ms).

Sign convention (matches src/av_window.rs + required_delay_ms):
  residual_ms = video_time - audio_time; residual > 0 => video LAGS audio.
  * pin: residual > 0 => pin DOWN (video presented earlier).
  * audio: OBS sync offset POSITIVE DELAYS audio; residual (remainder) > 0 => video still lags =>
    DELAY audio => offset INCREASES.

No live OBS: split_av_correction is pure; the apply path is driven through a fake OBS-WebSocket RPC
layer (same convention as tests/python/test_av_sync_calibrate.py's FakeObs).
"""
import json
import math
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import av_sync_calibrate  # noqa: E402
import av_sync_apply_guard  # noqa: E402


FRAME = av_sync_calibrate.STREAM_FRAME_MS  # 1000/30, the 30fps program grid


def _frac(pin_ms):
    return (float(pin_ms) / FRAME) % 1.0


# ---------------------------------------------------------------------------
# (a) split_av_correction -- the PURE split
# ---------------------------------------------------------------------------

class TestSplitAvCorrection:
    def test_frac_safe_small_residual_leaves_pin_and_sends_remainder_to_audio(self):
        # residual +20 (< one frame after gain), a frac-safe current pin -> the pin does NOT move
        # (round(gain*20/frame)=0 frames) and the whole damped residual goes to audio: 0.4*20 = +8.
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=20.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert pin == 987, "a frac-safe pin with a sub-frame correction must not move"
        assert audio == pytest.approx(8.0), "the whole gain*residual remainder -> audio"
        assert diag["frames"] == 0

    def test_residual_one_frame_moves_pin_down_snapped_audio_takes_remainder(self):
        # residual +60: round(0.4*60/33.333) = round(0.72) = 1 frame -> pin DOWN one frame, snapped
        # phase-safe; the sub-frame remainder (residual minus the whole-frame video shift) -> audio.
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=60.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert diag["frames"] == 1
        assert pin < 987, "positive residual (video lags) must move the pin DOWN"
        assert _frac(pin) >= 0.5, "the written pin must be phase-safe (never the prone < 0.5 band)"
        # video moved ONE frame earlier -> residual_eff = 60 - 33.333 = 26.667; audio = 0.4*26.667.
        expected_eff = 60.0 - FRAME
        assert audio == pytest.approx(0.4 * expected_eff, abs=0.6)
        assert audio == pytest.approx(diag["gain"] * diag["residual_eff_ms"])

    def test_prone_current_pin_is_never_written_even_at_zero_residual(self):
        # A currently-prone pin (974, frac 0.22) is snapped to phase-safe on the very next apply even
        # with a ZERO A/V residual -- the first-run snap that removes the 29/30 limit cycle. Snapping
        # within the same ceil-hold bucket does NOT shift the video, so audio must stay put.
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=0.0, current_pin_ms=974, current_audio_offset_ms=0.0, gain=0.4)
        assert pin != 974
        assert _frac(pin) >= 0.5
        assert _frac(974) < 0.5, "guard: 974 really is prone (this test would be vacuous otherwise)"
        # 974 and its snap both ceil to the same hold depth -> no video shift -> no audio move.
        assert math.ceil(pin / FRAME) == math.ceil(974 / FRAME)
        assert audio == pytest.approx(0.0)

    def test_sign_convention_negative_residual_pins_up_and_advances_audio(self):
        # residual -20 (video leads audio): sub-frame -> pin unchanged, audio -8 (advance audio).
        pin, audio, _ = av_sync_calibrate.split_av_correction(
            residual_ms=-20.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert pin == 987
        assert audio == pytest.approx(-8.0)

    def test_sign_convention_negative_one_frame_pins_up(self):
        # residual -60: pin UP one frame (video presented later), audio takes the negative remainder.
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=-60.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert diag["frames"] == -1
        assert pin > 987, "negative residual (video leads) must move the pin UP"
        assert _frac(pin) >= 0.5
        assert audio == pytest.approx(0.4 * (-60.0 + FRAME), abs=0.6)
        assert audio < 0.0

    def test_audio_offset_accumulates_on_the_current_offset(self):
        # The audio offset is a PERSISTENT absolute value: a new correction ADDS to the current one.
        pin, audio, _ = av_sync_calibrate.split_av_correction(
            residual_ms=20.0, current_pin_ms=987, current_audio_offset_ms=15.0, gain=0.4)
        assert pin == 987
        assert audio == pytest.approx(15.0 + 8.0)

    def test_audio_step_clamped_per_run(self):
        # A large residual drives audio past the +/- AV_SYNC_MAX_STEP_MS (50) per-run cap -> clamped.
        _, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=200.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert audio == pytest.approx(float(av_sync_calibrate.AV_SYNC_MAX_STEP_MS))
        assert diag["audio_step_clamped"] is True

    def test_audio_hardware_clamped_to_500ms(self):
        # The persistent audio offset is hardware-clamped to +/- AUDIO_OFFSET_CLAMP_MS (500).
        _, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=60.0, current_pin_ms=987,
            current_audio_offset_ms=av_sync_calibrate.AUDIO_OFFSET_CLAMP_MS - 2, gain=0.4)
        assert audio == pytest.approx(float(av_sync_calibrate.AUDIO_OFFSET_CLAMP_MS))
        assert diag["audio_hw_clamped"] is True

    def test_written_pin_is_always_phase_safe_and_move_is_bounded(self):
        # Across a sweep of currents + residuals, the WRITTEN pin is never in the prone < 0.5 band,
        # and its move stays bounded by the step clamp plus the snap search radius (the documented
        # rare edge where the phase snap wins over the +/-step cap -- phase-safety is this ticket).
        import e2e_measurement_pins as mp
        max_move = av_sync_calibrate.AV_SYNC_MAX_STEP_MS + mp.PHASE_SNAP_MAX_COST_MS
        for cur in (903, 927, 955, 963, 974, 987, 991, 1024):
            for resid in (-80, -33, -10, 0, 10, 33, 80):
                pin, _, _ = av_sync_calibrate.split_av_correction(
                    residual_ms=float(resid), current_pin_ms=cur,
                    current_audio_offset_ms=0.0, gain=0.4)
                assert _frac(pin) >= 0.5, f"prone pin {pin} written for cur={cur} resid={resid}"
                assert abs(pin - cur) <= max_move, f"pin move {abs(pin-cur)} exceeds bound"


# ---------------------------------------------------------------------------
# fake OBS-websocket RPC layer -- pin (genlock_latency_ms_src) + audio (inputAudioSyncOffset)
# ---------------------------------------------------------------------------

class FakeObs:
    def __init__(self, *, latency_ms=987, audio_ms=0, audio_readback_override=None,
                 latency_readback_override=None):
        self.latency_ms = latency_ms
        self.audio_ms = audio_ms
        self._audio_ro = audio_readback_override
        self._lat_ro = latency_readback_override
        self.calls = []

    def rpc(self, ws, method, params=None, ignore_err=False, timeout_s=None):
        self.calls.append((method, dict(params or {})))
        if method == "GetInputSettings":
            reported = self._lat_ro if self._lat_ro is not None else self.latency_ms
            return {"inputSettings": {av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY: reported}}
        if method == "SetInputSettings":
            self.latency_ms = params["inputSettings"][av_sync_calibrate.GENLOCK_SRC_LATENCY_KEY]
            return {}
        if method == "GetInputAudioSyncOffset":
            reported = self._audio_ro if self._audio_ro is not None else self.audio_ms
            return {av_sync_calibrate.AUDIO_SYNC_OFFSET_KEY: reported}
        if method == "SetInputAudioSyncOffset":
            self.audio_ms = params[av_sync_calibrate.AUDIO_SYNC_OFFSET_KEY]
            return {}
        return {}

    def set_latency_calls(self):
        return [p for m, p in self.calls if m == "SetInputSettings"]

    def set_audio_calls(self):
        return [p for m, p in self.calls if m == "SetInputAudioSyncOffset"]


# ---------------------------------------------------------------------------
# (b) read/apply the audio sync offset
# ---------------------------------------------------------------------------

class TestAudioOffsetRW:
    def test_read_current_audio_offset(self, monkeypatch):
        fake = FakeObs(audio_ms=12)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        assert av_sync_calibrate.read_current_audio_offset(None, "mbc") == 12

    def test_read_defaults_to_zero_when_absent(self, monkeypatch):
        def rpc(ws, method, params=None, ignore_err=False, timeout_s=None):
            return {}
        monkeypatch.setattr(av_sync_calibrate, "_rpc", rpc)
        assert av_sync_calibrate.read_current_audio_offset(None, "mbc") == 0

    def test_apply_audio_offset_sets_and_verifies(self, monkeypatch):
        fake = FakeObs(audio_ms=0)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        applied = av_sync_calibrate.apply_audio_offset(None, "mbc", 0, 11)
        assert applied == 11
        sets = fake.set_audio_calls()
        assert len(sets) == 1
        assert sets[0].get("inputName") == "mbc"
        assert sets[0][av_sync_calibrate.AUDIO_SYNC_OFFSET_KEY] == 11

    def test_apply_audio_offset_rollback_on_readback_mismatch(self, monkeypatch):
        fake = FakeObs(audio_ms=0, audio_readback_override=0)  # SET never takes -> mismatch
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        with pytest.raises(SystemExit):
            av_sync_calibrate.apply_audio_offset(None, "mbc", 0, 11)
        sets = fake.set_audio_calls()
        assert len(sets) == 2, "apply + rollback"
        assert sets[0][av_sync_calibrate.AUDIO_SYNC_OFFSET_KEY] == 11
        assert sets[1][av_sync_calibrate.AUDIO_SYNC_OFFSET_KEY] == 0, "must roll back to pre-change"


# ---------------------------------------------------------------------------
# (c) apply path (main --apply) -- writes BOTH pin and audio, rolls back BOTH on failure
# ---------------------------------------------------------------------------

def _split_argv(json_path, extra=None):
    argv = [
        "av_sync_calibrate.py", "--host", "10.77.9.204", "--source", "NDI 2ME PGM",
        "--audio-source", "mbc",
        "--offset-ms", "24.0",          # damped (0.4*60), what recording-e2e passes today
        "--combined-offset-ms", "60.0",  # RAW residual
        "--loop-gain", "0.4",
        "--apply", "--json-path", str(json_path),
    ]
    return argv + (extra or [])


class TestApplyPathSplit:
    def test_apply_writes_both_pin_and_audio_and_persists(self, monkeypatch, tmp_path):
        fake = FakeObs(latency_ms=987, audio_ms=0)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        jp = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(sys, "argv", _split_argv(jp))
        av_sync_calibrate.main()

        # pin: 987 - one frame, snapped phase-safe; audio: the sub-frame remainder (~+11 ms).
        assert fake.latency_ms < 987
        assert _frac(fake.latency_ms) >= 0.5
        assert len(fake.set_audio_calls()) == 1
        assert fake.audio_ms > 0
        data = json.loads(jp.read_text())
        assert data["applied_latency_ms"] == fake.latency_ms
        assert data["audio_offset_ms"] == fake.audio_ms
        assert data["audio_source"] == "mbc"
        assert data["offset_ms"] == 24.0, "the guard's proposed-vs-last still keys on the damped pin offset"

    def test_audio_readback_failure_rolls_back_both(self, monkeypatch, tmp_path):
        # The audio SET never takes -> its own rollback restores the audio AND the pin must be
        # rolled back to its pre-change value too (both-or-neither, never a half-set pair).
        fake = FakeObs(latency_ms=987, audio_ms=0, audio_readback_override=0)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        jp = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(sys, "argv", _split_argv(jp))
        with pytest.raises(SystemExit):
            av_sync_calibrate.main()
        assert fake.latency_ms == 987, "pin must be rolled back to the pre-change value on audio failure"
        assert fake.audio_ms == 0, "audio must be rolled back to the pre-change value"
        assert not jp.exists(), "no last-applied record when the pair failed"

    def test_pin_failure_never_writes_audio(self, monkeypatch, tmp_path):
        # The pin SET never takes -> fail loud BEFORE any audio write is attempted.
        fake = FakeObs(latency_ms=987, audio_ms=0, latency_readback_override=987)
        # readback always 987 -> a SET to a different value mismatches -> apply_latency raises.
        # (the pin genuinely changes because residual moves it a frame; readback_override forces mismatch)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        jp = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(sys, "argv", _split_argv(jp))
        with pytest.raises(SystemExit):
            av_sync_calibrate.main()
        assert fake.set_audio_calls() == [], "no audio write when the pin apply failed"

    def test_legacy_operator_path_writes_pin_only_no_audio(self, monkeypatch, tmp_path):
        # An operator --offset-ms call (NO loop-gain/combined) keeps the old pin-only behavior.
        fake = FakeObs(latency_ms=450, audio_ms=0)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        monkeypatch.setattr(av_sync_calibrate, "_conn", lambda host, password="": None)
        jp = tmp_path / "av-sync-last.json"
        monkeypatch.setattr(
            sys, "argv",
            ["av_sync_calibrate.py", "--host", "10.77.9.204", "--offset-ms", "30.0",
             "--apply", "--json-path", str(jp)],
        )
        av_sync_calibrate.main()
        assert fake.set_audio_calls() == [], "legacy operator path must not touch the audio offset"
        assert len(fake.set_latency_calls()) == 1


# ---------------------------------------------------------------------------
# (d) guard HOLD => no write at all (the real #1265 predicate gates the apply in recording-e2e.sh)
# ---------------------------------------------------------------------------

class TestGuardHoldNoWrite:
    def test_drifting_band_holds_and_gate_skips_both_writes(self, monkeypatch):
        # The real guard predicate returns a HOLD for a DRIFTING band; recording-e2e.sh then clears
        # the offset so the whole apply block (`if [ -n "$AV_SYNC_APPLY_OFFSET_MS" ]`) is skipped ->
        # neither the pin nor the audio is written. We mirror exactly that gate here with the real
        # guard + the real apply functions.
        reason = av_sync_apply_guard.hold_reason(
            residual_median_ms=-40.0, residual_spread_ms=10.0, band_verdict="DRIFTING",
            last_applied_offset_ms=None, proposed_offset_ms=-16.0)
        assert reason, "a DRIFTING band must produce a HOLD reason"

        fake = FakeObs(latency_ms=987, audio_ms=0)
        monkeypatch.setattr(av_sync_calibrate, "_rpc", fake.rpc)
        offset = "" if reason else "24.0"   # the exact bash gate: HOLD clears the offset
        if offset:  # pragma: no cover - documents the gate; HOLD path takes the else
            av_sync_calibrate.apply_latency(None, "NDI 2ME PGM", 987, 954)
        assert fake.set_latency_calls() == [], "HOLD must write no pin"
        assert fake.set_audio_calls() == [], "HOLD must write no audio"


class TestAudioTrimGain1333:
    """#1333 bod 4, live rerun finding (17.9.2026 run 2): the audio sync offset is a LINEAR,
    sample-fine actuator, so it converges with a higher loop gain than the frame-quantized pin
    (whose 0.4 damping exists to avoid pin oscillation). With the pin gain also applied to the
    audio remainder, a -34 ms residual needed 4-5 E2E runs to settle; a dedicated audio gain of 0.8
    settles it in 2 while still leaving 20 % damping against a single noisy median."""

    def test_audio_trim_gain_is_a_build_default_of_0_8(self):
        assert av_sync_calibrate.AUDIO_TRIM_LOOP_GAIN == pytest.approx(0.8)

    def test_audio_gain_applies_only_to_the_audio_remainder(self):
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=-20.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4,
            audio_gain=0.8)
        assert diag["frames"] == 0, "pin part still uses the pin gain: round(0.4*-20/33.3) = 0"
        assert pin == 987
        assert audio == pytest.approx(-16.0), "audio part uses audio_gain: 0.8 * -20 = -16"
        assert diag["audio_gain"] == pytest.approx(0.8)

    def test_audio_gain_defaults_to_the_pin_gain_when_omitted(self):
        pin, audio, diag = av_sync_calibrate.split_av_correction(
            residual_ms=-20.0, current_pin_ms=987, current_audio_offset_ms=0.0, gain=0.4)
        assert audio == pytest.approx(-8.0)
        assert diag["audio_gain"] == pytest.approx(0.4)

    def test_controller_path_passes_the_audio_trim_gain(self):
        src = pathlib.Path(av_sync_calibrate.__file__).read_text(encoding="utf-8")
        assert "audio_gain=AUDIO_TRIM_LOOP_GAIN" in src, (
            "the --apply split call must pass the dedicated audio trim gain, not the pin gain")
