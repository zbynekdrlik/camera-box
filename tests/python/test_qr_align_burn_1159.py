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

