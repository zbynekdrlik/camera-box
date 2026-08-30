"""#722 -- unit tests for scripts/qr_screenshot_check.py's PURE decode helpers: the "pixel
proof" gather step for the EVENT-mode CONTRACT's item 2 (a program screenshot per camera scene
must decode ZERO QR codes). This is the decisive check from the 2026-07-12 incident (#721) --
every OTHER signal (process checks, burn flags) can lie about what is actually ON SCREEN; only
reading the actual rendered pixels catches a QR that is genuinely live on air.

These tests generate REAL QR codes (via the `qrcode` package) and decode them back via
`cv2.QRCodeDetector` (the same library src/probe/qr.rs's own comments reference as the tool used
in prior debugging sessions) -- proving genuine pixel-level detection, never a stub that always
returns "found nothing".
"""

import io
import pathlib
import sys

from PIL import Image

_SCRIPTS = pathlib.Path(__file__).resolve().parents[2] / "scripts"
if str(_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS))

import qr_screenshot_check as qsc  # noqa: E402


def _blank_png_bytes(w=320, h=180):
    img = Image.new("RGB", (w, h), color=(40, 40, 40))
    buf = io.BytesIO()
    img.save(buf, format="PNG")
    return buf.getvalue()


def _qr_png_bytes(data: str, w=320, h=180):
    import qrcode

    qr = qrcode.QRCode(border=4, box_size=6)
    qr.add_data(data)
    qr.make()
    qr_img = qr.make_image(fill_color="black", back_color="white").convert("RGB")
    # Paste the QR onto a canvas the size a real OBS screenshot would be, like it's burned onto
    # a corner of the program frame.
    canvas = Image.new("RGB", (w, h), color=(40, 40, 40))
    canvas.paste(qr_img.resize((min(w, h) - 20, min(w, h) - 20)), (5, 5))
    buf = io.BytesIO()
    canvas.save(buf, format="PNG")
    return buf.getvalue()


def test_decode_qr_codes_from_image_bytes_finds_nothing_on_a_blank_frame():
    assert qsc.decode_qr_codes_from_image_bytes(_blank_png_bytes()) == []


def test_decode_qr_codes_from_image_bytes_finds_a_real_burned_qr():
    png = _qr_png_bytes("RUNID=911002;FRAME=88213")
    found = qsc.decode_qr_codes_from_image_bytes(png)
    assert found == ["RUNID=911002;FRAME=88213"]


def test_decode_qr_codes_from_image_bytes_finds_multiple_codes():
    # Two QR codes side by side (e.g. a dual-QR vernier frame) -- both must be reported.
    import qrcode

    w, h = 640, 320
    canvas = Image.new("RGB", (w, h), color=(40, 40, 40))
    for i, text in enumerate(["A=1", "B=2"]):
        qr = qrcode.QRCode(border=2, box_size=6)
        qr.add_data(text)
        qr.make()
        qr_img = qr.make_image(fill_color="black", back_color="white").convert("RGB")
        canvas.paste(qr_img.resize((150, 150)), (i * 300 + 10, 10))
    buf = io.BytesIO()
    canvas.save(buf, format="PNG")
    found = qsc.decode_qr_codes_from_image_bytes(buf.getvalue())
    assert set(found) == {"A=1", "B=2"}


def test_extract_png_bytes_from_plain_base64_data():
    import base64

    raw = _blank_png_bytes()
    b64 = base64.b64encode(raw).decode("ascii")
    assert qsc.extract_png_bytes(b64) == raw


def test_extract_png_bytes_from_data_url():
    import base64

    raw = _blank_png_bytes()
    b64 = base64.b64encode(raw).decode("ascii")
    data_url = f"data:image/png;base64,{b64}"
    assert qsc.extract_png_bytes(data_url) == raw


def test_extract_png_bytes_returns_none_on_empty_input():
    assert qsc.extract_png_bytes("") is None
    assert qsc.extract_png_bytes(None) is None


# ---------------------------------------------------------------------------
# #1225 -- screenshot_qr_findings must NEVER write a bare `None` for an unreadable scene: a
# `None` per-scene value crashed event_assert.pixel_proof_ok's `len(v)` call live on 2026-08-30.
# An explicit `{"error": ...}` record is unambiguous on its own, in addition to event_assert.py
# now being None-tolerant too.
# ---------------------------------------------------------------------------


class _FakeWS:
    def close(self):
        pass


def test_screenshot_qr_findings_never_writes_a_bare_none_on_missing_screenshot_data(monkeypatch):
    import obs_phase2

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password: _FakeWS())
    monkeypatch.setattr(obs_phase2, "_rpc", lambda ws, rtype, rdata=None, **kw: {})  # no imageData

    findings = qsc.screenshot_qr_findings("10.0.0.1", "pw", ["Cam 1"])
    assert findings["Cam 1"] is not None
    assert isinstance(findings["Cam 1"], dict)
    assert "error" in findings["Cam 1"]


def test_screenshot_qr_findings_never_writes_a_bare_none_on_rpc_exception(monkeypatch):
    import obs_phase2

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password: _FakeWS())

    def raising_rpc(ws, rtype, rdata=None, **kw):
        raise RuntimeError("transport closed")

    monkeypatch.setattr(obs_phase2, "_rpc", raising_rpc)

    findings = qsc.screenshot_qr_findings("10.0.0.1", "pw", ["Cam 2"])
    assert findings["Cam 2"] is not None
    assert isinstance(findings["Cam 2"], dict)
    assert findings["Cam 2"]["error"] == "transport closed"


def test_screenshot_qr_findings_still_returns_a_clean_list_when_the_screenshot_decodes(
    monkeypatch,
):
    import base64

    import obs_phase2

    raw = _blank_png_bytes()
    b64 = base64.b64encode(raw).decode("ascii")

    monkeypatch.setattr(obs_phase2, "_conn", lambda host, password: _FakeWS())
    monkeypatch.setattr(
        obs_phase2, "_rpc", lambda ws, rtype, rdata=None, **kw: {"imageData": b64}
    )

    findings = qsc.screenshot_qr_findings("10.0.0.1", "pw", ["Cam 3"])
    assert findings["Cam 3"] == []  # a real decode attempt, not an error record
