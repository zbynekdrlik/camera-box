"""#1354 -- unit tests for scripts/genlock_audit_snapshot.py, the pure genlock-fifo audit
before/after DELTA parser that feeds the E2E report's per-input conveyor section (scope 3).

Pure logic only -- no rig, no OBS, no ssh. Parses the `genlock-fifo audit '<name>':` log line
shape the vendored OBS emits (obs-source.c) and computes per-input window deltas.
"""
import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_audit_snapshot as gas  # noqa: E402

# A representative pair of audit tails (the issue-1354 cam4 window shape, trimmed). The vendored
# line carries many more fields; the parser must pick out only the key=value integer tokens.
_BEFORE = (
    "2026-09-22 15:20:00 genlock-fifo audit 'NDI cam4': received=100 consumed=99 underruns=0 "
    "holds=0 overruns=0 backward_steps=0 dropped_due=1 relocks=551 late_holds=0 locked=1 depth=1 "
    "peak=2 latency_ms=36 (≈1 frames @30.000fps) converge_sheds=375 clock_drift_ms=0\n"
    "2026-09-22 15:20:00 genlock-fifo audit 'NDI cam1': received=100 consumed=99 holds=0 "
    "dropped_due=1 relocks=200 converge_sheds=10\n"
)
_AFTER = (
    "2026-09-22 15:37:00 genlock-fifo audit 'NDI cam4': received=200 consumed=150 holds=21 "
    "dropped_due=1808 relocks=639 converge_sheds=650 depth=13\n"
    "2026-09-22 15:37:00 genlock-fifo audit 'NDI cam1': received=200 consumed=198 holds=6 "
    "dropped_due=3 relocks=206 converge_sheds=10\n"
)


def test_parse_picks_the_last_line_per_input():
    text = (
        "genlock-fifo audit 'NDI cam4': holds=1 relocks=2 converge_sheds=3 dropped_due=4\n"
        "genlock-fifo audit 'NDI cam4': holds=9 relocks=8 converge_sheds=7 dropped_due=6\n"
    )
    got = gas.parse_audit_counters(text)
    assert got["NDI cam4"]["holds"] == 9  # last line wins
    assert got["NDI cam4"]["relocks"] == 8


def test_parse_skips_non_integer_decoration_tokens():
    text = "genlock-fifo audit 'NDI cam2': holds=5 latency_ms=36 (≈1 fps=30.0 relocks=7\n"
    got = gas.parse_audit_counters(text)
    assert got["NDI cam2"]["holds"] == 5
    assert got["NDI cam2"]["relocks"] == 7
    assert "(≈1" not in got["NDI cam2"]  # decoration skipped, never a crash


def test_parse_empty_and_no_audit_lines_returns_empty():
    assert gas.parse_audit_counters("") == {}
    assert gas.parse_audit_counters("some unrelated log line\nanother\n") == {}


def test_window_deltas_and_victim():
    before = gas.parse_audit_counters(_BEFORE)
    after = gas.parse_audit_counters(_AFTER)
    d = gas.compute_window_deltas(before, after)
    assert d["inputs"]["NDI cam4"]["holds"] == 21
    assert d["inputs"]["NDI cam4"]["relocks"] == 88  # 639 - 551
    assert d["inputs"]["NDI cam4"]["converge_sheds"] == 275  # 650 - 375
    assert d["inputs"]["NDI cam1"]["holds"] == 6
    assert d["inputs"]["NDI cam1"]["converge_sheds"] == 0
    # cam4 has the most holds -> it is the named victim.
    assert d["victim"] == "NDI cam4"


def test_victim_is_none_when_no_input_accumulated_holds():
    before = gas.parse_audit_counters(
        "genlock-fifo audit 'NDI cam1': holds=5 relocks=5 converge_sheds=1 dropped_due=1\n"
    )
    after = gas.parse_audit_counters(
        "genlock-fifo audit 'NDI cam1': holds=5 relocks=5 converge_sheds=1 dropped_due=99\n"
    )
    d = gas.compute_window_deltas(before, after)
    assert d["victim"] is None  # holds delta 0 -> no ladder victim
    assert d["inputs"]["NDI cam1"]["holds"] == 0
    assert d["inputs"]["NDI cam1"]["dropped_due"] == 98  # context still tracked


def test_victim_tie_breaks_on_relocks():
    before = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=0 relocks=0 converge_sheds=0 dropped_due=0\n"
        "genlock-fifo audit 'B': holds=0 relocks=0 converge_sheds=0 dropped_due=0\n"
    )
    after = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=3 relocks=1 converge_sheds=0 dropped_due=0\n"
        "genlock-fifo audit 'B': holds=3 relocks=9 converge_sheds=0 dropped_due=0\n"
    )
    d = gas.compute_window_deltas(before, after)
    assert d["victim"] == "B"  # equal holds, B has more relocks


def test_input_only_in_after_is_partial_not_fabricated():
    before = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=0 relocks=0 converge_sheds=0 dropped_due=0\n"
    )
    after = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=1 relocks=0 converge_sheds=0 dropped_due=0\n"
        "genlock-fifo audit 'NEW': holds=9 relocks=9 converge_sheds=9 dropped_due=9\n"
    )
    d = gas.compute_window_deltas(before, after)
    assert "NEW" not in d["inputs"]  # no baseline -> never a fabricated delta
    assert d.get("partial") == ["NEW"]


def test_counter_going_backward_is_a_restart_not_a_negative_delta():
    before = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=100 relocks=100 converge_sheds=100 dropped_due=100\n"
    )
    after = gas.parse_audit_counters(
        "genlock-fifo audit 'A': holds=2 relocks=1 converge_sheds=0 dropped_due=5\n"
    )
    d = gas.compute_window_deltas(before, after)
    # OBS restarted between snapshots -> counters reset; deltas clamped at 0, flagged.
    assert d["inputs"]["A"]["holds"] == 0
    assert d.get("restarted") == ["A"]
    assert d["victim"] is None
