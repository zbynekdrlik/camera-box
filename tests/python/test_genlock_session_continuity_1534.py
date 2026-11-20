"""issue 1534 (owner ROZHODNUTE 18.11.2026): the full-path E2E is a STRICT zero frame-loss gate for the
WHOLE OBS session, not only the recording window -- the video half.

The pure decision scripts/genlock_session_continuity.py grades one box's whole-session read (the
`genlock-fifo audit '<input>':` lines and the `jxs-source '<input>': fps=` 5 s windows with their byte
offsets, reduced on the box by the awk of strih_log_remote_cmd linux session):
  * per genlock input, `underruns` and `relocks` at the log end must equal their value at the SETTLE
    point (the last audit line at or before the input's first audit line + 60 s): the startup
    underruns are normal, any later one is a repeated frame;
  * per JPEG XS input, the gated counters summed over every 5 s window after its first window with
    frames + 60 s must be 0 (the lines are per-window counts, read live 18.11.2026);
  * a declared input with no audit line, a source restart after the settle, another log than the
    window's, a log that has not reached the stop mark = UNKNOWN (fails).

The awk reduction is RUN here over synthetic whole-session logs in a fake home, and the decision is
read on two real reductions of 18.11.2026 (strih-snv, stream-snv).
"""
import json
import subprocess
import sys
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(REPO / "scripts"))
FIX = REPO / "tests/fixtures/genlock_session_continuity_1534"
READER = REPO / "scripts/lib/strih-log-read.sh"

import genlock_session_continuity as gsc  # noqa: E402

LOG = "2026-11-18 18-32-39.txt"
T0 = 18 * 3600 + 32 * 60 + 45.0


def ts(t):
    t %= 86400
    h, rem = divmod(t, 3600)
    m, s = divmod(rem, 60)
    return f"{int(h):02d}:{int(m):02d}:{s:06.3f}"


def audit(t, name, rx, u=0, r=0, dd=0, holds=0):
    return (f"{ts(t)}: genlock-fifo audit '{name}': received={rx} consumed={rx} underruns={u} holds={holds} "
            f"overruns=0 backward_steps=0 dropped_due={dd} relocks={r} late_holds=0 locked=1 depth=2 peak=6 "
            f"latency_ms=36 (≈1 frames @ 30.000fps) src_latency_ms=36 n2_early=0 stamp_gap=0")


def jxs(t, name, frames=150, lost=0, conc=0, und=0, dropped=0, foreign=0, reopens=0):
    return (f"{ts(t)}: [distroav] jxs-source '{name}': fps={frames / 5:.2f} frames={frames} lost_slices={lost} "
            f"concealed={conc} undecodable={und} reopens={reopens} decoder_reopens=0 bpp=8.00 reports=5 "
            f"report_errors=0 dropped_unread={dropped} foreign={foreign} camera=3 link_mbps=10000 "
            f"fec_repaired=0 (issue 1387)")


def session(minutes=10, names=("JXS cam3 src",), edits=None, start=T0, noise=True):
    """A whole-session OBS log: every input's audit + jxs line every 5 s. `edits` maps
    (kind, name, second-from-start) -> kwargs overriding that line (cumulative values for audit)."""
    edits = edits or {}
    lines = ["18:32:39.543: Command Line Arguments: --profile strih-snv"]
    state = {n: {"u": 0, "r": 0, "dd": 0} for n in names}
    for k in range(int(minutes * 12)):
        t = start + 5 * k
        for n in names:
            e = edits.get(("audit", n, 5 * k), {})
            state[n].update(e)
            lines.append(audit(t, n, 150 * (k + 1), state[n]["u"], state[n]["r"], state[n]["dd"]))
            lines.append(jxs(t - 0.05, n, **edits.get(("jxs", n, 5 * k), {})))
            if noise:
                lines.append(f"{ts(t)}: multiview-audit: rendered_fps=30.00")
    return "\n".join(lines) + "\n"


def grep_b(text):
    """What `grep -a -b` of the two line kinds prints (no awk): `<byte offset>:<line>`."""
    out, off = [], 0
    for line in text.split("\n"):
        if "genlock-fifo audit '" in line or ("] jxs-source '" in line and " fps=" in line):
            out.append(f"{off}:{line}")
        off += len(line.encode()) + 1
    return out


def raw_read(text, name=LOG):
    return f"#newest\t{name}\t{len(text.encode())}\n" + "\n".join(grep_b(text)) + "\n"


def box_read(tmp_path, text, name=LOG):
    """The box's own read: the reader's REAL remote text run in a fake home."""
    home = tmp_path / "home"
    logs = home / ".config/obs-studio/logs"
    logs.mkdir(parents=True, exist_ok=True)
    (logs / name).write_text(text)
    run = tmp_path / "run.sh"
    run.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
. "{READER}"
cmd="$(strih_log_remote_cmd linux session)"
HOME="{home}" bash -c "$cmd"
""")
    return subprocess.run(["bash", str(run)], capture_output=True, text=True, check=True).stdout


def decide(read, declared=("JXS cam3 src",), name=LOG, start_byte=0, stop_byte=None):
    if stop_byte is None:
        stop_byte = int(read.split("\n", 1)[0].split("\t")[2]) - 1
    return gsc.decide_box("strih", read, list(declared), log_name=name, start_byte=start_byte,
                          stop_byte=stop_byte)


def _inp(res, name):
    return next(i for i in res["inputs"] if i["input"] == name)


# --- the decision on raw grep lines -------------------------------------------------------------

def test_a_clean_session_passes():
    res = decide(raw_read(session()))
    assert res["verdict"] == "PASS", res
    i = _inp(res, "JXS cam3 src")
    assert i["counters"]["underruns"]["delta"] == 0 and i["counters"]["relocks"]["delta"] == 0
    assert i["jxs"]["lossy_windows"] == 0 and i["jxs"]["windows"] > 100


def test_startup_underruns_and_the_first_lossy_window_are_inside_the_settle():
    text = session(edits={("audit", "JXS cam3 src", 5): {"u": 4},
                          ("jxs", "JXS cam3 src", 0): {"lost": 40, "und": 1},
                          ("audit", "JXS cam3 src", 60): {"u": 6}})
    res = decide(raw_read(text))
    assert res["verdict"] == "PASS", res
    i = _inp(res, "JXS cam3 src")
    assert i["counters"]["underruns"]["settle"] == 6


def test_an_underrun_after_the_settle_fails_naming_input_counter_and_count():
    text = session(edits={("audit", "JXS cam3 src", 300): {"u": 3}})
    res = decide(raw_read(text))
    assert res["verdict"] == "FAIL"
    assert any("'JXS cam3 src'" in f and "underruns +3" in f for f in res["failures"]), res["failures"]
    assert _inp(res, "JXS cam3 src")["first_growth"].startswith(ts(T0 + 300))


def test_a_relock_after_the_settle_fails():
    res = decide(raw_read(session(edits={("audit", "JXS cam3 src", 200): {"r": 1}})))
    assert res["verdict"] == "FAIL"
    assert any("relocks +1" in f for f in res["failures"])


def test_lossy_jpeg_xs_windows_after_the_settle_are_summed():
    text = session(edits={("jxs", "JXS cam3 src", 200): {"lost": 7, "conc": 1},
                          ("jxs", "JXS cam3 src", 400): {"lost": 2, "conc": 1, "und": 1}})
    res = decide(raw_read(text))
    assert res["verdict"] == "FAIL"
    j = _inp(res, "JXS cam3 src")["jxs"]
    assert j["lossy_windows"] == 2
    assert j["sums"]["lost_slices"] == 9 and j["sums"]["concealed"] == 2 and j["sums"]["undecodable"] == 1
    assert any("lost_slices +9" in f and "concealed +2" in f for f in res["failures"]), res["failures"]


@pytest.mark.parametrize("key", ["dropped", "foreign", "reopens"])
def test_every_window_gate_counter_counts(key):
    res = decide(raw_read(session(edits={("jxs", "JXS cam3 src", 300): {key: 1}})))
    assert res["verdict"] == "FAIL", res


def test_a_late_camera_settles_from_its_first_window_with_frames():
    edits = {("jxs", "JXS cam3 src", s): {"frames": 0} for s in range(0, 200, 5)}
    edits[("jxs", "JXS cam3 src", 200)] = {"lost": 45, "und": 1}
    res = decide(raw_read(session(edits=edits)))
    assert res["verdict"] == "PASS", res


def test_dropped_due_is_context_never_a_failure():
    res = decide(raw_read(session(edits={("audit", "JXS cam3 src", 300): {"dd": 90}})))
    assert res["verdict"] == "PASS"
    assert _inp(res, "JXS cam3 src")["context"]["dropped_due"] == 90


def test_an_undeclared_input_is_graded_too():
    # CG-obs is report-only since the owner's 20.11.2026 ruling; any other non-camera input gates.
    text = session(names=("JXS cam3 src", "Zaloha kamera"), edits={("audit", "Zaloha kamera", 300): {"u": 9}})
    res = decide(raw_read(text))
    assert res["verdict"] == "FAIL"
    assert any("'Zaloha kamera'" in f and "underruns +9" in f for f in res["failures"])
    assert _inp(res, "Zaloha kamera")["declared"] is False


def test_a_declared_input_without_an_audit_line_is_unknown():
    res = decide(raw_read(session()), declared=("JXS cam3 src", "JXS cam5 src"))
    assert res["verdict"] == "UNKNOWN"
    assert any("'JXS cam5 src'" in u for u in res["unknown_reasons"])


def test_a_declared_jpeg_xs_input_without_window_lines_is_unknown():
    text = "\n".join(l for l in session().split("\n") if "jxs-source" not in l) + "\n"
    res = decide(raw_read(text))
    assert res["verdict"] == "UNKNOWN"


def test_a_source_restart_after_the_settle_is_unknown():
    text = session(minutes=10)
    lines = text.split("\n")
    i = next(k for k, l in enumerate(lines) if ts(T0 + 300) + ": genlock-fifo audit" in l)
    lines[i] = audit(T0 + 300, "JXS cam3 src", 10)
    res = decide(raw_read("\n".join(lines)))
    assert res["verdict"] == "UNKNOWN"
    assert any("restarted" in u for u in res["unknown_reasons"])


def test_another_log_than_the_window_is_unknown():
    res = decide(raw_read(session(), name="2026-11-18 20-00-00.txt"))
    assert res["verdict"] == "UNKNOWN"
    assert any("OBS restarted" in u for u in res["unknown_reasons"])


def test_a_log_that_has_not_reached_the_stop_mark_is_retryable():
    read = raw_read(session())
    size = int(read.split("\n", 1)[0].split("\t")[2])
    res = decide(read, stop_byte=size + 100)
    assert res["verdict"] == "UNKNOWN" and res["retryable"] is True


def test_the_settle_crosses_midnight():
    start = 86400 - 30.0
    text = session(start=start, edits={("audit", "JXS cam3 src", 40): {"u": 2}})
    res = decide(raw_read(text))
    assert res["verdict"] == "PASS", res
    text = session(start=start, edits={("audit", "JXS cam3 src", 100): {"u": 2}})
    assert decide(raw_read(text))["verdict"] == "FAIL"


def test_an_unknown_box_and_an_empty_read():
    u = gsc.unknown_box("stream", "the stream box is not a Linux OBS box", ["JXS PGM src"])
    assert u["verdict"] == "UNKNOWN" and u["box"] == "stream"
    res = gsc.decide_box("stream", "", ["JXS PGM src"], log_name=LOG, start_byte=0, stop_byte=0)
    assert res["verdict"] == "UNKNOWN"


def test_no_input_at_all_is_unknown():
    res = gsc.decide_box("strih", f"#newest\t{LOG}\t100\n", [], log_name=LOG, start_byte=0, stop_byte=10)
    assert res["verdict"] == "UNKNOWN"


# --- the box's awk reduction (the reader's real remote text) ------------------------------------

def test_the_awk_reduction_decides_like_the_raw_lines(tmp_path):
    for edits in ({}, {("audit", "JXS cam3 src", 400): {"u": 2}},
                  {("jxs", "JXS cam3 src", 500): {"lost": 3, "conc": 1}},
                  {("audit", "JXS cam3 src", 20): {"u": 5}, ("jxs", "JXS cam3 src", 0): {"lost": 40}}):
        text = session(minutes=30, names=("JXS cam3 src", "CG-obs"), edits=edits)
        a, b = decide(raw_read(text)), decide(box_read(tmp_path, text))
        assert a["verdict"] == b["verdict"], (edits, a, b)
        assert a["failures"] == b["failures"]


def test_the_awk_reduction_ships_a_bounded_read(tmp_path):
    text = session(minutes=180, names=("JXS cam3 src", "CG-obs"))
    out = box_read(tmp_path, text)
    assert len(out) < 80_000 < len(text) // 10
    counts = [l for l in out.split("\n") if l.startswith("#count")]
    assert "#count\taudit\t2160\t0\tJXS cam3 src" in counts
    assert "#count\tjxs\t2160\t0\tJXS cam3 src" in counts


def test_lossy_windows_past_the_awk_cap_still_fail(tmp_path):
    edits = {("jxs", "JXS cam3 src", s): {"lost": 1} for s in range(600, 600 + 5 * 2100, 5)}
    text = session(minutes=200, edits=edits, noise=False)
    res = decide(box_read(tmp_path, text))
    assert res["verdict"] == "FAIL"
    j = _inp(res, "JXS cam3 src")["jxs"]
    assert j["lossy_windows"] == 2100 and j["unlisted_lossy_windows"] == 100


# review round 1: the reduced read is SPARSE (first lines, changes, the last line), so two printed
# lines can be hours or days apart and a 12 h midnight heuristic on them mis-dates a later line
# before the settle point -- a real loss then read PASS. The box's awk counts the days.
OVERNIGHT = 8 * 3600.0  # an OBS started at 08:00


@pytest.mark.parametrize("minutes,loss_s", [(1830, 13 * 3600), (1830, 22 * 3600), (1600, 25 * 3600)])
def test_an_overnight_session_read_sparse_still_fails_a_loss(tmp_path, minutes, loss_s):
    text = session(minutes=minutes, start=OVERNIGHT, noise=False,
                   edits={("audit", "JXS cam3 src", loss_s): {"u": 6},
                          ("jxs", "JXS cam3 src", loss_s): {"lost": 3, "conc": 1}})
    res = decide(box_read(tmp_path, text))
    assert res["verdict"] == "FAIL", res
    i = _inp(res, "JXS cam3 src")
    assert i["counters"]["underruns"]["delta"] == 6 and i["counters"]["underruns"]["settle"] == 0
    assert i["jxs"]["lossy_windows"] == 1


def test_a_healthy_overnight_session_read_sparse_passes_on_its_real_settle(tmp_path):
    text = session(minutes=1830, start=OVERNIGHT, noise=False, edits={("audit", "JXS cam3 src", 20): {"u": 2}})
    res = decide(box_read(tmp_path, text))
    assert res["verdict"] == "PASS", res
    assert _inp(res, "JXS cam3 src")["settle"].startswith(ts(OVERNIGHT + 60))


def test_the_box_stamps_every_line_with_its_day(tmp_path):
    out = box_read(tmp_path, session(minutes=1830, start=OVERNIGHT, noise=False))
    rows = [l.split(":", 2)[1] for l in out.split("\n") if l and not l.startswith("#")]
    assert set(rows) == {"D0", "D1"}, set(rows)


def test_an_input_of_a_camera_acked_offline_is_report_only():
    text = session(names=("JXS cam3 src", "JXS cam1 src"), edits={("audit", "JXS cam1 src", 300): {"u": 9}})
    read = raw_read(text)
    size = int(read.split("\n", 1)[0].split("\t")[2])
    res = gsc.decide_box("strih", read, ["JXS cam3 src", "JXS cam1 src"], log_name=LOG, start_byte=0,
                         stop_byte=size - 1, excluded=["JXS cam1 src"])
    assert res["verdict"] == "PASS", res
    cam1 = _inp(res, "JXS cam1 src")
    assert cam1["verdict"] == "FAIL" and cam1["excluded"] is True and "report-only" in cam1["note"]


def test_both_parsers_read_the_first_of_a_repeated_key():
    line = audit(T0, "JXS cam3 src", 150, u=2) + " underruns=9"
    assert gsc._fields(line)["underruns"] == 2


# --- the real reductions of 18.11.2026 ----------------------------------------------------------

def _fixture(name):
    return (FIX / name).read_text()


def test_real_strih_snv_names_cam3s_late_underrun_and_cg_obs():
    read = _fixture("strih-snv-18-11-2026.txt")
    res = decide(read, declared=("JXS cam2 src", "JXS cam3 src", "JXS cam5 src"))
    assert res["verdict"] == "FAIL"
    assert any("'JXS cam3 src'" in f and "underruns +1" in f for f in res["failures"]), res["failures"]
    cg = _inp(res, "CG-obs")  # report-only since the owner's 20.11.2026 ruling
    assert cg["verdict"] == "FAIL" and cg["report_only"] is True, cg
    assert not any("'CG-obs'" in f for f in res["failures"]), res["failures"]
    for cam in ("JXS cam2 src", "JXS cam5 src"):
        assert _inp(res, cam)["verdict"] == "PASS", _inp(res, cam)
    assert _inp(res, "JXS cam3 src")["first_growth"].startswith("20:05:23.067")


def test_real_stream_snv_fails_on_the_program_input():
    read = _fixture("stream-snv-18-11-2026.txt")
    res = decide(read, declared=("JXS PGM src",), name="2026-11-18 14-56-39.txt")
    assert res["verdict"] == "FAIL"
    pgm = _inp(res, "JXS PGM src")
    assert pgm["counters"]["underruns"]["delta"] > 4000
    assert pgm["jxs"]["lossy_windows"] == 36  # the first of the 37 is the decoder-open window
    assert pgm["verdict"] == "FAIL"


# --- the block ----------------------------------------------------------------------------------

def test_combine_fail_wins_and_the_block_gates(tmp_path):
    ok = decide(raw_read(session()))
    bad = decide(raw_read(session(edits={("audit", "JXS cam3 src", 300): {"u": 1}})))
    bad["box"] = "stream"
    unk = gsc.unknown_box("stream", "x", [])
    assert gsc.combine([ok, unk])["verdict"] == "UNKNOWN"
    blk = gsc.combine([ok, bad])
    assert blk["verdict"] == "FAIL" and blk["pass"] is False and blk["gates_overall_pass"] is True
    assert any(f.startswith("stream ") for f in blk["failures"])
    assert gsc.combine([ok])["pass"] is True
    v = tmp_path / "verdict.json"
    v.write_text(json.dumps({"overall_pass": True}))
    gsc.merge_into(str(v), blk)
    assert json.loads(v.read_text())["genlock_session_continuity"]["verdict"] == "FAIL"
    text = "\n".join(gsc.summary_lines(blk))
    assert "underruns +1" in text and "FAIL" in text


def test_the_cli_round_trip(tmp_path):
    rd = tmp_path / "strih.read"
    rd.write_text(raw_read(session(edits={("audit", "JXS cam3 src", 300): {"u": 2}})))
    out = tmp_path / "strih.json"
    py = str(REPO / "scripts/genlock_session_continuity.py")
    subprocess.run([sys.executable, py, "--box", "strih", "--read", str(rd), "--input", "JXS cam3 src",
                    "--log-name", LOG, "--start-byte", "0", "--stop-byte", "10", "--box-out", str(out)],
                   check=True, capture_output=True)
    unk = tmp_path / "stream.json"
    subprocess.run([sys.executable, py, "--box", "stream", "--unknown", "no mark", "--box-out", str(unk)],
                   check=True, capture_output=True)
    blk = tmp_path / "block.json"
    r = subprocess.run([sys.executable, py, "--combine", str(out), str(unk), "--out", str(blk)],
                       capture_output=True, text=True)
    assert r.returncode == 1
    r = subprocess.run([sys.executable, py, "--from-block", str(blk)], capture_output=True, text=True)
    assert r.returncode == 1 and "underruns +2" in r.stdout


# --- the run's own camera handling (E2E 44502999091, 19.11.2026) ---------------------------------
# The E2E restarts the camboxes into burn units and back. The strih camera inputs then repeat frames
# for a moment: the RUN's own doing, recorded as byte ranges of the strih log (`touch`), never a loss
# of the production session. The recording window itself is graded by the window gates.

def _byte_at(text, second):
    """The byte offset of the first line written at T0 + second."""
    off = 0
    for line in text.split("\n"):
        if line.startswith(ts(T0 + second)) or line.startswith(ts(T0 + second - 0.05)):
            return off
        off += len(line.encode()) + 1
    raise AssertionError(second)


def _decide_touch(text, touches, declared=("JXS cam3 src",), inputs=None):
    read = raw_read(text)
    stop = int(read.split("\n", 1)[0].split("\t")[2]) - 1
    return gsc.decide_box("strih", read, list(declared), log_name=LOG, start_byte=0, stop_byte=stop,
                          touches=touches)


def test_underruns_inside_the_runs_own_camera_handling_are_excluded():
    text = session(minutes=20, edits={("audit", "JXS cam3 src", 400): {"u": 15},
                                       ("audit", "JXS cam3 src", 405): {"u": 52786}})
    res = _decide_touch(text, [(_byte_at(text, 390), _byte_at(text, 600))])
    assert res["verdict"] == "PASS", res["failures"]
    assert _inp(res, "JXS cam3 src")["context"]["run_touch_excluded_underruns"] == 52786


def test_a_loss_before_the_runs_camera_handling_still_fails():
    text = session(minutes=20, edits={("audit", "JXS cam3 src", 200): {"u": 2},
                                       ("audit", "JXS cam3 src", 400): {"u": 900}})
    res = _decide_touch(text, [(_byte_at(text, 390), _byte_at(text, 600))])
    assert res["verdict"] == "FAIL"
    assert any("underruns +2" in f for f in res["failures"]), res["failures"]


def test_a_loss_past_the_touch_settle_still_fails():
    text = session(minutes=20, edits={("audit", "JXS cam3 src", 400): {"u": 10},
                                       ("audit", "JXS cam3 src", 900): {"u": 13}})
    res = _decide_touch(text, [(_byte_at(text, 390), _byte_at(text, 600))])
    assert res["verdict"] == "FAIL"
    assert any("underruns +3" in f for f in res["failures"]), res["failures"]


def test_an_open_touch_excludes_everything_after_it():
    text = session(minutes=20, edits={("audit", "JXS cam3 src", 400): {"u": 10},
                                       ("audit", "JXS cam3 src", 900): {"u": 13}})
    res = _decide_touch(text, [(_byte_at(text, 390), None)])
    assert res["verdict"] == "PASS", res["failures"]


def test_lossy_jpeg_xs_windows_inside_the_touch_are_excluded():
    text = session(minutes=20, edits={("jxs", "JXS cam3 src", 400): {"lost": 21, "conc": 1}})
    res = _decide_touch(text, [(_byte_at(text, 390), _byte_at(text, 600))])
    assert res["verdict"] == "PASS", res["failures"]


def test_a_touch_never_excludes_a_non_camera_input():
    text = session(minutes=20, names=("Zaloha kamera",), edits={("audit", "Zaloha kamera", 400): {"u": 5}})
    res = _decide_touch(text, [(_byte_at(text, 390), _byte_at(text, 600))], declared=())
    assert res["verdict"] == "FAIL"
    assert any("'Zaloha kamera' underruns +5" in f for f in res["failures"]), res["failures"]


def test_the_cli_takes_touch_ranges(tmp_path):
    text = session(minutes=20, edits={("audit", "JXS cam3 src", 400): {"u": 15}})
    read = tmp_path / "r.txt"
    read.write_text(raw_read(text))
    out = tmp_path / "o.json"
    rc = subprocess.run([sys.executable, str(REPO / "scripts/genlock_session_continuity.py"), "--box", "strih",
                         "--read", str(read), "--input", "JXS cam3 src", "--log-name", LOG,
                         "--stop-byte", "10", "--touch", f"{_byte_at(text, 390)}-{_byte_at(text, 600)}",
                         "--box-out", str(out)], capture_output=True, text=True)
    assert rc.returncode == 0, rc.stderr
    assert json.loads(out.read_text())["verdict"] == "PASS"


def test_a_stale_open_touch_of_another_run_excludes_one_hour_only():
    """A run that died before its cleanup leaves its touch open; it must never hide a later loss."""
    text = session(minutes=90, edits={("audit", "JXS cam3 src", 400): {"u": 10},
                                       ("audit", "JXS cam3 src", 4800): {"u": 12}})
    res = _decide_touch(text, [(_byte_at(text, 390), "stale")])
    assert res["verdict"] == "FAIL"
    assert any("underruns +2" in f for f in res["failures"]), res["failures"]
