"""issue 1316 -- rig-health-audit's check_imag must render a neutral RETIRED row (never a permanent
FAIL "unreachable over ssh") once imag-nb is returned to the owner, and RETIRED must not change the
audit exit code (it is neutral like the NOTE verdict). Retirement is read from the rig-wide source
of truth (a `imag:`/`imag-nb:` ack in rig-fleet.txt), env-overridable via RIG_HEALTH_IMAG_RETIRED.

Pure (no ssh / no WS): check_imag short-circuits on retirement BEFORE any ssh, so this runs offline.
"""
import importlib.util
from pathlib import Path

import pytest

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"


def _load_module():
    spec = importlib.util.spec_from_file_location(
        "rig_health_audit", SCRIPTS / "rig-health-audit.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


_mod = _load_module()


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch):
    # Default: no override; each test sets what it needs. Clear the results accumulator per test.
    monkeypatch.delenv("RIG_HEALTH_IMAG_RETIRED", raising=False)
    _mod.results.clear()
    yield
    _mod.results.clear()


def test_env_override_marks_retired(monkeypatch, tmp_path):
    for truthy in ("1", "true", "yes", "returned-16.9.2026"):
        monkeypatch.setenv("RIG_HEALTH_IMAG_RETIRED", truthy)
        assert _mod.imag_is_retired() is True, truthy
    # An explicit falsey override WINS even when the checked-in rig-fleet.txt carries the real
    # imag:/imag-nb: acks (it does since 16.9.2026) -- no path redirect here on purpose.
    for falsey in ("0", "false", "no"):
        monkeypatch.setenv("RIG_HEALTH_IMAG_RETIRED", falsey)
        assert _mod.imag_is_retired() is False, falsey
    # EMPTY = no override -> falls through to the ack file; hermetic: point it at a dir without one.
    monkeypatch.setenv("RIG_HEALTH_IMAG_RETIRED", "")
    monkeypatch.setattr(_mod.os.path, "abspath", lambda p: str(tmp_path / "scripts" / "x.py"))
    (tmp_path / "scripts").mkdir()
    assert _mod.imag_is_retired() is False, "empty override + no ack file"


def test_rig_fleet_ack_marks_retired(monkeypatch, tmp_path):
    # An `imag:`/`imag-nb:` non-comment ack in rig-fleet.txt (pointed at via a temp file) -> retired.
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text(
        "# header comment\n"
        "# imag: this is only a COMMENT, must not count\n"
        "imag:returned-to-owner-2026-09-16\n"
        "imag-nb:returned-to-owner-2026-09-16\n",
        encoding="utf-8")
    # Redirect the module's rig-fleet lookup at the temp file by monkeypatching os.path.dirname's
    # base: simplest is to write the ack into a rig-fleet.txt next to a fake __file__ dir. Instead,
    # exercise the parser directly against the temp path via a tiny reimplementation-free check:
    monkeypatch.setattr(_mod.os.path, "abspath", lambda p: str(tmp_path / "scripts" / "x.py"))
    (tmp_path / "scripts").mkdir()
    assert _mod.imag_is_retired() is True


def test_no_ack_no_env_is_not_retired(monkeypatch, tmp_path):
    fleet = tmp_path / "rig-fleet.txt"
    fleet.write_text("# no imag ack here\ncam1:some-other-box\n", encoding="utf-8")
    monkeypatch.setattr(_mod.os.path, "abspath", lambda p: str(tmp_path / "scripts" / "x.py"))
    (tmp_path / "scripts").mkdir()
    assert _mod.imag_is_retired() is False


def test_missing_rig_fleet_is_not_retired(monkeypatch, tmp_path):
    # Fail-safe: a missing rig-fleet.txt must NOT read as retired (keep auditing).
    monkeypatch.setattr(_mod.os.path, "abspath", lambda p: str(tmp_path / "scripts" / "x.py"))
    (tmp_path / "scripts").mkdir()
    assert _mod.imag_is_retired() is False


def test_check_imag_emits_retired_row_and_is_exit_neutral(monkeypatch, capsys):
    monkeypatch.setenv("RIG_HEALTH_IMAG_RETIRED", "1")
    _mod.check_imag()
    out = capsys.readouterr().out
    assert "[RETIRED] imag" in out
    assert "RETIRED" in out and "returned" in out
    # Neutral for the exit code: main() counts only FAIL / WARN.
    assert _mod.results == [_mod.IMAG_RETIRED_VERDICT]
    assert _mod.results.count("FAIL") == 0
    assert _mod.results.count("WARN") == 0
