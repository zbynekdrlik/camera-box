"""#24 — regression guard: recording-e2e.sh's ALL_CAMBOX default sweep MUST stay consistent with
the CANONICAL strih NDI-input->camera mapping (set-ndi-mapping.py DEFAULT_MAP, #399).

The strih scene names ("Cam 1"/"Cam 3"/"Cam 5") are the SAME inversion as the NDI input labels
they show (.claude/skills/genlock/SKILL.md "strih NDI Input -> Camera Mapping (INVERTED)"): scene
"Cam N" renders input "NDI camN". scripts/recording-e2e.sh's `CAMBOX_SWEEP` default hard-codes a
`"<scene>:<CAMBOX-LABEL>"` string built by HAND against that mapping — exactly the kind of literal
the project's own mapping-drift bug recurs on (#399's docstring: "the strih NDI-input->camera-box
bindings drift from the pins ... the recurring bug"). #399 remapped 'Cam 1' from CAM4 to CAM3
*after* recording-e2e.sh's sweep default was written, silently leaving the default WRONG (it would
attribute CAM3's frames to the "CAM4" label in the switch-schedule) and leaving CAM3 excluded from
zero-loss coverage even though its earlier exclusion reason (#301, cam3 SSH down) has since closed.

This test parses BOTH sources of truth (no rig, no OBS, no I/O beyond reading the two files) and
asserts they agree, so a future re-map can never silently desync the sweep default again.
"""
import importlib.util
import re
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


def _load_default_map():
    """set-ndi-mapping.py's DEFAULT_MAP is pure stdlib at import time (websocket-client is
    imported lazily, inside the WS helpers) — safe to load directly, no rig/OBS needed."""
    spec = importlib.util.spec_from_file_location("set_ndi_mapping", SCRIPTS / "set-ndi-mapping.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.DEFAULT_MAP


def _camera_label(ndi_sender):
    """'CAM3 (usb)' -> 'CAM3' — the CAMBOX_SWEEP label format has no '(usb)' suffix."""
    return ndi_sender.split(" ", 1)[0]


def _scene_to_camera_from_default_map():
    """Derive {"Cam N": "CAMx"} from DEFAULT_MAP's `NDI camN -> CAMx (usb)` pins, using the
    documented inversion (scene "Cam N" renders input "NDI camN" — same label, same N)."""
    out = {}
    for ndi_input, sender in _load_default_map():
        m = re.fullmatch(r"NDI cam(\d+)", ndi_input)
        assert m, f"unexpected NDI input label in DEFAULT_MAP: {ndi_input!r}"
        out[f"Cam {m.group(1)}"] = _camera_label(sender)
    return out


def _extract_cambox_sweep_default():
    """Pull the CAMBOX_SWEEP default straight out of recording-e2e.sh's
    `CAMBOX_SWEEP="${CAMBOX_SWEEP:-<default>}"` line — the literal the harness actually runs with
    when the operator does not override it."""
    src = (SCRIPTS / "recording-e2e.sh").read_text()
    m = re.search(r'CAMBOX_SWEEP="\$\{CAMBOX_SWEEP:-([^}]*)\}"', src)
    assert m, "recording-e2e.sh: could not find the CAMBOX_SWEEP default assignment"
    return m.group(1)


def test_cambox_sweep_default_matches_canonical_ndi_mapping():
    """Every (scene, label) pair the sweep defaults to must match the CURRENT #399 pinned
    mapping — a scene label pointing at the wrong camera silently mis-attributes that camera's
    delivered frames to the wrong cambox in the zero-loss verdict."""
    canonical = _scene_to_camera_from_default_map()
    sweep_default = _extract_cambox_sweep_default()
    for scene, label in switch_schedule.parse_sweep(sweep_default):
        assert scene in canonical, (
            f"CAMBOX_SWEEP default references unknown scene {scene!r} "
            f"(not in set-ndi-mapping.py DEFAULT_MAP)"
        )
        assert canonical[scene] == label, (
            f"CAMBOX_SWEEP default says {scene!r} -> {label!r}, but the canonical #399 mapping "
            f"(set-ndi-mapping.py DEFAULT_MAP) says {scene!r} -> {canonical[scene]!r}. "
            "The default sweep is now stale against the live NDI-input pins — fix it or the "
            "switch-schedule will attribute frames to the wrong cambox."
        )


def test_cambox_sweep_default_covers_every_camera_in_the_canonical_mapping():
    """#24/#312: the default sweep must include EVERY camera in the canonical mapping — a camera
    that drops out of the default (e.g. because it used to be down, or was never wired) must be
    re-added once it's no longer excluded for a real reason.

    #333 used to exclude CAM2 here (the physical dual-QR painter box) on the theory that "while
    painting the monitor it does NOT capture/emit its OWN camera NDI" — but #291 (closed
    2026-06-28) fixed exactly that: cam2's camera-box daemon keeps CAPTURING + EMITTING its own
    NDI feed throughout a TEST run (only its framebuffer is freed for the separate painter
    process). #312 corrected the stale exclusion: cam2's OWN chain is now ALSO swept + digitally
    burn-measured, exactly like every other camera in the fleet (deployed via
    scripts/recording-e2e.sh's `[2b/8]` loop with `CAMERA_BOX_NO_DISPLAY=1` so the painter can
    still own /dev/fb0). There is no more "painter exclusion" at the scene/sweep level — cam2
    being the fixed optical-tick SOURCE is an orthogonal, separate role from being a swept
    camera-under-test."""
    canonical = _scene_to_camera_from_default_map()
    expected_cameras = set(canonical.values())

    sweep_default = _extract_cambox_sweep_default()
    swept_cameras = {label for _scene, label in switch_schedule.parse_sweep(sweep_default)}

    assert swept_cameras == expected_cameras, (
        f"CAMBOX_SWEEP default covers {sorted(swept_cameras)}, expected every camera in the "
        f"canonical mapping {sorted(expected_cameras)} (#312: cam2 is no longer excluded)"
    )
