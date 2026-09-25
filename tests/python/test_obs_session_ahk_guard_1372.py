"""Issue 1372 (owner ruling "ale ahk nespustaj"): the dev1 obs-session watchdog must not page when the
owner has AutoHotkey switched OFF on resolume.

A box whose AHK watcher is owner-controlled is graded in `guard` mode: AHK count 0 is healthy; a
DUPLICATED watcher (count > 1) or one stuck in session 0 is still a fault (it would respawn obs64
there). A `1` (managed) box keeps the strict exactly-one rule.
"""
import subprocess
from pathlib import Path

_LIB = Path(__file__).resolve().parents[2] / "scripts" / "lib" / "obs-session-visibility.sh"
_WATCHDOG = Path(__file__).resolve().parents[2] / "scripts" / "obs-session-watchdog.sh"


def _msg(probe: str, has_ahk: str) -> str:
    script = f'. "{_LIB}"\nobs_session_visibility_message "$PROBE" "$HAS_AHK"\n'
    r = subprocess.run(["bash", "-c", script], capture_output=True, text=True,
                       env={"PATH": "/usr/bin:/bin", "PROBE": probe, "HAS_AHK": has_ahk})
    assert r.returncode == 0, r.stderr
    return r.stdout


def _probe(ahk_count: int, ahk_session: int = 1) -> str:
    lines = ["ACTIVE_SESSION=1", "OWN_SESSION=0", "OBS_COUNT=1", "OBS_SESSION=1",
             "OBS_TITLE=OBS Studio build abc - Profile: cg", f"AHK_COUNT={ahk_count}"]
    if ahk_count:
        lines.append(f"AHK_SESSION={ahk_session}")
    return "\r\n".join(lines) + "\r\n"


def test_guard_mode_accepts_ahk_switched_off():
    assert _msg(_probe(0), "guard") == ""


def test_guard_mode_accepts_one_ahk_in_the_active_session():
    assert _msg(_probe(1), "guard") == ""


def test_guard_mode_still_faults_a_duplicated_watcher():
    assert "count=2" in _msg(_probe(2), "guard")


def test_guard_mode_still_faults_an_ahk_in_session_zero():
    assert "SessionId=0" in _msg(_probe(1, ahk_session=0), "guard")


def test_managed_mode_keeps_the_exactly_one_rule():
    assert "count=0" in _msg(_probe(0), "1")


def test_the_watchdog_grades_an_ahk_box_in_guard_mode():
    """obs_session_targets must hand the owner-controlled AHK box to the grader as `guard`."""
    text = _WATCHDOG.read_text()
    assert 'obs_fleet_has_ahk "$name"' in text
    assert "guard" in text.split("obs_session_targets()", 1)[1].split("\n}\n", 1)[0]
