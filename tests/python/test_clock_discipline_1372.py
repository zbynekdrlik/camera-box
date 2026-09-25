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
          "DANTESYNC_DEADBAND_MARGIN_US", "CLOCK_GUARD_BOUND_US", "DANTESYNC_STABILITY_US")


def _env(**over):
    env = {k: v for k, v in os.environ.items() if k not in _CLEAN}
    env.update(over)
    return env


def _rows():
    rows = []
    for line in _TSV.read_text().splitlines():
        if not line.strip() or line.startswith("#"):
            continue
        case, status, klass, date_verdict = line.split("\t")
        if status.startswith("@"):
            status = (_ROOT / status[1:]).read_text()
        rows.append(pytest.param(status, klass, date_verdict, id=case))
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

@pytest.mark.parametrize("status,klass,date_verdict", _rows())
def test_bash_classifier_matches_the_table(tmp_path, status, klass, date_verdict):
    assert _bash_call(tmp_path, "clock_discipline_class", status) == klass


@pytest.mark.parametrize("status,klass,date_verdict", _rows())
def test_python_classifier_matches_the_table(status, klass, date_verdict):
    assert df.classify_clock_discipline(json.loads(status)) == klass


@pytest.mark.parametrize("status,klass,date_verdict", _rows())
def test_bash_date_master_verdict_matches_the_table(tmp_path, status, klass, date_verdict):
    assert _bash_call(tmp_path, "date_master_verdict", status, _MARGIN_US) == date_verdict


@pytest.mark.parametrize("status,klass,date_verdict", _rows())
def test_python_date_master_verdict_matches_the_table(status, klass, date_verdict):
    assert df.date_master_verdict(json.loads(status), _MARGIN_US) == date_verdict


def test_the_table_covers_every_class_and_verdict():
    rows = [p.values for p in _rows()]
    assert {r[1] for r in rows} == {"PTP_PHASE_LOCK", "LEGACY_SLEW", "LEGACY_NO_SLEW", "UNKNOWN"}
    assert {r[2] for r in rows} == {"none", "ok", "out", "unknown"}


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
        if v is None and k.startswith("drop_"):
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
