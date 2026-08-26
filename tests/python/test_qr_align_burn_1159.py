"""#1159 -- the [4i/8align] aligner must decode the PAINTER dual-QR even when the E2E measurement
BURN QR is present in the same screenshot.

The measurement burn (vendor/distroav/src/ndi-burn-filter.cpp) emits its QR in the BYTE-IDENTICAL
painter wire format `P{run_id}.{frame_id}.{gen_ts_ns}.{crc}` (see that file's own comment: "round-
trips through the decoder"), differing ONLY in `run_id` -- a fixed per-node id derived from the host
role: BURN_RUN_ID_STRIH = 911002 on EVERY strih input (plus stream/imag/per-camera burns, the full
src/probe/recording.rs::NODE_BURN_RUN_IDS set 911001-911012). So "filter by payload SHAPE" cannot
discriminate them; the discriminator is the run_id.

Under E2E the align step runs AFTER [4b/8] adds the burns, so every barrier screenshot carries the
painter dual-QR (universal, ~1.8-billion epoch run_id) AND the strih burn (911002) -- BOTH present
on all on-air strih inputs. `dominant_run_id` breaks the tie to the SMALLEST id, so it returns the
BURN (911002), the aligner latches the strih-side burn (independent per-source counters), and every
round is invalid -> the "0 fully-decodable measurement round(s)" abort (E2E run 32414885839 attempt
5) while a manual burns-OFF run decoded 6/6 minutes later.

These tests reproduce that condition off-rig (pure decoded-text lists AND a real composited PNG
decoded through cv2) and pin the burn-aware fix.
"""
import pathlib
import sys
import zlib

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

PAINTER_RUN = 1_867_252_327   # a realistic painter epoch run_id (>> any burn id)
STRIH_BURN = 911002           # BURN_RUN_ID_STRIH -- on EVERY strih input under E2E burns
ID_NS = 8_333_333             # ~one dual-QR id step in ns (fixtures only; the code reads gen_ts)
SOURCES = ["NDI cam1", "NDI cam2", "NDI cam3", "NDI cam4"]


def _payload(run_id: int, frame_id: int, gen_ts_ns: int) -> str:
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    crc = zlib.crc32(body.encode()) & 0xFFFFFFFF
    return f"P{body}.{crc}"


def _painter_dual(frame_id: int, gen_ts_ns: int):
    """The painter dual-QR: the newest even/odd frame_id pair (max wins in pick_painter_tick)."""
    return [_payload(PAINTER_RUN, frame_id, gen_ts_ns),
            _payload(PAINTER_RUN, frame_id - 1, gen_ts_ns - ID_NS)]


def _raw_with_strih_burn(n_rounds=6):
    """`n_rounds` synthetic barrier rounds: each source shows the painter dual-QR (a per-source
    frame_id so cross-camera deltas exist) PLUS the strih burn (run_id 911002, a per-source burn
    frame_id) -- the exact E2E-burns-ON screenshot content."""
    raw = []
    for r in range(n_rounds):
        shot = {}
        for i, src in enumerate(SOURCES):
            p_fid = 5000 + r * 4 + i           # distinct painter frame_id per source/round
            p_ts = 900_000 + (r * 4 + i) * ID_NS
            texts = _painter_dual(p_fid, p_ts)
            texts.append(_payload(STRIH_BURN, 200 + r + i * 3, 111_111 + r))  # the strih burn
            t_send = 1_000_000 + (r * 4 + i) * 500_000
            shot[src] = (texts, t_send)
        raw.append(shot)
    return raw


def _painter_fid(r, i):
    return 5000 + r * 4 + i


class TestRunIdIgnoresBurn:
    def test_run_id_autodetect_ignores_the_strih_burn(self):
        # RED on current code: dominant_run_id breaks the 4-vs-4 tie to the SMALLEST id (911002).
        _rounds, run_id = qa.ticks_from_raw(_raw_with_strih_burn())
        assert run_id == PAINTER_RUN, (
            f"run_id auto-detect picked {run_id} -- the strih burn 911002 must NOT win over the "
            "painter run_id")

    def test_rounds_latch_the_painter_frame_id_not_the_burn(self):
        # RED on current code: latches the burn's per-source frame_id, not the painter's.
        rounds_ticks, _run_id = qa.ticks_from_raw(_raw_with_strih_burn())
        for r, rnd in enumerate(rounds_ticks):
            for i, src in enumerate(SOURCES):
                assert rnd[src] is not None, f"round {r} {src} undecoded"
                assert rnd[src][0] == _painter_fid(r, i), (
                    f"round {r} {src} latched frame_id {rnd[src][0]} (a burn id), not the painter "
                    f"{_painter_fid(r, i)}")



class TestBurnAwareHelpers:
    def test_is_burn_run_id(self):
        assert qa.is_burn_run_id(STRIH_BURN)          # 911002
        assert qa.is_burn_run_id(911004)              # stream
        assert not qa.is_burn_run_id(PAINTER_RUN)     # a painter epoch run
        assert not qa.is_burn_run_id(4242)

    def test_has_painter_payload_rejects_a_burn_only_screenshot(self):
        # The compounding decode_qr_texts guard bug: a burn is "P"-prefixed, so a plain
        # startswith("P") check thought a painter had decoded and skipped the recovery ladder.
        burn = _payload(STRIH_BURN, 200, 111_111)
        assert not qa.has_painter_payload([burn])
        assert not qa.has_painter_payload(["garbage", ""])
        painter = _payload(PAINTER_RUN, 5000, 900_000)
        assert qa.has_painter_payload([painter])
        assert qa.has_painter_payload([burn, painter])   # painter present alongside the burn

    def test_painter_run_id_excludes_the_burn(self):
        maps = [{PAINTER_RUN: 900_000 + i, STRIH_BURN: 111_111 + i} for i in range(4)]
        assert qa.painter_run_id(maps) == PAINTER_RUN
        # burn-only screenshots -> no painter run at all
        assert qa.painter_run_id([{STRIH_BURN: 111_111}, {STRIH_BURN: 222_222}]) is None
        assert qa.painter_run_id([]) is None

    def test_pick_painter_tick_never_latches_a_burn(self):
        burn = _payload(STRIH_BURN, 777, 111_111)
        # Even if a caller pins the run_id to the burn id, a burn is never a painter tick.
        assert qa.pick_painter_tick([burn], STRIH_BURN) is None
        painter = _payload(PAINTER_RUN, 5000, 900_000)
        assert qa.pick_painter_tick([painter, burn], PAINTER_RUN) == (5000, 900_000)


class TestRoundTableDiagnostics:
    def test_table_marks_undecoded_and_summarizes_per_camera(self):
        rounds = [
            {"NDI cam1": (100, 0, 0), "NDI cam2": (101, 0, 0), "NDI cam3": None},
            {"NDI cam1": (104, 0, 0), "NDI cam2": None, "NDI cam3": None},
        ]
        table = qa.format_round_table(rounds, ["NDI cam1", "NDI cam2", "NDI cam3"])
        assert "per-round painter frame_id table" in table
        assert "-- = undecoded" in table
        assert "100" in table and "104" in table       # decoded frame_ids shown
        assert "--" in table                            # undecoded cell marker
        # one dead camera: cam3 decoded 0/2; cam1 decoded 2/2
        assert "cam3=0/2" in table
        assert "cam1=2/2" in table

    def test_spread_is_shown_per_round(self):
        rounds = [{"NDI cam1": (100, 0, 0), "NDI cam2": (103, 0, 0)}]
        table = qa.format_round_table(rounds, ["NDI cam1", "NDI cam2"])
        assert "spread" in table
        assert "| 3" in table                            # 103 - 100 = 3


# --------------------------------------------------------------------------- #
# End-to-end integration: a REAL composited painter+burn PNG decoded through cv2. This is
# DETERMINISTIC AFTER THE FIX (the painter always decodes; burn-aware selection resolves it), and
# proves the whole live decode path -- not just the pure resolver. The deterministic RED lives in
# TestRunIdIgnoresBurn above; cv2's flaky multi-QR detect makes an image-level RED unreliable, so
# this test only asserts the fixed invariant. CI installs opencv-python-headless/numpy/Pillow/qrcode.
# --------------------------------------------------------------------------- #
def _qr_png_gray(text, box=6, border=2):
    import qrcode
    q = qrcode.QRCode(error_correction=qrcode.constants.ERROR_CORRECT_M, box_size=box, border=border)
    q.add_data(text)
    q.make(fit=True)
    return q.make_image(fill_color="black", back_color="white").convert("L")


def _compose_shot_png(painter_texts, burn_text, width=1920, height=1080):
    """A screenshot-shaped PNG: painter dual-QR top-left, the strih burn bottom-left corner
    (resolve_corner puts the strih burn there) -- the real E2E screenshot layout."""
    import io
    from PIL import Image
    canvas = Image.new("L", (width, height), 255)
    x = 40
    for t in painter_texts:
        im = _qr_png_gray(t)
        canvas.paste(im, (x, 40))
        x += im.width + 30
    burn = _qr_png_gray(burn_text)
    canvas.paste(burn, (40, height - burn.height - 40))
    buf = io.BytesIO()
    canvas.save(buf, format="PNG")
    return buf.getvalue()


class TestCompositedImageResolvesPainter:
    def test_composited_painter_plus_burn_decodes_the_painter(self):
        raw = []
        burn_seen = False
        for r in range(3):
            shot = {}
            for i, src in enumerate(SOURCES):
                p_fid = 5000 + r * 4 + i
                p_ts = 900_000 + (r * 4 + i) * ID_NS
                png = _compose_shot_png(_painter_dual(p_fid, p_ts),
                                        _payload(STRIH_BURN, 200 + r + i * 3, 111_111 + r))
                texts = qa.decode_qr_texts(png)
                # confirm the fixture genuinely exercises burn coexistence
                if any(t.startswith(f"P{STRIH_BURN}.") for t in texts):
                    burn_seen = True
                shot[src] = (texts, 1_000_000 + (r * 4 + i) * 500_000)
            raw.append(shot)
        assert burn_seen, "fixture never composited a decodable burn -- not exercising the bug"
        rounds_ticks, run_id = qa.ticks_from_raw(raw)
        assert run_id == PAINTER_RUN, f"cv2-decoded run_id was {run_id}, not the painter"
        for r, rnd in enumerate(rounds_ticks):
            for i, src in enumerate(SOURCES):
                assert rnd[src] is not None, f"round {r} {src} undecoded from the composited image"
                # The painter dual-QR carries frame_id and frame_id-1; cv2 may drop the higher
                # (even) half, so pick_painter_tick can legitimately return frame_id-1. The
                # invariant this proves is "the painter, never the burn" -- tolerate the ±1 half.
                assert rnd[src][0] in (_painter_fid(r, i), _painter_fid(r, i) - 1), (
                    f"round {r} {src} latched {rnd[src][0]} (not the painter dual-QR "
                    f"{_painter_fid(r, i)}/{_painter_fid(r, i) - 1})")


class TestNodeBurnRunIdMirror:
    def test_python_mirror_matches_the_rust_authority(self):
        """qr_align_pins.NODE_BURN_RUN_IDS is a hand-mirror of the Rust node-burn ids. Guard it
        against silent drift: parse every `pub const BURN_RUN_ID_*: u32 = N;` from the authority
        (src/probe/recording_latency.rs -- the values behind src/probe/recording.rs::
        NODE_BURN_RUN_IDS) and assert set-equality with the Python mirror. If a new BURN_RUN_ID_CAM8
        is ever added on the Rust side, this fails loudly instead of letting that burn silently
        hijack the aligner's run_id auto-detect again (#1159)."""
        import re
        repo = pathlib.Path(__file__).resolve().parents[2]
        rust = (repo / "src" / "probe" / "recording_latency.rs").read_text()
        # issue 1196: AUX_TICK_RUN_ID (the painted aux Vernier tick pair) is tick-excluded exactly
        # like the burns and lives in the same authority file under its own (non-BURN_) name --
        # include it, or adding it to the Python mirror would false-fail this drift guard.
        rust_ids = {int(v) for v in re.findall(
            r"pub const (?:BURN_RUN_ID_[A-Z0-9]+|AUX_TICK_RUN_ID): u32 = (\d+);", rust)}
        assert rust_ids, "parsed no BURN_RUN_ID_* consts -- the authority file moved or changed shape"
        assert rust_ids == set(qa.NODE_BURN_RUN_IDS), (
            f"NODE_BURN_RUN_IDS mirror drift: python={sorted(qa.NODE_BURN_RUN_IDS)} "
            f"rust={sorted(rust_ids)}")
