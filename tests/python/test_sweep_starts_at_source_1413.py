"""issue 1413 — the default ALL-CAMBOX sweep starts at the SOURCE camera.

recording-e2e.sh `[2/8]` puts the strih program on the source camera (camera_source_box: cam5,
the test-camera box, since 14.10.2026) before StartRecord. The sweep's first window must be that
same camera: a first window on another camera puts a program cut at the switch schedule's START,
where the frames before it sit outside every schedule window, and the strih 911002 per-input
counter jump there is charged as a real drop (runs 242352390 .. 70392849, one strih real drop at
~frame 280 each). Sources camera-set.sh for real, exactly as recording-e2e.sh and av-soak.sh do.
"""

import os
import subprocess
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[2] / "scripts"


def _bash(script, **env_over):
    env = {k: v for k, v in os.environ.items()
           if k not in ("CAMERA_ACTIVE_SET", "CAMERA_SOURCE_BOX", "CAMERA_TEST_CAMERA_BOX")}
    env.update(env_over)
    out = subprocess.run(
        ["bash", "-c", f'set -euo pipefail\n. "{SCRIPTS}/camera-set.sh"\n{script}'],
        capture_output=True, text=True, env=env, check=True,
    )
    return out.stdout


def _first(**env_over):
    return _bash('printf %s "$(camera_active_sweep_pairs)"', **env_over).split(" Cam ")[0]


def test_default_sweep_opens_on_the_default_source_camera():
    source = _bash("camera_source_box")
    assert source == "cam5", source
    assert _first() == "Cam 5:CAM5"


def test_default_sweep_keeps_every_active_camera_once():
    pairs = _bash('printf %s "$(camera_active_sweep_pairs)"')
    active = _bash('printf %s "$CAMERA_ACTIVE_SET"').split()
    labels = sorted(p.split(":")[1].strip() for p in pairs.replace("Cam ", "\n").split("\n") if ":" in p)
    assert labels == sorted(c.upper() for c in active), (pairs, active)


def test_rotation_keeps_the_set_order_after_the_source():
    # cam2 cam3 cam5 with cam5 as source: cam5 first, then wrap to cam2, cam3.
    assert _bash('printf %s "$(camera_active_sweep_pairs)"') == \
        "Cam 5:CAM5 Cam 2:CAM2 Cam 3:CAM3"


def test_explicit_source_override_leads_the_sweep():
    out = _bash('printf %s "$(camera_active_sweep_pairs)"',
                CAMERA_ACTIVE_SET="cam1 cam2 cam3", CAMERA_SOURCE_BOX="cam3")
    assert out == "Cam 3:CAM3 Cam 1:CAM1 Cam 2:CAM2", out


def test_a_set_whose_first_member_is_the_source_is_unchanged():
    out = _bash('printf %s "$(camera_active_sweep_pairs)"', CAMERA_ACTIVE_SET="cam1 cam2 cam3")
    assert out == "Cam 1:CAM1 Cam 2:CAM2 Cam 3:CAM3", out


def test_a_set_with_no_routable_source_keeps_its_order():
    # cam2 alone is the painter, never a source: the sweep is still the set, never empty.
    out = _bash('printf %s "$(camera_active_sweep_pairs)"', CAMERA_ACTIVE_SET="cam2")
    assert out == "Cam 2:CAM2", out
