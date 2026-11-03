"""issue 1404 -- the CG arm of the stream program-audio guard (the 3.11.2026 Content ID claim).

"Praise - Elevation Worship" (SME) was claimed on the E2E VOD 138687756: the song came from the CG
box (resolume-snv: Arena + SongPlayer + the cg OBS) through mbc, 15-20 dB under the cam2 QPSK
marker. The spectral share test read 13.6-21.3 % outside the marker band (calibrated measurement
up to 25.3 %) with the marker chain standing, so every window read MEASUREMENT. The fix listens to
the CG chain itself (scripts/program_audio_cg.py):

* the pure decision: CG audio at or over the -60 dBFS silence bar = FOREIGN `cg-audio`, a CG
  source not heard while the CG box is home = UNKNOWN, away = not graded, nothing to say = the
  window byte-identical;
* the regression fixture with the 3.11 numbers: marker present, share ~16 %, a CG source at
  -30 dBFS -> FOREIGN cg-audio; the CG silent -> MEASUREMENT unchanged;
* the sampler loop: the latch, the payload's `cg` block, no `cg` key without an arm;
* the guard's exit codes over payloads the real loop wrote and the real endpoint served;
* the wiring: the YouTube stage start refuses and the live watch stops on it, through the REAL
  guard (scripts/youtube_leg_e2e.py keys on the guard's exit code);
* the per-source capture monitor and the home probe.
"""
from __future__ import annotations

import ast
import json
import math
import os
import pathlib
import subprocess
import sys
from datetime import datetime, timezone

import numpy as np
import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
_HERE = pathlib.Path(__file__).resolve().parent
for p in (str(_SCRIPTS), str(_HERE)):
    if p not in sys.path:
        sys.path.insert(0, p)

import program_audio as pa  # noqa: E402
import program_audio_cg as pcg  # noqa: E402
import program_audio_ndi as pan  # noqa: E402
import program_audio_sampler as pas  # noqa: E402
import rig_serve_files as rsf  # noqa: E402
from qpsk_guard_shim_1404 import FixedChain  # noqa: E402
from youtube_leg_e2e_fakes_1404 import Fakes  # noqa: E402

GUARD = _SCRIPTS / "program_audio_guard.py"
YLE = _SCRIPTS / "youtube_leg_e2e.py"
SR = 48000
SP = "RESOLUME-SNV (SP-program)"
CGOBS = "RESOLUME-SNV (cg-obs)"


def _reading(source=SP, level=-95.0, age=0.1, conns=1, error=None):
    return pcg.CgReading(source, level, age, conns, error)


def _state(*readings, home=True, box="resolume-snv"):
    return pcg.CgState(box, home, tuple(readings) or (_reading(SP), _reading(CGOBS)))


# ---------------------------------------------------------------------------------------------
# the declaration
# ---------------------------------------------------------------------------------------------


def test_the_cg_sources_come_from_the_fleet_identity_and_the_one_output_list(monkeypatch):
    monkeypatch.delenv(pcg.CG_SOURCES_ENV, raising=False)
    monkeypatch.delenv("OBS_FLEET_IDENTITY", raising=False)
    assert pcg.CG_BOX_DEFAULT == "resolume-snv"
    assert pcg.CG_NDI_OUTPUTS == ("SP-program", "cg-obs")
    assert pcg.cg_sources("resolume-snv") == [SP, CGOBS]
    # an old fleet name reaches the renamed row (issue 1414 aliases)
    assert pcg.cg_sources("resolume") == [SP, CGOBS]


def test_a_cg_box_without_a_declared_computer_name_fails_loud(monkeypatch):
    monkeypatch.delenv(pcg.CG_SOURCES_ENV, raising=False)
    monkeypatch.setenv("OBS_FLEET_IDENTITY", "strih-pp=strih-pp")
    with pytest.raises(ValueError, match="no declared computer name"):
        pcg.cg_sources("resolume-snv")


def test_the_sources_override_is_a_pipe_list(monkeypatch):
    monkeypatch.setenv(pcg.CG_SOURCES_ENV, "A (x)| B (y) |")
    assert pcg.cg_sources("resolume-snv") == ["A (x)", "B (y)"]


def test_the_box_comes_from_its_env(monkeypatch):
    monkeypatch.delenv(pcg.CG_BOX_ENV, raising=False)
    assert pcg.cg_box() == "resolume-snv"
    monkeypatch.setenv(pcg.CG_BOX_ENV, "resolume-pp")
    assert pcg.cg_box() == "resolume-pp"


def test_the_constants_are_single_sourced():
    assert pcg.LEVEL_WINDOW_S == pa.WINDOW_S
    assert pcg.CG_NO_AUDIO_S == pas.NO_AUDIO_TIMEOUT_S
    src = (_SCRIPTS / "program_audio_cg.py").read_text(encoding="utf-8")
    assert "pa.SILENT_RMS_DBFS" in src
    retyped = [n for n in ast.walk(ast.parse(src)) if isinstance(n, ast.UnaryOp)
               and isinstance(n.op, ast.USub) and isinstance(n.operand, ast.Constant)
               and n.operand.value in (60, 60.0)]
    assert retyped == []  # the bar is never retyped in code


# ---------------------------------------------------------------------------------------------
# the pure decision
# ---------------------------------------------------------------------------------------------


def test_cg_audio_over_the_bar_is_foreign_with_the_source_and_its_level():
    v, r = pcg.overlay("MEASUREMENT", None, _state(_reading(SP, -30.04), _reading(CGOBS, -95.0)))
    assert v == "FOREIGN"
    assert r.startswith("cg-audio: 'RESOLUME-SNV (SP-program)' at -30.0 dBFS")
    assert CGOBS not in r


def test_the_bar_is_the_guards_silence_bar_inclusive():
    at = pcg.overlay("MEASUREMENT", None, _state(_reading(SP, pa.SILENT_RMS_DBFS), _reading(CGOBS)))
    under = pcg.overlay("MEASUREMENT", None, _state(_reading(SP, pa.SILENT_RMS_DBFS - 0.1), _reading(CGOBS)))
    assert at[0] == "FOREIGN" and under == ("MEASUREMENT", None)


@pytest.mark.parametrize("verdict, reason", [("SILENT", None), ("UNKNOWN", "warming up: 2 of 4 s"),
                                             ("FOREIGN", None)])
def test_cg_audio_turns_every_verdict_foreign(verdict, reason):
    v, r = pcg.overlay(verdict, reason, _state(_reading(CGOBS, -12.0), _reading(SP)))
    assert v == "FOREIGN" and r.startswith("cg-audio: 'RESOLUME-SNV (cg-obs)' at -12.0 dBFS")


def test_a_stream_foreign_keeps_its_own_reason_when_it_has_one():
    v, r = pcg.overlay("FOREIGN", "spectral", _state(_reading(SP, -20.0), _reading(CGOBS)))
    assert (v, r) == ("FOREIGN", "spectral")


def test_the_3_11_claim_regression_fixture():
    """The claimed windows: marker chain standing, ~16 % outside the band (MEASUREMENT by the share
    test), the song on the CG chain at -30 dBFS -> FOREIGN cg-audio; the CG silent -> unchanged."""
    rng = np.random.default_rng(1404)
    n = int(SR * pa.WINDOW_S)
    t = np.arange(n) / SR
    tone = 10 ** (-38 / 20) * np.sqrt(2) * np.sin(2 * np.pi * 442.0 * t)
    noise = rng.standard_normal(n)
    noise *= 10 ** (-45.2 / 20) / np.sqrt(np.mean(noise ** 2))
    win = np.stack([tone + noise, tone + noise], axis=1)
    rms, outside = pa.analyse(win, SR)
    assert 14.0 <= outside <= 18.0, outside  # the claim's 13.6-21.3 %, under the 30 % FOREIGN bar
    verdict = pa.classify(rms, outside, 6)
    assert verdict == "MEASUREMENT"
    song = 10 ** (-30 / 20) * rng.standard_normal((1024, 2))
    level = pcg.block_level_dbfs(song)
    assert -31.0 < level < -29.0
    loud = _state(_reading(SP, level), _reading(CGOBS, -95.0))
    v, r = pcg.overlay(verdict, None, loud)
    assert v == "FOREIGN" and r.startswith("cg-audio: ")
    quiet = _state(_reading(SP, pcg.block_level_dbfs(np.zeros((1024, 2)))), _reading(CGOBS, -95.0))
    assert pcg.overlay(verdict, None, quiet) == ("MEASUREMENT", None)


@pytest.mark.parametrize("bad", [
    _reading(CGOBS, None, None, 0),                 # never heard
    _reading(CGOBS, None, 6.0, 1),                  # nothing for longer than the no-audio timeout
    _reading(CGOBS, -95.0, 0.1, 1, "NDI receive error from 'x' (connection lost)"),
    _reading(CGOBS, float("nan"), 0.1, 1),          # a non-finite sample
])
@pytest.mark.parametrize("verdict", ["MEASUREMENT", "SILENT"])
def test_a_cg_source_not_heard_while_home_is_unknown(bad, verdict):
    v, r = pcg.overlay(verdict, None, _state(_reading(SP), bad))
    assert v == "UNKNOWN"
    assert "cg source not receivable while the CG box resolume-snv is home" in r and CGOBS in r


def test_a_source_heard_within_the_timeout_with_no_block_in_the_window_is_not_unknown():
    assert pcg.overlay("MEASUREMENT", None, _state(_reading(SP, None, 4.9), _reading(CGOBS))) == ("MEASUREMENT", None)


@pytest.mark.parametrize("verdict, reason", [("UNKNOWN", "marker span"), ("FOREIGN", None)])
def test_an_unheard_source_never_overrides_a_stream_unknown_or_foreign(verdict, reason):
    assert pcg.overlay(verdict, reason, _state(_reading(SP, None, None, 0), _reading(CGOBS))) == (verdict, reason)


def test_an_empty_source_list_while_home_is_unknown():
    v, r = pcg.overlay("MEASUREMENT", None, pcg.CgState("resolume-snv", True, ()))
    assert v == "UNKNOWN" and "no CG source declared" in r


def test_away_an_unheard_source_is_not_graded():
    st = _state(_reading(SP), _reading(CGOBS, None, None, 0), home=False)
    assert pcg.overlay("MEASUREMENT", None, st) == ("MEASUREMENT", None)
    assert pcg.overlay("SILENT", None, st) == ("SILENT", None)
    block = pcg.payload_block(st)
    assert block["graded"] is False and block["home"] is False and block["status"] == "away"


def test_cg_audio_actually_received_is_foreign_even_when_the_probe_reads_away():
    """The home probe reads the cg OBS's OBS-WS; SongPlayer can play while that OBS is down, and the
    probe lags by up to its interval. Audio the sampler really receives is on this LAN (review)."""
    st = _state(_reading(SP, -25.0), _reading(CGOBS, None, None, 0), home=False)
    v, r = pcg.overlay("MEASUREMENT", None, st)
    assert v == "FOREIGN" and r.startswith("cg-audio: 'RESOLUME-SNV (SP-program)' at -25.0 dBFS")
    assert pcg.payload_block(st)["status"] == "cg-audio"


def test_not_probed_yet_counts_as_home():
    v, _r = pcg.overlay("MEASUREMENT", None, _state(_reading(SP, -20.0), _reading(CGOBS), home=None))
    assert v == "FOREIGN"
    assert pcg.payload_block(_state(home=None))["graded"] is True


def test_no_arm_changes_nothing():
    assert pcg.overlay("MEASUREMENT", None, None) == ("MEASUREMENT", None)
    assert pcg.payload_block(None) is None


def test_the_payload_block_fields():
    st = _state(_reading(SP, -30.04, 0.26, 1), _reading(CGOBS, -95.0, 0.04, 1))
    assert pcg.payload_block(st) == {
        "box": "resolume-snv", "home": True, "graded": True, "status": "cg-audio",
        "silent_below_dbfs": pa.SILENT_RMS_DBFS,
        "sources": [{"name": SP, "level_dbfs": -30.0, "audio_age_s": 0.3, "connections": 1},
                    {"name": CGOBS, "level_dbfs": -95.0, "audio_age_s": 0.0, "connections": 1}]}
    assert pcg.payload_block(_state())["status"] == "silent"
    deaf = pcg.payload_block(_state(_reading(SP, None, None, 0, "capture failed: x"), _reading(CGOBS)))
    assert deaf["status"] == "unreceivable" and deaf["sources"][0]["error"] == "capture failed: x"


def test_block_level():
    assert pcg.block_level_dbfs(np.zeros((10, 2))) == pa.DIGITAL_SILENCE_DBFS
    assert pcg.block_level_dbfs(np.zeros((0, 2))) == pa.DIGITAL_SILENCE_DBFS
    assert abs(pcg.block_level_dbfs(np.full((100, 2), 0.1)) - (-20.0)) < 1e-9
    assert math.isnan(pcg.block_level_dbfs(np.array([[0.1, float("inf")]])))


def test_the_tone_bed_line_is_not_removed_from_a_cg_level():
    t = np.arange(4800) / SR
    bed = 10 ** (-30 / 20) * np.sqrt(2) * np.sin(2 * np.pi * pa.MEASUREMENT_TONE_LINES_HZ[0] * t)
    assert pcg.block_level_dbfs(bed[:, None]) > -31.0  # any CG audio is foreign, the bed included


# ---------------------------------------------------------------------------------------------
# the capture monitor + the home probe
# ---------------------------------------------------------------------------------------------


class _Clock:
    def __init__(self):
        self.t = 100.0

    def __call__(self):
        return self.t


class _Rx:
    def __init__(self, items, conns=1):
        self.items = list(items)
        self.conns = conns

    def capture(self, timeout_ms):
        if not self.items:
            return None
        it = self.items.pop(0)
        if isinstance(it, BaseException):
            raise it
        return it

    def connections(self):
        return self.conns


def _blk(dbfs, n=1024, ch=2):
    return pan.AudioBlock(SR, np.full((n, ch), 10 ** (dbfs / 20), dtype=np.float32), 1)


def test_the_monitor_keeps_the_loudest_block_of_the_trailing_window():
    clock = _Clock()
    rx = _Rx([_blk(-40), _blk(-20), _blk(-50)])
    m = pcg.CgSourceMonitor(rx, SP, mono=clock, sleep=lambda s: None)
    m.poll_once()
    clock.t += 0.5
    m.poll_once()
    clock.t += 0.5
    m.poll_once()
    r = m.reading()
    assert r.source == SP and abs(r.level_dbfs - (-20.0)) < 0.01 and r.audio_age_s == 0.0 and r.connections == 1
    clock.t += 1.6  # the -20 block (t=100.5) left the 2 s window, the -50 one (t=101.0) did not
    r = m.reading()
    assert abs(r.level_dbfs - (-50.0)) < 0.01 and abs(r.audio_age_s - 1.6) < 1e-9
    clock.t += 1.0
    r = m.reading()
    assert r.level_dbfs is None and abs(r.audio_age_s - 2.6) < 1e-9


def test_the_monitor_never_heard_reads_no_age():
    m = pcg.CgSourceMonitor(_Rx([None], conns=0), CGOBS, mono=_Clock(), sleep=lambda s: None)
    m.poll_once()
    assert m.reading() == pcg.CgReading(CGOBS, None, None, 0, None)


def test_an_error_frame_or_a_broken_capture_reads_unreceivable_until_audio_returns():
    clock = _Clock()
    slept = []
    rx = _Rx([ConnectionError("NDI receive error"), _blk(-90), RuntimeError("boom")])
    m = pcg.CgSourceMonitor(rx, SP, mono=clock, sleep=slept.append)
    m.poll_once()
    assert m.reading().error == "NDI receive error" and slept == [0.5]
    m.poll_once()
    assert m.reading().error is None
    m.poll_once()
    assert "boom" in m.reading().error
    v, _ = pcg.overlay("MEASUREMENT", None, pcg.CgState("resolume-snv", True, (m.reading(),)))
    assert v == "UNKNOWN"


def test_an_empty_or_rateless_block_is_no_audio():
    clock = _Clock()
    rx = _Rx([pan.AudioBlock(0, np.ones((10, 2), dtype=np.float32)),
              pan.AudioBlock(SR, np.zeros((0, 2), dtype=np.float32))])
    m = pcg.CgSourceMonitor(rx, SP, mono=clock, sleep=lambda s: None)
    m.poll_once()
    m.poll_once()
    assert m.reading().audio_age_s is None


def test_the_monitor_thread_starts_and_stops():
    m = pcg.CgSourceMonitor(_Rx([]), SP, timeout_ms=10, sleep=lambda s: None)
    m.start()
    assert m.stop(join_s=2.0) is True


def test_the_home_probe():
    answers = [True, False]
    p = pcg.HomeProbe("resolume-snv", is_home=lambda box: answers.pop(0))
    assert p.home is None
    assert p.probe_once() is True and p.home is True
    assert p.probe_once() is False

    def broken(_box):
        raise OSError("dns")

    p2 = pcg.HomeProbe("resolume-snv", is_home=broken)
    assert p2.probe_once() is None  # unreadable counts as home: fail closed


def test_the_arm_reads_every_monitor():
    clock = _Clock()
    m1 = pcg.CgSourceMonitor(_Rx([_blk(-25)]), SP, mono=clock, sleep=lambda s: None)
    m2 = pcg.CgSourceMonitor(_Rx([_blk(-90)]), CGOBS, mono=clock, sleep=lambda s: None)
    m1.poll_once()
    m2.poll_once()
    probe = pcg.HomeProbe("resolume-snv", is_home=lambda b: True)
    probe.probe_once()
    arm = pcg.CgArm("resolume-snv", [m1, m2], probe)
    st = arm.state()
    assert arm.sources == [SP, CGOBS] and st.home is True and st.box == "resolume-snv"
    assert pcg.overlay("MEASUREMENT", None, st)[0] == "FOREIGN"


# ---------------------------------------------------------------------------------------------
# the sampler loop: the latch, the payload, byte-compatibility
# ---------------------------------------------------------------------------------------------


class _FakeArm:
    def __init__(self, state):
        self.st = state

    def state(self, now=None, program=None):
        return self.st


def _tone_blocks(seconds, block=960):
    n = int(SR * seconds)
    t = np.arange(n) / SR
    x = (10 ** (-36 / 20) * np.sqrt(2) * np.sin(2 * np.pi * 442.0 * t)).astype(np.float32)
    st = np.stack([x, x], axis=1)
    step = int(round(block / SR * pa.NDI_TIME_UNITS_PER_S))
    return [pan.AudioBlock(SR, st[i:i + block], 10_000_000_000 + (i // block) * step)
            for i in range(0, n - block + 1, block)]


class _Feed:
    """A receiver that hands one block per call, and before each block lets the test change the
    CG arm's state (`at`: block index -> state)."""

    def __init__(self, blocks, arm, at):
        self.blocks = blocks
        self.arm = arm
        self.at = at
        self.i = 0

    def capture(self, timeout_ms):
        if self.i >= len(self.blocks):
            return None
        if self.i in self.at:
            self.arm.st = self.at[self.i]
        b = self.blocks[self.i]
        self.i += 1
        return b

    def connections(self):
        return 1


def _run(tmp_path, arm, at=None, seconds=12.0):
    pathlib.Path(tmp_path).mkdir(parents=True, exist_ok=True)
    blocks = _tone_blocks(seconds)
    rx = _Feed(blocks, arm, at or {})
    seen = []
    pas.run(rx, str(tmp_path), source="STREAM-SNV (stream)", decoder=FixedChain(),
            max_loops=len(blocks), on_write=seen.append, log=lambda m: None, cg=arm)
    return seen


def test_without_cg_audio_the_loop_reads_measurement_and_carries_the_cg_block(tmp_path):
    seen = _run(tmp_path, _FakeArm(_state()))
    assert [p["verdict"] for p in seen[1:]] == ["UNKNOWN", "MEASUREMENT", "MEASUREMENT", "MEASUREMENT",
                                                "MEASUREMENT", "MEASUREMENT"]
    assert seen[-1]["cg"]["status"] == "silent" and seen[-1]["last_foreign_ts_utc"] is None
    assert "reason" not in seen[-1]


def test_cg_audio_mid_run_is_foreign_and_latched(tmp_path):
    arm = _FakeArm(_state())
    # 100 blocks of 20 ms = one 2 s window (window k completes on block 100k + 99): the song plays
    # while window 4 completes, then stops
    seen = _run(tmp_path, arm, at={350: _state(_reading(SP, -30.0), _reading(CGOBS)), 400: _state()})
    verdicts = [p["verdict"] for p in seen[1:]]
    assert verdicts == ["UNKNOWN", "MEASUREMENT", "MEASUREMENT", "FOREIGN", "MEASUREMENT", "MEASUREMENT"]
    foreign = seen[1:][3]
    assert foreign["reason"].startswith("cg-audio: 'RESOLUME-SNV (SP-program)' at -30.0 dBFS")
    assert foreign["cg"]["status"] == "cg-audio"
    assert foreign["rms_dbfs"] is not None and foreign["marker_chain"] == 8  # the stream still measured
    # the latch: the clean windows after it keep the FOREIGN window's time
    assert seen[-1]["last_foreign_ts_utc"] == foreign["last_foreign_ts_utc"] is not None
    served = rsf.program_audio_response(str(tmp_path / rsf.PROGRAM_AUDIO_NAME), datetime.now(timezone.utc))
    assert served["verdict"] == "MEASUREMENT" and served["last_foreign_age_s"] < 30


def test_cg_audio_in_the_warm_up_is_foreign_too(tmp_path):
    seen = _run(tmp_path, _FakeArm(_state(_reading(CGOBS, -40.0), _reading(SP))), seconds=4.0)
    assert [p["verdict"] for p in seen] == ["UNKNOWN", "FOREIGN", "FOREIGN"]


def test_an_unheard_cg_source_makes_the_measurement_unknown(tmp_path):
    seen = _run(tmp_path, _FakeArm(_state(_reading(SP), _reading(CGOBS, None, None, 0))), seconds=6.0)
    assert [p["verdict"] for p in seen[1:]] == ["UNKNOWN", "UNKNOWN", "UNKNOWN"]
    assert "cg source not receivable" in seen[-1]["reason"] and seen[-1]["last_foreign_ts_utc"] is None


def test_away_the_loop_is_byte_identical_except_the_cg_block(tmp_path):
    with_arm = _run(tmp_path / "a", _FakeArm(_state(_reading(SP), _reading(CGOBS, None, None, 0), home=False)))
    without = []
    blocks = _tone_blocks(12.0)
    (tmp_path / "b").mkdir()
    pas.run(_Feed(blocks, _FakeArm(None), {}), str(tmp_path / "b"), source="STREAM-SNV (stream)",
            decoder=FixedChain(), max_loops=len(blocks), on_write=without.append, log=lambda m: None)
    assert len(with_arm) == len(without)
    for a, b in zip(with_arm[1:], without[1:]):
        assert a["cg"]["status"] == "away"
        a = {k: v for k, v in a.items() if k not in ("cg", "ts_utc")}
        b = {k: v for k, v in b.items() if k != "ts_utc"}
        assert a == b


def test_without_an_arm_no_payload_carries_a_cg_key(tmp_path):
    (tmp_path / "s").mkdir()
    blocks = _tone_blocks(6.0)
    seen = []
    pas.run(_Feed(blocks, _FakeArm(None), {}), str(tmp_path / "s"), source="S", decoder=FixedChain(),
            max_loops=len(blocks), on_write=seen.append, log=lambda m: None)
    assert seen and all("cg" not in p for p in seen)
    legacy = ["schema", "ts_utc", "age_s", "verdict", "rms_dbfs", "outside_band_pct", "window_s", "source",
              "last_foreign_ts_utc", "markers_decoded", "marker_chain", "holes_bridged", "bridged_ms",
              "queue_drops", "lag_ms", "sender_stalls"]
    assert [k for k in seen[-1] if k != "reason"] == legacy


def test_the_no_audio_unknown_also_turns_foreign_on_cg_audio(tmp_path):
    clock = {"t": 1000.0}

    class _Silent:
        def capture(self, timeout_ms):
            clock["t"] += timeout_ms / 1000.0
            return None

        def connections(self):
            return 0

    seen = []
    pas.run(_Silent(), str(tmp_path), source="S", decoder=FixedChain(), mono=lambda: clock["t"],
            max_loops=40, on_write=seen.append, log=lambda m: None,
            cg=_FakeArm(_state(_reading(SP, -25.0), _reading(CGOBS))))
    assert seen[-1]["verdict"] == "FOREIGN" and seen[-1]["reason"].startswith("cg-audio: ")
    assert seen[-1]["last_foreign_ts_utc"] is not None


# ---------------------------------------------------------------------------------------------
# the guard + the YouTube stage wiring, over payloads the real loop wrote
# ---------------------------------------------------------------------------------------------


def _served_file(tmp_path, serve_dir, name):
    """The payload as the sampler's endpoint serves it (ages recomputed), as a file:// URL."""
    served = rsf.program_audio_response(str(pathlib.Path(serve_dir) / rsf.PROGRAM_AUDIO_NAME),
                                        datetime.now(timezone.utc))
    p = tmp_path / name
    p.write_text(json.dumps(served), encoding="utf-8")
    return p


def _guard(path):
    r = subprocess.run([sys.executable, str(GUARD), "--url", path.as_uri()], capture_output=True,
                       text=True, timeout=30)
    return r.returncode, r.stdout.strip()


def test_the_guard_refuses_cg_audio_its_latch_and_an_unheard_source(tmp_path):
    _run(tmp_path / "loud", _FakeArm(_state(_reading(SP, -30.0), _reading(CGOBS))), seconds=6.0)
    rc, line = _guard(_served_file(tmp_path, tmp_path / "loud", "loud.json"))
    assert rc == 1 and "verdict=FOREIGN" in line and "reason=cg-audio: " in line

    _run(tmp_path / "latched", _FakeArm(_state()),
         at={350: _state(_reading(SP, -30.0), _reading(CGOBS)), 400: _state()})
    rc, line = _guard(_served_file(tmp_path, tmp_path / "latched", "latched.json"))
    assert rc == 1 and "latched; current MEASUREMENT" in line

    _run(tmp_path / "deaf", _FakeArm(_state(_reading(SP), _reading(CGOBS, None, None, 0))), seconds=6.0)
    rc, line = _guard(_served_file(tmp_path, tmp_path / "deaf", "deaf.json"))
    assert rc == 2 and "cg source not receivable" in line

    _run(tmp_path / "clean", _FakeArm(_state()), seconds=6.0)
    assert _guard(_served_file(tmp_path, tmp_path / "clean", "clean.json"))[0] == 0

    _run(tmp_path / "away", _FakeArm(_state(_reading(SP), _reading(CGOBS, None, None, 0), home=False)),
         seconds=6.0)
    assert _guard(_served_file(tmp_path, tmp_path / "away", "away.json"))[0] == 0

    _run(tmp_path / "away-loud", _FakeArm(_state(_reading(SP, -5.0), home=False)), seconds=6.0)
    assert _guard(_served_file(tmp_path, tmp_path / "away-loud", "away-loud.json"))[0] == 1


def _real_guard_fakes(tmp_path, payload_file):
    """The YouTube stage fakes, but the guard is the REAL program_audio_guard.py over the file the
    `current` pointer names (the test switches it between polls)."""
    fk = Fakes(tmp_path)
    fk.script("session", "start", 0, {"ok": True, "session_id": "s1", "broadcast_id": "b1"})
    fk.script("obs", "stream-start", 0, {"ok": True, "publish_utc": 1_790_000_000.0})
    fk.script("obs", "stream-stop", 0, {"ok": True, "unpublish_utc": 1_790_000_400.0, "was_active": True})
    fk.script("obs", "record-start", 0, {"ok": True, "record_start_utc": 1_790_000_060.0})
    fk.script("session", "wait-ready", 0, {"ok": True, "state": "ready"})
    pointer = tmp_path / "current"
    pointer.write_text(str(payload_file), encoding="utf-8")
    wrapper = tmp_path / "real-guard"
    wrapper.write_text(f'#!/bin/bash\nset -euo pipefail\nexec "{sys.executable}" "{GUARD}" '
                       f'--url "file://$(cat "{pointer}")"\n', encoding="utf-8")
    wrapper.chmod(0o755)
    fk.env["YOUTUBE_LEG_GUARD_CMD"] = str(wrapper)
    return fk, pointer


def _yle(fk, d, *args):
    return subprocess.run([sys.executable, str(YLE), "--dir", str(d), "--host", "10.77.9.204", "--user",
                           "newlevel", *args], capture_output=True, text=True, env=fk.env, timeout=120)


def test_the_youtube_stage_start_refuses_on_cg_audio(tmp_path):
    _run(tmp_path / "loud", _FakeArm(_state(_reading(SP, -30.0), _reading(CGOBS))), seconds=6.0)
    fk, _ptr = _real_guard_fakes(tmp_path / "f", _served_file(tmp_path, tmp_path / "loud", "loud.json"))
    r = _yle(fk, tmp_path / "yl", "start")
    assert r.returncode == 1, r.stdout + r.stderr
    st = json.loads((tmp_path / "yl" / "state.json").read_text())
    assert st["phase"] == "failed" and "channel guard refused" in st["failure"] and "cg-audio" in st["failure"]
    assert fk.names() == []  # the fake guard never ran; no session, no StartStream


def test_the_youtube_watch_stops_a_live_broadcast_on_cg_audio(tmp_path):
    _run(tmp_path / "clean", _FakeArm(_state()), seconds=6.0)
    _run(tmp_path / "loud", _FakeArm(_state(_reading(CGOBS, -22.0), _reading(SP))), seconds=6.0)
    clean = _served_file(tmp_path, tmp_path / "clean", "clean.json")
    loud = _served_file(tmp_path, tmp_path / "loud", "loud.json")
    fk, ptr = _real_guard_fakes(tmp_path / "f", clean)
    d = tmp_path / "yl"
    r = _yle(fk, d, "start")
    assert r.returncode == 0, r.stdout + r.stderr
    ptr.write_text(str(loud), encoding="utf-8")
    r = _yle(fk, d, "watch", "--parent", str(os.getpid()), "--poll", "0.05", "--max-s", "20")
    assert r.returncode == 0, r.stdout + r.stderr
    st = json.loads((d / "state.json").read_text())
    assert st["phase"] == "failed" and "cg-audio" in st["failure"]
    assert st["stream_stopped"] and st["session_stopped"]
    assert "obs.stream-stop" in fk.names() and "session.stop" in fk.names()


def test_the_guard_line_shows_the_samplers_foreign_reason_and_a_spectral_one_stays_unchanged():
    import program_audio_guard as guard

    base = {"verdict": "FOREIGN", "rms_dbfs": -20.0, "outside_band_pct": 88.0, "age_s": 1.0,
            "markers_decoded": None, "marker_chain": None}
    assert guard.decide(base, 10.0) == (1, "program-audio verdict=FOREIGN rms=-20.0 outside_band=88.0% "
                                           "age=1.0 markers=- chain=-")
    rc, line = guard.decide(dict(base, reason="cg-audio: 'X' at -30.0 dBFS"), 10.0)
    assert rc == 1 and line.endswith("reason=cg-audio: 'X' at -30.0 dBFS")
    rc, line = guard.decide(dict(base, age_s=40.0, reason="cg-audio: 'X'"), 10.0)
    assert rc == 1 and line.endswith("reason=stale (age 40.0 s > max 10 s), cg-audio: 'X'")
