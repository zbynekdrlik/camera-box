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
    # helper treats an all-zero (max luma 0) frame as black and a frame with any signal
    # as non-black.
    assert obs_phase2._luma_is_black(luma_max=0, luma_mean=0.0) is True
    assert obs_phase2._luma_is_black(luma_max=255, luma_mean=30.8) is False
    # A near-zero mean but real peak (a mostly-dark but signal-bearing camera frame, e.g.
    # the live 'Cam 5' read at mean≈30, max=255) must NOT be flagged black.
    assert obs_phase2._luma_is_black(luma_max=255, luma_mean=1.0) is False
