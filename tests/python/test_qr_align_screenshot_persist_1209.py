"""#1209 -- unit tests for the undecodable-round screenshot persistence in scripts/qr_align_pins.py.

The reproducible [4i/8align] abort (E2E run 33006989429, cam3 3-4/30 decoded) could not be
root-caused because the aligner NEVER persisted the pixels of the frame whose painter QR failed to
decode (barrier_screenshot decoded then discarded the PNG bytes). Deliverable 1: a fail-safe,
bounded persister so a post-mortem can look at the actual failing frame.

These are the Tier-0 pure tests (no rig, no cv2, no OBS -- pytest is allowed locally, cargo is not):
  (a) an undecodable round triggers a save with the expected `align-undecodable-<src>-round<N>.png`
      path shape;
  (b) the per-camera cap stops further saves (over-cap occurrences are counted, not written);
  (c) a save exception does NOT propagate (a save failure must never break a measurement round);
plus the gating helper (a decodable painter-QR frame is NOT saved; an empty / burn-only frame IS),
the saver=None no-op, source-name sanitization, and the summary shape.
"""
import pathlib
import sys
import zlib

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402


# --------------------------------------------------------------------------------------------------
# helpers: a valid painter QR wire string (same CRC-32 as src/probe/payload.rs), and a burn payload.
# --------------------------------------------------------------------------------------------------
def _payload(run_id, frame_id, gen_ts_ns):
    body = f"{run_id}.{frame_id}.{gen_ts_ns}"
    crc = zlib.crc32(body.encode()) & 0xFFFFFFFF
    return f"P{body}.{crc}"


_FAKE_PNG = b"\x89PNG\r\n\x1a\n-fake-1209"
RUN = 4242
BURN = 911002  # BURN_RUN_ID_STRIH -- a reserved node-burn id (NODE_BURN_RUN_IDS), NOT the painter


# --------------------------------------------------------------------------------------------------
# (a) an undecodable round triggers a save with the expected path shape
# --------------------------------------------------------------------------------------------------
def test_undecodable_save_writes_expected_path_and_bytes(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)
    p0 = saver.save("NDI cam3", _FAKE_PNG)
    assert p0 is not None
    assert pathlib.Path(p0).name == "align-undecodable-NDI_cam3-round0.png"
    assert pathlib.Path(p0).read_bytes() == _FAKE_PNG
    # the per-camera round index increments monotonically (collision-free across measure+verify)
    p1 = saver.save("NDI cam3", _FAKE_PNG)
    assert pathlib.Path(p1).name == "align-undecodable-NDI_cam3-round1.png"
    # a DIFFERENT camera keeps its own independent index
    q0 = saver.save("NDI cam4", _FAKE_PNG)
    assert pathlib.Path(q0).name == "align-undecodable-NDI_cam4-round0.png"


def test_save_creates_missing_output_dir(tmp_path):
    # the caller passes the run dir; a not-yet-existing dir must be created, never an error.
    target = tmp_path / "recording-e2e-33006989429"
    assert not target.exists()
    p = qa.ScreenshotSaver(str(target), cap=8).save("NDI cam3", _FAKE_PNG)
    assert p is not None and pathlib.Path(p).exists()


# --------------------------------------------------------------------------------------------------
# (b) the per-camera cap stops further saves (over-cap counted, not written)
# --------------------------------------------------------------------------------------------------
def test_cap_stops_further_saves_and_counts_over_cap(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=2)
    assert saver.save("NDI cam3", _FAKE_PNG) is not None      # round0
    assert saver.save("NDI cam3", _FAKE_PNG) is not None      # round1
    assert saver.save("NDI cam3", _FAKE_PNG) is None          # over cap -> not written
    assert saver.save("NDI cam3", _FAKE_PNG) is None          # still over cap
    written = sorted(p.name for p in tmp_path.glob("align-undecodable-NDI_cam3-*.png"))
    assert written == ["align-undecodable-NDI_cam3-round0.png",
                       "align-undecodable-NDI_cam3-round1.png"]
    # the over-cap occurrences are COUNTED so the summary can report "kept counting"
    summ = saver.summary()
    assert "cam3" in summ and "2" in summ and "over cap" in summ.lower()


def test_cap_is_per_camera(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=1)
    assert saver.save("NDI cam3", _FAKE_PNG) is not None      # cam3 round0
    assert saver.save("NDI cam3", _FAKE_PNG) is None          # cam3 over cap
    assert saver.save("NDI cam4", _FAKE_PNG) is not None      # cam4 has its OWN budget


# --------------------------------------------------------------------------------------------------
# (c) a save exception does NOT propagate (a save failure must never break a measurement round)
# --------------------------------------------------------------------------------------------------
def test_save_exception_is_swallowed_not_propagated(tmp_path, monkeypatch):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)

    def _boom(*_a, **_k):
        raise OSError("disk on fire")

    monkeypatch.setattr(qa.os, "makedirs", _boom)
    # MUST NOT raise -- returns None and swallows the error.
    assert saver.save("NDI cam3", _FAKE_PNG) is None
    assert not list(tmp_path.glob("*.png"))
    # the error is COUNTED (surfaced in the summary), never silently lost.
    assert "error" in saver.summary().lower()


def test_gate_helper_never_raises_on_save_failure(tmp_path, monkeypatch):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)
    monkeypatch.setattr(qa.os, "makedirs", lambda *_a, **_k: (_ for _ in ()).throw(OSError("nope")))
    # undecodable frame -> would save, but the save blows up: the gate helper still returns cleanly.
    assert qa.maybe_save_undecodable_screenshot(saver, "NDI cam3", _FAKE_PNG, []) is None


# --------------------------------------------------------------------------------------------------
# gating: only a frame that decoded NO valid painter QR is persisted (the '--' / undecodable case)
# --------------------------------------------------------------------------------------------------
def test_gate_saves_undecodable_frames_only(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)
    # A frame that DID decode a valid painter QR is readable -> NEVER persisted.
    decodable = [_payload(RUN, 100, 8_333_333), _payload(RUN, 99, 8_325_000)]
    assert qa.maybe_save_undecodable_screenshot(saver, "NDI cam3", _FAKE_PNG, decodable) is None
    # An EMPTY decode (no QR at all) -> undecodable -> persisted.
    p_empty = qa.maybe_save_undecodable_screenshot(saver, "NDI cam3", _FAKE_PNG, [])
    assert p_empty is not None and pathlib.Path(p_empty).exists()
    # A frame that decoded ONLY the node burn (no painter) is still painter-undecodable -> persisted.
    burn_only = [_payload(BURN, 500, 1_000_000)]
    p_burn = qa.maybe_save_undecodable_screenshot(saver, "NDI cam4", _FAKE_PNG, burn_only)
    assert p_burn is not None and pathlib.Path(p_burn).exists()


def test_gate_no_save_when_png_is_none(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)
    assert qa.maybe_save_undecodable_screenshot(saver, "NDI cam3", None, []) is None
    assert not list(tmp_path.glob("*.png"))


# --------------------------------------------------------------------------------------------------
# saver=None / dir=None is a total no-op: the gate's behaviour + exit codes stay byte-identical
# --------------------------------------------------------------------------------------------------
def test_saver_none_gate_is_noop():
    # no exception, returns None -- the persistence-off path.
    assert qa.maybe_save_undecodable_screenshot(None, "NDI cam3", _FAKE_PNG, []) is None


def test_disabled_saver_writes_nothing(tmp_path):
    saver = qa.ScreenshotSaver(None, cap=8)
    assert saver.save("NDI cam3", _FAKE_PNG) is None
    assert not list(tmp_path.glob("*.png"))


# --------------------------------------------------------------------------------------------------
# source-name sanitization + summary shape
# --------------------------------------------------------------------------------------------------
def test_sanitize_source_name():
    assert qa._sanitize_source_name("NDI cam3") == "NDI_cam3"
    assert qa._sanitize_source_name("NDI 2ME PGM") == "NDI_2ME_PGM"
    assert qa._sanitize_source_name("weird/name.png") == "weird_name_png"
    assert qa._sanitize_source_name("") == "src"  # never an empty slug


def test_summary_reports_total_and_dir(tmp_path):
    saver = qa.ScreenshotSaver(str(tmp_path), cap=8)
    saver.save("NDI cam3", _FAKE_PNG)
    saver.save("NDI cam3", _FAKE_PNG)
    saver.save("NDI cam4", _FAKE_PNG)
    summ = saver.summary()
    assert str(tmp_path) in summ
    assert "3" in summ  # total saved
    assert "cam3" in summ and "cam4" in summ


def test_summary_clean_when_nothing_undecodable(tmp_path):
    summ = qa.ScreenshotSaver(str(tmp_path), cap=8).summary()
    # no saves, no over-cap, no errors -> a benign "nothing to persist" note, never a scary line.
    assert "0" in summ or "no undecodable" in summ.lower()


def test_default_cap_constant_is_bounded():
    assert isinstance(qa.DEFAULT_SCREENSHOT_SAVE_CAP, int)
    assert 1 <= qa.DEFAULT_SCREENSHOT_SAVE_CAP <= 20  # bounded: a 30-round all-fail run must not dump 30/cam
