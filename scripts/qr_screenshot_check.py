#!/usr/bin/env python3
"""#722 -- the "pixel proof" gather step for the EVENT-mode CONTRACT (item 2): screenshot each
named OBS scene over the WebSocket and decode ANY QR codes present in the rendered pixels. This
is the DECISIVE check from the 2026-07-12 live incident (#721) -- the user caught a live QR by
EYE, minutes before broadcast, after every process/flag-based check had already said "clean".
Only reading the actual rendered pixels catches a QR that is genuinely on air.

Reuses the SAME OBS WebSocket connection helpers as scripts/obs_phase2.py (`_conn`/`_rpc`) --
never a second client implementation -- and the SAME `GetSourceScreenshot` call shape
scripts/obs_phase2.py's own `_program_luma` already uses for its non-black self-check. QR
decoding is `cv2.QRCodeDetector` (already a dev1 dependency; the same library
src/probe/qr.rs's own comments reference as the tool used in prior debugging sessions) -- no new
runtime dependency (`qrcode` is only used by this file's own tests, to GENERATE fixture QR
codes; it is never needed at gather time).

CLI: `qr_screenshot_check.py --host <ip> --password <pw> --scene <name> [--scene <name> ...]`
prints ONE JSON object to stdout: `{"<scene>": ["<decoded text>", ...], ...}` -- an empty list
per scene means the pixel proof passed for that scene. A scene whose screenshot could not be
decoded at all (a transport/RPC failure) is reported as an explicit error record
`{"error": "<reason>"}` (NEVER as an empty list, and NEVER a bare `null` -- see
`event_assert.pixel_proof_ok`'s fail-closed contract: the caller must be able to tell "checked,
found nothing" apart from "could not check". #1225: a bare `null` here crashed
`event_assert.pixel_proof_ok`'s `len(v)` call live on 2026-08-30 -- an explicit dict record is
unambiguous and keeps failing closed even for a caller that forgets the None-tolerant check
event_assert.py now has).
"""

import argparse
import base64
import json
import sys

import cv2
import numpy as np

DEFAULT_WIDTH = 1280
DEFAULT_HEIGHT = 720


def extract_png_bytes(data):
    """Given OBS WebSocket's GetSourceScreenshot `imageData` field (either a bare base64 string
    or a `data:image/png;base64,...` URL -- mirrors scripts/obs_phase2.py's `_program_luma`
    parsing), return the raw PNG bytes. Returns None for empty/missing input."""
    if not data:
        return None
    b64 = data.split(",", 1)[1] if data.startswith("data:") else data
    return base64.b64decode(b64)


def decode_qr_codes_from_image_bytes(png_bytes: bytes) -> list:
    """Decode every QR code present in a PNG image's raw bytes. Returns a list of the decoded
    text payloads (empty list if none are found). Uses `cv2.QRCodeDetector.detectAndDecodeMulti`
    directly on the decoded image array -- real pixel-level detection, not a stub."""
    arr = np.frombuffer(png_bytes, dtype=np.uint8)
    img = cv2.imdecode(arr, cv2.IMREAD_COLOR)
    if img is None:
        return []
    detector = cv2.QRCodeDetector()
    found, decoded, _points, _straight = detector.detectAndDecodeMulti(img)
    if not found:
        return []
    return [text for text in decoded if text]


def screenshot_qr_findings(host, password, scenes, width=DEFAULT_WIDTH, height=DEFAULT_HEIGHT):
    """Connect to OBS at `host`, screenshot each scene in `scenes`, and decode QR codes from
    each. Returns {scene: [decoded_text, ...] | {"error": "<reason>"}}. An "error" record means
    the screenshot/decode itself could not be obtained (RPC failure) -- distinct from a
    genuinely empty (clean) result. #1225: this is NEVER a bare `None` -- a bare None crashed
    `event_assert.pixel_proof_ok`'s `len(v)` call live on 2026-08-30 (a screenshot RPC failure on
    one scene); an explicit dict record is unambiguous on its own, in addition to
    event_assert.py now being None-tolerant too."""
    import obs_phase2  # local import: keeps this module importable (for unit tests) without a
    # live OBS connection ever being attempted at import time.

    findings = {}
    ws = obs_phase2._conn(host, password)
    try:
        for scene in scenes:
            try:
                res = obs_phase2._rpc(
                    ws,
                    "GetSourceScreenshot",
                    {
                        "sourceName": scene,
                        "imageFormat": "png",
                        "imageWidth": width,
                        "imageHeight": height,
                    },
                    ignore_err=True,
                )
                png_bytes = extract_png_bytes(res.get("imageData") if res else None)
                if png_bytes is None:
                    findings[scene] = {"error": "no screenshot data (RPC returned nothing)"}
                    continue
                findings[scene] = decode_qr_codes_from_image_bytes(png_bytes)
            except Exception as e:  # noqa: BLE001 -- best-effort per scene, never abort the sweep
                sys.stderr.write(f"WARNING: qr_screenshot_check: scene '{scene}' on {host}: {e}\n")
                findings[scene] = {"error": str(e)}
    finally:
        ws.close()
    return findings


def main(argv=None):
    ap = argparse.ArgumentParser(description="#722 pixel-proof QR-decode check")
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default="")
    ap.add_argument("--scene", action="append", required=True, dest="scenes")
    ap.add_argument("--width", type=int, default=DEFAULT_WIDTH)
    ap.add_argument("--height", type=int, default=DEFAULT_HEIGHT)
    a = ap.parse_args(argv)

    findings = screenshot_qr_findings(a.host, a.password, a.scenes, a.width, a.height)
    print(json.dumps(findings))
    # Non-zero exit whenever ANY scene found a QR or could not be checked at all (an "error"
    # record, #1225 -- never a bare None any more) -- a caller that only cares about the exit
    # code (rather than parsing the JSON) still gets a fail-loud signal.
    any_problem = any(not isinstance(v, list) or len(v) > 0 for v in findings.values())
    sys.exit(1 if any_problem else 0)


if __name__ == "__main__":
    main()
