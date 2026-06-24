"""#209: unit tests for scripts/latency-line-report.py — the continuous-line latency proof.

The user's original ask is to SEE the per-frame latency as a CONTINUOUS LINE over the
whole run, not just summary p50/p99 stats. recording-verdict (--latency-csv) writes a
per-frame CSV; this plotter turns it into one line per hop. These tests pin:

  * the CSV header the plotter expects equals the Rust LatencyCsvRow::HEADER contract,
  * build_series turns rows into per-hop (time_s, latency_ms) point lists,
  * an EMPTY hop cell contributes NO point to that hop's line (a gap = a lost frame),
  * the plotter produces a non-empty PNG from a synthetic CSV (no rig, no recording).

A SYNTHETIC CSV is fine here per the issue (the real PNG comes from the next rig run).
"""
import csv
import importlib.util
import os
import pathlib
import re
import sys

import pytest

# Import scripts/latency-line-report.py by path (it is a script, not an installed module).
_MOD_PATH = (
    pathlib.Path(__file__).resolve().parents[2] / "scripts" / "latency-line-report.py"
)
_spec = importlib.util.spec_from_file_location("latency_line_report", _MOD_PATH)
llr = importlib.util.module_from_spec(_spec)
sys.modules["latency_line_report"] = llr
_spec.loader.exec_module(llr)


# This MUST match recording_latency::LatencyCsvRow::HEADER (the single source of truth in
# the Rust writer). If the Rust column order changes, this assertion catches the drift.
EXPECTED_HEADER = [
    "frame_id",
    "gen_ts_ns",
    "flip_ts_ns",
    "cam1_strih_ms",
    "strih_stream_ms",
    "cam1_stream_ms",
    "cam2_cam1_ms",  # #216 — the cam2→cam1 OPTICAL-injection leg (gaps honestly on read failure)
]


def _write_csv(path, rows):
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(EXPECTED_HEADER)
        for r in rows:
            w.writerow(r)


def test_plotter_expected_header_matches_the_rust_contract():
    # The plotter's hardcoded EXPECTED_HEADER is the contract with the Rust writer.
    assert llr.EXPECTED_HEADER == EXPECTED_HEADER
    # The hop columns it draws are the three burn hops PLUS the cam2→cam1 optical leg (#216),
    # in flow order. HOP_COLUMNS is now (col, label, color, is_optical).
    cols = [c for c, _label, _color, _opt in llr.HOP_COLUMNS]
    assert cols == [
        "cam1_strih_ms",
        "strih_stream_ms",
        "cam1_stream_ms",
        "cam2_cam1_ms",
    ]
    # Exactly ONE hop is the optical leg (it gaps on read failure); the other three are burns.
    optical = [c for c, _l, _color, opt in llr.HOP_COLUMNS if opt]
    assert optical == ["cam2_cam1_ms"], "only cam2→cam1 is the optical-injection leg"


def test_plotter_header_equals_the_rust_LatencyCsvRow_HEADER_const():
    # CROSS-BOUNDARY contract: the plotter's EXPECTED_HEADER MUST equal the Rust
    # LatencyCsvRow::HEADER const (the verdict's CSV column order). Without this, a Rust
    # column reorder/rename stays green in CI (the Rust test pins the const against itself)
    # and only breaks at runtime ON THE RIG when the plotter rejects the real CSV. This
    # test reads the const STRAIGHT FROM THE RUST SOURCE so CI catches the drift.
    rust_src = (
        pathlib.Path(__file__).resolve().parents[2]
        / "src"
        / "probe"
        / "recording_latency.rs"
    )
    text = rust_src.read_text()
    # Match: pub const HEADER: &'static str = "<...>";  (the value may wrap to the next
    # line after `=`, as rustfmt does — capture the first quoted string after HEADER =).
    m = re.search(
        r"pub const HEADER:\s*&'static str\s*=\s*\"([^\"]*)\"\s*;",
        text,
        re.DOTALL,
    )
    assert m, "could not find LatencyCsvRow::HEADER const in recording_latency.rs"
    rust_header_cols = m.group(1).split(",")
    assert rust_header_cols == llr.EXPECTED_HEADER, (
        "Rust LatencyCsvRow::HEADER diverged from the plotter's EXPECTED_HEADER — "
        f"Rust={rust_header_cols!r} plotter={llr.EXPECTED_HEADER!r}. Keep them in lockstep."
    )


def test_build_series_makes_one_point_list_per_hop_on_a_seconds_axis():
    # Three frames, ~33 ms apart on the wall clock, all hops present.
    base = 1_000_000_000  # ns
    rows = [
        {
            "frame_id": "100",
            "gen_ts_ns": str(base),
            "flip_ts_ns": "",
            "cam1_strih_ms": "10.0",
            "strih_stream_ms": "15.0",
            "cam1_stream_ms": "25.0",
            "cam2_cam1_ms": "120.0",
        },
        {
            "frame_id": "102",
            "gen_ts_ns": str(base + 33_000_000),
            "flip_ts_ns": "",
            "cam1_strih_ms": "10.5",
            "strih_stream_ms": "15.2",
            "cam1_stream_ms": "25.7",
            "cam2_cam1_ms": "121.0",
        },
        {
            "frame_id": "104",
            "gen_ts_ns": str(base + 66_000_000),
            "flip_ts_ns": "",
            "cam1_strih_ms": "10.1",
            "strih_stream_ms": "15.0",
            "cam1_stream_ms": "25.1",
            "cam2_cam1_ms": "120.5",
        },
    ]
    series, t0 = llr.build_series(rows)
    assert t0 == base, "origin = first frame's gen_ts_ns"
    xs, ys = series["cam1_strih_ms"]
    assert ys == [10.0, 10.5, 10.1]
    # x axis is seconds since run start: 0, 0.033, 0.066
    assert xs[0] == pytest.approx(0.0)
    assert xs[1] == pytest.approx(0.033, abs=1e-6)
    assert xs[2] == pytest.approx(0.066, abs=1e-6)
    assert series["cam1_stream_ms"][1] == [25.0, 25.7, 25.1]


def _real(ys):
    """The non-NaN data points (NaN entries are pen-lift markers that BREAK the line)."""
    return [y for y in ys if y == y]  # NaN != NaN


def test_build_series_empty_hop_cell_is_a_gap_no_point_for_that_hop():
    # The middle frame is missing strih→stream + cam1→stream + cam2→cam1 (only cam1→strih
    # present) — those hops must NOT plot a real point there (a gap in the line, NOT a 0). The
    # gap is realized as a NaN pen-lift marker (#216) so the line truly BREAKS, never a faint
    # connector drawn across the failure.
    base = 2_000_000_000
    rows = [
        {
            "frame_id": "1",
            "gen_ts_ns": str(base),
            "flip_ts_ns": "",
            "cam1_strih_ms": "10.0",
            "strih_stream_ms": "15.0",
            "cam1_stream_ms": "25.0",
            "cam2_cam1_ms": "120.0",
        },
        {
            "frame_id": "2",
            "gen_ts_ns": str(base + 33_000_000),
            "flip_ts_ns": "",
            "cam1_strih_ms": "11.0",
            "strih_stream_ms": "",  # gap
            "cam1_stream_ms": "",  # gap
            "cam2_cam1_ms": "",  # optical read failed ⇒ gap on the cam2→cam1 line
        },
        {
            "frame_id": "3",
            "gen_ts_ns": str(base + 66_000_000),
            "flip_ts_ns": "",
            "cam1_strih_ms": "10.0",
            "strih_stream_ms": "15.0",
            "cam1_stream_ms": "25.0",
            "cam2_cam1_ms": "120.0",
        },
    ]
    series, _t0 = llr.build_series(rows)
    # cam1→strih present on all three frames (no break).
    assert _real(series["cam1_strih_ms"][1]) == [10.0, 11.0, 10.0]
    # The gapped hops keep only the two REAL points (frames 1 and 3); the empty middle cell is
    # a NaN break, not a real point.
    assert _real(series["strih_stream_ms"][1]) == [15.0, 15.0]
    assert _real(series["cam1_stream_ms"][1]) == [25.0, 25.0]
    # cam2→cam1 OPTICAL leg gaps at the dropout frame too — the honest break (#216).
    assert _real(series["cam2_cam1_ms"][1]) == [120.0, 120.0]
    # A NaN pen-lift marker is present in each gapped hop so the line BREAKS (not connected).
    assert any(y != y for y in series["strih_stream_ms"][1]), "a NaN break exists"
    assert any(y != y for y in series["cam2_cam1_ms"][1]), "cam2→cam1 breaks at the dropout"


def test_optical_gaps_detects_the_cam2_cam1_read_dropout_window():
    # #216 — optical_gaps finds the WINDOW where the cam2→cam1 read failed (consecutive empty
    # cam2_cam1 cells longer than MIN_OPTICAL_GAP_S). The burn hops are present throughout.
    base = 5_000_000_000_000  # ns
    rows = []
    for i in range(40):
        g = base + i * 1_000_000_000  # 1 s apart
        # cam2_cam1 empty for a 5 s stretch (frames 10..14) — an optical-read dropout.
        cam2 = "" if 10 <= i <= 14 else "120.0"
        rows.append(
            {
                "frame_id": "" if cam2 == "" else str(i),
                "gen_ts_ns": str(g),
                "flip_ts_ns": "",
                "cam1_strih_ms": "10.0",  # burns present throughout
                "strih_stream_ms": "15.0",
                "cam1_stream_ms": "25.0",
                "cam2_cam1_ms": cam2,
            }
        )
    _series, t0 = llr.build_series(rows)
    gaps = llr.optical_gaps(rows, t0)
    assert len(gaps) == 1, f"one optical-read dropout window: {gaps}"
    gs, ge, dur = gaps[0]
    assert dur >= 4.0, f"the dropout lasted several seconds: {gaps}"
    assert gs < ge, "window has a positive span"


def test_plotter_produces_a_png_from_a_synthetic_csv(tmp_path):
    # End-to-end: write a synthetic per-frame CSV, run the plotter's main(), assert a
    # non-empty PNG is produced. No rig, no recording, no stream box.
    csv_path = tmp_path / "latency-per-frame.csv"
    base = 3_000_000_000
    rows = []
    for i in range(50):
        g = base + i * 33_000_000
        # A near-flat line with tiny jitter — the "stable latency" the user wants to SEE. The
        # cam2→cam1 optical leg is present except for a short mid-run read dropout (frames
        # 20..29) — the plotter must GAP + annotate it honestly (#216) while the burns continue.
        cam2 = "" if 20 <= i <= 29 else f"{120.0 + (i % 4) * 0.1:.6f}"
        rows.append(
            [
                "" if cam2 == "" else 100 + 2 * i,  # frame_id (empty on optical dropout)
                g,
                "",  # flip_ts empty
                f"{10.0 + (i % 3) * 0.1:.6f}",
                f"{15.0 + (i % 2) * 0.1:.6f}",
                f"{25.0 + (i % 5) * 0.1:.6f}",
                cam2,
            ]
        )
    _write_csv(csv_path, rows)

    out_png = tmp_path / "latency-line.png"
    # Drive the plotter exactly as the operator/CI would, but skip the LAN-share step by
    # monkeypatching _share (it shells out to airuleset, irrelevant to the PNG render).
    llr._share = lambda png: None
    argv = sys.argv
    try:
        sys.argv = [
            "latency-line-report.py",
            "--csv",
            str(csv_path),
            "--out",
            str(out_png),
        ]
        llr.main()
    finally:
        sys.argv = argv

    assert out_png.exists(), "plotter must write the PNG"
    assert out_png.stat().st_size > 1000, "PNG must be non-trivial (real render, not empty)"
    # PNG magic bytes — prove it is actually a PNG, not a stray text file.
    with open(out_png, "rb") as f:
        assert f.read(8) == b"\x89PNG\r\n\x1a\n"


def test_plotter_rejects_a_csv_with_the_wrong_header(tmp_path):
    bad = tmp_path / "bad.csv"
    with open(bad, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["wrong", "columns"])
        w.writerow([1, 2])
    with pytest.raises(SystemExit):
        llr.read_rows(str(bad))
