"""#105 recording-based E2E: unit tests for the obs_phase2 `record` subcommand wiring.

The recording-based full-path harness (scripts/recording-e2e.sh) drives OBS program
recording via `obs_phase2.py record --action start|stop|status`. These tests pin the
argument-parsing + dispatch wiring (NOT the live WebSocket calls, which need a real
OBS) so a future edit that drops the `record` subcommand or its `--action` choices
fails loudly in CI.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_rec", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_rec"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_record_function_exists():
    # The recording-based harness depends on this entrypoint existing.
    assert callable(obs_phase2.record)


def test_record_subcommand_parses_each_action(monkeypatch):
    # `obs_phase2.py record --action {start,stop,status} --host H` must parse and
    # dispatch to record() — never to setup/teardown. Patch record() to capture args.
    captured = {}

    def fake_record(a):
        captured["host"] = a.host
        captured["action"] = a.action

    monkeypatch.setattr(obs_phase2, "record", fake_record)
    for action in ("start", "stop", "status"):
        captured.clear()
        monkeypatch.setattr(
            sys, "argv",
            ["obs_phase2.py", "record", "--host", "10.77.9.202", "--action", action],
        )
        obs_phase2.main()
        assert captured == {"host": "10.77.9.202", "action": action}


def test_record_rejects_unknown_action(monkeypatch):
    # An invalid --action must be rejected by argparse (choices guard), never silently
    # passed through to a no-op.
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "record", "--host", "h", "--action", "bogus"],
    )
    with pytest.raises(SystemExit):
        obs_phase2.main()


def test_record_action_is_required(monkeypatch):
    # Omitting --action must fail (a recording call with no action is a harness bug).
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "record", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()


# ---------------------------------------------------------------------------
# #163: `prod-scene` — route OBS program to a CERTIFIED PRODUCTION scene and record
# IT, instead of pointing the colliding `phase2-probe-src` ndi_source at a source-name
# the always-on prod input already holds (which records pure BLACK — see #163). These
# tests pin the new entrypoint + its non-black fail-fast self-check (pure luma helper),
# without any live WebSocket.
# ---------------------------------------------------------------------------

def test_prod_scene_function_exists():
    # The recording harness routes program via this new entrypoint (records the prod
    # scene program, never the colliding probe input).
    assert callable(obs_phase2.prod_scene)


def test_prod_scene_subcommand_parses_and_dispatches(monkeypatch):
    # `obs_phase2.py prod-scene --host H --program-scene "Cam 5"` must parse and
    # dispatch to prod_scene() — never to setup/record/teardown.
    captured = {}

    def fake_prod_scene(a):
        captured["host"] = a.host
        captured["program_scene"] = a.program_scene

    monkeypatch.setattr(obs_phase2, "prod_scene", fake_prod_scene)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "prod-scene", "--host", "10.77.9.202",
         "--program-scene", "Cam 5"],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "program_scene": "Cam 5"}


def test_prod_scene_requires_program_scene(monkeypatch):
    # The program scene name is mandatory — routing program with no target scene is a
    # harness bug, never a silent no-op.
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "prod-scene", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()


def test_is_black_luma_helper_flags_black_and_passes_nonblack():
    # The fail-fast non-black self-check (#163 candidate fix 3): a recorded-black probe
    # ingest must be caught BEFORE StartRecord wastes a full run. The pure decision
    # helper treats an all-zero (peak luma 0) frame as black and a frame with any signal
    # (non-zero peak) as non-black — the decision is on the PEAK only.
    assert obs_phase2._luma_is_black(luma_max=0) is True
    assert obs_phase2._luma_is_black(luma_max=255) is False
    # A frame with a real peak (e.g. the live 'Cam 5' read peak=255) is NON-black even
    # when its mean is low (a mostly-dark but signal-bearing camera frame).
    assert obs_phase2._luma_is_black(luma_max=1) is False


def test_blackcheck_verdict_polls_until_deadline_before_failing():
    # REGRESSION (#111 deploy): the non-black self-check used a SINGLE read 2 s after the
    # scene switch. A cold DistroAV NDI receiver (high genlock_preload, re-establishing
    # from idle) needs longer than 2 s to fill its FIFO and render the first non-black
    # frame — so the single-shot read saw BLACK and aborted a fully-healthy run. The fix
    # makes the check POLL: a black read BEFORE the deadline means WAIT (keep polling),
    # not FAIL; only black AT/AFTER the deadline is a real FAIL.
    #
    # The pure verdict over (luma_max, elapsed_s, timeout_s):
    #   - non-black peak           -> "OK"   (proceed immediately, however early)
    #   - black, elapsed < timeout -> "WAIT" (receiver may still be filling; keep polling)
    #   - black, elapsed >= timeout-> "BLACK"(genuinely dead source; abort)
    #   - luma unreadable (None)   -> "WAIT" while in budget, else "UNKNOWN" (never silent OK)
    assert obs_phase2._blackcheck_verdict(luma_max=255, elapsed_s=0.0, timeout_s=15.0) == "OK"
    assert obs_phase2._blackcheck_verdict(luma_max=1, elapsed_s=14.9, timeout_s=15.0) == "OK"
    # Black early in the budget = WAIT (this is the exact case the single-shot 2 s read
    # mis-classified as FAIL and aborted the #111 run).
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=2.0, timeout_s=15.0) == "WAIT"
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=14.0, timeout_s=15.0) == "WAIT"
    # Black at/after the deadline = genuine BLACK -> abort.
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=15.0, timeout_s=15.0) == "BLACK"
    assert obs_phase2._blackcheck_verdict(luma_max=0, elapsed_s=20.0, timeout_s=15.0) == "BLACK"
    # Unreadable luma: WAIT while in budget (retry the screenshot), UNKNOWN past it (never
    # a silent OK — the caller warns + proceeds, recording-verdict still catches all-black).
    assert obs_phase2._blackcheck_verdict(luma_max=None, elapsed_s=3.0, timeout_s=15.0) == "WAIT"
    assert obs_phase2._blackcheck_verdict(luma_max=None, elapsed_s=15.0, timeout_s=15.0) == "UNKNOWN"


SCENES = ["Cam 5", "Cam 1", "test 2", "REC-STRIH-TMP", "POST"]


def test_restore_target_real_prod_scene_is_faithful_even_when_it_was_program():
    # strih 'Cam 5' is a REAL prod scene. If it was ALREADY the live program when the run
    # started, the faithful restore keeps the box on 'Cam 5' — never bumps it onto an
    # arbitrary other scene (the smoke-run regression that left strih on 'test 2').
    assert obs_phase2._restore_target(
        prev="Cam 5", target="Cam 5", ephemeral=False, scenes=SCENES
    ) == "Cam 5"
    # And if a different scene was program, that scene is restored.
    assert obs_phase2._restore_target(
        prev="POST", target="Cam 5", ephemeral=False, scenes=SCENES
    ) == "POST"


def test_restore_target_ephemeral_scene_is_never_restored_to_itself():
    # The stream temp scene (built via --ensure-source) is EPHEMERAL: restoring program to
    # it would strand the throwaway record scene as live program. If it was program (a
    # crash), recover the saved prior; else fall back to any OTHER existing scene.
    assert obs_phase2._restore_target(
        prev="REC-STRIH-TMP", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
        saved_prev="POST",
    ) == "POST"
    # No saved prior → fall back to some other (never the ephemeral target).
    got = obs_phase2._restore_target(
        prev="REC-STRIH-TMP", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
    )
    assert got != "REC-STRIH-TMP" and got in SCENES
    # A normal prior program (the usual case) is restored verbatim.
    assert obs_phase2._restore_target(
        prev="POST", target="REC-STRIH-TMP", ephemeral=True, scenes=SCENES,
    ) == "POST"
