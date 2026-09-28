"""issue 1367 -- the restart matrix's PURE decision (scripts/av_restart_matrix_decision.py).

Every window the matrix measures is ONE run of the soak (`av-soak.sh --run --hours 0`), so the
window's evidence is the soak's own CSV row (scripts/av_soak_decision.py `row_from_verdict`). These
tests build those rows from synthetic verdicts with the soak's own row builder and grade them
POINTWISE (one window = one sample: no slope, no cadence), then the whole matrix: the baseline, each
restart kind x its repeats (3/3 required), time-to-healthy, and the exit codes.
"""
import json
import os
import subprocess
import sys

import pytest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.normpath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, os.path.join(REPO, "scripts"))

import av_soak_decision as soak  # noqa: E402
import av_restart_matrix_decision as m  # noqa: E402

DECISION = os.path.join(REPO, "scripts", "av_restart_matrix_decision.py")
CAMS = ["cam1", "cam2", "cam3"]
BOUNDS = soak.load_gate_bounds()
TOL = BOUNDS["av_tolerance_ms"]
THR = BOUNDS["spread_threshold_ms"]


def verdict(offsets=None, expected=0.0, loss=None, hops=(True, True), cont=True, statuses=None,
            cam_burns=None, multi=(), run_wide=None):
    offsets = offsets or {"cam1": 3.0, "cam2": 4.0, "cam3": 5.0}
    statuses = statuses or {}
    loss = loss or {}
    v = {"all_cambox_av_sync": {"expected_ms": expected, "judged_cameras": len(offsets)},
         "all_cambox_latency": {"cross_camera_spread_ms": None},
         "all_cambox_delivery_latency": {"cross_camera_spread_ms": None},
         "all_cambox_continuity": {"segments": [], "overall_pass": cont},
         "full_chain": {"loss": {}}}
    if run_wide is not None:
        # the run-wide half of the gate's fold (src/probe/recording_segments.rs), as serialized
        v["all_cambox_continuity"].update(run_wide_undecodable_within_floor=run_wide,
                                          undecodable_floor_gates_overall_pass=True)
    for c, off in offsets.items():
        st = statuses.get(c, "measured")
        v["all_cambox_av_sync"][c] = {"verdict": st, "av_offset_ms": off, "gate_pass": True}
        v["all_cambox_continuity"]["segments"].append(
            {"cambox": c.upper(), "pass": loss.get(c, True), "relaxed_pass": loss.get(c, True),
             "copies": 0 if loss.get(c, True) else 3, "gaps": 0, "undecodable": 0, "frames": 60,
             "multi_source": c in multi})
    for node, ok in zip(soak.HOP_NODES, hops):
        if ok is not None:
            v["full_chain"]["loss"][node] = {"zero_loss": ok, "real_drops": 0 if ok else 2}
    for c, ok in (cam_burns or {}).items():
        v["full_chain"]["loss"][c] = {"zero_loss": ok, "real_drops": 0 if ok else 1}
    return v


def row(v, outcome="ok", cams=CAMS):
    return soak.row_from_verdict(v, cams, {"epoch_s": 1790000000, "slot": 0, "slot_s": 600,
                                           "window_s": 90, "outcome": outcome})


def write_window(d, v, outcome="ok", cams=CAMS):
    os.makedirs(d, exist_ok=True)
    soak.append_row(os.path.join(d, "soak.csv"), row(v, outcome, cams), cams)
    return d


# --- one window, graded pointwise ---------------------------------------------------------------


def test_a_clean_window_passes():
    g = m.grade_window(row(verdict()), CAMS, BOUNDS)
    assert g["verdict"] == m.PASS, g
    assert g["reasons"] == []


def test_the_av_bound_is_inclusive_and_read_from_its_source():
    # cam1 and cam3 move together, so the camera spread (cam2 excluded) stays within its bound
    edge = m.grade_window(row(verdict({"cam1": TOL, "cam2": 0.0, "cam3": TOL})), CAMS, BOUNDS)
    assert edge["verdict"] == m.PASS, edge
    over = m.grade_window(row(verdict({"cam1": TOL + 0.01, "cam2": 0.0, "cam3": TOL})), CAMS, BOUNDS)
    assert over["verdict"] == m.FAIL
    assert any("av cam1" in r for r in over["reasons"])
    assert not any("av cam3" in r for r in over["reasons"]), "exactly at the bound is inside"


def test_the_av_residual_is_against_the_verdicts_expected_offset():
    g = m.grade_window(row(verdict({"cam1": 100.0, "cam2": 100.0, "cam3": 100.0}, expected=100.0)),
                       CAMS, BOUNDS)
    assert g["verdict"] == m.PASS, g


def test_a_camera_without_a_measured_offset_is_unknown_never_a_pass():
    g = m.grade_window(row(verdict(statuses={"cam3": "derived"})), CAMS, BOUNDS)
    assert g["verdict"] == m.UNKNOWN
    assert any("av cam3" in r for r in g["reasons"])


def test_an_operator_excluded_camera_is_not_required():
    # four cameras: with cam4 excluded, cam1 + cam3 still give the stream-output spread
    cams = CAMS + ["cam4"]
    v = verdict({"cam1": 3.0, "cam2": 4.0, "cam3": 5.0, "cam4": 0.0}, statuses={"cam4": "excluded"})
    g = m.grade_window(row(v, cams=cams), cams, BOUNDS)
    assert g["verdict"] == m.PASS, g


def test_an_excluded_camera_still_fails_on_a_measured_loss_or_camera_burn():
    # the soak only drops the REQUIREMENT for an excluded camera; a measured `false` still fails
    cams = CAMS + ["cam4"]
    base = {"cam1": 3.0, "cam2": 4.0, "cam3": 5.0, "cam4": 0.0}
    lost = verdict(base, statuses={"cam4": "excluded"}, loss={"cam4": False})
    assert m.grade_window(row(lost, cams=cams), cams, BOUNDS)["verdict"] == m.FAIL
    burnt = verdict(base, statuses={"cam4": "excluded"}, cam_burns={"cam4": False})
    assert m.grade_window(row(burnt, cams=cams), cams, BOUNDS)["verdict"] == m.FAIL


def test_a_camera_burn_that_lost_frames_fails_and_one_never_measured_is_not_required():
    assert m.grade_window(row(verdict(cam_burns={"cam3": False})), CAMS, BOUNDS)["verdict"] == m.FAIL
    assert m.grade_window(row(verdict(cam_burns={"cam3": True})), CAMS, BOUNDS)["verdict"] == m.PASS


def test_a_report_only_multi_source_loss_window_is_a_sample_never_a_breach():
    r = row(verdict(loss={"cam2": False}, multi=("cam2",)))
    assert r["loss_cam2_pass"] == "report_only"
    assert m.grade_window(r, CAMS, BOUNDS)["verdict"] == m.PASS


def test_mirrored_terms_that_disagree_with_the_fold_are_unknown_never_a_pass():
    # the run-wide term fails while the verdict's own fold says pass: the copy may have drifted
    r = row(verdict(run_wide=False))
    assert r["loss_run_wide_pass"] == "false" and r["cont_overall_pass"] == "true"
    g = m.grade_window(r, CAMS, BOUNDS)
    assert g["verdict"] == m.UNKNOWN
    assert any("disagree" in x for x in g["reasons"])


def test_a_breach_wins_over_missing_evidence():
    g = m.grade_window(row(verdict({"cam1": TOL + 5, "cam2": 0.0, "cam3": 0.0},
                                   statuses={"cam2": "unknown"})), CAMS, BOUNDS)
    assert g["verdict"] == m.FAIL


def test_the_camera_spread_at_the_stream_output_is_graded_against_its_threshold():
    # av_spread_ms excludes cam2 (the soak's rule): cam1 vs cam3 = THR + 1 apart
    g = m.grade_window(row(verdict({"cam1": 0.0, "cam2": 0.0, "cam3": THR + 1.0})), CAMS,
                       dict(BOUNDS, av_tolerance_ms=THR + 10))
    assert g["verdict"] == m.FAIL
    assert any("spread av_spread_ms" in r for r in g["reasons"])


def test_a_graded_spread_column_that_was_never_measured_is_unknown():
    g = m.grade_window(row(verdict()), CAMS, BOUNDS, spread_columns=("delivery_spread_ms",))
    assert g["verdict"] == m.UNKNOWN
    assert any("delivery_spread_ms" in r for r in g["reasons"])


def test_a_loss_window_fails():
    g = m.grade_window(row(verdict(loss={"cam3": False})), CAMS, BOUNDS)
    assert g["verdict"] == m.FAIL
    assert any("loss cam3" in r for r in g["reasons"])


def test_the_hop_burns_are_required_and_a_hop_loss_fails():
    missing = m.grade_window(row(verdict(hops=(True, None))), CAMS, BOUNDS)
    assert missing["verdict"] == m.UNKNOWN
    assert any("burn stream" in r for r in missing["reasons"])
    lost = m.grade_window(row(verdict(hops=(False, True))), CAMS, BOUNDS)
    assert lost["verdict"] == m.FAIL


def test_the_gates_own_continuity_fold_failing_fails_the_window():
    g = m.grade_window(row(verdict(cont=False)), CAMS, BOUNDS)
    assert g["verdict"] == m.FAIL


def test_a_window_without_a_verdict_is_unknown():
    g = m.grade_window(row(None, outcome="no_verdict:decode_failed"), CAMS, BOUNDS)
    assert g["verdict"] == m.UNKNOWN
    assert any("decode_failed" in r for r in g["reasons"])
    assert m.grade_window(None, CAMS, BOUNDS)["verdict"] == m.UNKNOWN


def test_the_window_row_is_read_from_the_soaks_own_csv(tmp_path):
    d = write_window(str(tmp_path / "w"), verdict())
    cams, r = m.read_window_row(d)
    assert cams == CAMS and r["outcome"] == "ok"
    assert m.read_window_row(str(tmp_path / "missing")) == (None, None)


def test_the_bounds_are_never_retyped(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "av_window.rs").write_text("pub const AV_OFFSET_GATE_TOLERANCE_MS: f64 = 5.0;\n")
    (tmp_path / "src" / "switch_latency.rs").write_text("pub const SPREAD_THRESHOLD_MS: f64 = 24.0;\n")
    b = soak.load_gate_bounds(str(tmp_path))
    g = m.grade_window(row(verdict({"cam1": 6.0, "cam2": 0.0, "cam3": 0.0})), CAMS, b)
    assert g["verdict"] == m.FAIL, "a 6 ms offset fails a 5 ms bound read from the source"


# --- the matrix ----------------------------------------------------------------------------------


def make_matrix(tmp_path, kinds=("strih-obs", "cambox"), repeats=3, windows=None, extra=None,
                skip=()):
    """A matrix dir: matrix.conf + one baseline + kinds x repeats measured steps."""
    d = tmp_path / "matrix"
    d.mkdir()
    (d / "matrix.conf").write_text(
        f"kinds={' '.join(kinds)}\nrepeats={repeats}\nsettle_s=120\nhealthy_timeout_s=300\n"
        "spread_columns=av_spread_ms\n")
    windows = windows or {}
    tsv = str(d / "matrix.tsv")
    step = 0
    wd = write_window(str(d / "w-00-baseline"), windows.get(("baseline", 0), verdict()))
    m.append_step(tsv, {"step": 0, "kind": "baseline", "repeat": 0, "target": "-",
                        "window_dir": wd, "window_rc": 2, "outcome": "measured"})
    for k in kinds:
        for r in range(1, repeats + 1):
            step += 1
            if (k, r) in skip:
                continue
            wd = write_window(str(d / f"w-{step:02d}-{k}-r{r}"), windows.get((k, r), verdict()))
            st = {"step": step, "kind": k, "repeat": r, "target": "10.0.0.1",
                  "restart_epoch": 1790000000 + step * 1000, "healthy_epoch": 1790000000 + step * 1000 + 20 + r,
                  "healthy": 1, "window_dir": wd, "window_rc": 2, "outcome": "measured"}
            st.update((extra or {}).get((k, r), {}))
            m.append_step(tsv, st)
    return d


def report(d, *extra):
    return subprocess.run([sys.executable, DECISION, "report", "--dir", str(d), *extra],
                          capture_output=True, text=True, timeout=60)


def test_every_kind_passing_3_of_3_passes(tmp_path):
    d = make_matrix(tmp_path)
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["verdict"] == m.PASS, rep["reasons"]
    assert rep["baseline"]["verdict"] == m.PASS
    assert rep["kinds"]["strih-obs"]["verdict"] == m.PASS
    assert rep["kinds"]["strih-obs"]["passed"] == 3
    r = report(d, "--json", str(d / "report.json"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert "AV-RESTART-MATRIX" in r.stdout and "VERDICT: PASS" in r.stdout
    assert json.loads((d / "report.json").read_text())["verdict"] == "PASS"


def test_time_to_healthy_is_recorded_per_repeat_and_its_max_per_kind(tmp_path):
    d = make_matrix(tmp_path)
    rep = m.evaluate_dir(str(d), BOUNDS)
    reps = rep["kinds"]["cambox"]["repeats"]
    assert [x["time_to_healthy_s"] for x in reps] == [21, 22, 23]
    assert rep["kinds"]["cambox"]["max_time_to_healthy_s"] == 23
    assert "healthy after 23 s" in m.render_text(rep)


def test_one_failing_repeat_fails_the_kind_and_the_matrix(tmp_path):
    d = make_matrix(tmp_path, windows={("strih-obs", 2): verdict({"cam1": TOL + 8, "cam2": 0.0,
                                                                  "cam3": 0.0})})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["strih-obs"]["verdict"] == m.FAIL
    assert rep["kinds"]["cambox"]["verdict"] == m.PASS
    assert rep["verdict"] == m.FAIL
    assert any("strih-obs r2" in x and "av cam1" in x for x in rep["reasons"])
    assert report(d).returncode == 1


def test_a_component_that_never_came_back_healthy_fails(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", 1): {"healthy": 0, "healthy_epoch": "",
                                                     "outcome": "not_healthy"}})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["cambox"]["verdict"] == m.FAIL
    assert "healthy within 300 s" in rep["kinds"]["cambox"]["repeats"][0]["reason"]


def test_a_failed_restart_command_fails_but_a_restart_never_performed_is_unknown(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", 1): {"outcome": "restart_failed",
                                                     "note": "systemctl restart failed"},
                                     ("strih-obs", 1): {"outcome": "not_performed",
                                                        "note": "strih-obs.service not installed"}})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["cambox"]["verdict"] == m.FAIL
    assert rep["kinds"]["strih-obs"]["verdict"] == m.UNKNOWN
    assert rep["verdict"] == m.FAIL


def test_a_window_the_soak_stopped_is_unknown(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", 2): {"outcome": "window_stopped",
                                                     "note": "STOP: the rig left TEST mode"}})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["cambox"]["verdict"] == m.UNKNOWN
    assert "TEST mode" in rep["kinds"]["cambox"]["repeats"][1]["reason"]


def test_the_restarted_cameras_receiver_state_is_reported_never_graded(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", r): {"receiver": "parked"} for r in (1, 2, 3)})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["cambox"]["verdict"] == m.PASS, "the receiver state is context, not a gate"
    assert [x["receiver"] for x in rep["kinds"]["cambox"]["repeats"]] == ["parked"] * 3
    assert rep["kinds"]["cambox"]["receivers"] == {"parked": 3}
    text = m.render_text(rep)
    assert "receiver parked" in text
    assert "parked during 3/3" in text


def test_a_missing_repeat_is_unknown_never_a_pass(tmp_path):
    d = make_matrix(tmp_path, skip={("cambox", 3)})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["cambox"]["verdict"] == m.UNKNOWN
    assert rep["kinds"]["cambox"]["passed"] == 2
    assert "2/3" in rep["kinds"]["cambox"]["reason"]
    assert rep["verdict"] == m.UNKNOWN
    assert report(d).returncode == 2


def test_a_refused_window_is_unknown(tmp_path):
    d = make_matrix(tmp_path, extra={("strih-obs", 3): {"window_rc": 4, "outcome": "window_refused",
                                                        "note": "the stream program is 'Other'"}})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["kinds"]["strih-obs"]["verdict"] == m.UNKNOWN
    assert "Other" in rep["kinds"]["strih-obs"]["repeats"][2]["reason"]


def test_a_failing_baseline_fails_the_matrix_and_a_missing_one_is_unknown(tmp_path):
    d = make_matrix(tmp_path, windows={("baseline", 0): verdict(loss={"cam1": False})})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["baseline"]["verdict"] == m.FAIL
    assert rep["verdict"] == m.FAIL
    assert any(x.startswith("baseline") for x in rep["reasons"])
    d2 = tmp_path / "two"
    d2.mkdir()
    (d2 / "matrix.conf").write_text("kinds=cambox\nrepeats=3\nsettle_s=120\nhealthy_timeout_s=300\n")
    rep2 = m.evaluate_dir(str(d2), BOUNDS)
    assert rep2["baseline"]["verdict"] == m.UNKNOWN and rep2["verdict"] == m.UNKNOWN


def test_the_repeat_count_comes_from_the_run(tmp_path):
    d = make_matrix(tmp_path, kinds=("dantesync",), repeats=1)
    assert m.evaluate_dir(str(d), BOUNDS)["verdict"] == m.PASS


def test_the_step_table_writes_one_header_and_strips_tabs_and_newlines(tmp_path):
    tsv = str(tmp_path / "matrix.tsv")
    m.append_step(tsv, {"step": 0, "kind": "baseline", "outcome": "measured", "note": "a\tb\nc"})
    m.append_step(tsv, {"step": 1, "kind": "cambox", "repeat": 1, "outcome": "not_healthy"})
    lines = open(tsv).read().splitlines()
    assert lines[0].split("\t") == list(m.STEP_FIELDS)
    assert len(lines) == 3
    assert m.read_steps(tsv)[0]["note"] == "a b c"


def test_the_record_cli_appends_a_step(tmp_path):
    tsv = tmp_path / "matrix.tsv"
    r = subprocess.run([sys.executable, DECISION, "record", "--tsv", str(tsv), "--step", "3",
                        "--kind", "cambox", "--repeat", "2", "--target", "10.77.9.61",
                        "--restart-epoch", "100", "--healthy-epoch", "130", "--healthy", "1",
                        "--window-dir", "/x", "--window-rc", "2", "--outcome", "measured"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr
    st = m.read_steps(str(tsv))[0]
    assert st["kind"] == "cambox" and st["repeat"] == "2" and st["healthy_epoch"] == "130"


def test_an_unknown_outcome_or_kind_is_a_usage_error(tmp_path):
    r = subprocess.run([sys.executable, DECISION, "record", "--tsv", str(tmp_path / "t.tsv"),
                        "--step", "1", "--kind", "cambox", "--outcome", "bogus"],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 3
    d = tmp_path / "m"
    d.mkdir()
    (d / "matrix.conf").write_text("kinds=reboot\nrepeats=3\n")
    assert report(d).returncode == 3
    assert report(tmp_path / "nothing-here").returncode == 3


def test_grade_window_cli_prints_the_verdict_for_the_orchestrator(tmp_path):
    d = write_window(str(tmp_path / "w"), verdict({"cam1": TOL + 1, "cam2": 0.0, "cam3": 0.0}))
    r = subprocess.run([sys.executable, DECISION, "grade-window", "--window-dir", d],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 1
    assert r.stdout.splitlines()[0] == "verdict=FAIL"
    ok = write_window(str(tmp_path / "ok"), verdict())
    r = subprocess.run([sys.executable, DECISION, "grade-window", "--window-dir", ok],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and r.stdout.splitlines()[0] == "verdict=PASS"


@pytest.mark.parametrize("kind", ["strih-obs", "cambox", "dantesync", "stream-obs"])
def test_the_four_restart_kinds_are_declared_once(kind):
    assert kind in m.KINDS


def test_a_pass_on_parked_or_unread_receivers_carries_a_caveat_in_the_verdict_and_the_json(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", 1): {"receiver": "parked"},
                                     ("cambox", 2): {"receiver": "unread"},
                                     ("cambox", 3): {"receiver": "connected"}})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["verdict"] == m.PASS
    assert len(rep["caveats"]) == 1 and "cambox" in rep["caveats"][0]
    assert "parked 1" in rep["caveats"][0] and "unread 1" in rep["caveats"][0]
    text = m.render_text(rep)
    lines = text.splitlines()
    v = next(i for i, line in enumerate(lines) if "VERDICT: PASS" in line)
    assert "CAVEAT" in lines[v + 1], "the caveat sits right under the verdict"
    r = report(d, "--json", str(d / "report.json"))
    assert r.returncode == 0
    assert json.loads((d / "report.json").read_text())["caveats"] == rep["caveats"]


def test_connected_receivers_carry_no_caveat(tmp_path):
    d = make_matrix(tmp_path, extra={("cambox", r): {"receiver": "connected"} for r in (1, 2, 3)})
    rep = m.evaluate_dir(str(d), BOUNDS)
    assert rep["caveats"] == []
    assert "CAVEAT" not in m.render_text(rep)
