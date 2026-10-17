"""issue 1367 -- the soak cuts the strih program with the Cut transition, like the E2E sweep.

Found live 17.10.2026 (finding 6228341862, design 6228916582): the 8 h soak read one 21-22-slot gap
on every camera after the first, ~1 s after each cut. strih was on "Fade 500 ms": the blended frames
of the outgoing camera decode as the wrong camera, so the new camera's tick sequence jumps. The E2E
forces Cut since 16.10.2026 (scripts/lib/strih-transition.sh); the soak reuses that lib:
  - setup selects Cut after the setup-mutation rig-busy guard, before the burns and slot 0;
  - a failed selection refuses the run (exit 4) before anything is recorded;
  - cleanup puts the operator's transition back on EVERY exit that reached the selection, also
    during a broadcast (a transition setting is not a cut), and a second signal cannot cut it short;
  - a run refused before the selection never touches the transition.

Reuses the soak's fake rig from tests/python/test_av_soak_orchestrator_1367.py (the `rig` fixture;
its fake obs_phase2.py answers transition-cut / transition-restore, AV_SOAK_OBS_DIR is the seam).
"""
import json
import os
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from test_av_soak_orchestrator_1367 import (  # noqa: E402,F401
    QUICK_TWO_WINDOWS, SOAK, STRIH, _assert_rig_restored, _calls, _csv_rows, _host, _soak,
    _wait_for_sweep, rig)


def _obs_lines(p):
    """Every fake call in log order: ("obs"|"burn", argv)."""
    out = []
    for line in p["log"].read_text().splitlines():
        for kind in ("obs", "burn"):
            if line.startswith(kind + " ["):
                out.append((kind, json.loads(line[len(kind) + 1:])))
    return out


def _transition_calls(p, which):
    return [c for c in _calls(p["log"], "obs") if c[:1] == [which]]


def _restored_once(p, env):
    restores = _transition_calls(p, "transition-restore")
    assert len(restores) == 1, restores
    c = restores[0]
    assert _host(c) == STRIH
    assert c[c.index("--save") + 1] == env["STRIH_TRANSITION_SAVE"], "the save the selection wrote"
    assert (p["state"] / "done-transition-restore").exists(), "the restore ran to its end"
    assert not os.path.exists(env["STRIH_TRANSITION_SAVE"]), "put back: the save is consumed"
    return c


def test_setup_selects_cut_after_the_setup_guard_and_before_slot_0(rig):
    env, p = rig
    r = _soak(env, "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    calls = _obs_lines(p)
    kinds = [c[1][0] if c[0] == "obs" else "burn " + c[1][0] for c in calls]
    assert kinds.count("transition-cut") == 1
    cut = kinds.index("transition-cut")
    cut_argv = calls[cut][1]
    assert _host(cut_argv) == STRIH
    assert cut_argv[cut_argv.index("--save") + 1] == env["STRIH_TRANSITION_SAVE"], \
        "the E2E's durable save (STRIH_TRANSITION_SAVE), so an aborted run never loses the operator's choice"
    assert kinds[:cut].count("rig-busy-check") >= 3, "after the setup reads, the idle proof and the mutation guard"
    assert "burn add" not in kinds[:cut], "selected before the first mutation (the burns)"
    assert not [k for k in kinds[:cut] if k in ("switch", "record")], "before slot 0's first cut"
    first_switch = kinds.index("switch")
    assert cut < first_switch


def test_a_normal_end_restores_the_transition_after_the_strih_program(rig):
    env, p = rig
    r = _soak(env, "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    _restored_once(p, env)
    assert "the operator's choice put back" in r.stdout
    kinds = [c[1][0] if c[0] == "obs" else "burn " + c[1][0] for c in _obs_lines(p)]
    restore = kinds.index("transition-restore")
    last_switch = len(kinds) - 1 - kinds[::-1].index("switch")
    assert last_switch < restore, "the soak's own program restore still cuts with Cut"
    assert "burn remove" in kinds[restore:], "before the burns go back and the lease is released"
    _assert_rig_restored(p)


def test_a_failed_cut_selection_refuses_before_anything_is_recorded(rig):
    env, p = rig
    for i, fail in enumerate(("1", "after-save")):
        p["log"].write_text("")
        r = _soak(dict(env, FAKE_TRANSITION_CUT_FAIL=fail,
                       AV_SOAK_RUN_DIR=str(p["run"]) + f"-{i}"), "--run")
        assert r.returncode == 4, (fail, r.stdout + r.stderr)
        out = r.stdout + r.stderr
        assert "could not set the strih program transition to Cut" in out, fail
        obs = _calls(p["log"], "obs")
        assert not [c for c in obs if c[0] in ("record", "switch")], "nothing recorded, no cut"
        assert not [c for c in _calls(p["log"], "burn") if c[0] == "add"], "no burn turned on"
        assert not os.path.exists(str(p["run"]) + f"-{i}/soak.csv")
        restores = _transition_calls(p, "transition-restore")
        if fail == "1":
            # nothing was saved: nothing is sent
            assert restores == [] and "nothing was saved, nothing to put back" in out
        else:
            # the selection saved the operator's transition and then failed: it is put back
            assert len(restores) == 1 and "the operator's choice put back" in out
            assert not os.path.exists(env["STRIH_TRANSITION_SAVE"])
        _assert_rig_restored(p)


def test_a_restore_that_fails_is_a_warning_and_keeps_the_save(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_TRANSITION_RESTORE_FAIL="1"), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    out = r.stdout + r.stderr
    assert "transition was NOT put back" in out
    assert "could not restore the operator's strih program transition" in r.stderr, \
        "the lib's own WARNING (manual step) reaches the operator, not only cleanup.log"
    assert "the operator's choice put back" not in out
    assert os.path.exists(env["STRIH_TRANSITION_SAVE"]), "the save stays for the next run"
    _assert_rig_restored(p)


def test_an_earlier_runs_save_is_kept_and_put_back(rig):
    # an aborted E2E left the operator's real transition saved (strih now on Cut): the soak's
    # selection keeps that save (operator-wins) and its cleanup puts the real choice back
    env, p = rig
    with open(env["STRIH_TRANSITION_SAVE"], "w") as fh:
        fh.write('{"name": "Stinger", "duration_ms": null, "fixed": true}')
    r = _soak(env, "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert len(_transition_calls(p, "transition-cut")) == 1
    _restored_once(p, env)


def test_a_run_refused_before_the_selection_never_touches_the_transition(rig):
    env, p = rig
    for i, extra in enumerate(({"FAKE_BUSY": "1"}, {"FAKE_STREAM_PROGRAM": "Other"},
                               {"FAKE_PAINTER_ACTIVE": "inactive"})):
        p["log"].write_text("")
        r = _soak(dict(env, AV_SOAK_RUN_DIR=str(p["run"]) + f"-{i}", **extra), "--run")
        assert r.returncode == 4, (extra, r.stdout + r.stderr)
        assert not _transition_calls(p, "transition-cut"), extra
        assert not _transition_calls(p, "transition-restore"), extra


def test_the_stop_file_restores_the_transition(rig):
    env, p = rig
    stop = p["run"] / "STOP"
    r = _soak(dict(env, AV_SOAK_HOURS="1", FAKE_TOUCH_STOP=str(stop)), "--run")
    assert r.returncode == 2, r.stdout + r.stderr
    assert "STOP file found" in r.stdout
    _restored_once(p, env)
    _assert_rig_restored(p)


def test_sigterm_mid_slot_restores_the_transition(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"], env=dict(env, AV_SOAK_SEGMENT_SECS="20"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    assert _wait_for_sweep(p), "the sweep never started"
    proc.send_signal(signal.SIGTERM)
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    _restored_once(p, env)
    _assert_rig_restored(p)


def test_a_broadcast_between_slots_still_restores_the_transition(rig):
    # the strih program is NOT cut back during a broadcast (a cut on air), but the transition is a
    # setting, not a cut: the operator gets his own transition back for the show
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_STOPS="1", **QUICK_TWO_WINDOWS), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "a broadcast went live" in r.stdout + r.stderr
    assert "strih program NOT restored" in r.stdout + r.stderr
    _restored_once(p, env)


def test_a_broadcast_mid_sweep_still_restores_the_transition(rig):
    env, p = rig
    r = _soak(dict(env, FAKE_LIVE_AFTER_SWITCHES="2"), "--run")
    assert r.returncode == 5, r.stdout + r.stderr
    assert "RECORDING MAY STILL BE RUNNING" in r.stdout + r.stderr
    _restored_once(p, env)


def test_a_second_ctrl_c_cannot_cut_the_transition_restore_short(rig):
    env, p = rig
    proc = subprocess.Popen(["bash", SOAK, "--run"],
                            env=dict(env, AV_SOAK_SEGMENT_SECS="20", FAKE_TRANSITION_RESTORE_DELAY="3"),
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                            start_new_session=True)
    assert _wait_for_sweep(p), "the sweep never started"
    os.killpg(proc.pid, signal.SIGINT)
    deadline = time.time() + 60
    while time.time() < deadline and '"transition-restore"' not in p["log"].read_text():
        time.sleep(0.1)
    assert '"transition-restore"' in p["log"].read_text(), "cleanup never reached the transition restore"
    os.killpg(proc.pid, signal.SIGINT)  # the second Ctrl-C, while the restore runs
    out, err = proc.communicate(timeout=60)
    assert proc.returncode == 5, out + err
    _restored_once(p, env)
    _assert_rig_restored(p)


def test_the_plan_names_the_cut_selection_and_its_restore(rig):
    env, p = rig
    r = _soak(env, "--plan")
    assert r.returncode == 0, r.stderr
    out = r.stdout
    cut = out.find("transition-cut --host 10.77.9.202")
    restore = out.find("transition-restore --host 10.77.9.202")
    burns_on = out.find("obs_burn_filter.py add")
    assert cut != -1 and restore != -1, out
    assert cut < burns_on, "the plan selects Cut before the burns"
    assert out.find("CLEANUP") < restore, "the restore is a cleanup step"
    assert p["log"].read_text() == "", "the plan still touches nothing"


def test_the_soak_sources_the_e2e_transition_lib_once():
    src = open(SOAK).read()
    line = '. "$HERE/lib/strih-transition.sh"'
    assert src.count(line) == 1
    assert src.index(line) < src.index("SETUP_STARTED=0"), "sourced with the soak's other libs"
    assert "strih_transition_force_cut" in src, "the setup selection is the lib's own"
    lib = open(os.path.join(os.path.dirname(SOAK), "lib", "av-soak.sh")).read()
    assert "strih_transition_restore" in lib and "av_soak_transition_restore" in src, \
        "the cleanup restore is the lib's own, through the soak lib's one helper"
