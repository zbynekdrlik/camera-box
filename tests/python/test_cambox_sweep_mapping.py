"""#24 — regression guard: recording-e2e.sh's ALL_CAMBOX default sweep MUST stay consistent with
the CANONICAL strih NDI-input->camera ACTIVE mapping (set-ndi-mapping.py active_map(), #399/#827).

The strih scene names ("Cam 1"/"Cam 3"/"Cam 5") are the SAME inversion as the NDI input labels
they show (.claude/skills/genlock/SKILL.md "strih NDI Input -> Camera Mapping (INVERTED)"): scene
"Cam N" renders input "NDI camN". #399 remapped 'Cam 1' from CAM4 to CAM3 *after*
recording-e2e.sh's sweep default was written, silently leaving the default WRONG (it would
attribute CAM3's frames to the "CAM4" label in the switch-schedule) and leaving CAM3 excluded from
zero-loss coverage even though its earlier exclusion reason (#301, cam3 SSH down) has since closed
— exactly the kind of hand-written-literal drift this test exists to catch.

#827 (2026-07-27, binding owner directive): BOTH sources of truth now DERIVE from the SAME
CAMERA_ACTIVE_SET (scripts/camera-set.sh) instead of a hardcoded literal, so cam5/cam6/cam7 are
excluded today (retired -- grabber cards returned, boxes powered off) but flow back into BOTH the
mapping AND the sweep the moment CAMERA_ACTIVE_SET is widened, with zero code changes. Because
recording-e2e.sh's own CAMBOX_SWEEP default is now a bash command-substitution expression (not
static text), this test RESOLVES it for real by sourcing camera-set.sh and letting bash evaluate
it -- never regex-scraping a literal that no longer exists.
"""
import importlib.util
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
SCRIPTS = HERE.parent.parent / "scripts"

sys.path.insert(0, str(SCRIPTS))
_switch_schedule_spec = importlib.util.spec_from_file_location(
    "switch_schedule", SCRIPTS / "switch_schedule.py"
)
switch_schedule = importlib.util.module_from_spec(_switch_schedule_spec)
sys.modules["switch_schedule"] = switch_schedule
_switch_schedule_spec.loader.exec_module(switch_schedule)


def _load_set_ndi_mapping():
    """set-ndi-mapping.py's pure helpers are stdlib at import time (websocket-client is imported
    lazily, inside the WS helpers) — safe to load directly, no rig/OBS needed."""
    spec = importlib.util.spec_from_file_location("set_ndi_mapping", SCRIPTS / "set-ndi-mapping.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _camera_label(ndi_sender):
    """'CAM3 (usb)' -> 'CAM3' — the CAMBOX_SWEEP label format has no '(usb)' suffix."""
    return ndi_sender.split(" ", 1)[0]


def _scene_to_camera_from_active_map(active_set=None):
    """Derive {"Cam N": "CAMx"} from active_map()'s `NDI camN -> CAMx (usb)` pins (#827: filtered
    to CAMERA_ACTIVE_SET, not the full historical fact table), using the documented inversion
    (scene "Cam N" renders input "NDI camN" — same label, same N)."""
    m = _load_set_ndi_mapping()
    out = {}
    for ndi_input, sender in m.active_map(active_set):
        match = re.fullmatch(r"NDI cam(\d+)", ndi_input)
        assert match, f"unexpected NDI input label in active_map(): {ndi_input!r}"
        out[f"Cam {match.group(1)}"] = _camera_label(sender)
    return out


def _extract_cambox_sweep_default_expr():
    """Pull the CAMBOX_SWEEP default EXPRESSION straight out of recording-e2e.sh's
    `CAMBOX_SWEEP="${CAMBOX_SWEEP:-<expr>}"` line. #827: <expr> is now `$(camera_active_sweep_pairs)`
    (a live derivation from CAMERA_ACTIVE_SET), never a static literal -- resolved for real below."""
    src = (SCRIPTS / "recording-e2e.sh").read_text()
    m = re.search(r'CAMBOX_SWEEP="\$\{CAMBOX_SWEEP:-([^}]*)\}"', src)
    assert m, "recording-e2e.sh: could not find the CAMBOX_SWEEP default assignment"
    return m.group(1)


def _resolve_cambox_sweep_default(active_set=None):
    """#827: resolve CAMBOX_SWEEP's default for REAL — source camera-set.sh and let bash evaluate
    the actual default expression extracted above, exactly as recording-e2e.sh does at runtime."""
    import os

    expr = _extract_cambox_sweep_default_expr()
    script = (
        f'set -euo pipefail\n. "{SCRIPTS}/camera-set.sh"\n'
        f'printf %s "${{CAMBOX_SWEEP:-{expr}}}"\n'
    )
    env = os.environ.copy()
    if active_set is not None:
        env["CAMERA_ACTIVE_SET"] = active_set
    out = subprocess.run(
        ["bash", "-c", script], capture_output=True, text=True, check=True, env=env
    )
    return out.stdout


def test_cambox_sweep_default_matches_canonical_active_ndi_mapping():
    """Every (scene, label) pair the sweep defaults to must match the CURRENT #399/#827 pinned
    ACTIVE mapping — a scene label pointing at the wrong camera silently mis-attributes that
    camera's delivered frames to the wrong cambox in the zero-loss verdict."""
    canonical = _scene_to_camera_from_active_map()
    sweep_default = _resolve_cambox_sweep_default()
    for scene, label in switch_schedule.parse_sweep(sweep_default):
        assert scene in canonical, (
            f"CAMBOX_SWEEP default references unknown scene {scene!r} "
            f"(not in set-ndi-mapping.py's active_map())"
        )
        assert canonical[scene] == label, (
            f"CAMBOX_SWEEP default says {scene!r} -> {label!r}, but the canonical #399/#827 "
            f"active mapping (set-ndi-mapping.py active_map()) says {scene!r} -> "
            f"{canonical[scene]!r}. The default sweep is now stale against the live NDI-input "
            "pins — fix it or the switch-schedule will attribute frames to the wrong cambox."
        )


def test_cambox_sweep_default_covers_every_camera_in_the_canonical_active_mapping():
    """#24/#312/#827: the default sweep must include EVERY camera in the canonical ACTIVE
    mapping — a camera that drops out of the default (e.g. because it used to be down, or was
    never wired) must be re-added once it's no longer excluded for a real reason.

    #333 used to exclude CAM2 here (the physical dual-QR painter box) on the theory that "while
    painting the monitor it does NOT capture/emit its OWN camera NDI" — but #291 (closed
    2026-06-28) fixed exactly that: cam2's camera-box daemon keeps CAPTURING + EMITTING its own
    NDI feed throughout a TEST run (only its framebuffer is freed for the separate painter
    process). #312 corrected the stale exclusion: cam2's OWN chain is now ALSO swept + digitally
    burn-measured, exactly like every other camera in the fleet."""
    canonical = _scene_to_camera_from_active_map()
    expected_cameras = set(canonical.values())

    sweep_default = _resolve_cambox_sweep_default()
    swept_cameras = {label for _scene, label in switch_schedule.parse_sweep(sweep_default)}

    assert swept_cameras == expected_cameras, (
        f"CAMBOX_SWEEP default covers {sorted(swept_cameras)}, expected every camera in the "
        f"canonical active mapping {sorted(expected_cameras)} (#312: cam2 is no longer excluded)"
    )


def test_default_active_set_is_exactly_cam1_cam2_cam3_1198():
    """#827: cam5/cam6/cam7 retired. #947: cam4 retired. #939 (2026-08-13): cam3 re-activated.
    issue 1198 (2026-08-27, owner ruling): cam1 (#1110 "hardware-defective") and cam2 (#1170
    "camera-under-test retired") are RESTORED -- both diagnoses were built from EPISODES, not a
    permanent card state, and a live journal check on all four cam boxes confirmed both cards are
    healthy today; the owner refused the physical swap outright. Today's declared active
    (measured) fleet is exactly cam1/cam2/cam3."""
    canonical = _scene_to_camera_from_active_map()
    assert set(canonical.values()) == {"CAM1", "CAM2", "CAM3"}, canonical
    for retired in ("CAM4", "CAM5", "CAM6", "CAM7"):
        assert retired not in canonical.values(), (
            f"{retired} is retired from CAMERA_ACTIVE_SET -- it must not appear in the "
            f"default resolved map: {canonical}"
        )


def test_reactivating_a_retired_camera_flows_through_both_sources_of_truth():
    """#827 REVERSIBILITY PROOF: widening CAMERA_ACTIVE_SET to include a retired camera (cam5)
    must make BOTH set-ndi-mapping.py's active_map() AND recording-e2e.sh's resolved CAMBOX_SWEEP
    default cover it -- with ZERO code changes beyond the env var. A comment claiming the reversal
    works is not proof; this end-to-end resolution is."""
    active = "cam1 cam2 cam3 cam4 cam5"
    canonical = _scene_to_camera_from_active_map(active)
    assert "Cam 5" in canonical and canonical["Cam 5"] == "CAM5", canonical

    sweep_default = _resolve_cambox_sweep_default(active)
    swept = dict(switch_schedule.parse_sweep(sweep_default))
    assert swept.get("Cam 5") == "CAM5", (
        f"#827: CAMBOX_SWEEP's resolved default must cover 'Cam 5:CAM5' once cam5 is added back "
        f"to CAMERA_ACTIVE_SET: {sweep_default!r}"
    )
    assert "Cam 6" not in swept and "Cam 7" not in swept, swept
