"""#312 Phase-2 — unit tests for the obs_phase2 `switch` subcommand wiring.

The all-cambox sweep (scripts/recording-e2e.sh ALL_CAMBOX=1) cuts each cambox into strih
PROGRAM via `obs_phase2.py switch --host H --program-scene "<scene>"`, which prints the switch
epoch-ns boundary. These tests pin the argument-parsing + dispatch wiring (NOT the live
WebSocket SetCurrentProgramScene / non-black self-check, which need a real OBS) so a future edit
that drops the `switch` subcommand or its required `--program-scene` fails loudly in CI.
"""
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_switch", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_switch"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_switch_function_exists():
    # The all-cambox sweep depends on this entrypoint existing.
    assert callable(obs_phase2.switch)


def test_switch_subcommand_parses_and_dispatches(monkeypatch):
    # `obs_phase2.py switch --host H --program-scene "Cam 5"` must parse and dispatch to switch()
    # — never to setup/record/teardown/prod_scene. Patch switch() to capture the parsed args.
    captured = {}

    def fake_switch(a):
        captured["host"] = a.host
        captured["program_scene"] = a.program_scene

    monkeypatch.setattr(obs_phase2, "switch", fake_switch)
    monkeypatch.setattr(
        sys, "argv",
        ["obs_phase2.py", "switch", "--host", "10.77.9.202", "--program-scene", "Cam 5"],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "program_scene": "Cam 5"}


def test_switch_requires_program_scene(monkeypatch):
    # The program scene to cut to is mandatory — a switch with no target scene is a harness bug,
    # never a silent no-op.
    monkeypatch.setattr(sys, "argv", ["obs_phase2.py", "switch", "--host", "h"])
    with pytest.raises(SystemExit):
        obs_phase2.main()
