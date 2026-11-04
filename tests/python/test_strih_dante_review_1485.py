"""Issue 1485, the fresh-context review of the supervisor's commits (before the release merge of PR 1491).

  * finding 2: setup-strih step 6 derives the JPEG XS program encoder's cores BEFORE baseline step 11
    writes /etc/strih-isolated-cpus.conf, so a FRESH Dante box got jxs_cores "" (the encoder on every
    CPU, the reserved Dante CPU included) until a second run. Step 13 re-derives it, and verify item 49
    FAILs an empty value or one holding the reserved CPU on a Dante box;
  * finding 3: a derived value is recorded (`jxs_cores_derived`) and re-derived when the reserved CPU
    moves; an operator's own value is kept, a WARNING when it holds the reserved CPU;
  * finding 7 / 8: the managers' re-exec on install (PID 1 vs the user manager named apart, a failure is a
    WARNING, `timeout 30` on each) and on remove;
  * finding 9: the Dante lib not sourced is an error by name, never a silent "not a Dante box";
  * finding 10: item 9's start line is graded by the key PRESENT in it.
Every bash run sources the libs under the caller's `set -euo pipefail`; values reach bash as arguments.
"""
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_strih_dante_input_1485 as di  # noqa: E402
import test_strih_dante_reserved_cpu_1485 as rc  # noqa: E402
import test_strih_jxs_provision_1387 as jx  # noqa: E402
from strih_dante_rx_pin_fakes_1485 import stub  # noqa: E402

REPO = HERE.parent.parent
OUT_REL = ".config/obs-studio/plugin_config/distroav/jxs-output.json"


def _user():
    return subprocess.run(["id", "-un"], capture_output=True, text=True).stdout.strip()


def _env(tmp, isolated=None):
    env = dict(rc._box(tmp), **jx._seams(tmp))
    iso = Path(env["STRIH_DANTE_RX_PIN_ROOT"]) / "etc" / "strih-isolated-cpus.conf"
    if isolated is None:
        iso.unlink(missing_ok=True)
    else:
        iso.write_text(isolated + "\n")
    return env


def _step6(tmp, env):
    home = tmp / "home"
    return jx._bash('strih_box_load "$1"\nstrih_jxs_provision "$2/.config/obs-studio" "$3" "$2" "$4"',
                    "strih-snv", home, _user(), REPO, env=env, baseline=True), home


def _reconcile(tmp, env, box="strih-snv"):
    return jx._bash('strih_box_load "$1" >/dev/null\nstrih_jxs_output_cores_reconcile "$2" "$3"',
                    box, tmp / "home", _user(), env=env, baseline=True)


def _out(home):
    return json.loads((home / OUT_REL).read_text())


def _write_out(home, **keys):
    f = home / OUT_REL
    f.parent.mkdir(parents=True, exist_ok=True)
    d = json.loads(jx.LIVE_OUTPUT)
    d.update(keys)
    f.write_text(json.dumps(d) + "\n")


# --- finding 2: a fresh Dante box gets the cores at step 13, not at the second run --------------------------

def test_a_fresh_dante_box_gets_the_encoder_cores_at_step_13(tmp_path):
    r, home = _step6(tmp_path, _env(tmp_path))          # step 6: no OBS mask on the box yet
    assert r.returncode == 0, r.stdout + r.stderr
    assert _out(home)["jxs_cores"] == ""
    root, r = rc._install(tmp_path)                     # step 13 (strih_dante_input_install), after step 11
    assert r.returncode == 0, r.stdout + r.stderr
    d = _out(home)
    assert d["jxs_cores"] == "2-14" and d["jxs_cores_derived"] == "2-14", d
    assert d["jxs_target"] == "10.77.9.204:7100" and d["enabled"] is True   # nothing else touched


def test_a_dante_box_whose_cores_cannot_be_derived_at_step_13_fails_by_name(tmp_path):
    env = _env(tmp_path)                                # no OBS mask: nothing to derive from
    _write_out(tmp_path / "home")
    r = _reconcile(tmp_path, env)
    assert r.returncode == 1 and "strih-jxs:" in r.stderr and "jxs_cores" in r.stderr, r.stdout + r.stderr


def test_step_13_leaves_a_box_without_the_file_alone(tmp_path):
    r = _reconcile(tmp_path, _env(tmp_path, "2-15"))
    assert r.returncode == 0, r.stderr
    assert not (tmp_path / "home" / OUT_REL).exists()


# --- finding 3: a derived value follows the reserved CPU, an operator's is kept -----------------------------

def test_a_derived_value_is_re_derived_when_the_reserved_cpu_moves(tmp_path):
    _write_out(tmp_path / "home", jxs_cores="2-14", jxs_cores_derived="2-14")
    r = _reconcile(tmp_path, _env(tmp_path, "2-13"))    # the OBS mask shrank: CPU 13 is reserved now
    assert r.returncode == 0, r.stdout + r.stderr
    d = _out(tmp_path / "home")
    assert d["jxs_cores"] == "2-12" and d["jxs_cores_derived"] == "2-12", d


def test_an_operators_own_value_is_kept_and_warned_only_when_it_holds_the_reserved_cpu(tmp_path):
    _write_out(tmp_path / "home", jxs_cores="4-7", jxs_cores_derived="2-14")
    r = _reconcile(tmp_path, _env(tmp_path, "2-15"))
    assert r.returncode == 0 and "WARNING" not in r.stderr, r.stderr
    assert _out(tmp_path / "home")["jxs_cores"] == "4-7"
    _write_out(tmp_path / "home", jxs_cores="10-15")
    r = _reconcile(tmp_path, _env(tmp_path, "2-15"))
    assert r.returncode == 0, r.stderr
    assert _out(tmp_path / "home")["jxs_cores"] == "10-15"
    assert "WARNING" in r.stderr and "reserved Dante CPU 15" in r.stderr, r.stderr


def test_a_box_that_stops_taking_the_dante_program_drops_the_derived_value(tmp_path):
    _write_out(tmp_path / "home", jxs_cores="2-14", jxs_cores_derived="2-14")
    r = _reconcile(tmp_path, _env(tmp_path, "2-15"), box="strih-pp")
    assert r.returncode == 0, r.stdout + r.stderr
    d = _out(tmp_path / "home")
    assert d["jxs_cores"] == "" and "jxs_cores_derived" not in d, d


def test_the_live_file_renders_byte_identical_where_nothing_is_derived(tmp_path):
    f = tmp_path / "jxs-output.json"
    f.write_text(jx.LIVE_OUTPUT)
    r = jx._bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100 "" "" 15', f)
    assert r.returncode == 0 and r.stdout == jx.LIVE_OUTPUT, r.stdout + r.stderr


def test_the_live_strih_snv_file_gets_its_cores_once_then_renders_unchanged(tmp_path):
    f = tmp_path / "jxs-output.json"
    f.write_text(jx.LIVE_OUTPUT)
    r = jx._bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100 "" 2-14 15', f)
    assert r.returncode == 0, r.stderr
    d = json.loads(r.stdout)
    assert d["jxs_cores"] == "2-14" and d["jxs_cores_derived"] == "2-14" and "WARNING" not in r.stderr
    f.write_text(r.stdout)
    again = jx._bash('strih_jxs_output_render "$1" 10.77.9.204:7100 100 "" 2-14 15', f)
    assert again.returncode == 0 and again.stdout == r.stdout


def test_a_failed_reconcile_fails_step_13_on_a_non_dante_box_too(tmp_path):
    out = tmp_path / "home" / OUT_REL
    out.parent.mkdir(parents=True)
    out.symlink_to(tmp_path / "elsewhere.json")
    # setup-strih calls step 13 as `strih_dante_input_install ... || fail`: errexit is OFF inside it
    env = rc._box(tmp_path)
    env.update({"STRIH_DANTE_INPUT_TOML": str(tmp_path / "home" / ".camera-box" / "dante-input.toml")})
    body = ('. "%s"; strih_box_load strih-pp >/dev/null; '
            'strih_dante_input_install "$1" "$2" || { echo "STEP13-FAILED" >&2; exit 1; }') % rc.BASE
    r = rc._bash(rc.PROVISION, body, _user(), tmp_path / "home", env=env)
    assert r.returncode == 1 and "is a symlink" in r.stderr and "STEP13-FAILED" in r.stderr, r.stdout + r.stderr


# --- finding 2: verify item 49's (jxs-cores) row --------------------------------------------------------------

def _verify(tmp, cores):
    env = dict(_env(tmp, "2-15"), STRIH_JXS_OBS_PORT=str(jx._closed_port()), STRIH_OPERATOR_OBS_RUNNING_CMD="false")
    home = tmp / "home"
    _write_out(home, jxs_cores=cores)
    r = jx._bash('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
                 'strih_box_load "$1" >/dev/null\nstrih_jxs_config_verify_row "$2"', "strih-snv", home, env=env)
    return [ln for ln in r.stdout.splitlines() if "(jxs-cores)" in ln]


def test_verify_fails_empty_cores_on_a_dante_box(tmp_path):
    rows = _verify(tmp_path, "")
    assert len(rows) == 1 and rows[0].startswith("FAIL (jxs-cores)") and "every" in rows[0], rows


def test_verify_fails_cores_that_hold_the_reserved_cpu(tmp_path):
    rows = _verify(tmp_path, "10-15")
    assert len(rows) == 1 and rows[0].startswith("FAIL (jxs-cores)") and "15" in rows[0], rows


def test_verify_passes_cores_off_the_reserved_cpu(tmp_path):
    rows = _verify(tmp_path, "2-14")
    assert len(rows) == 1 and rows[0].startswith("PASS (jxs-cores)"), rows
    assert "next Stop/StartOutput" in rows[0]


def test_verify_grades_no_cores_without_an_enabled_output_file(tmp_path):
    env = dict(_env(tmp_path, "2-15"), STRIH_JXS_OBS_PORT=str(jx._closed_port()), STRIH_OPERATOR_OBS_RUNNING_CMD="false")
    r = jx._bash('ok() { echo "PASS $1"; }\nbad() { echo "FAIL $1"; }\nnote() { echo "NOTE $1"; }\n'
                 'strih_box_load "$1" >/dev/null\nstrih_jxs_config_verify_row "$2"', "strih-snv", tmp_path / "home", env=env)
    assert "FAIL (jxs-output)" in r.stdout and "(jxs-cores)" not in r.stdout, r.stdout


# --- finding 9 -----------------------------------------------------------------------------------------------

def test_the_dante_lib_not_sourced_is_an_error_by_name(tmp_path):
    r = jx._bash('strih_box_load strih-snv >/dev/null\nunset -f strih_program_dante_declared\n'
                 'strih_jxs_output_default_cores', env=_env(tmp_path, "2-15"), baseline=True)
    assert r.returncode == 1 and "strih_program_dante_declared" in r.stderr, r.stdout + r.stderr


def test_the_cpulist_check_not_sourced_is_an_error_by_name_too(tmp_path):
    # the operator-cores WARNING and the (jxs-cores) row decide with it: never skipped silently
    r = jx._bash('strih_box_load strih-snv >/dev/null\nunset -f _strih_dante_cpulist_has\n'
                 'strih_jxs_output_default_cores', env=_env(tmp_path, "2-15"), baseline=True)
    assert r.returncode == 1 and "_strih_dante_cpulist_has" in r.stderr, r.stdout + r.stderr


# --- finding 7 / 8: the managers' re-exec ----------------------------------------------------------------------

def _reexec_env(tmp, fail=""):
    env = rc._box(tmp)
    b = tmp / "bin"
    b.mkdir(exist_ok=True)
    log = tmp / "calls.log"
    stub(b, "systemctl", 'printf "systemctl %%s\\n" "$*" >> "%s"\ncase "$*" in %s) exit 1 ;; esac\n'
         % (log, fail or "__never__"))
    stub(b, "timeout", 'printf "timeout %%s\\n" "$*" >> "%s"\nshift\nexec "$@"\n' % log)
    env["PATH"] = "%s:%s" % (b, os.environ["PATH"])
    return env, log


def _reserved_install(tmp, user, env):
    return rc._bash(rc.PROVISION, '. "%s"; strih_dante_reserved_cpu_install "$1"' % rc.BASE, user, env=env)


def test_each_re_exec_is_bounded_by_timeout_30(tmp_path):
    env, log = _reexec_env(tmp_path)
    r = _reserved_install(tmp_path, _user(), env)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = log.read_text().splitlines()
    assert "timeout 30 systemctl daemon-reexec" in calls, calls
    assert "timeout 30 systemctl --user -M %s@ daemon-reexec" % _user() in calls, calls


def test_an_empty_user_re_executes_pid_1_only(tmp_path):
    env, log = _reexec_env(tmp_path)
    r = _reserved_install(tmp_path, "", env)
    assert r.returncode == 0, r.stdout + r.stderr
    calls = [c for c in log.read_text().splitlines() if c.startswith("systemctl")]
    assert calls == ["systemctl daemon-reexec"], calls
    assert "re-read: PID 1 re-executed (daemon-reexec)" in r.stdout, r.stdout


def test_a_failed_pid_1_re_exec_is_a_named_warning_never_a_failure(tmp_path):
    env, _log = _reexec_env(tmp_path, fail="daemon-reexec")
    r = _reserved_install(tmp_path, _user(), env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "WARNING" in r.stderr and "PID 1" in r.stderr, r.stderr


def test_a_failed_user_manager_re_exec_is_its_own_warning(tmp_path):
    env, _log = _reexec_env(tmp_path, fail="--user*")
    r = _reserved_install(tmp_path, _user(), env)
    assert r.returncode == 0, r.stdout + r.stderr
    warn = [ln for ln in r.stderr.splitlines() if "WARNING" in ln]
    assert len(warn) == 1 and "user manager of %s" % _user() in warn[0] and "PID 1" not in warn[0], warn


def test_each_manager_that_re_executed_is_named(tmp_path):
    env, _log = _reexec_env(tmp_path, fail="daemon-reexec")
    r = _reserved_install(tmp_path, _user(), env)
    assert "re-read: the user manager of %s re-executed (daemon-reexec)" % _user() in r.stdout, r.stdout


def test_removing_the_drop_ins_re_executes_both_managers(tmp_path):
    root, r = rc._install(tmp_path)
    assert r.returncode == 0, r.stderr
    log = tmp_path / "systemctl.log"
    log.write_text("")
    _, r = rc._install(tmp_path, box="strih-pp")
    assert r.returncode == 0, r.stdout + r.stderr
    calls = log.read_text().splitlines()
    assert "daemon-reexec" in calls and "--user -M %s@ daemon-reexec" % _user() in calls, calls
    log.write_text("")                                   # nothing left to remove: no re-exec
    _, r = rc._install(tmp_path, box="strih-pp")
    assert "daemon-reexec" not in log.read_text()


# --- finding 10 ------------------------------------------------------------------------------------------------

def test_item9_grades_the_converts_key_when_present_even_with_an_empty_value(tmp_path):
    start = di.START.replace("halved=true", "converts= halved=true")
    r = di._grade(tmp_path, [start, di._status("12:00:10.000", 4800)])
    assert "wants 96000 / true" in r.stdout and "converts=" in r.stdout, r.stdout
