"""issue 1417: scene -> camera for the tally lights (owner rule: exactly one camera lights it)."""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "scripts"))
import strih_tally_decision as d  # noqa: E402


def item(name, kind="input", enabled=True):
    return {"name": name, "kind": kind, "enabled": enabled}


CAMERAS = {f"NDI cam{n}": n for n in range(1, 8)}
TREE = {
    **{f"Cam {n}": [item(f"NDI cam{n}"), item("ASIO zvuk")] for n in range(1, 8)},
    "Moderatori": [item("NDI cam3"), item("ASIO zvuk"), item("Odpocet")],
    "MULTIVIEW": [item("NDI 2ME PGM"), item("NDI cam1"), item("NDI cam2"), item("NDI cam5")],
    "Interkom": [item("Ableset"), item("NDI 2ME PVW"), item("Camera info", "scene")],
    "Camera info": [item("Browser camera crew")],
    "CG bridge": [item("CG-obs", enabled=False), item("CG-presenter")],
    "Development": [item("Cam 5", "scene")],
    "Grading": [item("Cam 1", "scene", enabled=False), item("Cam 2", "scene")],
    "Grouped": [item("G1", "group")],
    "G1": [item("NDI cam6")],
    "Loop A": [item("Loop B", "scene")],
    "Loop B": [item("Loop A", "scene"), item("NDI cam4")],
    "Twice": [item("NDI cam3"), item("NDI cam3")],
    "Same cam two inputs": [item("NDI cam3"), item("NDI cam3 copy")],
}


def test_camera_of_ndi_name():
    assert d.camera_of_ndi_name("CAM3 (usb)") == 3
    assert d.camera_of_ndi_name("CAM12 (usb)") == 12
    assert d.camera_of_ndi_name("STRIH-SNV (2ME PGM)") == 0
    assert d.camera_of_ndi_name("RESOLUME-SNV (SP-program)") == 0
    assert d.camera_of_ndi_name(None) == 0
    assert d.camera_of_ndi_name("cam3 (usb)") == 0


def test_cam_n_scene():
    for n in range(1, 8):
        assert d.camera_of_scene(f"Cam {n}", TREE, CAMERAS) == n


def test_single_camera_composite_scene():
    assert d.camera_of_scene("Moderatori", TREE, CAMERAS) == 3


def test_multi_camera_scene_lights_nothing():
    assert d.camera_of_scene("MULTIVIEW", TREE, CAMERAS) == 0


def test_no_camera_scene_lights_nothing():
    assert d.camera_of_scene("Interkom", TREE, CAMERAS) == 0
    assert d.camera_of_scene("CG bridge", TREE, CAMERAS) == 0


def test_nested_scene_camera_found():
    assert d.camera_of_scene("Development", TREE, CAMERAS) == 5


def test_disabled_items_do_not_count():
    assert d.camera_of_scene("Grading", TREE, CAMERAS) == 2


def test_group_contents_count():
    assert d.camera_of_scene("Grouped", TREE, CAMERAS) == 6


def test_scene_cycle_terminates():
    assert d.camera_of_scene("Loop A", TREE, CAMERAS) == 4


def test_same_camera_twice_is_one_camera():
    cams = {**CAMERAS, "NDI cam3 copy": 3}
    assert d.camera_of_scene("Twice", TREE, cams) == 3
    assert d.camera_of_scene("Same cam two inputs", TREE, cams) == 3


def test_unknown_or_none_scene():
    assert d.camera_of_scene("No such scene", TREE, CAMERAS) == 0
    assert d.camera_of_scene(None, TREE, CAMERAS) == 0


def test_state_of_studio_off_has_no_preview():
    assert d.state_of("Cam 3", "Cam 5", False, TREE, CAMERAS) == (3, 0)
    assert d.state_of("Cam 3", "Cam 5", True, TREE, CAMERAS) == (3, 5)


def test_messages_order_and_noop():
    assert d.messages(None, (3, 5)) == ["TALLY-PVW 5", "TALLY-PGM 3"]
    assert d.messages((3, 5), (3, 5)) == []
    assert d.messages((3, 5), (5, 3)) == ["TALLY-PVW 3", "TALLY-PGM 5"]
    assert d.messages((3, 5), (3, 2)) == ["TALLY-PVW 2"]
    assert d.messages((3, 5), (4, 5)) == ["TALLY-PGM 4"]
    assert d.full_messages((0, 0)) == ["TALLY-PVW 0", "TALLY-PGM 0"]
