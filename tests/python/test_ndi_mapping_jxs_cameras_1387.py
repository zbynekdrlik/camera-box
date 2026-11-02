"""Issue 1387: the NDI mapping never re-arms the receive of a JPEG XS camera (owner 1.11.2026)."""
import importlib.util
import pathlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
spec = importlib.util.spec_from_file_location("set_ndi_mapping_jxs", _SCRIPTS / "set-ndi-mapping.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def test_the_box_facts_name_the_jpeg_xs_cameras_by_address():
    assert m.jxs_cameras_of("10.77.9.202") == {"cam2", "cam3", "cam5"}
    assert m.jxs_cameras_of("10.77.8.202") == set(), "strih-pp declares none"
    assert m.jxs_cameras_of("192.0.2.1") == set()


def test_a_jpeg_xs_camera_leaves_the_enforced_map():
    want = m.without_jxs_cameras(m.active_map("cam2 cam3 cam4"), {"cam2", "cam3"})
    assert want == [("NDI cam4", "CAM4 (usb)")]


def test_main_filters_the_default_map_only():
    src = (_SCRIPTS / "set-ndi-mapping.py").read_text()
    body = src[src.index("want = parse_map_args(args.map, args.active)"):]
    assert "if not args.map" in body[:300] and "without_jxs_cameras" in body[:300]
