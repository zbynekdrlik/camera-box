"""Issue 1367 slice D1b -- the dev1 consumers of the N>=2 present age read the D1 grid twin.

Root cause (the D1 lane's review, 5880725367): scripts/qr_align_pins.py (the [4i/8align] budget
check), scripts/prerecord_phase_calibrate.py (`measured_by_camera`) and
scripts/arrival_floor_decompose.py estimate a camera's on-air present age as
`latency_ms + mean_head_skew_ms`. Under D1 an N>=2 input presents exactly 66.7 ms at pin 3 and the
audit head sits one source frame older than that target, so the old estimate reads ~86 ms: a one
frame hold reads 86 + 16.7 = 103 > 94 and the planner soft-releases BUDGET_BOUND instead of planning.

Fix (design 5881031249, Approach 1): for a CONFIRMED grid input (every audit line carries
`n2_early=` and received ~= N x consumed, N >= 2 -- read from the raw audit log the jitter JSON was
made from) the present age is `scripts/genlock_n2_grid.py`'s twin at the input's pin; the budget
tests the RESULTING present age after the planned pin; a hold becomes whole source frames through
the twin. Pre-D1 logs, N==1 inputs and a JSON with no log keep today's arithmetic byte-for-byte
(golden outputs captured from the unchanged tools).
"""
import json
import os
import pathlib
import subprocess
import sys
import zlib

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import arrival_floor_decompose as afd  # noqa: E402
import genlock_n2_grid as g  # noqa: E402
import prerecord_phase_calibrate as ppc  # noqa: E402
import qr_align_pins as qa  # noqa: E402

FIX = _ROOT / "tests" / "fixtures" / "genlock_n2_d1_audit_1367"
D1_DIR = FIX / "recording-e2e-1367901"
D1_LOG = D1_DIR / "qr-align-strih-1367901.log"
D1_JSON = D1_DIR / "qr-align-jitter-1367901.json"
GOLDEN = FIX / "pre_d1_golden"
PRE_D1_RUNS = ("659887078", "1363366080")
I60 = 16_666_666
CAMS7 = [f"NDI cam{n}" for n in range(1, 8)]
SRC = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]
EQ_SOURCES = "NDI cam1,NDI cam3,NDI cam4,NDI cam5,NDI cam6,NDI cam7"


def _pre_d1_dir(run):
    return _ROOT / "tests" / "fixtures" / "arrival_floor_1168" / f"recording-e2e-{run}"


def _d1_json():
    return json.loads(D1_JSON.read_text(encoding="utf-8"))


def _d1_grid():
    return g.read_grid_inputs(str(D1_LOG))


def _run(args, **kw):
    return subprocess.run([sys.executable] + [str(a) for a in args], capture_output=True, text=True,
                          **kw)


# =============================================================== qr_align_pins: the floors + the plan
class TestQrAlignArrivalFloors:
    def test_a_grid_input_reads_the_twin_present_age_at_its_pin(self):
        floors = qa.arrival_floors_from_jitter(_d1_json(), CAMS7, grid_inputs=_d1_grid())
        assert floors == {s: pytest.approx(66.667, abs=1e-3) for s in CAMS7}

    def test_without_the_grid_the_old_estimate_reads_about_86(self):
        # the pre-fix number, kept for a JSON with no audit log (byte-for-byte old arithmetic)
        floors = qa.arrival_floors_from_jitter(_d1_json(), CAMS7)
        assert all(85.0 < v < 88.0 for v in floors.values())

    def test_a_grid_input_below_min_samples_is_still_dropped(self):
        jj = _d1_json()
        jj["NDI cam4"]["samples"] = 2
        floors = qa.arrival_floors_from_jitter(jj, CAMS7, grid_inputs=_d1_grid())
        assert "NDI cam4" not in floors and len(floors) == 6


class TestQrAlignPlanOnTheGrid:
    """The [4i/8align] planner (floor_aware_partition): the design's acceptance at pin 3."""

    def _floors(self):
        return qa.arrival_floors_from_jitter(_d1_json(), SRC, grid_inputs=_d1_grid())

    def test_one_frame_spread_at_pin_3_plans_a_pin_not_budget_bound(self):
        deltas = {"NDI cam1": 0.0, "NDI cam2": 0.0, "NDI cam3": 16.7, "NDI cam4": 0.0}
        pins = {s: 3 for s in SRC}
        plan, over, missing = qa.floor_aware_partition(
            self._floors(), deltas, current_pins=pins, grid_inputs=_d1_grid())
        assert over == [] and missing == []
        assert plan == {"NDI cam1": 3, "NDI cam2": 3, "NDI cam3": 20, "NDI cam4": 3}
        assert g.present_age_ms(20, I60) == pytest.approx(83.333, abs=1e-3)  # <= 94

    def test_the_old_estimate_budget_bounds_the_same_one_frame_spread(self):
        # what [4i/8align] did before: 86 + 16.7 = 103 > 94 -> over budget, no pin
        deltas = {"NDI cam1": 0.0, "NDI cam2": 0.0, "NDI cam3": 16.7, "NDI cam4": 0.0}
        floors = qa.arrival_floors_from_jitter(_d1_json(), SRC)
        plan, over, _m = qa.floor_aware_partition(floors, deltas,
                                                  current_pins={s: 3 for s in SRC})
        assert [o[0] for o in over] == ["NDI cam3"] and plan["NDI cam3"] == 3

    def test_a_hold_rounds_to_whole_source_frames_through_the_twin(self):
        pins = {s: 3 for s in SRC}
        # 13 ms -> 1 frame (pin 20; the unrounded 3 + 13 = 16 would move NOTHING on the grid)
        plan, over, _m = qa.floor_aware_partition(
            self._floors(), {"NDI cam1": 0.0, "NDI cam2": 13.0, "NDI cam3": 0.0, "NDI cam4": 7.0},
            current_pins=pins, grid_inputs=_d1_grid())
        assert plan["NDI cam2"] == 20
        assert plan["NDI cam4"] == 3          # 7 ms < half a frame -> 0 frames, the pin stays
        assert over == []
        for s, pin in plan.items():           # every planned pin is a whole-frame step
            assert (g.present_age_frames(pin, I60) - g.present_age_frames(3, I60)
                    == g.frames_for_hold({"NDI cam2": 13.0, "NDI cam4": 7.0}.get(s, 0.0), I60))

    def test_two_frames_at_pin_3_read_100_ms_over_the_ceiling(self):
        # the RESULTING present age after the planned pin: 66.7 + 2 x 16.7 = 100 > 94
        plan, over, _m = qa.floor_aware_partition(
            self._floors(), {"NDI cam1": 0.0, "NDI cam2": 28.0, "NDI cam3": 0.0, "NDI cam4": 0.0},
            current_pins={s: 3 for s in SRC}, grid_inputs=_d1_grid())
        assert len(over) == 1
        src, base, hold, target = over[0]
        assert src == "NDI cam2" and plan["NDI cam2"] == 3
        assert base == pytest.approx(66.667, abs=1e-3)
        assert hold == pytest.approx(33.333, abs=1e-3)
        assert target == pytest.approx(100.0, abs=1e-3)
        assert "arrival floor 67ms + delta 33ms = 100ms > bound 94ms" in qa.over_budget_arithmetic(over)

    def test_a_grid_input_at_another_pin_uses_its_own_pin(self):
        # current pin 17 is already one frame deeper (83.3): a one-frame hold -> 100 > 94
        plan, over, _m = qa.floor_aware_partition(
            self._floors(), {"NDI cam1": 0.0, "NDI cam2": 16.7, "NDI cam3": 0.0, "NDI cam4": 0.0},
            current_pins={"NDI cam1": 3, "NDI cam2": 17, "NDI cam3": 3, "NDI cam4": 3},
            grid_inputs=_d1_grid())
        assert [o[0] for o in over] == ["NDI cam2"] and over[0][3] == pytest.approx(100.0, abs=1e-3)
        assert plan["NDI cam2"] == 17

    def test_a_non_grid_input_keeps_the_old_arithmetic_byte_for_byte(self):
        floors = {"NDI cam1": 66.0, "NDI cam2": 63.0, "NDI cam3": 33.0, "NDI cam4": 46.0}
        deltas = {"NDI cam1": 0.0, "NDI cam2": 3.0, "NDI cam3": 33.0, "NDI cam4": 20.0}
        pins = {"NDI cam1": 3, "NDI cam2": 6, "NDI cam3": 17, "NDI cam4": 22}
        old = qa.floor_aware_partition(floors, deltas, current_pins=pins)
        assert qa.floor_aware_partition(floors, deltas, current_pins=pins, grid_inputs={}) == old
        assert qa.floor_aware_partition(floors, deltas, current_pins=pins,
                                        grid_inputs={"cg": 33_333_333}) == old
        assert qa.floor_aware_partition(floors, deltas, current_pins=pins, grid_inputs=None) == old


# =============================================================== qr_align_pins: the [4i/8align] flow
RUN = 1_867_252_327
ID_NS = 8_333_333
_BASE_NS = 30_000 * ID_NS


def _payload(frame_id, gen_ts_ns):
    body = f"{RUN}.{frame_id}.{gen_ts_ns}"
    return f"P{body}.{zlib.crc32(body.encode()) & 0xFFFFFFFF}"


class _GridBarrier:
    """A barrier stand-in modelling D1 strih: each camera's painter-QR age = the grid present age at
    its live pin (the twin) + its capture lag in whole source frames (the issue-1349 per-open draw)."""

    def __init__(self, lag_frames, pins):
        self.lag, self.pins = lag_frames, pins

    def __call__(self, sources, host, password, width, height):
        shot = {}
        for s in sources:
            present_ms = g.present_age_ms(self.pins[s], I60) + self.lag[s] * 1000.0 / 60.0
            gen_ts = int(_BASE_NS - present_ms * 1e6)
            fid = int(round(gen_ts / ID_NS))
            shot[s] = ([_payload(fid, gen_ts), _payload(fid - 1, gen_ts - ID_NS)], 0)
        return shot


def _main_json(monkeypatch, capsys, lag, extra):
    pins = {s: 3 for s in SRC}
    monkeypatch.setattr(qa, "barrier_screenshot", _GridBarrier(lag, pins))
    monkeypatch.setattr(qa, "read_current_pins", lambda s, h, p: dict(pins))
    monkeypatch.setattr("time.sleep", lambda s: None)
    rc = qa.main(["--host", "h", "--sources", ",".join(SRC), "--jitter-json", str(D1_JSON)] + extra)
    out = capsys.readouterr()
    assert rc == 0, out.err
    return json.loads(out.out.strip().splitlines()[-1]), out.err


class TestAlignFlowOnD1:
    def test_a_two_frame_spread_budget_bounds_on_the_grid_arithmetic(self, monkeypatch, capsys):
        res, err = _main_json(monkeypatch, capsys,
                              {"NDI cam1": 2, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0},
                              ["--strih-log", str(D1_LOG)])
        assert res["status"] == "budget-bound"
        assert res["arrival_floors_ms"] == {s: 66.7 for s in SRC}
        assert {o["target_ms"] for o in res["over_budget"]} == {100.0}
        assert res["n2_grid_inputs"] == SRC
        assert "N>=2 grid" in err

    def test_without_the_log_the_same_run_keeps_the_old_arithmetic(self, monkeypatch, capsys):
        res, _err = _main_json(monkeypatch, capsys,
                               {"NDI cam1": 2, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0}, [])
        assert res["status"] == "budget-bound"
        assert all(85.0 < v < 88.0 for v in res["arrival_floors_ms"].values())
        assert "n2_grid_inputs" not in res

    def test_a_one_frame_spread_is_taken_by_the_quantum_gate_first(self, monkeypatch, capsys):
        # the full flow: a spread under 1.5 source frames is already-aligned-quantum (issue 1252),
        # which runs BEFORE the planner -- unchanged by this slice
        res, _err = _main_json(monkeypatch, capsys,
                               {"NDI cam1": 1, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0},
                               ["--strih-log", str(D1_LOG)])
        assert res["status"] == "already-aligned-quantum"

    def test_an_unreadable_log_warns_and_keeps_the_old_arithmetic(self, monkeypatch, capsys, tmp_path):
        res, err = _main_json(monkeypatch, capsys,
                              {"NDI cam1": 2, "NDI cam2": 0, "NDI cam3": 0, "NDI cam4": 0},
                              ["--strih-log", str(tmp_path / "absent.log")])
        assert "WARNING" in err and "--strih-log" in err
        assert all(85.0 < v < 88.0 for v in res["arrival_floors_ms"].values())


class TestQrAlignCliEqualization:
    @pytest.mark.parametrize("run", PRE_D1_RUNS)
    def test_a_pre_d1_log_leaves_the_equalization_output_byte_for_byte(self, run):
        d = _pre_d1_dir(run)
        golden = (GOLDEN / f"equalization-{run}.json").read_text(encoding="utf-8")
        base = ["--equalization-plan", "--host", "x", "--sources", EQ_SOURCES,
                "--jitter-json", d / f"qr-align-jitter-{run}.json"]
        for extra in ([], ["--strih-log", d / f"qr-align-strih-{run}.log"]):
            r = _run([_SCRIPTS / "qr_align_pins.py"] + base + extra)
            assert r.returncode == 0, r.stderr
            assert r.stdout == golden

    def test_the_d1_log_puts_every_grid_camera_on_the_twin(self):
        r = _run([_SCRIPTS / "qr_align_pins.py", "--equalization-plan", "--host", "x",
                  "--sources", EQ_SOURCES, "--jitter-json", D1_JSON, "--strih-log", D1_LOG])
        assert r.returncode == 0, r.stderr
        out = json.loads(r.stdout)
        assert set(out["arrival_floors_ms"].values()) == {66.7}
        assert out["floor_spread_ms"] == 0.0


# =============================================================== prerecord_phase_calibrate
class TestPrerecordMeasuredByCamera:
    def test_a_grid_input_measures_the_twin_present_age(self):
        got = ppc.measured_by_camera(_d1_json(), grid_inputs=_d1_grid())
        assert got == {n: pytest.approx(66.667, abs=1e-3) for n in range(1, 8)}

    def test_no_grid_keeps_latency_plus_skew(self):
        jj = _d1_json()
        got = ppc.measured_by_camera(jj)
        assert got[1] == pytest.approx(3 + jj["NDI cam1"]["mean_head_skew_ms"])

    def test_cli_with_the_d1_log(self, tmp_path):
        out = tmp_path / "m.json"
        r = _run([_SCRIPTS / "prerecord_phase_calibrate.py", "--jitter-json", D1_JSON,
                  "--strih-log", D1_LOG, "--out", out])
        assert r.returncode == 0, r.stderr
        assert json.loads(out.read_text()) == {s: pytest.approx(66.667, abs=1e-3) for s in CAMS7}

    @pytest.mark.parametrize("run", PRE_D1_RUNS)
    def test_cli_with_a_pre_d1_log_is_byte_for_byte(self, run, tmp_path):
        d = _pre_d1_dir(run)
        out = tmp_path / "m.json"
        r = _run([_SCRIPTS / "prerecord_phase_calibrate.py", "--jitter-json",
                  d / f"qr-align-jitter-{run}.json", "--strih-log", d / f"qr-align-strih-{run}.log",
                  "--out", out])
        assert r.returncode == 0, r.stderr
        assert out.read_text() == (GOLDEN / f"prerecord-measured-{run}.json").read_text()

    def test_the_4g8_step_passes_its_calibration_log(self):
        e2e = (_SCRIPTS / "recording-e2e.sh").read_text(encoding="utf-8")
        start = e2e.index('if python3 "$HERE/prerecord_phase_calibrate.py"')
        call = e2e[start:e2e.index("; then", start)]
        assert '--strih-log "$CALIB_LOG"' in call


# =============================================================== arrival_floor_decompose
class TestDecompose:
    @pytest.mark.parametrize("run", PRE_D1_RUNS)
    @pytest.mark.parametrize("fmt", ["txt", "json"])
    def test_a_pre_d1_run_is_byte_for_byte(self, run, fmt):
        args = [_SCRIPTS / "arrival_floor_decompose.py", "--run-dir", _pre_d1_dir(run)]
        if fmt == "json":
            args.append("--json")
        r = _run(args)
        assert r.returncode == 0, r.stderr
        assert r.stdout == (GOLDEN / f"decompose-{run}.{fmt}").read_text(encoding="utf-8")

    def test_a_d1_run_reads_the_grid_present_age(self):
        res = afd.mine_run_dir(str(D1_DIR))
        rows = {r["src"]: r for r in res["rows"]}
        assert set(rows) == set(CAMS7)
        for s in CAMS7:
            assert rows[s]["floor_ms"] == pytest.approx(66.667, abs=1e-3)
            assert rows[s]["n2_grid"] is True
            assert rows[s]["source_interval_ns"] == I60
            assert rows[s]["mean_head_skew_ms"] > 80          # kept: the arrival-lag diagnostic
        assert res["summary"]["floor_spread_ms"] == pytest.approx(0.0, abs=1e-9)
        assert res["summary"]["n2_grid_sources"] == CAMS7
        # cam3's +1 ms arrival lag is named a diagnostic, never an owner of the present age
        assert rows["NDI cam3"]["owner"].startswith("within-noise")

    def test_the_d1_table_marks_the_grid_rows_and_names_the_model(self):
        r = _run([_SCRIPTS / "arrival_floor_decompose.py", "--run-dir", D1_DIR])
        assert r.returncode == 0, r.stderr
        assert "cam1*" in r.stdout
        assert "N>=2 grid" in r.stdout and "arrival-lag diagnostic" in r.stdout

    def test_a_grid_camera_arrival_lag_is_a_labelled_diagnostic(self):
        jj = _d1_json()
        jj["NDI cam5"]["mean_head_skew_ms"] = 95.0             # +11.7 ms arrival lag vs cam1
        res = afd.decompose(jj, {}, {}, CAMS7, grid_inputs=_d1_grid())
        row = {r["src"]: r for r in res["rows"]}["NDI cam5"]
        assert row["floor_ms"] == pytest.approx(66.667, abs=1e-3)   # the lag does not move the age
        assert row["owner"].startswith("within-noise")
        assert "arrival lag +12ms" in row["owner"] and "diagnostic" in row["owner"]

    def test_a_grid_camera_with_a_deeper_pin_is_strih_config(self):
        jj = _d1_json()
        jj["NDI cam2"]["latency_ms"] = 20                     # one frame deeper on the grid
        res = afd.decompose(jj, {}, {}, CAMS7, grid_inputs=_d1_grid())
        rows = {r["src"]: r for r in res["rows"]}
        assert rows["NDI cam2"]["floor_ms"] == pytest.approx(83.333, abs=1e-3)
        assert "strih-config +17ms latency pin" in rows["NDI cam2"]["owner"]


# =============================================================== scripts/lib/qr-align.sh
_QR_ALIGN_HARNESS = r'''
set -uo pipefail
T="$1"; R="$2"
mkdir -p "$T/bin" "$T/probe" "$T/out"
cat > "$T/bin/python3" <<'STUB'
#!/usr/bin/env bash
printf 'PY:%s\n' "$*" >> "$STUB_LOG"
exit 0
STUB
chmod +x "$T/bin/python3"
printf '#!/usr/bin/env bash\necho "{}"\n' > "$T/probe/genlock-jitter-report"
chmod +x "$T/probe/genlock-jitter-report"
export PATH="$T/bin:$PATH" STUB_LOG="$T/stub.log"
: > "$STUB_LOG"
strih_log_line_count() { echo 5; }
strih_log_since_line() { echo "12:00:00.000: genlock-fifo audit 'NDI cam1': received=2 consumed=1 n2_early=0"; }
export STRIH_USER=newlevel PROBE_BIN_DIR="$T/probe" OUTDIR="$T/out" RUN_ID=t1367 \
  QR_ALIGN_SOURCES='NDI cam1' QR_ALIGN_RESET_SETTLE_S=0 QR_ALIGN_AUDIT_WINDOW_S=0
. "$R/scripts/lib/qr-align.sh"
rc=0
qr_align_run 10.77.9.202 pw || rc=$?
echo "QRRC=$rc"
'''


class TestQrAlignShPassesTheAuditLog:
    def _run(self, tmp_path, env=None):
        full = dict(os.environ)
        full.update(env or {})
        r = subprocess.run(["bash", "-c", _QR_ALIGN_HARNESS, "h", str(tmp_path), str(_ROOT)],
                           capture_output=True, text=True, env=full)
        assert "QRRC=0" in r.stdout, r.stdout + r.stderr
        return [ln for ln in (tmp_path / "stub.log").read_text().splitlines()
                if "--execute" in ln]

    def test_the_fetched_window_log_rides_next_to_the_jitter_json(self, tmp_path):
        calls = self._run(tmp_path)
        assert len(calls) == 1
        log = f"{tmp_path}/out/qr-align-strih-t1367.log"
        assert f"--jitter-json {tmp_path}/out/qr-align-jitter-t1367.json" in calls[0]
        assert f"--strih-log {log}" in calls[0]

    def test_an_override_json_takes_the_override_log_only(self, tmp_path):
        jj = tmp_path / "given.json"
        jj.write_text("{}")
        calls = self._run(tmp_path, {"QR_ALIGN_JITTER_JSON": str(jj)})
        assert "--strih-log" not in calls[0]
        calls = self._run(tmp_path, {"QR_ALIGN_JITTER_JSON": str(jj),
                                     "QR_ALIGN_STRIH_LOG": str(tmp_path / "given.log")})
        assert f"--strih-log {tmp_path}/given.log" in calls[0]
