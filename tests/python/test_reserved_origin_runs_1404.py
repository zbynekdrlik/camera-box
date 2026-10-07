"""Issue 1404 Task 5 part b -- the stream av-sync dock's reserved ORIGIN run list is the repo's one.

The dock (vendor/av-sync-dock/src/camera-box-qr.hpp `CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS`) refuses to
pair a QR of the CG path's origin runs: SongPlayer 911014, the cg OBS 911015, the measurement clip
911016. The list is pinned here against the Rust ids (src/probe/recording_latency.rs) and every
python mirror that lists the reserved ids, so a new CG origin id lands everywhere or fails here.
tests/av_sync_dock_reserved_origin_1404.rs pins the dock's wiring; the g++ self-test its behaviour.
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

import mv_skew_snapshot as mvs  # noqa: E402
import qr_align_pins as qa  # noqa: E402
import youtube_leg_ticks as ylt  # noqa: E402

RUST_NAMES = ("BURN_RUN_ID_SONGPLAYER", "BURN_RUN_ID_CG", "MEASUREMENT_CLIP_RUN_ID")


def _dock_ids():
    hdr = (ROOT / "vendor/av-sync-dock/src/camera-box-qr.hpp").read_text()
    found = re.findall(r"#define CAMERA_BOX_RESERVED_ORIGIN_RUN_IDS \{([^}]*)\}", hdr)
    assert len(found) == 1, found
    return [int(v.strip().rstrip("u")) for v in found[0].split(",")]


def _rust_ids():
    text = (ROOT / "src/probe/recording_latency.rs").read_text()
    out = []
    for name in RUST_NAMES:
        m = re.search(rf"pub const {name}: u32 = ([0-9_]+);", text)
        assert m, f"`pub const {name}` is gone from recording_latency.rs"
        out.append(int(m.group(1).replace("_", "")))
    return out


def test_the_dock_list_is_the_rust_origin_ids_in_order():
    assert _rust_ids() == [911014, 911015, 911016]
    assert _dock_ids() == _rust_ids()


def test_every_python_mirror_reserves_each_origin_id():
    for run in _dock_ids():
        assert run in qa.NODE_BURN_RUN_IDS, run
        assert run in mvs.RESERVED_RUN_IDS, run
    assert ylt.MEASUREMENT_CLIP_RUN_ID in _dock_ids() and set(ylt.CLIP_RUNS) <= set(_dock_ids())


def test_the_youtube_leg_tick_decoder_refuses_the_burn_origins_even_when_asked_for_the_clip():
    import zlib

    def payload(run, tick, gen=5):
        body = f"{run}.{tick}.{gen}"
        return f"P{body}.{zlib.crc32(body.encode())}"

    assert ylt.painter_payload(payload(911016, 10), ylt.CLIP_RUNS) == (911016, 10)
    assert ylt.painter_payload(payload(911014, 10), ylt.CLIP_RUNS) is None
    assert ylt.painter_payload(payload(911015, 10), ylt.CLIP_RUNS) is None
