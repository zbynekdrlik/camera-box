"""Issue 1424 (design 6386467303 layer 3) -- verify-strih item 4c reads the retry supervisor's lines.

With the in-OBS retry an output's log can hold `program scanout LIVE` from before its monitor went away,
then `retry: '<c>' lost its present loop`, the supervisor's own `stopped (vk-direct, '<c>')` (the
release) and `retry: '<c>' dormant -- <cause>` lines. The old grader read that as LIVE: the stop after the
loop's exit line cleared the death, and the old LIVE line stayed. Now:

  * `strih_drm_vk_present_dead`: a `lost its present loop` keeps the output dead through the
    supervisor's release stop, until `retry: '<c>' LIVE again`;
  * `strih_drm_output_retry_state` names the LAST retry line of an output (live / dormant / none), and
    the per-output grader says so for a dormant output instead of a bare "not live".
"""
import os
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent.parent
DRM_LIB = REPO / "scripts" / "lib" / "strih-drm-output.sh"

LIVE = "10:00:00.000: drm-output: program scanout LIVE (vk-direct: a published frame reached 'HDMI-0')\n"
EXIT = "10:05:00.000: drm-output: vk-direct present loop exited on 'HDMI-0' after 18000 presents (0 publishes skipped behind a ready frame)\n"
LOST = "10:05:00.200: drm-output: retry: 'HDMI-0' lost its present loop -- releasing the display and retrying its start (no OBS restart)\n"
STOP = "10:05:00.300: drm-output: stopped (vk-direct, 'HDMI-0')\n"
DORM = "10:05:02.300: drm-output: retry: 'HDMI-0' dormant -- vk-direct: no Vulkan device drives output 'HDMI-0' (attempt 1, X reads it disconnected; this line repeats only when the reason changes)\n"
DORM0 = "10:00:00.500: drm-output: retry: 'HDMI-0' dormant -- its start at the OBS launch failed (read the lines above); retrying every 2 s (30 s while X reads it disconnected, at once when it comes back)\n"
AGAIN = "10:07:00.000: drm-output: retry: 'HDMI-0' LIVE again after 12 attempt(s) -- no OBS restart\n"


def _bash(body, stdin=""):
    r = subprocess.run(["bash", "-c", 'set -euo pipefail\n. "%s"\n%s' % (DRM_LIB, body)], input=stdin,
                       capture_output=True, text=True, cwd=str(REPO), env=dict(os.environ))
    assert r.returncode == 0, r.stderr
    return r.stdout.strip()


def _dead(log):
    return _bash("if strih_drm_vk_present_dead; then echo dead; else echo alive; fi", log)


def test_a_lost_output_stays_dead_through_the_release_stop():
    assert _dead(LIVE + EXIT + LOST + STOP) == "dead"
    assert _dead(LIVE + EXIT + LOST + STOP + DORM) == "dead"


def test_live_again_clears_it():
    assert _dead(LIVE + EXIT + LOST + STOP + DORM + AGAIN) == "alive"


def test_the_old_meanings_hold():
    assert _dead(LIVE + EXIT) == "dead"
    assert _dead(LIVE + EXIT + STOP) == "alive", "a clean halt -> exit -> stop is no death"
    assert _dead(LIVE) == "alive"


def _state(log):
    return _bash("strih_drm_output_retry_state", log)


def test_retry_state_is_the_last_retry_line():
    assert _state(LIVE) == ""
    assert _state(DORM0) == "dormant"
    assert _state(LIVE + EXIT + LOST + STOP) == "dormant"
    assert _state(LIVE + EXIT + LOST + STOP + DORM + AGAIN) == "live"
    assert _state(DORM0 + AGAIN + EXIT + LOST) == "dormant"


def test_the_per_output_grader_names_a_dormant_output():
    body = DRM_LIB.read_text().split("strih_drm_outputs_verify_report() {", 1)[1].split("\n}\n", 1)[0]
    assert "strih_drm_output_retry_state" in body
    assert "retried by OBS itself" in body
