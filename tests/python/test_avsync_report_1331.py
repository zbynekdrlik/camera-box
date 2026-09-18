"""#1331 -- unit tests for scripts/avsync_report.py, the PURE verified-A/V-sync SESSION report
decider (the owner's 18.9.2026 request: report that the live A/V was VERIFIED across the whole
broadcast and whether it FITS, not a lone unaggregated clip).

Pure-python (no I/O, no subprocess) -- imports the decider directly, the SAME `sys.path.insert`
convention tests/python/test_avsync_lineup.py uses. Confidence floor + in-band threshold are the
ones IMPORTED from avsync_lineup.py (never retyped), so these tests also pin that they stayed
single-sourced (the 17.9. conf 3.2 line is NOT a verdict; the conf 5.4 line IS).
"""

import pathlib
import sys

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import avsync_lineup as al  # noqa: E402
import avsync_report as ar  # noqa: E402


def _measured(offset_ms, conf, stamp="2026-09-17 19:00:59", db=-5.0):
    """A realistic `measured:` heartbeat status carrying a parseable AV offset + conf. offset_frames
    is display-only (parse_offset reads the ms + conf), so a fixed +N fr is fine."""
    fr = offset_ms // 40 if offset_ms else 0
    return "measured: db={} [{}] AV offset {:+d} fr ({:+d} ms) conf {:.1f} :: x".format(
        db, stamp, fr, offset_ms, conf
    )


def _unmeasurable(db=-5.0, stamp="2026-09-17 19:00:59"):
    return "measured: db={} [{}] UNMEASURABLE window (best confidence 1.2 < 3.0)".format(db, stamp)


def _no_signal():
    return "no-signal: clip too small (94869 B)"


def _run(start, count, step=90, offset_ms=0, conf=6.0):
    return [(start + i * step, _measured(offset_ms, conf)) for i in range(count)]


BASE = 1_800_000_000  # a fixed epoch far from any tz DST edge relevance for these structural tests


# ── the pure sub-helpers ─────────────────────────────────────────────────────


def test_confident_filter_uses_the_imported_floor_not_a_retyped_one():
    # The floor is avsync_lineup.OFFSET_CONF_FLOOR (4.0) -- imported, never a local literal.
    assert ar.OFFSET_CONF_FLOOR is al.OFFSET_CONF_FLOOR
    assert ar.OFFSET_ALARM_MS_DEFAULT is al.OFFSET_ALARM_MS_DEFAULT
    # The 17.9. lines the owner quoted: conf 5.4 IS a confident clip, conf 3.2 is NOT.
    row_54 = ar._Row(BASE, _measured(120, 5.4))
    row_32 = ar._Row(BASE, _measured(80, 3.2))
    assert row_54.confident is True
    assert row_32.confident is False
    # exactly at the floor counts as usable (>=)
    assert ar._Row(BASE, _measured(0, 4.0)).confident is True


def test_unmeasurable_and_no_signal_rows_are_never_confident():
    assert ar._Row(BASE, _unmeasurable()).confident is False
    assert ar._Row(BASE, _no_signal()).measured is False
    assert ar._Row(BASE, _no_signal()).confident is False


def test_session_split_on_the_600s_gap():
    rows = ar._parse_rows(
        _run(BASE, 3) + _run(BASE + 3 * 90 + 700, 2)  # a 700 s gap (> 600) splits
    )
    sessions = ar._split_sessions([r for r in rows if r.measured])
    assert len(sessions) == 2
    assert len(sessions[0]) == 3 and len(sessions[1]) == 2
    # a sub-600 gap does NOT split
    rows2 = ar._parse_rows(_run(BASE, 2) + _run(BASE + 2 * 90 + 500, 2))
    assert len(ar._split_sessions([r for r in rows2 if r.measured])) == 1


def test_median_p10_p90():
    vals = [0, 40, 80, 120, 160]
    assert ar._median(vals) == 80
    assert ar._percentile(sorted(vals), 10) == 16.0
    assert ar._percentile(sorted(vals), 90) == 144.0
    txt = ar._median_paren(vals)
    assert "medián +80 ms" in txt
    assert "p10 +16" in txt and "p90 +144" in txt


def test_advice_text_parity_with_av_sync_measure_for_plus_120ms():
    # av_sync_measure.py prints "ZNIZ '2ME PGM' latency o 120" for +120 ms (audio ahead).
    assert ar._advice(120) == "ZNIZ '2ME PGM' latency o 120"
    assert ar._advice(-40) == "ZVYS '2ME PGM' latency o 40"
    assert ar._advice(0) == "A/V sync OK"


# ── decide(): START ──────────────────────────────────────────────────────────


def test_start_fires_once_on_the_first_confident_clip():
    rows = _run(BASE, 1, offset_ms=120, conf=5.4)
    msgs, st = ar.decide(rows, {}, BASE + 10)
    assert any("📐 A/V overené" in m and "začiatok vysielania" in m for m in msgs)
    assert any("1. meranie +120 ms conf 5.4" in m for m in msgs)
    assert st["start_posted"] is True
    # idempotent: the same rows + returned state emit nothing new.
    msgs2, _st2 = ar.decide(rows, st, BASE + 10)
    assert msgs2 == []


def test_start_does_not_fire_when_the_only_clips_are_low_confidence():
    rows = _run(BASE, 2, offset_ms=80, conf=3.2)
    msgs, st = ar.decide(rows, {}, BASE + 100)
    assert not any("začiatok vysielania" in m for m in msgs)
    assert st["start_posted"] is False


# ── decide(): PERIODIC (20-min) ──────────────────────────────────────────────


def test_periodic_summary_after_20_minutes_sedi():
    # >= 3 confident clips within the trailing 1200 s window, median 0 -> SEDÍ.
    rows = _run(BASE, 14, step=90, offset_ms=0, conf=6.0)  # 14*90 = 1260 s of clips
    now = BASE + 1200
    msgs, st = ar.decide(rows, {}, now)
    periodic = [m for m in msgs if "za 20 min" in m]
    assert len(periodic) == 1
    assert "→ SEDÍ" in periodic[0]
    assert "medián +0 ms" in periodic[0]
    assert st["last_verdict"] == "SEDI"


def test_periodic_summary_nesedi_carries_the_knob_advice():
    rows = _run(BASE, 14, step=90, offset_ms=120, conf=6.0)
    now = BASE + 1200
    msgs, _st = ar.decide(rows, {}, now)
    periodic = [m for m in msgs if "za 20 min" in m]
    assert len(periodic) == 1
    assert "NESEDÍ o 120 ms" in periodic[0]
    assert "ZNIZ '2ME PGM' latency o 120" in periodic[0]


def test_periodic_counts_nemerateľne_clips():
    # 5 confident + 3 unmeasurable within the window -> N=8, K=3.
    rows = _run(BASE, 5, offset_ms=0, conf=6.0)
    rows += [(BASE + 5 * 90 + i * 90, _unmeasurable()) for i in range(3)]
    now = BASE + 1200
    msgs, _st = ar.decide(rows, {}, now)
    periodic = [m for m in msgs if "za 20 min" in m][0]
    assert "8 meraní (3 nemerateľných)" in periodic


def test_periodic_below_three_confident_has_no_verdict():
    # Only 2 confident clips, but the session stays LIVE (unmeasurable rows keep flowing to near now).
    rows = _run(BASE, 2, offset_ms=0, conf=6.0)
    rows += [(BASE + 180 + i * 90, _unmeasurable()) for i in range(12)]  # last ~BASE+1170
    now = BASE + 1200
    msgs, st = ar.decide(rows, {}, now)
    periodic = [m for m in msgs if "za 20 min" in m][0]
    assert "bez verdiktu" in periodic
    assert st["last_verdict"] is None


# ── decide(): CHANGE ─────────────────────────────────────────────────────────


def test_change_fires_immediately_when_the_verdict_flips():
    # First evaluation: a SEDÍ window.
    rows1 = _run(BASE, 14, step=90, offset_ms=0, conf=6.0)
    _m1, st1 = ar.decide(rows1, {}, BASE + 1200)
    assert st1["last_verdict"] == "SEDI"
    # Later: a fresh 20-min worth of clips at +120 ms so the trailing window flips to NESEDÍ.
    later = BASE + 1200
    rows2 = rows1 + _run(later + 90, 14, step=90, offset_ms=120, conf=6.0)
    now2 = later + 90 + 14 * 90
    m2, st2 = ar.decide(rows2, st1, now2)
    assert any("verdikt sa zmenil" in m and "NESEDÍ o 120 ms" in m for m in m2)
    assert st2["last_verdict"] == "NESEDI"
    # idempotent: re-running with the same rows/state does not re-post the CHANGE.
    m3, _st3 = ar.decide(rows2, st2, now2)
    assert not any("verdikt sa zmenil" in m for m in m3)


def test_no_change_on_the_first_definite_verdict():
    rows = _run(BASE, 14, offset_ms=120, conf=6.0)
    msgs, _st = ar.decide(rows, {}, BASE + 1200)
    assert not any("verdikt sa zmenil" in m for m in msgs)


# ── decide(): END ────────────────────────────────────────────────────────────


def test_end_summary_when_the_session_stops_for_600s():
    rows = _run(BASE, 14, step=90, offset_ms=120, conf=6.0)
    se = BASE + 13 * 90
    now = se + 700  # no measured row for >600 s -> the session has ended
    msgs, st = ar.decide(rows, {}, now)
    end = [m for m in msgs if "📐 A/V počas vysielania" in m]
    assert len(end) == 1
    assert "NESEDÍ o 120 ms" in end[0]
    assert "ZNIZ '2ME PGM' latency o 120" in end[0]
    assert st["end_posted"] is True
    # idempotent
    msgs2, _st2 = ar.decide(rows, st, now)
    assert not any("📐 A/V počas vysielania" in m for m in msgs2)


def test_previous_session_end_is_emitted_when_a_new_session_starts():
    # State says session A is being tracked and not yet ended.
    rows_a = _run(BASE, 5, offset_ms=0, conf=6.0)
    _ma, st = ar.decide(rows_a, {}, BASE + 5 * 90)  # A is live
    assert st["end_posted"] is False and st["session_start"] == BASE
    # A new session B starts 700 s after A's last clip; the fetch still carries A's rows.
    b_start = BASE + 4 * 90 + 700
    rows_ab = rows_a + _run(b_start, 3, offset_ms=120, conf=6.0)
    msgs, st2 = ar.decide(rows_ab, st, b_start + 2 * 90)
    assert any("📐 A/V počas vysielania" in m for m in msgs), "session A's END must be emitted"
    assert st2["session_start"] == b_start


# ── state cursor ─────────────────────────────────────────────────────────────


def test_cursor_advances_to_the_max_row_epoch():
    rows = _run(BASE, 4, offset_ms=0, conf=6.0)
    _m, st = ar.decide(rows, {}, BASE + 4 * 90)
    assert st["last_row_epoch"] == BASE + 3 * 90


def test_no_measured_rows_is_a_noop_but_advances_the_cursor():
    rows = [(BASE, _no_signal()), (BASE + 90, _no_signal())]
    msgs, st = ar.decide(rows, {}, BASE + 100)
    assert msgs == []
    assert st["last_row_epoch"] == BASE + 90


# ── CLI wrapper ──────────────────────────────────────────────────────────────


def test_cli_prints_messages_and_rewrites_state(tmp_path, capsys):
    rows_file = tmp_path / "rows.tsv"
    state_file = tmp_path / "state.json"
    with open(rows_file, "w", encoding="utf-8") as fh:
        for epoch, status in _run(BASE, 1, offset_ms=120, conf=5.4):
            fh.write("{}\t{}\n".format(epoch, status))
    rc = ar.main(["--state", str(state_file), "--rows", str(rows_file), "--now", str(BASE + 10)])
    assert rc == 0
    out = capsys.readouterr().out
    assert "📐 A/V overené" in out and "začiatok vysielania" in out
    assert state_file.exists()
    import json

    st = json.loads(state_file.read_text(encoding="utf-8"))
    assert st["start_posted"] is True
    assert st["last_row_epoch"] == BASE
