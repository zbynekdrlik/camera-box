"""issue 1349 -- `qr_align_pins.py --measure-only`: converge the stable tail EXACTLY as the DRY-RUN
path does, then print ONLY a JSON object of per-source capture-lag in SOURCE FRAMES relative to the
FASTEST source (0 = fastest, negative = frames behind), plus spread_frames + rounds_used. It writes
NOTHING to OBS (no SetInputSettings / no pins) -- it is the read-only measurement the [4i/8align]
re-init loop (scripts/lib/qr-align-reinit.sh) polls to decide which laggard burn instance to re-init.

Root cause (design 5742993777): each cambox's capture lag `k` (source frames) is DRAWN when the
V4L2 device is opened at [2/8]/[2b/8] and then HOLDS; 7 boxes make it a per-open lottery that the
94 ms floor-aware ceiling cannot absorb. measure-only reports the painter frame_id table so the
loop can re-init the outliers and re-measure instead of aborting the run.

These tests are RED until measure_only_table() + the --measure-only flow exist. They use the
module's OWN measurement functions over a stubbed frame_id table (no rig, no cv2, no WS).
"""
import json
import pathlib
import sys
import types

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

SOURCES = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]
SF_NS = int(round(qa.SOURCE_FRAME_MS * 1e6))   # one 60 fps source frame in ns
BASE_GEN = 1_000_000_000
BASE_SEND = 5_000_000_000


def _tick(frame_id, frames_behind):
    """A painter tick (frame_id, gen_ts_ns, t_send_ns) that is `frames_behind` source frames older
    than the freshest camera. All cameras share t_send (a simultaneous barrier), so a behind camera
    latches an OLDER painter frame == a SMALLER gen_ts == a larger present age."""
    return (frame_id, BASE_GEN - frames_behind * SF_NS, BASE_SEND)


def _rounds(behind_by, n_rounds=4):
    """n identical full rounds: {src: tick} with each src `behind_by[src]` source frames behind."""
    out = []
    for r in range(n_rounds):
        out.append({src: _tick(5000 + r, behind_by[src]) for src in SOURCES})
    return out


def test_measure_only_table_lag_relative_to_fastest():
    # cam3 is 3 source frames behind; everyone else is the (tied) fastest.
    rounds = _rounds({"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 3, "NDI cam4": 0})
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES, min_valid_rounds=2)
    assert out["NDI cam4"] == 0
    assert out["NDI cam1"] == 0
    assert out["NDI cam3"] == -3, out
    assert out["spread_frames"] == 3
    assert out["rounds_used"] >= 2


def test_measure_only_table_already_aligned_is_spread_zero():
    rounds = _rounds({s: 0 for s in SOURCES})
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES, min_valid_rounds=2)
    assert out["spread_frames"] == 0
    assert all(out[s] == 0 for s in SOURCES)


def test_measure_only_table_unmeasurable_tail_is_error_not_crash():
    # No decoded ticks at all -> robust_deltas cannot form a valid round -> a graceful error dict
    # (spread_frames None) the re-init loop degrades on, never an exception.
    rounds = [{s: None for s in SOURCES} for _ in range(4)]
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES, min_valid_rounds=2)
    assert out.get("spread_frames") is None
    assert "error" in out


def test_measure_only_cli_prints_only_json_and_writes_nothing(monkeypatch, capsys):
    # >= DEFAULT_MIN_VALID_ROUNDS clean rounds so the CLI default (no --min-valid-rounds override)
    # measures rather than reporting the too-few-rounds error dict.
    rounds = _rounds({"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 3, "NDI cam4": 0}, n_rounds=6)
    status = qa.TailStatus(True, "converged-stable", 0)
    monkeypatch.setattr(qa, "measure_stable_tail",
                        lambda *a, **k: (rounds, 1_867_252_327, status))

    # Any pin write must go through apply_latency_pins.apply_pins / obs_phase2 -- install recording
    # fakes and prove NEITHER is touched in measure-only (no SetInputSettings ever sent).
    writes = []
    fake_apply = types.ModuleType("apply_latency_pins")
    fake_apply.apply_pins = lambda ws, plan, verify: (writes.append(plan) or plan)
    monkeypatch.setitem(sys.modules, "apply_latency_pins", fake_apply)

    conns = []
    fake_obs = types.ModuleType("obs_phase2")

    class _WS:
        def call(self, req):
            conns.append(("call", req))

        def close(self):
            pass

    fake_obs._conn = lambda h, p: (conns.append(("conn", h)) or _WS())
    fake_obs.read_current_pin = lambda ws, src: 3
    monkeypatch.setitem(sys.modules, "obs_phase2", fake_obs)

    rc = qa.main(["--host", "10.77.9.202", "--sources", ",".join(SOURCES), "--measure-only"])
    assert rc == 0
    out = capsys.readouterr().out.strip()
    data = json.loads(out.splitlines()[-1])
    assert data["NDI cam3"] == -3
    assert data["spread_frames"] == 3
    assert writes == [], f"measure-only wrote pins: {writes}"
    assert conns == [], f"measure-only opened an OBS connection: {conns}"


# ---- issue 1349 lane: --measure-only must NOT exit non-zero on an unconverged tail ------------
# Runs 3 + 5 (E2E 35465567329) failed with `measure unavailable round N` and the loop never
# re-inited anything. The measure-only mode must ALWAYS exit 0 and, when the stable tail never
# converged within the budget, print a best-effort table from the LAST complete round flagged
# `converged: false` (the loop then treats it as measured-but-flagged, not an abort). Only when NOT
# ONE complete round exists is it a true error dict.


def test_measure_only_table_converged_true_on_normal_measure():
    rounds = _rounds({"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 1, "NDI cam4": 0})
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES, min_valid_rounds=2)
    assert out["converged"] is True, out


def test_measure_only_table_unconverged_falls_back_to_last_complete_round():
    # tail too short to robustly measure (min_valid_rounds high) but a complete round exists ->
    # a best-effort table from the last complete round, converged False, NOT the error dict.
    rounds = _rounds({"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 2, "NDI cam4": 0}, n_rounds=2)
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES,
                                min_valid_rounds=99, converged=False)
    assert out["converged"] is False, out
    assert out["NDI cam3"] == -2, out
    assert out["spread_frames"] == 2, out
    assert "error" not in out, out


def test_measure_only_table_no_complete_round_is_error_converged_false():
    # NOT one complete round exists -> the graceful error dict, converged False, spread None.
    rounds = [{s: None for s in SOURCES} for _ in range(4)]
    out = qa.measure_only_table(rounds, tail_start=0, sources=SOURCES,
                                min_valid_rounds=2, converged=False)
    assert out.get("spread_frames") is None, out
    assert "error" in out, out
    assert out["converged"] is False, out


def test_measure_only_cli_unconverged_exits_zero_with_converged_false(monkeypatch, capsys):
    # a run whose tail never converged (status.done False) -> the CLI still EXITS 0 and prints a
    # best-effort table flagged converged:false (never a non-zero exit that the loop swallows).
    rounds = _rounds({"NDI cam1": 0, "NDI cam2": 0, "NDI cam3": 2, "NDI cam4": 0}, n_rounds=3)
    status = qa.TailStatus(False, "unstable", None)
    monkeypatch.setattr(qa, "measure_stable_tail",
                        lambda *a, **k: (rounds, 1_867_252_327, status))
    rc = qa.main(["--host", "10.77.9.202", "--sources", ",".join(SOURCES), "--measure-only"])
    assert rc == 0
    out = capsys.readouterr().out.strip()
    data = json.loads(out.splitlines()[-1])
    assert data["converged"] is False, data
    assert data["NDI cam3"] == -2, data
    assert data["spread_frames"] == 2, data
