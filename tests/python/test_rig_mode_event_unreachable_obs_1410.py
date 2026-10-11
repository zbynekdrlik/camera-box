"""issue 1410: an unreachable OBS box must never stop the EVENT switch half-way.

On 11.10.2026 strih-lx (10.77.9.202) was powered off when the owner asked for EVENT mode. The
stray-recording guard got `OSError: [Errno 113] No route to host`, and rig-mode.sh called it bare
under `set -euo pipefail`, so EVENT ended there: the cam2 painter stop, the relay start, the
program-audio stop, the burn clear, the EVENT contract and the Discord confirmation never ran. The
burn clear (`toggle_burn event`) and the strih NDI-mapping enforce also reach strih and return its
rc, so with only the guard fixed EVENT would still end at the burn step.

Every one of those three steps now gets the issue-868 / issue-1407 record-and-fold shape: the rc is
recorded, a named WARNING goes to stderr, every later step runs, each failure has its own RESULT
line, the run exits non-zero even when the contract passes, and the owner's Discord confirmation
carries one plain-Slovak line on top so the phone never reads "clean" for a failed run.

These tests RUN the real EVENT body through the issue-1407 harness (tests/python/
test_rig_mode_relay_rc_1407.py): every other function is a step-logging stub, the relay chain runs
against healthy fake read-only boxes, and the three steps are overridden to fail. Tier-0: no cargo,
no rig, no network.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_rig_mode_relay_rc_1407 import _at, _run  # noqa: E402

FAIL_GUARD = "stop_stray_recordings() { _stub_step stop_stray_recordings; return 1; }"
FAIL_BURN = 'toggle_burn() { _stub_step toggle_burn "$@"; [ "$1" != event ]; }'
FAIL_MAP = "enforce_strih_ndi_mapping() { _stub_step enforce_strih_ndi_mapping; return 1; }"
FAIL_FIRST_CAM_SSH = (
    'cam_ssh() { _stub_step cam_ssh; if [ ! -e "$STEPS.camfail" ]; then : >"$STEPS.camfail"; return 7; fi; }'
)

# the EVENT steps that must run after the stray-recording guard, in this order
AFTER_GUARD = ("cam_ssh", "toggle_burn event", "enforce_strih_ndi_mapping", "print_genlock_relaunch_note event",
               "event_mode_assert", "event_mode_discord_confirm_send")


def _results(proc):
    return [ln for ln in proc.stderr.splitlines() if ln.startswith("RESULT:")]


def _warnings(proc):
    return [ln for ln in proc.stderr.splitlines() if ln.startswith("WARNING issue 1410")]


def _ran_in_order(ran, steps):
    order = [_at(ran, s) for s in steps]
    assert order == sorted(order), "\n".join(ran)


def test_strih_powered_off_runs_every_event_step_and_names_each_failure(tmp_path):
    # the 11.10.2026 case: strih unreachable makes all three strih-touching steps fail
    proc, ran, _boxes, sent = _run(tmp_path, "event", extra="\n".join((FAIL_GUARD, FAIL_BURN, FAIL_MAP)))
    out = proc.stdout + proc.stderr
    assert proc.returncode == 1, out
    _ran_in_order(ran, ("stop_stray_recordings",) + AFTER_GUARD)
    assert len(_warnings(proc)) == 3, proc.stderr
    results = _results(proc)
    assert any("stray-recording guard FAILED (issue 1410" in ln for ln in results), proc.stderr
    assert any("genlock_burn OFF FAILED (issue 1410" in ln for ln in results), proc.stderr
    assert any("strih NDI mapping enforce FAILED (issue 1410" in ln for ln in results), proc.stderr
    assert not any("CONTRACT CONFIRMED" in ln for ln in proc.stdout.splitlines()), out
    top = sent.splitlines()[0]
    assert top.startswith("⚠️") and "nahrávok" in top and "QR" in top and "NDI" in top, sent


def test_failed_stray_guard_alone_with_a_passing_contract_still_exits_nonzero(tmp_path):
    proc, ran, _boxes, sent = _run(tmp_path, "event", extra=FAIL_GUARD)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    _ran_in_order(ran, ("stop_stray_recordings",) + AFTER_GUARD)
    warn = _warnings(proc)
    assert len(warn) == 1 and "stray-recording guard FAILED (rc=1)" in warn[0], proc.stderr
    assert [ln for ln in _results(proc) if "issue 1410" in ln] == [
        ln for ln in _results(proc) if "stray-recording guard FAILED (issue 1410, rc=1)" in ln
    ], proc.stderr
    assert any("stray-recording guard" in ln for ln in _results(proc)), proc.stderr
    assert sent.splitlines()[0].startswith("⚠️") and "nahrávok" in sent.splitlines()[0], sent


def test_failed_burn_clear_alone_still_runs_the_mapping_the_contract_and_discord(tmp_path):
    proc, ran, _boxes, sent = _run(tmp_path, "event", extra=FAIL_BURN)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    _ran_in_order(ran, AFTER_GUARD)
    results = _results(proc)
    assert any("genlock_burn OFF FAILED (issue 1410, rc=1)" in ln for ln in results), proc.stderr
    assert not any("stray-recording guard" in ln for ln in results), proc.stderr
    assert sent.splitlines()[0].startswith("⚠️") and "QR" in sent.splitlines()[0], sent
    # the burn clear also reaches an unacknowledged imag: the line must not blame only strih/stream
    assert "imag" in sent.splitlines()[0], sent


def test_failed_ndi_mapping_alone_still_runs_the_contract_and_discord(tmp_path):
    proc, ran, _boxes, sent = _run(tmp_path, "event", extra=FAIL_MAP)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    _ran_in_order(ran, AFTER_GUARD)
    assert any("strih NDI mapping enforce FAILED (issue 1410, rc=1)" in ln for ln in _results(proc)), proc.stderr
    assert sent.splitlines()[0].startswith("⚠️") and "NDI" in sent.splitlines()[0], sent


def test_healthy_event_adds_no_issue_1410_line_and_passes(tmp_path):
    proc, _ran, _boxes, sent = _run(tmp_path, "event")
    assert proc.returncode == 0, proc.stdout + proc.stderr
    assert "issue 1410" not in proc.stdout + proc.stderr
    assert sent.splitlines()[0] == "EVENT kontrakt potvrdeny", sent


def test_a_failed_cam_side_restore_never_hides_a_failed_contract(tmp_path):
    # review finding: every folded failure is printed; the contract line used to be an `elif` behind
    # the cam-side line, so a run where both failed named only the cam side
    proc, _ran, _boxes, _sent = _run(tmp_path, "event", assert_pass=1, extra=FAIL_FIRST_CAM_SSH)
    assert proc.returncode == 1, proc.stdout + proc.stderr
    results = _results(proc)
    assert any("cam-side restore FAILED" in ln for ln in results), proc.stderr
    assert any("#722 CONTRACT FAILED" in ln for ln in results), proc.stderr
