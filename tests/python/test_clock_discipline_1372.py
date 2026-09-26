"""Issue 1372: camera-box's dantesync consumers must grade the dantesync 1.9.0 clock discipline.

WHY: dantesync 1.9.0 (dantesync#117) made `ptp_phase_lock` the default discipline -- the rate comes
from the PTP tick only, phase slew is not used (`phase_slew_enabled=false`), and
`clock_discipline="legacy"` restores the old behaviour. The NTP master (`date_authority=master`)
now lets the fleet date sit up to `date_step_bound_ms` (50) off UTC and then makes a coordinated
fleet step (dantesync#88). Every camera-box consumer still graded the pre-1.9.0 discipline, so
the E2E `[0/8]` gate refused (rc=20, every node `PHASE-SLEW DISABLED`) and verify-strih check 6
failed the master's `(date authority, ...)` journal line against the 2 ms UTC bound.

The fix (main's design, issue 1372 comment 5840555837, Approach 1) is ONE discipline classifier
and ONE date-master verdict, bash in scripts/clock-offset-guard.sh + a python twin in
scripts/dantesync_fleet.py, pinned by ONE shared table
(tests/fixtures/dantesync_clock_discipline_1372.tsv) whose @ rows are /status captured read-only
from the live 1.9.0 fleet. Every consumer calls them; none re-parses.

Tier-0: bash is driven through `subprocess.run(["bash", <file>])` (no cargo).
"""
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import time

import pytest

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_GUARD = _ROOT / "scripts" / "clock-offset-guard.sh"
_GATE = _ROOT / "scripts" / "dantesync-gate.sh"
_MAINT = _ROOT / "scripts" / "dantesync-maintenance-gate.sh"
_TSV = _ROOT / "tests" / "fixtures" / "dantesync_clock_discipline_1372.tsv"
_STATUS = _ROOT / "tests" / "fixtures" / "dantesync_status_1372"
_CFG = pathlib.Path(__file__).resolve().parent / "fixtures" / "dantesync_config_1372"
_MARGIN_US = 1000
_LIVE_GM = "10.77.9.230"

_spec = importlib.util.spec_from_file_location("dantesync_fleet", _ROOT / "scripts" / "dantesync_fleet.py")
df = importlib.util.module_from_spec(_spec)
sys.modules["dantesync_fleet"] = df
_spec.loader.exec_module(df)

_pspec = importlib.util.spec_from_file_location(
    "dantesync_config_patch", _ROOT / "scripts" / "dantesync_config_patch.py")
dcp = importlib.util.module_from_spec(_pspec)
_pspec.loader.exec_module(dcp)

_CLEAN = ("DANTESYNC_FLEET", "OBS_FLEET", "OBS_FLEET_HOME", "DANTESYNC_AUDIO_GM_HOST",
          "RIG_GRANDMASTER_HOST", "RIG_GRANDMASTER_IP", "DANTESYNC_GATE_GM_ENFORCE",
          "DANTESYNC_GATE_PHASE_SLEW_ENFORCE", "DANTESYNC_NTP_MASTER_NAME",
          "DANTESYNC_DEADBAND_MARGIN_US", "CLOCK_GUARD_BOUND_US", "DANTESYNC_STABILITY_US",
          "DANTESYNC_DATE_MARGIN_US", "DANTESYNC_DATE_MICRO_BOUND_MS")


def _env(**over):
    env = {k: v for k, v in os.environ.items() if k not in _CLEAN}
    env.update(over)
    return env


def _rows():
    rows = []
    for line in _TSV.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        case, status, klass, date_verdict, unlocked = line.split("\t")
        if status.startswith("@"):
            status = (_ROOT / status[1:]).read_text()
        rows.append(pytest.param(status, klass, date_verdict, unlocked, id=case))
    return rows


def _sourced(tmp_path, body, **env):
    """Source clock-offset-guard.sh (its own source-guard defines only the pure functions) and run
    BODY with errexit off, so an rc-returning check can be captured."""
    script = tmp_path / "run.sh"
    script.write_text(f"set -euo pipefail\n. '{_GUARD}'\nset +e\n{body}\n")
    return subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env(**env))


def _bash_call(tmp_path, fn, status, *args):
    (tmp_path / "status.json").write_text(status)
    extra = " ".join(f"'{a}'" for a in args)
    r = _sourced(tmp_path, f'S="$(cat "{tmp_path / "status.json"}")"\n{fn} "$S" {extra}; echo')
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


# ---------------------------------------------------------------------------------------------
# the ONE classifier + the ONE date-master verdict, bash and python pinned by the same table
# ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_bash_classifier_matches_the_table(tmp_path, status, klass, date_verdict, unlocked):
    assert _bash_call(tmp_path, "clock_discipline_class", status) == klass


@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_python_classifier_matches_the_table(status, klass, date_verdict, unlocked):
    assert df.classify_clock_discipline(json.loads(status)) == klass


@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_bash_date_master_verdict_matches_the_table(tmp_path, status, klass, date_verdict, unlocked):
    assert _bash_call(tmp_path, "date_master_verdict", status, _MARGIN_US) == date_verdict


@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_python_date_master_verdict_matches_the_table(status, klass, date_verdict, unlocked):
    assert df.date_master_verdict(json.loads(status), _MARGIN_US) == date_verdict


@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_bash_unlocked_helper_matches_the_table(tmp_path, status, klass, date_verdict, unlocked):
    """The named PTP-PHASE UNLOCKED state is part of the ONE classifier module, never re-derived
    from the raw fields by a consumer (review finding on the first GREEN)."""
    assert _bash_call(tmp_path, "clock_discipline_unlocked", status) == unlocked


@pytest.mark.parametrize("status,klass,date_verdict,unlocked", _rows())
def test_python_unlocked_helper_matches_the_table(status, klass, date_verdict, unlocked):
    assert df.clock_discipline_unlocked(json.loads(status)) is (unlocked == "yes")


def test_the_table_covers_every_class_and_verdict():
    rows = [p.values for p in _rows()]
    assert {r[1] for r in rows} == {"PTP_PHASE_LOCK", "LEGACY_SLEW", "LEGACY_NO_SLEW", "UNKNOWN"}
    assert {r[2] for r in rows} == {"none", "ok", "out", "paused", "unknown"}
    assert {r[3] for r in rows} == {"yes", "no"}


def test_the_classifier_lives_in_its_own_lib_sourced_by_the_guard():
    """clock-offset-guard.sh was already ~2000 lines; the issue-1372 group is its own lib, sourced by
    the guard so every consumer that sources the guard gets it unchanged."""
    lib = (_ROOT / "scripts" / "lib" / "dantesync-clock-discipline.sh").read_text()
    guard = _GUARD.read_text()
    for fn in ("clock_discipline_class()", "clock_discipline_unlocked()", "clock_discipline_check()",
               "date_master_verdict()", "date_master_check()", "dantesync_journal_clock_verdict()"):
        assert fn + " {" in lib and fn + " {" not in guard, fn
    assert "lib/dantesync-clock-discipline.sh" in guard


def test_python_twin_accepts_the_raw_json_text_too():
    text = (_STATUS / "stream-slave-1.9.0.json").read_text()
    assert df.classify_clock_discipline(text) == "PTP_PHASE_LOCK"
    assert df.classify_clock_discipline("not json") == "UNKNOWN"
    assert df.date_master_verdict("", _MARGIN_US) == "none"


# ---------------------------------------------------------------------------------------------
# the check line every consumer prints (name + rc)
# ---------------------------------------------------------------------------------------------

@pytest.mark.parametrize("status,rc,needle", [
    (json.dumps({"clock_discipline": "ptp_phase_lock", "ptp_phase_locked": True, "ptp_phase_error_us": 11.2}),
     0, "CLOCK PTP-PHASE-LOCK"),
    (json.dumps({"phase_slew_enabled": True}), 0, "PHASE-SLEW ENABLED"),
    (json.dumps({"phase_slew_enabled": False}), 2, "PHASE-SLEW DISABLED"),
    (json.dumps({"clock_discipline": "legacy", "phase_slew_enabled": False}), 2, "PHASE-SLEW DISABLED"),
    (json.dumps({"clock_discipline": "ptp_phase_lock", "ptp_phase_locked": False}), 2, "PTP-PHASE UNLOCKED"),
    (json.dumps({"clock_discipline": "ptp_phase_lock"}), 3, "CLOCK-DISCIPLINE UNKNOWN"),
    (json.dumps({"mode": "LOCK"}), 3, "PHASE-SLEW UNKNOWN"),
    ("", 3, "CLOCK-DISCIPLINE UNKNOWN"),
])
def test_clock_discipline_check_names_the_state_and_returns_its_rc(tmp_path, status, rc, needle):
    (tmp_path / "s.json").write_text(status)
    r = _sourced(tmp_path, f'clock_discipline_check node "$(cat "{tmp_path / "s.json"}")"; echo "rc=$?"')
    assert r.returncode == 0, r.stderr
    assert needle in r.stdout and f"rc={rc}" in r.stdout, r.stdout


@pytest.mark.parametrize("status,rc,needle", [
    ('{"date_authority":"master","date_offset_error_ms":-25.217,"date_step_bound_ms":50.0}', 0, "DATE MASTER OK"),
    ('{"date_authority":"master","date_offset_error_ms":-60.0,"date_step_bound_ms":50.0}', 2, "DATE MASTER OUT"),
    ('{"date_authority":"master","date_offset_error_ms":null,"date_step_bound_ms":50.0}', 3, "DATE MASTER UNKNOWN"),
])
def test_date_master_check_names_the_verdict(tmp_path, status, rc, needle):
    (tmp_path / "s.json").write_text(status)
    r = _sourced(tmp_path, f'date_master_check strih "$(cat "{tmp_path / "s.json"}")" 1000; echo "rc=$?"')
    assert needle in r.stdout and f"rc={rc}" in r.stdout, r.stdout


def test_date_master_check_is_silent_on_a_non_master(tmp_path):
    (tmp_path / "s.json").write_text((_STATUS / "stream-slave-1.9.0.json").read_text())
    r = _sourced(tmp_path, f'date_master_check stream "$(cat "{tmp_path / "s.json"}")" 1000; echo "rc=$?"')
    assert r.stdout.strip() == "rc=0", r.stdout


def test_date_master_effective_bound(tmp_path):
    master = (_STATUS / "strih-lx-master-1.9.0.json").read_text()
    slave = (_STATUS / "stream-slave-1.9.0.json").read_text()
    assert _bash_call(tmp_path, "date_master_effective_bound_us", master, 2000, 1000) == "51000"
    assert _bash_call(tmp_path, "date_master_effective_bound_us", slave, 2000, 1000) == "2000"


# ---------------------------------------------------------------------------------------------
# dantesync 1.11.0 (PR 121): the master holds the fleet date within ~2-3 ms by 500 us
# micro-corrections, so a master that carries date_correction_falling_behind is graded on the
# micro bound (DANTESYNC_DATE_MICRO_BOUND_MS, default 5) + margin; a 1.9.0/1.10.0 master keeps the
# step-bound grade (issue 1372, main's design comment 5846047309, Approach 1)
# ---------------------------------------------------------------------------------------------

_MICRO = ('{"date_authority":"master","date_offset_error_ms":%s,"date_step_bound_ms":50.0,'
          '"date_correction_falling_behind":%s,"date_micro_paused":%s}')


def test_date_master_effective_bound_on_a_1_11_0_master_is_the_micro_bound(tmp_path):
    master = (_STATUS / "strih-lx-master-1.11.0.json").read_text()
    slave = (_STATUS / "stream-slave-1.11.0.json").read_text()
    assert _bash_call(tmp_path, "date_master_effective_bound_us", master, 2000, 1000) == "6000"
    assert _bash_call(tmp_path, "date_master_effective_bound_us", master, 8000, 1000) == "8000"
    assert _bash_call(tmp_path, "date_master_effective_bound_us", slave, 2000, 1000) == "2000"


@pytest.mark.parametrize("micro_ms,err_ms,want", [
    ("2", "-2.9", "ok"), ("2", "-3.1", "out"), ("1.5", "2.5", "ok"), ("10", "-28.776", "out"),
    ("0", "-0.1", "unknown"), ("-1", "-0.1", "unknown"), ("abc", "-0.1", "unknown"),
    ("", "-5.9", "ok"), ("", "-6.1", "out"),
])
def test_micro_bound_is_the_one_env_knob_on_both_twins(tmp_path, monkeypatch, micro_ms, err_ms, want):
    status = _MICRO % (err_ms, "false", "false")
    (tmp_path / "s.json").write_text(status)
    r = _sourced(tmp_path, f'date_master_verdict "$(cat "{tmp_path / "s.json"}")" 1000; echo',
                 DANTESYNC_DATE_MICRO_BOUND_MS=micro_ms)
    assert r.stdout.strip() == want, r.stdout + r.stderr
    monkeypatch.setenv("DANTESYNC_DATE_MICRO_BOUND_MS", micro_ms)
    assert df.date_master_verdict(json.loads(status), _MARGIN_US) == want


def test_micro_bound_multi_line_env_is_unreadable_on_both_twins(tmp_path, monkeypatch):
    """Review round 1: a line-based grep shape check let `5<newline>abc` through in bash while
    python's fullmatch rejected it."""
    status = _MICRO % ("-0.1", "false", "false")
    (tmp_path / "s.json").write_text(status)
    r = _sourced(tmp_path, f'date_master_verdict "$(cat "{tmp_path / "s.json"}")" 1000; echo',
                 DANTESYNC_DATE_MICRO_BOUND_MS="5\nabc")
    assert r.stdout.strip() == "unknown", r.stdout + r.stderr
    monkeypatch.setenv("DANTESYNC_DATE_MICRO_BOUND_MS", "5\nabc")
    assert df.date_master_verdict(json.loads(status), _MARGIN_US) == "unknown"


def test_an_explicit_empty_micro_bound_argument_is_the_default_on_both_twins(tmp_path):
    """Review round 1: bash `${3:-...}` reads an explicit "" as the default; python must too."""
    status = _MICRO % ("-5.9", "false", "false")
    assert _bash_call(tmp_path, "date_master_verdict", status, _MARGIN_US, "") == "ok"
    assert df.date_master_verdict(json.loads(status), _MARGIN_US, "") == "ok"


def test_micro_bound_never_touches_a_pre_1_11_0_master(tmp_path, monkeypatch):
    status = '{"date_authority":"master","date_offset_error_ms":-28.8,"date_step_bound_ms":50.0}'
    (tmp_path / "s.json").write_text(status)
    r = _sourced(tmp_path, f'date_master_verdict "$(cat "{tmp_path / "s.json"}")" 1000; echo',
                 DANTESYNC_DATE_MICRO_BOUND_MS="abc")
    assert r.stdout.strip() == "ok", r.stdout + r.stderr
    monkeypatch.setenv("DANTESYNC_DATE_MICRO_BOUND_MS", "abc")
    assert df.date_master_verdict(json.loads(status), _MARGIN_US) == "ok"


@pytest.mark.parametrize("err,behind,paused,rc,needles", [
    ("-2.14", "false", "false", 0, ["DATE MASTER OK", "micro bound 5ms"]),
    ("-7.0", "false", "false", 2, ["DATE MASTER OUT", "micro bound 5ms"]),
    ("-3.0", "true", "false", 2, ["DATE MASTER OUT", "date_correction_falling_behind=true"]),
    ("-1.0", "false", "true", 4, ["DATE MASTER PAUSED", "date_micro_paused=true", "no UTC reading"]),
    ("null", "false", "false", 3, ["DATE MASTER UNKNOWN"]),
])
def test_date_master_check_names_the_1_11_0_verdict(tmp_path, err, behind, paused, rc, needles):
    (tmp_path / "s.json").write_text(_MICRO % (err, behind, paused))
    r = _sourced(tmp_path, f'date_master_check strih "$(cat "{tmp_path / "s.json"}")" 1000; echo "rc=$?"')
    assert f"rc={rc}" in r.stdout, r.stdout + r.stderr
    for needle in needles:
        assert needle in r.stdout, r.stdout


def test_date_master_check_keeps_the_step_bound_line_for_a_1_10_0_master(tmp_path):
    (tmp_path / "s.json").write_text(
        '{"date_authority":"master","date_offset_error_ms":-28.8,"date_step_bound_ms":50.0,"date_slew_active":false}')
    r = _sourced(tmp_path, f'date_master_check strih "$(cat "{tmp_path / "s.json"}")" 1000; echo "rc=$?"')
    assert "DATE MASTER OK" in r.stdout and "step bound 50.0ms" in r.stdout and "rc=0" in r.stdout, r.stdout


def test_the_version_pin_is_the_release_this_date_grading_implements():
    body = (_ROOT / "scripts" / "dantesync-version-gate.sh").read_text()
    assert 'DANTESYNC_VERSION_PIN="${DANTESYNC_VERSION_PIN:-1.11.1}"' in body


# ---------------------------------------------------------------------------------------------
# the journal path (verify-strih check 6, the verify-imag fallback): the master's
# `(date authority, ...)` line is graded on its own step bound, never the 2 ms UTC bound
# ---------------------------------------------------------------------------------------------

def _journal(*offsets, tail=""):
    lines = []
    for i, off in enumerate(offsets):
        lines.append(f"2026-09-26T08:00:{i * 10:02d}+02:00 strih-lx dantesync[1]: [NTP] offset:{off}{tail}")
    return "\n".join(lines) + "\n"


def _date_line(off_us, bound_us=50000):
    return f"{off_us:+d}us (date authority, fleet line {off_us:+d}us, step bound {bound_us}us)"


def test_journal_step_bound_is_read_from_the_date_authority_line(tmp_path):
    j = _journal(_date_line(-25217), _date_line(-25230))
    assert _bash_call(tmp_path, "date_step_bound_us_from_journal", j) == "50000"
    assert _bash_call(tmp_path, "date_step_bound_us_from_journal", _journal("+228us (threshold:810us, adaptive)")) == ""


@pytest.mark.parametrize("journal,want", [
    (_journal(_date_line(-25217), _date_line(-25230), _date_line(-25240)), "ok"),
    (_journal(_date_line(-49000), _date_line(+50900)), "ok"),
    (_journal(_date_line(-52000), _date_line(-52100), _date_line(-52200)), "drift"),
    (_journal(_date_line(-25217, 20000), _date_line(-25230, 20000)), "drift"),
    (_journal("-25217us (threshold:810us, adaptive)", "-25230us (threshold:810us, adaptive)"), "drift"),
    (_journal("+228us (threshold:810us, adaptive)", "+240us (threshold:810us, adaptive)"), "ok"),
    ("", "absent"),
])
def test_dantesync_journal_clock_verdict(tmp_path, journal, want):
    (tmp_path / "j.log").write_text(journal)
    r = _sourced(tmp_path, f'dantesync_journal_clock_verdict "$(cat "{tmp_path / "j.log"}")" 300 2000 2000 1000')
    assert r.stdout.strip() == want, r.stdout + r.stderr


def test_verify_strih_check_6_grades_the_date_master_through_the_shared_verdict():
    body = (_ROOT / "scripts" / "verify-strih.sh").read_text()
    assert 'dantesync_journal_clock_verdict "$DS_JOURNAL"' in body
    assert 'case "$(dantesync_offset_verdict "$DS_JOURNAL"' not in body


def test_verify_imag_grades_the_discipline_and_the_date_master():
    body = (_ROOT / "scripts" / "verify-imag.sh").read_text()
    assert 'clock_discipline_check imag "$DS_HTTP_STATUS"' in body
    assert 'date_master_effective_bound_us "$DS_HTTP_STATUS"' in body
    assert 'date_master_check imag "$DS_HTTP_STATUS"' in body
    assert 'dantesync_journal_clock_verdict "$DS_JOURNAL"' in body
    assert "phase_slew_check imag" not in body


# ---------------------------------------------------------------------------------------------
# the enforced E2E gate (dantesync-gate.sh) on live-captured 1.9.0 /status
# ---------------------------------------------------------------------------------------------

def _fresh(tmp_path, name, src, **edits):
    """A single-read DANTESYNC_GATE_WIN_HTTP_<NAME> fixture: a captured /status with its timestamps
    moved to now (the gate grades freshness against its own `date +%s`), plus EDITS."""
    s = json.loads(src.read_text()) if isinstance(src, pathlib.Path) else dict(src)
    now = int(time.time())
    s["updated_ts"] = now
    if "ntp_updated_ts" in s:
        s["ntp_updated_ts"] = now - 1
        s["ntp_age_s"] = 1
    for k, v in edits.items():
        if k.startswith("drop_"):
            s.pop(k[5:], None)
        else:
            s[k] = v
    p = tmp_path / f"{name}.json"
    p.write_text(json.dumps(s))
    return p


def _gate(args, **env):
    full = {"RIG_GRANDMASTER_IP": _LIVE_GM, "DANTESYNC_GATE_GM_ENFORCE": "1",
            "DANTESYNC_GATE_PHASE_SLEW_ENFORCE": "1"}
    full.update(env)
    r = subprocess.run([str(_GATE), *args], capture_output=True, text=True, cwd=str(_ROOT), env=_env(**full))
    return r.returncode, r.stdout, r.stderr


_STREAM_ONLY = ["--linux", "", "--win-http", "stream=10.77.9.204", "--ntp-master", "",
                "--samples", "1", "--min-distinct", "1", "--window-s", "0"]
_MASTER_ONLY = ["--linux", "", "--win-http", "strih=10.77.9.202", "--ntp-master", "strih",
                "--samples", "1", "--min-distinct", "1", "--window-s", "0"]


def test_gate_passes_the_live_1_9_0_slave_under_enforce(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "stream-slave-1.9.0.json")
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 0, out + err
    assert "CLOCK PTP-PHASE-LOCK" in out and "GATE PASS" in out


def test_gate_passes_the_live_1_9_0_master_on_its_date_bound(tmp_path):
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json")
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 0, out + err
    assert "DATE MASTER OK" in out and "CLOCK PTP-PHASE-LOCK" in out


def test_gate_grades_the_date_master_on_date_step_bound_not_the_deadband(tmp_path):
    """Before this fix the master only passed by accident, through the 51 ms deadband widening. A
    master that stops reporting ntp_deadband_us is still graded on its date step bound."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json",
               drop_ntp_deadband_us=None, drop_ntp_step_threshold_us=None)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 0, out + err
    assert "DATE MASTER OK" in out


def test_gate_fails_a_date_master_past_its_step_bound(tmp_path):
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json",
               ntp_offset_us=-60000, date_offset_error_ms=-60.0)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 20, out + err
    assert "DATE MASTER OUT" in out


def test_gate_fails_a_date_master_whose_date_is_out_while_its_median_is_in_bound(tmp_path):
    """Isolates the date rc fold: ntp_offset_us stays in the median bound, only
    date_offset_error_ms is past step bound + margin."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json", date_offset_error_ms=-60.0)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 20, out + err
    assert "DATE MASTER OUT" in out


@pytest.mark.parametrize("field", ["date_offset_error_ms", "date_step_bound_ms"])
def test_gate_is_incomplete_when_the_date_master_fields_are_unreadable(tmp_path, field):
    """A master with a null date error or step bound is UNKNOWN (11), and its median keeps the
    #1021/#1119 widening instead of failing DRIFT on the bare 2 ms bound (review finding)."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json", **{field: None})
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 11, out + err
    assert "DATE MASTER UNKNOWN" in out
    assert "DRIFT" not in out


def test_gate_never_grades_the_date_of_a_stale_master(tmp_path):
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json",
               ntp_failed=True, date_offset_error_ms=-60.0)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 11, out + err
    assert "DATE MASTER" not in out


def test_gate_date_margin_is_the_one_shared_knob(tmp_path):
    """DANTESYNC_DATE_MARGIN_US is the ONE date margin every consumer reads (the gate, verify-imag,
    verify-strih); the gate's deadband margin no longer doubles as it."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.9.0.json", date_offset_error_ms=-50.5)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 0, out + err
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p), DANTESYNC_DATE_MARGIN_US="0")
    assert code == 20, out + err
    assert "DATE MASTER OUT" in out
    for script in ("verify-imag.sh", "verify-strih.sh"):
        body = (_ROOT / "scripts" / script).read_text()
        assert "DATE_MASTER_MARGIN_US" in body and "IMAG_CLOCK_DATE_MARGIN_US" not in body, script


def test_gate_passes_a_1_11_0_master_on_its_micro_bound(tmp_path):
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.11.0.json")
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 0, out + err
    assert "DATE MASTER OK" in out and "micro bound 5ms" in out
    assert "bound 6000us" in out, out


def test_gate_fails_a_1_11_0_master_that_sits_inside_the_old_step_bound(tmp_path):
    """The 1.9.0 live master error (-28.776 ms) passed the 50 ms step bound; a 1.11.0 master holds
    the date to ~2-3 ms, so the same error is a master that stopped correcting."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.11.0.json",
               ntp_offset_us=-28776, date_offset_error_ms=-28.776)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 20, out + err
    assert "DATE MASTER OUT" in out


def test_gate_fails_a_1_11_0_master_falling_behind(tmp_path):
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.11.0.json",
               date_correction_falling_behind=True)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 20, out + err
    assert "DATE MASTER OUT" in out and "falling_behind" in out


def test_gate_refuses_a_1_11_0_master_whose_micro_corrections_are_paused(tmp_path):
    """Paused is WARN-level in the report consumers but fail-closed in the E2E [0/8] gate: the
    master has no UTC reading, so the fleet date is unverified (INCOMPLETE, 11)."""
    p = _fresh(tmp_path, "strih", _STATUS / "strih-lx-master-1.11.0.json", date_micro_paused=True)
    code, out, err = _gate(_MASTER_ONLY, DANTESYNC_GATE_WIN_HTTP_STRIH=str(p))
    assert code == 11, out + err
    assert "DATE MASTER PAUSED" in out


def test_gate_passes_the_1_11_0_slave(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "stream-slave-1.11.0.json")
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 0, out + err
    assert "DATE MASTER" not in out


def test_verify_imag_reports_a_paused_date_master_as_a_warning():
    body = (_ROOT / "scripts" / "verify-imag.sh").read_text()
    start = body.index('if [ "$rc_date" -eq 4 ]; then')
    branch = body[start:body.index("elif", start)]
    assert 'warn "' in branch and 'fail "' not in branch, branch
    assert "date_micro_paused" in branch


def test_gate_help_documents_the_micro_bound():
    code, out, err = _gate(["--help"])
    assert "DANTESYNC_DATE_MICRO_BOUND_MS" in out + err and "DATE MASTER PAUSED" in out + err


def test_gate_journal_fallback_grades_a_date_master_line_on_its_step_bound(tmp_path):
    """The gate's linux journal FALLBACK (HTTP down) goes through the shared journal verdict too."""
    j = tmp_path / "strih-lx.log"
    j.write_text(_journal(_date_line(-25217), _date_line(-25230), _date_line(-25240))
                 + "2026-09-26T08:00:30+02:00 strih-lx dantesync[1]: [PTP] LOCK  Drift: 12ns/s\n")
    args = ["--linux", "strih-lx=10.77.9.202", "--ntp-master", "", "--samples", "1",
            "--min-distinct", "1", "--window-s", "0"]
    code, out, err = _gate(args, DANTESYNC_GATE_LINUX_HTTP_STRIH_LX="/nonexistent-1372",
                           DANTESYNC_GATE_LINUX_JOURNAL_STRIH_LX=str(j))
    assert "NTP OK" in out and "date master step bound 50000us" in out, out + err
    assert code == 0, out + err


def test_gate_fails_a_phase_lock_node_that_is_not_phase_locked_by_name(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "stream-slave-1.9.0.json", ptp_phase_locked=False)
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 20, out + err
    assert "PTP-PHASE UNLOCKED" in out


def test_gate_still_fails_a_legacy_node_without_slew(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "legacy-no-slew-1.8.52.json")
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 20, out + err
    assert "PHASE-SLEW DISABLED" in out


def test_gate_passes_a_legacy_node_with_slew(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "legacy-slew-1.8.52.json")
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 0, out + err
    assert "PHASE-SLEW ENABLED" in out


def test_gate_is_incomplete_on_an_unreadable_discipline(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "stream-slave-1.9.0.json", clock_discipline="foo")
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p))
    assert code == 11, out + err
    assert "CLOCK-DISCIPLINE UNKNOWN" in out


def test_gate_discipline_check_stays_report_only_without_enforce(tmp_path):
    p = _fresh(tmp_path, "stream", _STATUS / "stream-slave-1.9.0.json", ptp_phase_locked=False)
    code, out, err = _gate(_STREAM_ONLY, DANTESYNC_GATE_WIN_HTTP_STREAM=str(p),
                           DANTESYNC_GATE_PHASE_SLEW_ENFORCE="0")
    assert code == 0, out + err
    assert "PTP-PHASE UNLOCKED" in out


# ---------------------------------------------------------------------------------------------
# the maintenance gate (traveling resolume)
# ---------------------------------------------------------------------------------------------

def _maint(tmp_path, status):
    (tmp_path / "s.json").write_text(status)
    script = tmp_path / "m.sh"
    script.write_text(
        f"set -euo pipefail\n. '{_MAINT}'\nset +e\n"
        f'dantesync_maintenance_verdict resolume 1 "dantesync 1.9.0" "$(cat "{tmp_path / "s.json"}")" '
        f'1.9.0 120 2000; echo "rc=$?"\n')
    r = subprocess.run(["bash", str(script)], capture_output=True, text=True, env=_env())
    return r.stdout


def test_maintenance_gate_accepts_the_1_9_0_discipline(tmp_path):
    out = _maint(tmp_path, (_STATUS / "stream-slave-1.9.0.json").read_text())
    assert "clock ptp_phase_lock" in out and "rc=0" in out, out


def test_maintenance_gate_alarms_on_an_unlocked_phase_lock(tmp_path):
    s = json.loads((_STATUS / "stream-slave-1.9.0.json").read_text())
    s["ptp_phase_locked"] = False
    out = _maint(tmp_path, json.dumps(s))
    assert "ptp-phase UNLOCKED" in out and "rc=30" in out, out


# ---------------------------------------------------------------------------------------------
# the canonical config policy: clock_discipline absent-or-ptp_phase_lock, legacy = drift,
# phase_slew no longer a policy
# ---------------------------------------------------------------------------------------------

def _with_env(monkeypatch):
    for k in _CLEAN:
        monkeypatch.delenv(k, raising=False)


@pytest.mark.parametrize("name,role", [("cam1", "video"), ("stream", "video"), ("strih-lx", "ntp-master")])
def test_the_live_configs_still_match_with_their_inert_phase_slew_key(name, role, monkeypatch):
    _with_env(monkeypatch)
    assert df.drift((_CFG / f"{name}.json").read_bytes(), role) == (df.OK, [])


@pytest.mark.parametrize("role", ["video", "audio", "ntp-master"])
def test_the_template_asserts_clock_discipline_not_phase_slew(role, monkeypatch):
    _with_env(monkeypatch)
    base = json.loads((_CFG / "strih-lx.json").read_text()) if role == "ntp-master" \
        else json.loads((_CFG / "stream.json").read_text())
    if role == "audio":
        base["system"]["gm_allowlist"] = [df.role_gm_host("audio")]
    del base["system"]["phase_slew"]
    assert df.drift(json.dumps(base).encode(), role) == (df.OK, [])
    base["system"]["clock_discipline"] = "ptp_phase_lock"
    assert df.drift(json.dumps(base).encode(), role) == (df.OK, [])
    base["system"]["phase_slew"] = {"enabled": False}
    assert df.drift(json.dumps(base).encode(), role) == (df.OK, [])
    base["system"]["clock_discipline"] = "legacy"
    verdict, diffs = df.drift(json.dumps(base).encode(), role)
    assert verdict == df.DRIFT
    assert diffs == ['system.clock_discipline: "legacy" (canonical "ptp_phase_lock")']


# ---------------------------------------------------------------------------------------------
# the config patcher and the provisioning writers stop asserting phase_slew for 1.9.0
# ---------------------------------------------------------------------------------------------

def test_ignore_rule_takes_only_true(monkeypatch):
    _with_env(monkeypatch)
    with pytest.raises(ValueError):
        df.drift(b'{"system": {"phase_slew": {"enabled": true}}}', "video",
                 {"video": {"system": {"phase_slew": {"$ignore": False}}}})


def test_patcher_writes_the_1_9_0_discipline_and_leaves_phase_slew_alone():
    out = json.loads(dcp.patch_config(json.dumps({"ntp_server": "strih.lan"})))
    assert out["system"] == {"clock_discipline": "ptp_phase_lock"}
    legacy = json.loads(dcp.patch_config(json.dumps({"system": {"clock_discipline": "legacy",
                                                                "phase_slew": {"enabled": False}}})))
    assert legacy["system"]["clock_discipline"] == "ptp_phase_lock"
    assert legacy["system"]["phase_slew"] == {"enabled": False}


def test_patcher_legacy_mode_keeps_the_old_phase_slew_flip():
    out = json.loads(dcp.patch_config(json.dumps({}), legacy_phase_slew=True))
    assert out["system"] == {"phase_slew": {"enabled": True}}


@pytest.mark.parametrize("script", ["setup-device.sh", "setup-imag.sh"])
def test_provisioning_writes_the_1_9_0_discipline_not_phase_slew(script):
    body = (_ROOT / "scripts" / script).read_text()
    start = body.index("cat > /etc/dantesync/config.json <<DANTECFGEOF")
    block = body[start:body.index("\nDANTECFGEOF\n", start)]
    assert '"clock_discipline": "ptp_phase_lock"' in block
    assert "phase_slew" not in block
