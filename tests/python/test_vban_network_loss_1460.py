"""Issue 1460: an unscheduled loss the receiver concealed is matched to its OBS-log line.

The receiver logs `source '<input>': missing N packet(s) before frame F -- concealed` (F = the nuFrame of the
packet after the run). With the schedule's start_frame that is the same packet index the scheduled losses use,
so the grade measures it by the bridge across it and reports a network loss instead of a bare phase step.
"""
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_vban_sine_continuity_1460 as t  # noqa: E402

vs = t.vs
FS = t.FS
INPUT = "VBAN bench"
START = 4000


def _sched(losses):
    s = t.schedule(losses)
    s["start_frame"] = START
    return s


def _line(first_packet, count, start=START, name=INPUT):
    nu = (start + first_packet + count) % (1 << 32)
    return ("22:22:45.960: source '%s': missing %d packet(s) before frame %d -- concealed: the previous "
            "packet repeated, crossfaded" % (name, count, nu))


def _case():
    p1, p2 = 8 * FS // 256, 14 * FS // 256
    x = t._conceal(t._conceal(t.synth(20), p1 * 256 / FS, 2), p2 * 256 / FS, 2)
    return p1, p2, x


def test_without_the_log_an_unscheduled_concealed_loss_is_a_bare_phase_step():
    p1, p2, x = _case()
    sched = _sched([(p1, 2)])
    g = vs.grade(t.scheduled(x, sched), sched)
    assert any(r.startswith("phase step") for r in g["reasons"]), g["reasons"]


def test_the_log_line_makes_it_a_network_loss_and_the_grade_passes():
    p1, p2, x = _case()
    sched = _sched([(p1, 2)])
    net = vs.network_losses([_line(p1, 2), _line(p2, 2), "other line"], INPUT, sched)
    assert [(lo["packet"], lo["count"], lo["network"]) for lo in net] == [(p2, 2, True)]
    full = vs.with_network_losses(sched, net)
    g = vs.grade(t.scheduled(x, full), full)
    assert g["verdict"] == "PASS", g["reasons"]
    assert [r["result"] for r in g["losses"]] == ["concealed", "concealed"], g["losses"]
    assert any(n.startswith("network loss of 2 packet(s) at %.3f s, concealed by the receiver" % (p2 * 256 / FS))
               for n in g["notes"]), g["notes"]


def test_a_network_loss_that_moved_the_timeline_still_fails():
    p2 = 14 * FS // 256
    x = t._conceal(t.synth(20, jumps=[(p2 * 256 / FS + 2 * 256 / FS, 2)]), p2 * 256 / FS, 2)
    sched = _sched([])
    full = vs.with_network_losses(sched, vs.network_losses([_line(p2, 2)], INPUT, sched))
    g = vs.grade(t.scheduled(x, full), full)
    assert g["verdict"] == "FAIL", g
    assert any("network loss" in r and "timeline moved" in r for r in g["reasons"]), g["reasons"]


def test_only_concealed_lines_of_our_input_count_and_the_frame_wraps():
    sched = _sched([])
    sched["start_frame"] = (1 << 32) - 10
    lines = [_line(20, 1, start=sched["start_frame"]),
             _line(30, 2, start=sched["start_frame"], name="other input"),
             "x: source '%s': missing 5 packet(s) before frame 99 -- silence for those slots" % INPUT,
             "x: source '%s': missing 2 packet(s) before the stretch ended -- concealed: the previous" % INPUT]
    net = vs.network_losses(lines, INPUT, sched)
    assert [(lo["packet"], lo["count"]) for lo in net] == [(20, 1)]
    assert net[0]["sample"] == 20 * 256


def test_the_cli_takes_the_obs_log(tmp_path):
    a = vs._args(["rec.mkv", "--obs-log", str(tmp_path / "obs.log"), "--input-name", INPUT])
    assert a.obs_log == str(tmp_path / "obs.log") and a.input_name == INPUT


def test_the_bench_passes_the_profile_obs_log_to_the_grade():
    sh = (Path(vs.__file__).parent / "vban-receiver-bench.sh").read_text()
    call = sh[sh.index('python3 "$ANALYZE" "$RUN_DIR/p$_i-audio.mkv"'):]
    call = call[:call.index("|| _arc=$?")]
    assert '--obs-log "$RUN_DIR/p$_i-obs.log"' in call and '--input-name "$INPUT_NAME"' in call, call
