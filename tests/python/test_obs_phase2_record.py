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
