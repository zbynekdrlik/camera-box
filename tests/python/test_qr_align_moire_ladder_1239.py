"""#1239 -- decode_qr_texts (scripts/qr_align_pins.py) misses the big optical painter QR under
monitor-filming moire on ~35% of [4i/8align] rounds. The current ladder (raw multi-detect, then a
2x-upscale/autocontrast/threshold-sweep fallback) is a SHARPENING strategy -- it never helps
against moire/screen-door aliasing, which needs AVERAGING instead.

Real evidence (E2E run 33358916887, /tmp/recording-e2e-2020434563): 29 of 84 rounds undecoded
(uniformly across cam1-cam7). Offline replay of the exact live predicate,
`pick_painter_tick(decode_qr_texts(png), 2020434563)`, against all 29 saved #1209
undecodable-round fixtures: the CURRENT ladder recovers 0/29; the extended ladder (four
moire-averaging passes appended after the existing ones -- half-res INTER_AREA+Otsu,
medianBlur(5)+Otsu, third-res INTER_AREA+Otsu, L/R crop+medianBlur(5)+2x+Otsu) recovers 29/29.

This test carries 3 representative real fixtures (per
.claude/rules/pattern-change-needs-decode-fixture.md -- never tune blind against a synthetic
image). It is RED against the pre-#1239 decode_qr_texts (every fixture returns no painter tick)
and GREEN once the moire-averaging passes are appended -- same assertion both times, only the
implementation changes.
"""
import pathlib
import sys

import pytest

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_align_pins as qa  # noqa: E402

cv2 = pytest.importorskip("cv2")  # the real decode path needs opencv -- CI installs it (ci.yml)

RUN_ID = 2020434563  # the painter run_id this E2E run's fixtures were captured under

FIXTURE_DIR = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "qr-align-moire-1239"
FIXTURES = sorted(p.name for p in FIXTURE_DIR.glob("*.png"))


def test_fixture_dir_carries_the_expected_real_captures():
    """Guard against an empty/misnamed fixture dir silently turning every test below into a
    vacuous pass (an empty parametrize list runs zero cases, not a failure)."""
    assert len(FIXTURES) == 3, (
        f"expected 3 committed moire fixtures under {FIXTURE_DIR}, found {FIXTURES}"
    )
    for name in FIXTURES:
        assert name.startswith("align-undecodable-NDI_cam"), name


@pytest.mark.parametrize("fixture_name", FIXTURES)
def test_decode_qr_texts_recovers_the_painter_tick_under_moire(fixture_name):
    """The exact live predicate from qr_align_pins.measure_stable_tail's barrier_screenshot path:
    pick_painter_tick(decode_qr_texts(png_bytes), run_id). RED against the pre-#1239 ladder (cv2's
    binarizer never survives monitor-filmed moire on the big optical painter QR); GREEN once the
    moire-averaging passes are appended to decode_qr_texts."""
    png_bytes = (FIXTURE_DIR / fixture_name).read_bytes()
    texts = qa.decode_qr_texts(png_bytes)
    tick = qa.pick_painter_tick(texts, RUN_ID)
    assert tick is not None, (
        f"{fixture_name}: decode_qr_texts ladder did not recover a run_id={RUN_ID} painter tick "
        f"-- decoded texts: {texts!r}"
    )
    frame_id, gen_ts_ns = tick
    assert frame_id > 0
    assert gen_ts_ns > 0


def test_decode_qr_texts_recovers_a_painter_shaped_payload_for_every_fixture():
    """Same evidence, phrased as the issue's own summary form (29/29, not merely 'some'): every
    committed fixture must independently decode a painter-shaped (non-burn) payload -- a single
    aggregate assertion catches an implementation that only handles the EASIEST fixture."""
    misses = []
    for name in FIXTURES:
        png_bytes = (FIXTURE_DIR / name).read_bytes()
        texts = qa.decode_qr_texts(png_bytes)
        if not qa.has_painter_payload(texts):
            misses.append(name)
    assert not misses, f"decode_qr_texts missed a painter-shaped payload on: {misses}"
