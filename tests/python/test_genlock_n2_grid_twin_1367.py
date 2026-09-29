"""Issue 1367 slice D1b -- the dev1 Python twin of the grid-exact N>=2 present age.

Since D1 an N>=2 input (a 60 fps camera into the 30 fps strih canvas) presents
`grid_floor(T - GENLOCK_N2_AGE_BASE_NS - pin, canvas / N)`, so its present age is a pure function of
the pin: 66.7 ms at pins 1..16 ms, one source frame more per source interval of pin. The dev1
consumers read `scripts/genlock_n2_grid.py` for it. Pinned to the Rust authority two ways:
  * the constant is READ from src/genlock_n2_grid.rs (the av_soak_decision.py pattern, never retyped);
  * the present-age table tests/fixtures/genlock_n2_present_age_1367.tsv is read HERE and by the Rust
    test `present_age_table_shared_with_the_python_twin_1367` in src/genlock_n2_grid.rs.
Which inputs run the grid is read from the raw audit lines (`n2_early=` on every line + N >= 2).
"""
import json
import pathlib
import sys

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_SCRIPTS = _ROOT / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import genlock_audit_snapshot as gas  # noqa: E402
import genlock_n2_grid as g  # noqa: E402

TSV = _ROOT / "tests" / "fixtures" / "genlock_n2_present_age_1367.tsv"
D1_DIR = _ROOT / "tests" / "fixtures" / "genlock_n2_d1_audit_1367" / "recording-e2e-1367901"
D1_LOG = D1_DIR / "qr-align-strih-1367901.log"
D1_JSON = D1_DIR / "qr-align-jitter-1367901.json"
PRE_D1 = [_ROOT / "tests" / "fixtures" / "arrival_floor_1168" / f"recording-e2e-{r}"
          / f"qr-align-strih-{r}.log" for r in ("659887078", "1363366080")]

I30 = 33_333_333          # the 30 fps canvas interval (1e9 / 30, integer)
I60 = 16_666_666          # its 60 fps source interval (canvas / 2) -- the Rust n2_source_interval_ns
S = 1_790_640_000_000_000_000   # a whole second (the Rust tests' S)


def _rows():
    out = []
    for line in TSV.read_text(encoding="utf-8").splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        f = line.split("\t")
        out.append((int(f[0]), int(f[1]), int(f[2]), int(f[3]), f[4] if len(f) > 4 else ""))
    return out


# --------------------------------------------------------------------------- the constant
class TestAgeBaseFromTheRustSource:
    def test_read_from_src_genlock_n2_grid_rs_never_retyped(self):
        text = (_ROOT / "src" / "genlock_n2_grid.rs").read_text(encoding="utf-8")
        assert g.parse_rust_u64_const(text, "GENLOCK_N2_AGE_BASE_NS") == 50_000_000
        assert g.load_age_base_ns() == 50_000_000
        # the twin keeps no literal copy of the value (the av_soak "never retyped" rule)
        src = (_SCRIPTS / "genlock_n2_grid.py").read_text(encoding="utf-8")
        assert "50_000_000" not in src and "50000000" not in src

    def test_a_changed_rust_constant_moves_the_twin(self, tmp_path):
        rs = (_ROOT / "src" / "genlock_n2_grid.rs").read_text(encoding="utf-8")
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "genlock_n2_grid.rs").write_text(
            rs.replace("GENLOCK_N2_AGE_BASE_NS: u64 = 50_000_000;",
                       "GENLOCK_N2_AGE_BASE_NS: u64 = 60_000_000;"), encoding="utf-8")
        base = g.load_age_base_ns(str(tmp_path))
        assert base == 60_000_000
        # 60 + 3 = 63 ms -> still four 60 fps frames; 60 + 7 = 67 ms -> five
        assert g.present_age_ns(3, I60, age_base_ns=base) == 66_666_666
        assert g.present_age_ns(7, I60, age_base_ns=base) == 83_333_333

    def test_a_missing_constant_fails_closed(self, tmp_path):
        (tmp_path / "src").mkdir()
        (tmp_path / "src" / "genlock_n2_grid.rs").write_text("// no constant here\n", encoding="utf-8")
        with pytest.raises(g.N2GridConstantError):
            g.load_age_base_ns(str(tmp_path))
        with pytest.raises(g.N2GridConstantError):
            g.load_age_base_ns(str(tmp_path / "absent"))

    def test_the_parser_is_exact_name_and_u64(self):
        text = "pub const GENLOCK_N2_AGE_BASE_NS_X: u64 = 7;\npub const GENLOCK_N2_AGE_BASE_NS: u64 = 1_000;\n"
        assert g.parse_rust_u64_const(text, "GENLOCK_N2_AGE_BASE_NS") == 1000
        assert g.parse_rust_u64_const("pub const A: f64 = 1.5;", "A") is None


# --------------------------------------------------------------------------- the shared table
class TestPresentAgeTableSharedWithRust:
    def test_the_table_has_the_rows_the_rust_test_requires(self):
        assert len(_rows()) >= 20

    @pytest.mark.parametrize("row", _rows(), ids=lambda r: r[4] or str(r[0]))
    def test_twin_present_age_equals_the_table(self, row):
        pin_ns, canvas, n, age, _note = row
        si = g.source_interval_ns(canvas, n)
        assert g.present_age_ns_for_pin_ns(pin_ns, si) == age

    @pytest.mark.parametrize("row", _rows(), ids=lambda r: r[4] or str(r[0]))
    def test_every_tick_of_the_python_grid_port_agrees(self, row):
        # the same check the Rust test makes, on the twin's own grid port: every canvas tick of one
        # second presents the table age or one ns more (the per-second grid alternates slot lengths)
        pin_ns, canvas, n, age, _note = row
        fps = g.integer_fps(canvas)
        ticks = ([S + j * g.NS_PER_SECOND // fps for j in range(fps)] if fps
                 else [(S // canvas + j) * canvas for j in range(30)])
        for t in ticks:
            got = t - g.target_stamp_ns(t, pin_ns, canvas, n)
            assert got in (age, age + 1), (row, t, got)


# ------------------------------------------------ the Rust test values, quoted (src/genlock_n2_grid.rs)
class TestRustTestValues:
    def test_production_pin_targets_four_source_frames_back(self):
        # Rust production_pin_targets_four_source_frames_back: every 30 fps tick of a second targets a
        # 60 fps grid point 66_666_666..=66_666_667 ns back
        for j in range(30):
            t = S + j * 1_000_000_000 // 30
            target = g.target_stamp_ns(t, 3_000_000, I30, 2)
            assert g.grid_floor_ns(target, I60) == target
            assert 66_666_666 <= t - target <= 66_666_667
        assert g.present_age_ms(3, I60) == pytest.approx(66.667, abs=1e-3)

    def test_one_source_interval_of_pin_is_exactly_one_frame(self):
        # Rust one_source_interval_of_pin_is_exactly_one_frame: pin = 3 ms + k x 16_666_667 ns
        base = g.present_age_ns_for_pin_ns(3_000_000, I60)
        for k in range(40):
            got = g.present_age_ns_for_pin_ns(3_000_000 + k * 16_666_667, I60)
            assert (got - base + I60 // 2) // I60 == k
        for pin_ms in range(1, 17):
            assert g.present_age_ns(pin_ms, I60) == base
        assert g.present_age_ns(17, I60) == 83_333_333

    def test_source_interval_is_the_canvas_interval_over_n(self):
        assert g.source_interval_ns(I30, 2) == I60
        assert g.source_interval_ns(I30, 3) == 11_111_111
        assert g.source_interval_ns(I30, 0) == 0


# --------------------------------------------------------------------------- grid pin arithmetic
class TestPinArithmeticOnTheGrid:
    def test_a_whole_frame_step_moves_exactly_one_frame_from_every_pin(self):
        for cur in range(1, 400):
            f0 = g.present_age_frames(cur, I60)
            for k in range(0, 6):
                pin = g.pin_for_frames(cur, k, I60)
                assert g.present_age_frames(pin, I60) == f0 + k, (cur, k, pin)

    def test_pin_for_frames_is_current_plus_whole_intervals(self):
        assert g.pin_for_frames(3, 1, I60) == 20
        assert g.pin_for_frames(3, 2, I60) == 36
        assert g.pin_for_frames(16, 1, I60) == 33      # top of the band stays in the next band
        assert g.pin_for_frames(3, 0, I60) == 3

    def test_frames_for_hold_rounds_to_the_nearest_frame(self):
        assert g.frames_for_hold(0.0, I60) == 0
        assert g.frames_for_hold(8.0, I60) == 0
        assert g.frames_for_hold(9.0, I60) == 1
        assert g.frames_for_hold(16.7, I60) == 1
        assert g.frames_for_hold(28.0, I60) == 2
        assert g.frames_for_hold(33.3, I60) == 2

    def test_a_non_whole_hold_added_to_the_pin_does_not_move_whole_frames_predictably(self):
        # WHY the plan rounds: current_pin + hold moves 0 frames for a 13 ms hold at pin 3, a whole
        # frame for 14 ms, and a whole frame for a 1 ms hold at pin 16.
        assert g.present_age_frames(3 + 13, I60) == g.present_age_frames(3, I60)
        assert g.present_age_frames(3 + 14, I60) == g.present_age_frames(3, I60) + 1
        assert g.present_age_frames(16 + 1, I60) == g.present_age_frames(16, I60) + 1


# --------------------------------------------------------------------------- which inputs run the grid
class TestGridInputsFromAudit:
    def test_the_d1_fixture_confirms_the_seven_cameras_and_not_the_n1_inputs(self):
        grid = g.grid_inputs_from_audit(D1_LOG.read_text(encoding="utf-8"))
        assert grid == {f"NDI cam{n}": I60 for n in range(1, 8)}
        c = g.classify_audit_inputs(D1_LOG.read_text(encoding="utf-8"))
        assert c["cg"]["n"] == 1 and c["cg"]["n2_marker"] and not c["cg"]["grid"]
        assert c["NDI 2ME PGM (mv)"]["grid"] is False

    @pytest.mark.parametrize("path", PRE_D1, ids=lambda p: p.parent.name)
    def test_a_real_pre_d1_log_has_no_grid_input_although_its_cameras_are_n2(self, path):
        text = path.read_text(encoding="utf-8", errors="replace")
        c = g.classify_audit_inputs(text)
        assert c["NDI cam1"]["n"] == 2                  # a 60-into-30 camera ...
        assert c["NDI cam1"]["n2_marker"] is False      # ... on a pre-D1 build (no n2_early=)
        assert g.grid_inputs_from_audit(text) == {}
        assert g.read_grid_inputs(str(path)) == {}

    def test_a_window_mixing_a_pre_d1_line_is_not_confirmed(self):
        lines = D1_LOG.read_text(encoding="utf-8").splitlines()
        cam1 = [ln for ln in lines if "audit 'NDI cam1'" in ln]
        mixed = "\n".join([cam1[0].replace(" n2_early=2 ", " ")] + cam1[1:])
        assert g.grid_inputs_from_audit(mixed) == {}

    def test_one_line_uses_the_cumulative_ratio_and_no_line_is_empty(self):
        cam1 = [ln for ln in D1_LOG.read_text(encoding="utf-8").splitlines()
                if "audit 'NDI cam1'" in ln]
        assert g.grid_inputs_from_audit(cam1[-1]) == {"NDI cam1": I60}
        assert g.grid_inputs_from_audit("") == {}
        assert g.grid_inputs_from_audit(None) == {}

    @staticmethod
    def _cam1_lines(pairs):
        """cam1's D1 audit line re-counted: one line per (received, consumed) pair."""
        import re
        line = [ln for ln in D1_LOG.read_text(encoding="utf-8").splitlines()
                if "audit 'NDI cam1'" in ln][0]
        out = []
        for rec, con in pairs:
            ln = re.sub(r"received=\d+", f"received={rec}", line)
            out.append(re.sub(r"consumed=\d+", f"consumed={con}", ln))
        return "\n".join(out)

    def test_a_non_integral_rate_ratio_is_not_confirmed(self):
        # review finding (D1b): round-half-up turned a 1.5 ratio into N = 2. A ratio more than a
        # quarter away from an integer is inconclusive, never a guessed multiple.
        assert g.grid_inputs_from_audit(self._cam1_lines([(1000, 1000), (1300, 1200)])) == {}
        c = g.classify_audit_inputs(self._cam1_lines([(1000, 1000), (1300, 1200)]))
        assert c["NDI cam1"]["n"] is None
        # a few holds keep a real 60-into-30 pair near 2 (2.2 here): still confirmed
        assert g.grid_inputs_from_audit(self._cam1_lines([(1000, 500), (1330, 650)])) == {
            "NDI cam1": I60}

    def test_a_just_restarted_cumulative_count_is_inconclusive(self):
        # one line with received=3 consumed=2 (1.5 -> 2 under plain rounding) and one with too few
        # consumed ticks to trust: neither confirms a grid input
        assert g.grid_inputs_from_audit(self._cam1_lines([(3, 2)])) == {}
        assert g.grid_inputs_from_audit(self._cam1_lines([(40, 20)])) == {}
        assert g.grid_inputs_from_audit(self._cam1_lines([(120, 60)])) == {"NDI cam1": I60}

    def test_the_n2_early_rate_over_the_window_ticks(self):
        # the design budget: n2_early delta / ticks <= 0.1 %; ticks = consumed + holds + late holds
        c = g.classify_audit_inputs(D1_LOG.read_text(encoding="utf-8"))
        assert c["NDI cam3"]["n2_early_rate"] == pytest.approx(2 / 300)
        assert c["NDI cam1"]["n2_early_rate"] == 0.0
        assert g.classify_audit_inputs(self._cam1_lines([(120, 60)]))["NDI cam1"][
            "n2_early_rate"] is None                         # one line: no window, no rate

    @staticmethod
    def _cam1_counted(rows):
        """cam1's D1 audit line with the given counters overridden, one line per dict."""
        import re
        line = [ln for ln in D1_LOG.read_text(encoding="utf-8").splitlines()
                if "audit 'NDI cam1'" in ln][0]
        out = []
        for counters in rows:
            ln = line
            for key, val in counters.items():
                ln = re.sub(r"(?<![a-z_])" + key + r"=\d+", f"{key}={val}", ln)
            out.append(ln)
        return "\n".join(out)

    def test_underrun_ticks_count_in_the_n2_early_rate(self):
        # review round 2: an empty-queue tick (underruns=) is a tick too; leaving it out overstated
        # the rate in a stall window against the "0.1 % of the ticks" budget
        text = self._cam1_counted([
            {"received": 1000, "consumed": 500, "underruns": 0, "n2_early": 0},
            {"received": 1300, "consumed": 640, "underruns": 10, "n2_early": 1}])
        assert g.classify_audit_inputs(text)["NDI cam1"]["n2_early_rate"] == pytest.approx(1 / 150)

    def test_a_stalling_camera_keeps_its_rate_multiple(self):
        # review round 2: held ticks present nothing, so received / consumed read 2.31 on a real
        # 60-into-30 camera with 13 % holds and dropped it to the head-skew model. Every tick
        # (consumed + holds + late holds + underruns) takes two 60 fps frames: 300 / 150 = 2.
        window = self._cam1_counted([
            {"received": 1000, "consumed": 500, "holds": 0, "late_holds": 0},
            {"received": 1300, "consumed": 630, "holds": 12, "late_holds": 8}])
        assert g.grid_inputs_from_audit(window) == {"NDI cam1": I60}
        one_line = self._cam1_counted([
            {"received": 240, "consumed": 100, "holds": 15, "late_holds": 0, "underruns": 5}])
        assert g.grid_inputs_from_audit(one_line) == {"NDI cam1": I60}

    def test_the_d1_jitter_json_is_what_genlock_jitter_report_makes_of_the_log(self):
        # the fixture JSON is hand-derived from its log; keep it honest: samples = line count,
        # latency_ms = the last line's, mean_head_skew_ms = the mean of ts_head_skew_ms
        per = {}
        for line in D1_LOG.read_text(encoding="utf-8").splitlines():
            parsed = gas.parse_audit_line(line)
            if parsed:
                per.setdefault(parsed[0], []).append(parsed[1])
        jj = json.loads(D1_JSON.read_text(encoding="utf-8"))
        assert set(jj) == set(per)
        for src, rows in per.items():
            assert jj[src]["samples"] == len(rows)
            assert jj[src]["latency_ms"] == rows[-1]["latency_ms"]
            skews = [r["ts_head_skew_ms"] for r in rows]
            assert jj[src]["mean_head_skew_ms"] == pytest.approx(sum(skews) / len(skews))
            assert jj[src]["max_abs_head_skew_ms"] == max(abs(s) for s in skews)

    def test_the_d1_heads_sit_one_source_frame_older_than_the_target(self):
        # the design's D1 shape: latency + mean head skew reads about 86 ms (target 66.7 + 16.7 + pin)
        jj = json.loads(D1_JSON.read_text(encoding="utf-8"))
        for n in range(1, 8):
            e = jj[f"NDI cam{n}"]
            old = e["latency_ms"] + e["mean_head_skew_ms"]
            assert 85.0 <= old <= 88.0
            assert old - g.present_age_ms(e["latency_ms"], I60) == pytest.approx(16.7 + 3, abs=1.5)


class TestAuditTokenizerShared:
    def test_parse_audit_counters_is_unchanged_over_a_real_pre_d1_log(self):
        text = PRE_D1[0].read_text(encoding="utf-8", errors="replace")
        last = gas.parse_audit_counters(text)
        assert last["NDI cam1"]["received"] == 1649994      # the last cam1 line of the real log
        assert "n2_early" not in last["NDI cam1"]
        assert gas.parse_audit_line("not an audit line") is None
        assert gas.parse_audit_line("genlock-fifo audit 'unterminated") is None
