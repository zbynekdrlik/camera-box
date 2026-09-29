"""#1302 — the CG_CHAIN=1 profile turns the cg OBS hop burn (911015) ON for the recording and OFF
again (scripts/lib/cg-chain-e2e.sh).

The cg OBS hop burn is the DistroAV burn filter on the cg OBS input that carries the SongPlayer
output (`sp-fast_video`). Like strih/stream it renders only while that input's `genlock_burn` is
true, toggled over OBS-WS by `scripts/obs_burn_filter.py add|remove` and read back by `check`.
Before this slice the profile toggled only the SongPlayer burn (911014), so the cg recording of
run 36291465574 carried 911014 and no 911015 at all.

Everything here is Tier-0: the lib is sourced under the caller's real `set -euo pipefail` and its
runners are driven against FAKE `obs_burn_filter.py` / `obs_phase2.py` / `cg_chain_scene.py`
scripts (run by the real python3), a fake `curl` (the SongPlayer health read-back) and a
pass-through `timeout` that logs its budget. No OBS, no SongPlayer, no network.
"""
from __future__ import annotations

import pathlib
import subprocess

_ROOT = pathlib.Path(__file__).resolve().parents[2]
_LIB = _ROOT / "scripts" / "lib" / "cg-chain-e2e.sh"
_E2E = _ROOT / "scripts" / "recording-e2e.sh"

HOST = "10.77.9.201"

# The fake OBS-WS helpers. Every call appends `<script> <argv...>` to $CALL_LOG, so the ORDER of
# the program cut, the burn toggle and StartRecord is visible in one file.
_FAKE_BURN_FILTER = r'''
import os, sys
log = os.environ["CALL_LOG"]
with open(log, "a") as f:
    f.write("obs_burn_filter.py " + " ".join(sys.argv[1:]) + "\n")
action = sys.argv[1]
state_file = os.environ["BURN_STATE_FILE"]
mode = os.environ.get("FAKE_CHECK_MODE", "follow")
if action == "add":
    if os.environ.get("FLAG_SEEN_LOG"):
        with open(os.environ["FLAG_SEEN_LOG"], "a") as f:
            f.write(os.environ.get("CG_BURN_ON", "unset") + "\n")
    if os.environ.get("FAKE_ADD_RC", "0") != "0":
        sys.exit("[burn] FAIL: genlock_burn did not turn on")
    open(state_file, "w").write("on")
    print("[burn] ON  genlock_burn=true")
elif action == "remove":
    if os.environ.get("FAKE_REMOVE_SLEEP"):
        import time
        time.sleep(float(os.environ["FAKE_REMOVE_SLEEP"]))
    open(state_file, "w").write("off")
    print("[burn] OFF genlock_burn=false")
elif action == "check":
    state = open(state_file).read().strip() if os.path.exists(state_file) else "off"
    if mode == "unreachable":
        sys.exit("ConnectionRefusedError: [Errno 111] Connection refused")
    if mode == "stuck-on":
        state = "on"
    if mode == "filter-disabled" and state == "on":
        print("[burn] burn_on=False genlock_burn=True filter_on_input=True filter_enabled=False "
              "kind_registered=True input='" + sys.argv[-1] + "'")
        sys.exit(0)
    if state == "on":
        print("[burn] burn_on=True genlock_burn=True filter_on_input=True filter_enabled=True "
              "kind_registered=True input='" + sys.argv[-1] + "'")
    else:
        print("[burn] burn_on=False genlock_burn=False filter_on_input=True filter_enabled=True "
              "kind_registered=True input='" + sys.argv[-1] + "'")
'''

_FAKE_OBS_PHASE2 = r'''
import os, sys
with open(os.environ["CALL_LOG"], "a") as f:
    f.write("obs_phase2.py " + " ".join(sys.argv[1:]) + "\n")
if "start" in sys.argv:
    sys.exit(int(os.environ.get("FAKE_START_RC", "0")))
if "stop" in sys.argv:
    print(os.environ.get("FAKE_STOP_OUT", ""))
'''

_FAKE_SCENE = r'''
import os, sys
with open(os.environ["CALL_LOG"], "a") as f:
    f.write("cg_chain_scene.py " + " ".join(sys.argv[1:]) + "\n")
if sys.argv[1] == "program":
    sys.exit(int(os.environ.get("FAKE_CUT_RC", "0")))
'''

# Fake curl: a GET of the SongPlayer health URL answers SP-fast burn_on=$FAKE_SP_BURN_ON; the burn
# POST is logged and answers nothing.
_FAKE_CURL = r'''#!/usr/bin/env bash
url=""
for a in "$@"; do case "$a" in http*) url="$a" ;; esac; done
case "$url" in
  */api/v1/ndi/health)
    printf '[{"ndi_name":"SP-fast","burn_on":%s}]' "${FAKE_SP_BURN_ON:-false}" ;;
  *)
    if [ -n "${FAKE_CURL_POST_SLEEP:-}" ]; then sleep "$FAKE_CURL_POST_SLEEP"; fi
    printf 'curl POST %s\n' "$url" >> "$CALL_LOG" ;;
esac
'''

# Pass-through `timeout` that records the per-call budget it was given.
_FAKE_TIMEOUT = r'''#!/usr/bin/env bash
printf '%s\n' "$1" >> "$TIMEOUT_LOG"
shift
exec "$@"
'''


def _run(tmp_path, snippet, env=None):
    """Source the lib under `set -euo pipefail` with the fakes installed and run `snippet`.

    Returns (returncode, stdout, stderr, calls) where calls = the ordered helper-call log lines.
    The fake scripts live in `<tmp>/scripts/`, so `$PY` (= `<tmp>/scripts/obs_phase2.py`) is what
    the snippet passes as the obs_phase2.py path; the lib resolves its siblings from it."""
    scripts = tmp_path / "scripts"
    scripts.mkdir(exist_ok=True)
    (scripts / "obs_burn_filter.py").write_text(_FAKE_BURN_FILTER)
    (scripts / "obs_phase2.py").write_text(_FAKE_OBS_PHASE2)
    (scripts / "cg_chain_scene.py").write_text(_FAKE_SCENE)
    fbin = tmp_path / "bin"
    fbin.mkdir(exist_ok=True)
    for name, body in (("curl", _FAKE_CURL), ("timeout", _FAKE_TIMEOUT)):
        p = fbin / name
        p.write_text(body)
        p.chmod(0o755)
    call_log = tmp_path / "calls.log"
    call_log.touch()
    full_env = {
        "PATH": f"{fbin}:/usr/local/bin:/usr/bin:/bin",
        "HOME": str(tmp_path),
        "CALL_LOG": str(call_log),
        "TIMEOUT_LOG": str(tmp_path / "timeout.log"),
        "BURN_STATE_FILE": str(tmp_path / "burn.state"),
        "CG_CHAIN_BURN_RETRY_SLEEP": "0",
        "CG_CHAIN_STATE_DIR": str(tmp_path),
        "PY": str(scripts / "obs_phase2.py"),
        "HOST": HOST,
    }
    if env:
        full_env.update(env)
    script = f'set -euo pipefail\n. "{_LIB}"\n{snippet}\n'
    out = subprocess.run(["/bin/bash", "-c", script], env=full_env, capture_output=True,
                         text=True, check=False)
    calls = [ln for ln in call_log.read_text().splitlines() if ln]
    return out.returncode, out.stdout, out.stderr, calls


def _burn_calls(calls):
    return [c for c in calls if c.startswith("obs_burn_filter.py")]


# ---- pure: the input name + the check-answer classifier --------------------------------------


def test_burn_input_is_the_cg_scene_video_input(tmp_path):
    rc, out, err, _ = _run(tmp_path, "unset CG_CHAIN_CG_BURN_INPUT CG_CHAIN_CG_SCENE; "
                                     "cg_chain_cg_burn_input")
    assert rc == 0, err
    assert out == "sp-fast_video", "the live cg OBS input of the SP-fast scene"


def test_burn_input_follows_the_scene_and_the_output(tmp_path):
    _, a, _, _ = _run(tmp_path, "CG_CHAIN_CG_SCENE=sp-slow cg_chain_cg_burn_input")
    assert a == "sp-slow_video"
    _, b, _, _ = _run(tmp_path, "unset CG_CHAIN_CG_SCENE; CG_CHAIN_SONGPLAYER_OUTPUT=SP-Program "
                                "cg_chain_cg_burn_input")
    assert b == "sp-program_video", "the default scene is the lower-cased output name"


def test_burn_input_env_override_wins(tmp_path):
    _, out, _, _ = _run(tmp_path, "CG_CHAIN_CG_SCENE=sp-slow CG_CHAIN_CG_BURN_INPUT='SP program' "
                                  "cg_chain_cg_burn_input")
    assert out == "SP program"


def test_burn_filter_py_sits_next_to_obs_phase2(tmp_path):
    _, out, _, _ = _run(tmp_path, "cg_chain_burn_filter_py /x/scripts/obs_phase2.py")
    assert out == "/x/scripts/obs_burn_filter.py"


def test_check_state_on_needs_burn_on_and_an_enabled_filter(tmp_path):
    snippet = r'''
for chk in \
  "[burn] burn_on=True genlock_burn=True filter_on_input=True filter_enabled=True kind_registered=True input='sp-fast_video'" \
  "[burn] burn_on=False genlock_burn=False filter_on_input=True filter_enabled=True kind_registered=True input='sp-fast_video'" \
  "[burn] burn_on=False genlock_burn=None filter_on_input=False filter_enabled=None kind_registered=True input='sp-fast_video'" \
  "[burn] burn_on=False genlock_burn=True filter_on_input=True filter_enabled=False kind_registered=True input='sp-fast_video'" \
  "[burn] burn_on=True genlock_burn=True filter_on_input=True filter_enabled=False kind_registered=True input='x'" \
  "Traceback (most recent call last): ConnectionRefusedError" \
  "" \
  "[burn] burn_on=Truee genlock_burn=True filter_enabled=True"; do
  cg_chain_cg_burn_check_state "$chk"; echo
done
'''
    rc, out, err, _ = _run(tmp_path, snippet)
    assert rc == 0, err
    assert out.split() == ["on", "off", "off", "unknown", "unknown", "unknown", "unknown",
                           "unknown"], (
        "on = burn_on=True AND filter_enabled=True; off = burn_on=False with genlock_burn not "
        "True; a disabled filter holding genlock_burn=True, a traceback or no answer = unknown"
    )


def test_check_state_reads_a_multi_line_answer(tmp_path):
    snippet = r'''
chk="$(printf '%s\n%s' "[burn] burn_on=True genlock_burn=True filter_on_input=True filter_enabled=True kind_registered=True input='a'" "[burn]   NOTE: extra line")"
cg_chain_cg_burn_check_state "$chk"
'''
    _, out, _, _ = _run(tmp_path, snippet)
    assert out == "on"


# ---- the runner: ON / OFF, verified or loud ---------------------------------------------------


def test_burn_on_adds_then_checks_and_sets_the_flag(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 cg_chain_cg_burn on "$HOST" "$PY" 7; printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"')
    assert rc == 0, err
    assert _burn_calls(calls) == [
        f"obs_burn_filter.py add --host {HOST} --input sp-fast_video",
        f"obs_burn_filter.py check --host {HOST} --input sp-fast_video",
    ]
    assert "VERIFIED" in out and "FLAG=1" in out, out
    assert (tmp_path / "timeout.log").read_text().split() == ["7", "7"], \
        "every OBS-WS call runs under the caller's per-call timeout"


def test_burn_on_is_a_no_op_when_the_profile_is_off(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'unset CG_CHAIN; cg_chain_cg_burn on "$HOST" "$PY" 7; printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"')
    assert rc == 0, err
    assert _burn_calls(calls) == [], "CG_CHAIN unset never turns a burn on"
    assert "FLAG=unset" in out
    assert err == ""


def test_burn_off_removes_then_checks_and_clears_the_flag(tmp_path):
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_BURN_ON=1; cg_chain_cg_burn off "$HOST" "$PY" 7; printf "FLAG=%s\\n" "$CG_BURN_ON"')
    assert rc == 0, err
    assert _burn_calls(calls) == [
        f"obs_burn_filter.py remove --host {HOST} --input sp-fast_video",
        f"obs_burn_filter.py check --host {HOST} --input sp-fast_video",
    ]
    assert "VERIFIED" in out and "FLAG=0" in out, out


def test_unverified_on_is_a_loud_warning_rolled_back_off_never_fatal(tmp_path):
    # The add reaches OBS but every check read-back fails (an unreachable / wedged WS). The add may
    # have landed, so the burn is rolled straight back OFF; the flag never reads 1.
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_CHAIN_BURN_ATTEMPTS=2 cg_chain_cg_burn on "$HOST" "$PY" 7; echo REACHED; '
        'printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"',
        env={"FAKE_CHECK_MODE": "unreachable"})
    assert rc == 0, "a failed read-back is loud, never fatal to the run"
    assert "REACHED" in out
    assert "WARNING" in err and "cg OBS burn ON not confirmed" in err, err
    verbs = [c.split()[1] for c in _burn_calls(calls)]
    assert verbs[:4] == ["add", "check", "add", "check"], "ON is retried CG_CHAIN_BURN_ATTEMPTS times"
    assert "remove" in verbs[4:], "an unverified ON is rolled back OFF"
    # the rollback OFF could not be verified either (the check is unreachable): cleanup must retry
    assert "LEAK" in err
    assert "FLAG=1" in out, "a burn that may still be on keeps cleanup()'s OFF owed"


def test_unverified_on_with_a_verified_rollback_leaves_nothing_owed(tmp_path):
    # The filter is attached but DISABLED: genlock_burn=True yet nothing renders — not a verified
    # ON. The rollback remove then reads back a clean OFF.
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_cg_burn on "$HOST" "$PY" 7; '
        'printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"',
        env={"FAKE_CHECK_MODE": "filter-disabled"})
    assert rc == 0, err
    assert "cg OBS burn ON not confirmed" in err
    assert [c.split()[1] for c in _burn_calls(calls)] == ["add", "check", "remove", "check"]
    assert "LEAK" not in err
    assert "FLAG=0" in out


def test_off_that_never_reads_back_off_is_a_leak_and_keeps_the_flag(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_BURN_ON=1 CG_CHAIN_BURN_ATTEMPTS=3; cg_chain_cg_burn off "$HOST" "$PY" 7; echo REACHED; '
        'printf "FLAG=%s\\n" "$CG_BURN_ON"',
        env={"FAKE_CHECK_MODE": "stuck-on"})
    assert rc == 0
    assert "REACHED" in out
    assert [c.split()[1] for c in _burn_calls(calls)].count("remove") == 3
    assert "LEAK" in err and "obs_burn_filter.py remove" in err, \
        "the LEAK line names the manual off command"
    assert "FLAG=1" in out


def test_off_without_a_host_is_loud_and_sends_nothing(tmp_path):
    rc, out, err, calls = _run(tmp_path, 'CG_BURN_ON=1; cg_chain_cg_burn off "" "$PY" 7; echo REACHED')
    assert rc == 0 and "REACHED" in out
    assert _burn_calls(calls) == []
    assert "LEAK" in err


def test_a_typo_action_sends_nothing(tmp_path):
    rc, _, err, calls = _run(tmp_path, 'CG_CHAIN=1 cg_chain_cg_burn onn "$HOST" "$PY" 7')
    assert rc == 0
    assert _burn_calls(calls) == []
    assert "not on|off" in err


def test_the_flag_is_raised_before_the_add_is_sent(tmp_path):
    # A signal can land between the `add` and its read-back; cleanup() must already owe the OFF.
    seen = tmp_path / "flag-seen.log"
    rc, _, err, _ = _run(
        tmp_path,
        'export CG_BURN_ON=0\nCG_CHAIN=1 cg_chain_cg_burn on "$HOST" "$PY" 7',
        env={"FLAG_SEEN_LOG": str(seen)})
    assert rc == 0, err
    assert seen.read_text().split() == ["1"]


def test_the_check_is_authoritative_over_the_add_exit_code(tmp_path):
    # The add fails (e.g. the filter re-enable raced) but the burn already renders: a verified ON.
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 cg_chain_cg_burn on "$HOST" "$PY" 7; printf "FLAG=%s\\n" "$CG_BURN_ON"',
        env={"FAKE_ADD_RC": "1"})
    assert rc == 0, err
    assert [c.split()[1] for c in _burn_calls(calls)] == ["add", "check"]
    assert "VERIFIED" in out and "FLAG=1" in out


def test_the_input_override_reaches_every_call(tmp_path):
    rc, _, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_CHAIN_CG_BURN_INPUT="SP program" cg_chain_cg_burn on "$HOST" "$PY" 7')
    assert rc == 0, err
    assert _burn_calls(calls) == [
        f"obs_burn_filter.py add --host {HOST} --input SP program",
        f"obs_burn_filter.py check --host {HOST} --input SP program",
    ]


def test_the_cg_obs_password_is_passed_only_when_set(tmp_path):
    rc, _, err, calls = _run(
        tmp_path, 'CG_CHAIN=1 CG_CHAIN_OBS_PASSWORD=s3cret cg_chain_cg_burn on "$HOST" "$PY" 7')
    assert rc == 0, err
    assert _burn_calls(calls) == [
        f"obs_burn_filter.py add --host {HOST} --input sp-fast_video --password s3cret",
        f"obs_burn_filter.py check --host {HOST} --input sp-fast_video --password s3cret",
    ]
    assert "s3cret" not in err


def test_burn_obs_timeout_defaults_and_rejects_garbage(tmp_path):
    _, d, _, _ = _run(tmp_path, "unset CG_CHAIN_BURN_OBS_TIMEOUT; cg_chain_burn_obs_timeout")
    assert d == "10"
    _, o, _, _ = _run(tmp_path, "CG_CHAIN_BURN_OBS_TIMEOUT=4 cg_chain_burn_obs_timeout")
    assert o == "4"
    _, g, _, _ = _run(tmp_path, "CG_CHAIN_BURN_OBS_TIMEOUT=abc cg_chain_burn_obs_timeout")
    assert g == "10"
    _, z, _, _ = _run(tmp_path, "CG_CHAIN_BURN_OBS_TIMEOUT=0 cg_chain_burn_obs_timeout")
    assert z == "10"


# ---- [5/8]: ON only after a verified SongPlayer burn ON + the cg program cut ------------------


def test_songplayer_burn_verified_on_sets_the_sp_flag(tmp_path):
    rc, out, err, _ = _run(
        tmp_path,
        'cg_chain_songplayer_burn on; printf "SP=%s\\n" "$CG_SP_BURN_ON"; '
        'FAKE_SP_BURN_ON=false CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_songplayer_burn on; '
        'printf "SP=%s\\n" "$CG_SP_BURN_ON"',
        env={"FAKE_SP_BURN_ON": "true"})
    assert rc == 0, err
    assert [ln for ln in out.splitlines() if ln.startswith("SP=")] == ["SP=1", "SP=0"], (
        "the SP flag is 1 only after a VERIFIED SongPlayer ON; an unconfirmed ON resets it"
    )


def test_songplayer_burn_off_clears_the_sp_flag(tmp_path):
    _, out, _, _ = _run(tmp_path, 'CG_SP_BURN_ON=1; cg_chain_songplayer_burn off; '
                                  'printf "SP=%s\\n" "$CG_SP_BURN_ON"')
    assert "SP=0" in out


def test_record_start_turns_the_cg_burn_on_between_the_cut_and_start_record(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1; cg_chain_songplayer_burn on\n'
        'if cg_chain_record_start "$HOST" "$PY" 5; then echo STARTED; fi\n'
        'printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"',
        env={"FAKE_SP_BURN_ON": "true"})
    assert rc == 0, err
    assert "STARTED" in out and "FLAG=1" in out, out
    order = [c for c in calls if not c.startswith("curl")]
    cut = order.index(f"cg_chain_scene.py program --host {HOST} --scene sp-fast "
                      f"--state-file {tmp_path}/cg-chain-cg-program-state.json")
    add = order.index(f"obs_burn_filter.py add --host {HOST} --input sp-fast_video")
    start = order.index(f"obs_phase2.py record --host {HOST} --action start")
    assert cut < add < start, (
        "the cg burn goes ON after the program cut and BEFORE StartRecord, so the cg recording "
        f"carries it from its first frame: {order}"
    )


def test_record_start_burn_calls_use_the_short_burn_budget_not_the_record_timeout(tmp_path):
    # recording-e2e.sh passes the record timeout (up to 90 s) to record_start; six burn calls under
    # it could hold [5/8] for many minutes while strih + stream already record.
    rc, _, err, _ = _run(
        tmp_path,
        'CG_CHAIN=1; cg_chain_songplayer_burn on\n'
        'if cg_chain_record_start "$HOST" "$PY" 90; then echo STARTED; fi',
        env={"FAKE_SP_BURN_ON": "true"})
    assert rc == 0, err
    assert (tmp_path / "timeout.log").read_text().split() == ["90", "10", "10", "90"], (
        "cut + StartRecord keep the record timeout; the burn add + check run under "
        "CG_CHAIN_BURN_OBS_TIMEOUT (default 10)"
    )


def test_record_start_keeps_the_burn_off_without_a_verified_songplayer_burn(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_CHAIN_BURN_ATTEMPTS=1; cg_chain_songplayer_burn on\n'
        'if cg_chain_record_start "$HOST" "$PY" 5; then echo STARTED; fi\n'
        'printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"',
        env={"FAKE_SP_BURN_ON": "false"})
    assert rc == 0, err
    assert "STARTED" in out, "the cg recording still starts (the SongPlayer leg is its own proof)"
    assert _burn_calls(calls) == [], "no verified SongPlayer burn = no cg burn"
    assert "FLAG=unset" in out
    assert "cg OBS burn stays OFF" in err


def test_record_start_keeps_the_burn_off_when_the_program_cut_failed(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1; cg_chain_songplayer_burn on\n'
        'if cg_chain_record_start "$HOST" "$PY" 5; then echo STARTED; fi\n'
        'printf "FLAG=%s\\n" "${CG_BURN_ON:-unset}"',
        env={"FAKE_SP_BURN_ON": "true", "FAKE_CUT_RC": "3"})
    assert rc == 0, err
    assert _burn_calls(calls) == [], "the cg program does not show the SP scene: no cg burn"
    assert "FLAG=unset" in out
    assert "program cut to 'sp-fast' failed" in err


def test_record_start_failure_turns_the_cg_burn_straight_back_off(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1; cg_chain_songplayer_burn on\n'
        'if cg_chain_record_start "$HOST" "$PY" 5; then echo STARTED; else echo NOT-STARTED; fi\n'
        'printf "FLAG=%s\\n" "$CG_BURN_ON"',
        env={"FAKE_SP_BURN_ON": "true", "FAKE_START_RC": "1"})
    assert rc == 0, err
    assert "NOT-STARTED" in out
    verbs = [c.split()[1] for c in _burn_calls(calls)]
    assert verbs == ["add", "check", "remove", "check"], (
        "no cg recording = nothing judges the cg burn, so it never stays on the cg output"
    )
    assert "FLAG=0" in out


# ---- after the [7/8] StopRecord + cleanup(): OFF, keyed on CG_BURN_ON --------------------------


def test_after_stoprecord_turns_the_cg_burn_off_after_the_cg_stop(tmp_path):
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_BURN_ON=1\n'
        'cg_chain_after_stoprecord "$HOST" "$PY" 5\n'
        'printf "FLAG=%s\\n" "$CG_BURN_ON"',
        env={"FAKE_STOP_OUT": "C:/cg.mkv"})
    assert rc == 0, err
    stop = calls.index(f"obs_phase2.py record --host {HOST} --action stop")
    remove = calls.index(f"obs_burn_filter.py remove --host {HOST} --input sp-fast_video")
    assert stop < remove, "the cg burn stays on for the whole cg recording"
    assert "FLAG=0" in out


def test_after_stoprecord_never_touches_a_burn_this_run_did_not_turn_on(tmp_path):
    rc, _, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_RECORDING_STARTED=1 CG_BURN_ON=0\ncg_chain_after_stoprecord "$HOST" "$PY" 5')
    assert rc == 0, err
    assert _burn_calls(calls) == []


def test_cleanup_turns_the_cg_burn_off_even_after_an_early_abort(tmp_path):
    # An abort before StartRecord reported back: the burn is on, no cg recording is flagged.
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_RECORDING_STARTED=0 CG_BURN_ON=1\n'
        'cg_chain_cleanup "$HOST" "$PY" 30\n'
        'printf "FLAG=%s\\n" "$CG_BURN_ON"')
    assert rc == 0, err
    assert [c.split()[1] for c in _burn_calls(calls)] == ["remove", "check"]
    assert "FLAG=0" in out
    assert f"obs_phase2.py record --host {HOST} --action stop" not in calls, \
        "cleanup never stops a cg recording this run did not start"
    budgets = (tmp_path / "timeout.log").read_text().split()
    assert budgets[:2] == ["3", "3"], (
        "cleanup's cg burn OFF uses the SHORT per-request timeout, like the SongPlayer burn's, so "
        f"an unreachable cg OBS never stalls the teardowns after it: {budgets}"
    )


def test_cleanup_short_timeout_is_env_overridable(tmp_path):
    (tmp_path / "burn.state").write_text("on")
    rc, _, err, _ = _run(
        tmp_path,
        'CG_CHAIN=1 CG_BURN_ON=1 CG_CHAIN_CLEANUP_BURN_TIMEOUT=5\ncg_chain_cleanup "$HOST" "$PY" 30')
    assert rc == 0, err
    assert (tmp_path / "timeout.log").read_text().split()[:2] == ["5", "5"]


def test_cleanup_sends_nothing_when_the_burn_never_turned_on(tmp_path):
    rc, _, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_RECORDING_STARTED=0\nunset CG_BURN_ON\ncg_chain_cleanup "$HOST" "$PY" 30')
    assert rc == 0, err
    assert _burn_calls(calls) == [], "no ON this run = no cg OBS call from cleanup()"


# ---- cleanup()'s first pass: one quick background OFF right after the StopRecord-first block ----

_WAIT_ALL = 'for p in $CG_EARLY_BURNS_PIDS; do wait "$p"; done\n'


def test_songplayer_owed_flag_follows_what_was_sent(tmp_path):
    # OWED = 1 from the moment an ON is sent, 0 only after a verified OFF, 1 again after an OFF that
    # never verified — so a [7/8] OFF that ends in a LEAK still gets cleanup()'s first pass.
    rc, out, err, _ = _run(
        tmp_path,
        'CG_SP_BURN_OWED=0; FAKE_SP_BURN_ON=false CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_songplayer_burn on\n'
        'printf "A=%s\\n" "$CG_SP_BURN_OWED"\n'
        'FAKE_SP_BURN_ON=false cg_chain_songplayer_burn off; printf "B=%s\\n" "$CG_SP_BURN_OWED"\n'
        'FAKE_SP_BURN_ON=true CG_CHAIN_BURN_ATTEMPTS=1 cg_chain_songplayer_burn off\n'
        'printf "C=%s\\n" "$CG_SP_BURN_OWED"')
    assert rc == 0, err
    assert [ln for ln in out.splitlines() if ln[:2] in ("A=", "B=", "C=")] == ["A=1", "B=0", "C=1"]


def test_first_pass_sends_nothing_when_no_burn_is_owed(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_SP_BURN_OWED=0 CG_BURN_ON=0\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n'
        'printf "PIDS=%s\\n" "${CG_EARLY_BURNS_PIDS:-none}"')
    assert rc == 0, err
    assert calls == [] and "PIDS=none" in out and "first pass" not in out


def test_first_pass_is_inert_without_the_profile(tmp_path):
    rc, out, err, calls = _run(
        tmp_path,
        'unset CG_CHAIN; CG_SP_BURN_OWED=1 CG_BURN_ON=1\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n'
        'printf "PIDS=%s\\n" "${CG_EARLY_BURNS_PIDS:-none}"')
    assert rc == 0, err
    assert calls == [] and "PIDS=none" in out and err == ""


def test_first_pass_turns_both_burns_off_once_in_two_background_jobs(tmp_path):
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_SP_BURN_OWED=1 CG_BURN_ON=1\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n'
        'set -- $CG_EARLY_BURNS_PIDS; printf "JOBS=%s\\n" "$#"\n'
        'if kill -0 "$1" 2>/dev/null; then echo RUNNING; fi\n' + _WAIT_ALL + 'echo JOINED',
        env={"FAKE_REMOVE_SLEEP": "1", "FAKE_CHECK_MODE": "stuck-on"})
    assert rc == 0, err
    assert "JOBS=2" in out, "the cg OFF and the SongPlayer OFF are separate jobs"
    assert "RUNNING" in out and "JOINED" in out, (
        "the first pass returns at once and runs next to the camera restores, never before them"
    )
    assert any(c.startswith("curl POST") and c.endswith("/api/v1/ndi/burn") for c in calls), \
        "the SongPlayer burn gets its quick OFF too"
    assert [c.split()[1] for c in _burn_calls(calls)] == ["remove", "check"], (
        "ONE attempt only, even when the read-back fails — cg_chain_cleanup owns the retries"
    )
    assert (tmp_path / "timeout.log").read_text().split() == ["3", "3"], \
        "every call runs under the SHORT cleanup budget"
    tagged = [ln for ln in out.splitlines() if "LEAK" in ln]
    assert tagged and all("cleanup first pass" in ln for ln in tagged), (
        f"a one-try LEAK from the first pass is tagged as such, never read as the verdict: {out}"
    )


def test_first_pass_cg_off_never_waits_for_a_slow_songplayer(tmp_path):
    # SongPlayer answers slowly: the cg OFF (the burn that survives an OBS restart) must not queue
    # behind it inside the SIGKILL grace window.
    (tmp_path / "burn.state").write_text("on")
    rc, _, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_SP_BURN_OWED=1 CG_BURN_ON=1\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n' + _WAIT_ALL,
        env={"FAKE_CURL_POST_SLEEP": "3"})
    assert rc == 0, err
    remove = calls.index(f"obs_burn_filter.py remove --host {HOST} --input sp-fast_video")
    post = next(i for i, c in enumerate(calls) if c.startswith("curl POST"))
    assert remove < post, f"the cg remove ran while the SongPlayer POST was still pending: {calls}"


def test_first_pass_skips_a_burn_this_run_never_turned_on(tmp_path):
    rc, _, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_SP_BURN_OWED=1 CG_BURN_ON=0\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n' + _WAIT_ALL,
        env={"FAKE_SP_BURN_ON": "false"})
    assert rc == 0, err
    assert _burn_calls(calls) == [], "no cg burn ON this run = no cg OBS call"
    assert any(c.startswith("curl POST") for c in calls)


def test_cleanup_waits_for_the_first_pass_before_its_retries(tmp_path):
    # The first pass's remove is slow (1 s). Without the wait, cleanup's own remove would run while
    # the first pass is still inside its remove: [remove, remove, check, check].
    (tmp_path / "burn.state").write_text("on")
    rc, out, err, calls = _run(
        tmp_path,
        'CG_CHAIN=1 CG_RECORDING_STARTED=0 CG_SP_BURN_OWED=0 CG_BURN_ON=1\n'
        'cg_chain_cleanup_burns_first "$HOST" "$PY"\n'
        'cg_chain_cleanup "$HOST" "$PY" 30\n'
        'printf "FLAG=%s PIDS=%s\\n" "$CG_BURN_ON" "${CG_EARLY_BURNS_PIDS:-none}"',
        env={"FAKE_REMOVE_SLEEP": "1"})
    assert rc == 0, err
    assert [c.split()[1] for c in _burn_calls(calls)] == ["remove", "check", "remove", "check"]
    assert "FLAG=0 PIDS=none" in out, out


def test_disabled_profile_never_touches_the_cg_burn(tmp_path):
    # Even with a stale CG_BURN_ON=1 in the environment, CG_CHAIN unset is byte-for-byte inert.
    rc, out, err, calls = _run(
        tmp_path,
        'unset CG_CHAIN\nCG_BURN_ON=1\n'
        'cg_chain_after_stoprecord "$HOST" "$PY" 5\ncg_chain_cleanup "$HOST" "$PY" 5\necho DONE')
    assert rc == 0, err
    assert calls == []
    assert out.strip() == "DONE" and err == ""


# ---- recording-e2e.sh wiring (static reads) ----------------------------------------------------


def test_recording_e2e_initialises_the_flag_before_the_trap():
    s = _E2E.read_text()
    init = s.index("\nCG_BURN_ON=0\n")
    trap = s.index("\ntrap cleanup EXIT HUP INT TERM")
    assert init < trap, "cleanup()'s cg burn OFF must read an initialised flag on an early abort"


def test_recording_e2e_cleanup_sends_the_first_pass_right_after_stoprecord_first():
    s = _E2E.read_text()
    body = s[s.index("\ncleanup() {"):s.index("\ntrap cleanup EXIT HUP INT TERM")]
    imag_stop = body.index('record --host "$IMAG_IP" --action stop')
    first = body.index('cg_chain_cleanup_burns_first "${CG_HOST_IP:-}" "$HERE/obs_phase2.py"')
    heartbeat = body.index("rig_heartbeat_stop")
    device_free = body.index("rm -f /tmp/camera-box-burn-*")
    late = body.index('cg_chain_cleanup "${CG_HOST_IP:-}"')
    assert imag_stop < first < heartbeat < device_free < late, (
        "the quick burn OFF follows the StopRecord-first block, ahead of everything slower, and "
        "the retrying cg_chain_cleanup stays after the camera restores"
    )


def test_recording_e2e_starts_the_cg_leg_only_inside_the_profile_gate():
    s = _E2E.read_text()
    gate = s.index("if cg_chain_enabled; then\n  echo \"[5/8] #1301 CG_CHAIN=1")
    start = s.index("cg_chain_record_start \"$CG_HOST_IP\"")
    end = s.index("\nfi\n", start)
    assert gate < start < end, "the cg burn ON (inside cg_chain_record_start) runs only with CG_CHAIN=1"
