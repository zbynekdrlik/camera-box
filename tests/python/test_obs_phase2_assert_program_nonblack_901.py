"""#901 gap 1 — unit tests for the obs_phase2 `assert-program-nonblack` subcommand.

Live evidence (2026-08-04 supervisor comment on issue 901): the cam2 painter process was alive,
its pidfile matched, its marker CSV was growing, and ALSA reported RUNNING -- yet the PROGRAM was
BLACK for a whole 150s recording. "process alive" is not "QR on screen". This subcommand is a
thin, read-only wrapper around the EXISTING `_assert_program_nonblack` helper (the same
#163/#111/#312 polled luma-peak self-check `switch()` already uses) applied to whatever scene is
CURRENTLY on program, WITHOUT switching to it -- a pure verification call, never a control op.
"""
import argparse
import importlib.util
import pathlib
import sys

import pytest

_MOD_PATH = pathlib.Path(__file__).resolve().parents[2] / "scripts" / "obs_phase2.py"
_spec = importlib.util.spec_from_file_location("obs_phase2_apn", _MOD_PATH)
obs_phase2 = importlib.util.module_from_spec(_spec)
sys.modules["obs_phase2_apn"] = obs_phase2
_spec.loader.exec_module(obs_phase2)


def test_assert_program_nonblack_function_exists():
    assert callable(obs_phase2.assert_program_nonblack)


def test_assert_program_nonblack_subcommand_parses_and_dispatches(monkeypatch):
    captured = {}

    def fake(a):
        captured["host"] = a.host
        captured["scene"] = a.scene
        captured["label"] = a.label
        captured["min_mean"] = a.min_mean

    monkeypatch.setattr(obs_phase2, "assert_program_nonblack", fake)
    monkeypatch.setattr(
        sys, "argv",
        [
            "obs_phase2.py", "assert-program-nonblack", "--host", "10.77.9.202",
            "--scene", "Cam 2", "--label", "#901", "--min-mean", "30",
        ],
    )
    obs_phase2.main()
    assert captured == {"host": "10.77.9.202", "scene": "Cam 2", "label": "#901", "min_mean": 30.0}


def test_assert_program_nonblack_scene_label_min_mean_are_optional(monkeypatch):
    captured = {}

    def fake(a):
        captured["scene"] = a.scene
        captured["label"] = a.label
        captured["min_mean"] = a.min_mean

    monkeypatch.setattr(obs_phase2, "assert_program_nonblack", fake)
    monkeypatch.setattr(
        sys, "argv", ["obs_phase2.py", "assert-program-nonblack", "--host", "h"]
    )
    obs_phase2.main()
    assert captured == {"scene": "", "label": "", "min_mean": None}


class _FakeWS:
    def close(self):
        pass


def test_assert_program_nonblack_resolves_current_scene_and_calls_the_shared_helper(monkeypatch, capsys):
    calls = {}

    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        assert rtype == "GetCurrentProgramScene"
        return {"currentProgramSceneName": "Cam 2"}

    def fake_assert_nonblack(ws, host, scene, label, hint, min_mean=None):
        calls["host"] = host
        calls["scene"] = scene
        calls["label"] = label
        calls["min_mean"] = min_mean

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", fake_assert_nonblack)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="", label="", min_mean=None)
    obs_phase2.assert_program_nonblack(a)
    assert calls["host"] == "10.77.9.202"
    assert calls["scene"] == "Cam 2"
    out = capsys.readouterr().out
    assert "Cam 2" in out
    assert "PASS" in out


def test_assert_program_nonblack_uses_given_scene_without_resolving_current(monkeypatch, capsys):
    calls = {}

    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        raise AssertionError("must not call GetCurrentProgramScene when --scene is given")

    def fake_assert_nonblack(ws, host, scene, label, hint, min_mean=None):
        calls["scene"] = scene

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", fake_assert_nonblack)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="Cam 5", label="", min_mean=None)
    obs_phase2.assert_program_nonblack(a)
    assert calls["scene"] == "Cam 5"


def test_assert_program_nonblack_propagates_the_black_systemexit(monkeypatch):
    def fake_conn(host, password=""):
        return _FakeWS()

    def fake_rpc(ws, rtype, rdata=None, ignore_err=False, timeout_s=None):
        return {"currentProgramSceneName": "Cam 2"}

    def fake_assert_nonblack(ws, host, scene, label, hint, min_mean=None):
        raise SystemExit(f"[obs] {host}: {label} self-check FAIL — program scene '{scene}' renders BLACK")

    monkeypatch.setattr(obs_phase2, "_conn", fake_conn)
    monkeypatch.setattr(obs_phase2, "_rpc", fake_rpc)
    monkeypatch.setattr(obs_phase2, "_assert_program_nonblack", fake_assert_nonblack)
    a = argparse.Namespace(host="10.77.9.202", password="", scene="", label="", min_mean=None)
    with pytest.raises(SystemExit):
        obs_phase2.assert_program_nonblack(a)
