"""Issue 1460 slice 1 -- the sine-continuity analysis of a recorded VBAN pilot.

scripts/vban_sine_continuity.py must see what a VBAN receiver does to the sender's timeline: a
dropped / repeated sample or a moved timeline (a phase STEP, with its size), a lost packet played as
silence for exactly its slot (a SILENCE SLOT with the phase continuing across it), and the sender's
clock offset (the MEASURED rate). These tests build the recorded signal from a sender CONTENT INDEX
(sample t of the recording plays sender sample c[t]), so every receiver behaviour is exact, and one
test runs it through OBS's recording codec (ffmpeg's AAC) first.

Tier-0: numpy + ffmpeg (the CI python job installs it), no rig, no cargo.
"""
import json
import pathlib
import shutil
import subprocess
import sys
import wave

import numpy as np
import pytest

SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
sys.path.insert(0, str(SCRIPTS))

import vban_sine_continuity as vs  # noqa: E402

FS = 48000
F = 997.0
AMP = 0.25
PERIOD = FS / F


def synth(seconds, ppm=0.0, jumps=(), holes=(), lead_s=0.0, ramp=None, noise_db=None, seed=0):
    """The recorded track: sample t plays sender sample c[t] = t (1 + ppm) + the jumps (a jump of +k
    at a time = k sender samples skipped); a hole zeroes samples (the timeline keeps running);
    ramp = (t0_s, t1_s, ppm1) moves the rate linearly from `ppm` to ppm1; noise_db adds white noise
    relative to the pilot."""
    n = int(seconds * FS)
    t = np.arange(n, dtype=np.float64)
    rate = np.full(n, ppm * 1e-6)
    if ramp:
        t0, t1, ppm1 = ramp
        a, b = int(t0 * FS), int(t1 * FS)
        rate[a:b] = np.linspace(ppm, ppm1, b - a) * 1e-6
        rate[b:] = ppm1 * 1e-6
    c = t + np.concatenate(([0.0], np.cumsum(rate[:-1])))
    for at_s, size in jumps:
        c[int(at_s * FS):] += size
    x = AMP * np.sin(2 * np.pi * F * c / FS)
    for at_s, length in holes:
        i = int(at_s * FS)
        x[i:i + length] = 0.0
    if noise_db is not None:
        rng = np.random.default_rng(seed)
        x += rng.normal(0.0, AMP / np.sqrt(2) * 10 ** (noise_db / 20), n)
    if lead_s:
        x = np.concatenate((np.zeros(int(lead_s * FS)), x))
    return x


def analyze(x, **kw):
    kw.setdefault("settle_s", 1.0)
    return vs.analyze(x, pilot_hz=F, **kw)


def schedule(losses, duration_s=20.0, ppm=0.0):
    return {"ppm": ppm, "duration_s": duration_s, "frames_per_packet": 256, "pilot_hz": F,
            "losses": [{"packet": p, "count": c, "sample": p * 256, "time_s": p * 256 / FS} for p, c in losses]}


def mod_period(size):
    """A size the phase can see: size modulo one pilot period, in (-P/2, P/2]."""
    return (size + PERIOD / 2) % PERIOD - PERIOD / 2


# --- the measurement ------------------------------------------------------------------------------

def test_a_clean_pilot_has_no_event_and_no_rate_offset():
    a = analyze(synth(20))
    assert a["events"] == [] and a["silence_slots"] == []
    assert abs(a["rate_ppm"]) < 0.05
    assert a["coverage"] > 0.95
    assert a["residual_db_p99"] < -60
    assert a["pilot_dbfs"] == pytest.approx(20 * np.log10(AMP), abs=0.5)


@pytest.mark.parametrize("size", [1, -1, 3, -7])
def test_a_dropped_or_repeated_sample_is_a_step_of_its_size(size):
    a = analyze(synth(20, jumps=[(7.0, size)]))
    assert len(a["steps"]) == 1 and not a["slews"]
    ev = a["steps"][0]
    assert ev["size_samples"] == pytest.approx(size, abs=0.05)
    assert abs(ev["time_s"] - 7.0) < 0.02


def test_a_half_sample_step_is_seen():
    a = analyze(synth(20, jumps=[(9.0, 0.5)]))
    assert [round(e["size_samples"], 1) for e in a["steps"]] == [0.5]


def test_a_lost_slot_played_as_silence_keeps_the_timeline():
    a = analyze(synth(20, holes=[(8.0, 256)]))
    assert a["steps"] == []
    assert len(a["silence_slots"]) == 1
    s = a["silence_slots"][0]
    assert abs(s["sample"] - 8 * FS) <= 16 and abs(s["length"] - 256) <= 16
    bridge = [b for b in a["bridges"] if s["sample"] in b["silence_slots"]]
    assert len(bridge) == 1 and abs(bridge[0]["size_samples"]) < 0.1


def test_a_lost_slot_that_was_dropped_is_a_step_of_one_packet():
    a = analyze(synth(20, jumps=[(8.0, 256)]))
    assert a["silence_slots"] == []
    assert len(a["steps"]) == 1
    assert a["steps"][0]["size_samples"] == pytest.approx(mod_period(256), abs=0.05)


def test_a_silence_that_pushed_the_timeline_is_a_step_across_its_slot():
    # the receiver inserted 256 samples of silence but then played on where it had stopped
    a = analyze(synth(20, holes=[(8.0, 256)], jumps=[(8.0 + 256 / FS, -256)]))
    assert len(a["silence_slots"]) == 1
    bridge_steps = [e for e in a["steps"] if e["where"] == "bridge"]
    assert len(bridge_steps) == 1
    assert bridge_steps[0]["size_samples"] == pytest.approx(mod_period(-256), abs=0.05)
    assert bridge_steps[0]["silence_slots"] == [a["silence_slots"][0]["sample"]]


@pytest.mark.parametrize("ppm", [50.0, -50.0, 1.4])
def test_the_measured_rate_is_the_senders_offset(ppm):
    a = analyze(synth(30, ppm=ppm))
    assert a["rate_ppm"] == pytest.approx(ppm, abs=0.1)
    assert a["events"] == []


def test_a_rate_that_walks_in_like_the_asrc_is_never_a_step():
    a = analyze(synth(40, ppm=0.0, ramp=(5.0, 25.0, 50.0)))
    assert a["steps"] == []


def test_a_1000_ppm_correction_over_seconds_is_not_a_step():
    # the ASRC books a correction at 1000 ppm (1 ms a second): a slow time warp, never a pop
    a = analyze(synth(30, ramp=(10.0, 12.0, 1000.0)))
    assert a["steps"] == []


def test_the_lead_level_is_measured():
    a = analyze(synth(20, lead_s=3.0), lead_s=2.0)
    assert a["lead_dbfs"] < -200
    a2 = analyze(synth(20, lead_s=3.0) + 0.01, lead_s=2.0)
    assert a2["lead_dbfs"] == pytest.approx(-40.0, abs=0.5)


def test_no_pilot_is_an_analysis_error():
    with pytest.raises(vs.AnalysisError):
        analyze(np.zeros(5 * FS))
    with pytest.raises(vs.AnalysisError):
        analyze(np.zeros(FS // 2))


# --- the verdict ----------------------------------------------------------------------------------

def test_every_loss_played_as_its_silence_passes():
    p1, p2 = 8 * FS // 256, 14 * FS // 256
    x = synth(20, holes=[(p1 * 256 / FS, 256), (p2 * 256 / FS, 512)])
    g = vs.grade(analyze(x), schedule([(p1, 1), (p2, 2)]))
    assert g["verdict"] == "PASS", g["reasons"]
    assert [r["result"] for r in g["losses"]] == ["ok", "ok"]


def test_a_loss_without_its_silence_fails():
    p1 = 8 * FS // 256
    g = vs.grade(analyze(synth(20, jumps=[(p1 * 256 / FS, 256)])), schedule([(p1, 1)]))
    assert g["verdict"] == "FAIL"
    assert any("no silence slot" in r for r in g["reasons"])
    assert any("phase step" in r for r in g["reasons"])


def test_a_loss_whose_silence_has_the_wrong_length_fails():
    p1 = 8 * FS // 256
    g = vs.grade(analyze(synth(20, holes=[(p1 * 256 / FS, 768)])), schedule([(p1, 1)]))
    assert g["verdict"] == "FAIL"
    assert any("samples of silence, not" in r for r in g["reasons"])


def test_a_loss_that_moved_the_timeline_fails_once():
    p1 = 8 * FS // 256
    t = p1 * 256 / FS
    g = vs.grade(analyze(synth(20, holes=[(t, 256)], jumps=[(t + 256 / FS, -256)])), schedule([(p1, 1)]))
    assert g["verdict"] == "FAIL"
    assert [r for r in g["reasons"] if "timeline moved" in r]
    assert not [r for r in g["reasons"] if "phase step" in r]


def test_the_receivers_first_second_hold_does_not_hide_the_losses():
    # Live 28.10.2026 (stress +50): the receiver holds its first second (`emitting ... frame=187`),
    # so the pilot starts ~1 s of SENDER time late and every loss sits ~1 s before where the grade
    # looked (a +-0.2 s search): the 4- and 8-packet silences read as "unscheduled".
    hold = 187 * 256                     # the sender samples the receiver never played
    p1, p2 = 8 * FS // 256, 14 * FS // 256
    x = synth(20, holes=[((p1 * 256 - hold) / FS, 1024), ((p2 * 256 - hold) / FS, 2048)])
    g = vs.grade(analyze(x), schedule([(p1, 4), (p2, 8)]))
    assert [r["result"] for r in g["losses"]] == ["ok", "ok"], g["reasons"]
    assert not any("unscheduled" in r for r in g["reasons"]), g["reasons"]


def test_the_held_first_second_is_not_a_short_pilot():
    # Live 28.10.2026 (stress -50): "the pilot plays 599.0 s of the 600.0 s the emulator sent" --
    # the second the receiver holds at the start is never played, by design.
    g = vs.grade(analyze(synth(18.5)), schedule([], duration_s=20.0))
    assert not any("the pilot plays" in r for r in g["reasons"]), g["reasons"]
    g2 = vs.grade(analyze(synth(17.5)), schedule([], duration_s=20.0))
    assert any("the pilot plays" in r for r in g2["reasons"]), g2["reasons"]


def test_unscheduled_silence_fails():
    g = vs.grade(analyze(synth(20, holes=[(9.0, 256)])), schedule([]))
    assert g["verdict"] == "FAIL" and any("unscheduled silence" in r for r in g["reasons"])


def test_a_step_outside_the_losses_fails():
    g = vs.grade(analyze(synth(20, jumps=[(11.0, 1)])), schedule([]))
    assert g["verdict"] == "FAIL" and any("phase step of +1.00" in r for r in g["reasons"])


def test_a_rate_off_the_injected_offset_fails():
    a = analyze(synth(30, ppm=50.0))
    assert vs.grade(a, schedule([], 30.0, ppm=50.0))["verdict"] == "PASS"
    g = vs.grade(a, schedule([], 30.0, ppm=40.0))
    assert g["verdict"] == "FAIL" and any("measured rate" in r for r in g["reasons"])


def test_a_dirty_track_fails():
    g = vs.grade(analyze(synth(20, noise_db=-20)), schedule([]))
    assert any("not clean" in r for r in g["reasons"])


def test_a_loud_lead_fails():
    x = synth(20, lead_s=3.0)
    x[: 2 * FS] += 0.01
    g = vs.grade(analyze(x, lead_s=2.0), schedule([], 20.0))
    assert any("before the emulator started" in r for r in g["reasons"])


def test_a_short_pilot_fails():
    g = vs.grade(analyze(synth(10)), schedule([], 20.0))
    assert any("plays 10.0 s of the 20.0 s" in r for r in g["reasons"])


def test_the_alignment_finds_losses_after_an_unknown_reserve():
    # the receiver started the pilot 47 ms after the recording's first sound: r0 is fitted
    p1 = 8 * FS // 256
    x = synth(20, holes=[(p1 * 256 / FS, 256)])
    x = np.concatenate((np.zeros(int(0.047 * FS)), x))
    g = vs.grade(analyze(x), schedule([(p1, 1)]))
    assert g["verdict"] == "PASS", g["reasons"]


# --- through OBS's recording codec + the CLI -------------------------------------------------------

def _write_wav(path, x, channels=2):
    pcm = (np.clip(x, -1, 1) * 32767).astype("<i2")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(2)
        w.setframerate(FS)
        w.writeframes(np.repeat(pcm, channels).tobytes())


def test_an_aac_recording_keeps_the_silence_and_the_step(tmp_path):
    assert shutil.which("ffmpeg"), "ffmpeg is needed (OBS records the test track as AAC)"
    p1 = 8 * FS // 256
    x = synth(20, holes=[(p1 * 256 / FS, 256)], jumps=[(14.0, 1)])
    wav, mkv = tmp_path / "in.wav", tmp_path / "out.mkv"
    _write_wav(wav, x)
    subprocess.run(["ffmpeg", "-v", "error", "-nostdin", "-y", "-i", str(wav), "-c:a", "aac", "-b:a", "160k",
                    str(mkv)], check=True)
    a = analyze(vs.read_channel(str(mkv)))
    assert [round(e["size_samples"]) for e in a["steps"]] == [1]
    assert len(a["silence_slots"]) == 1 and abs(a["silence_slots"][0]["length"] - 256) <= 32
    assert a["residual_db_p99"] < vs.DIRTY_DB
    g = vs.grade(a, schedule([(p1, 1)]))
    assert g["reasons"] == ["phase step of +1.00 samples at %.3f s" % a["steps"][0]["time_s"]]


def test_the_cli_grades_a_file_against_its_schedule(tmp_path):
    p1 = 8 * FS // 256
    wav = tmp_path / "rec.wav"
    _write_wav(wav, synth(20, holes=[(p1 * 256 / FS, 256)]))
    sched = tmp_path / "s.json"
    sched.write_text(json.dumps(schedule([(p1, 1)])))
    cmd = [sys.executable, str(SCRIPTS / "vban_sine_continuity.py"), str(wav), "--schedule", str(sched),
           "--settle-s", "1", "--json"]
    r = subprocess.run(cmd, capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr
    doc = json.loads(r.stdout)
    assert doc["verdict"] == "PASS" and doc["analysis"]["silence_slots"]
    silent = tmp_path / "silent.wav"
    _write_wav(silent, np.zeros(5 * FS))
    r2 = subprocess.run([sys.executable, str(SCRIPTS / "vban_sine_continuity.py"), str(silent)],
                        capture_output=True, text=True)
    assert r2.returncode == 2 and "verdict=ERROR" in r2.stdout
