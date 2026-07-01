#!/usr/bin/env python3
"""#365 — frozen-camera freshness gate (Python OBS-WS harness).

Connects to strih OBS WebSocket, captures a screenshot of each raw NDI camera
input (NOT the Multiview tile — see NOTE), SHA-1 hashes the PNG bytes per
sample, and feeds the per-camera hash timeline to the `frozen-camera-gate`
Rust binary for the verdict.

NOTE: why raw NDI inputs, not Multiview tiles?
  The Multiview tiles on strih contain animating overlays (AbleSet spinner, CG
  text) that keep updating even when the underlying camera NDI is stuck — so
  hashing a tile would *false-pass* a genuinely frozen camera.  We hash the raw
  OBS source named "NDI cam1", "NDI cam2", etc. via GetSourceScreenshot.
  A live-but-dark camera still has sensor noise (hash changes every sample at
  mean luma ~5–7), while a frozen NDI repeats identical PNG bytes → STATIC.

Precondition (#276): the Multiview projector must be OPEN on strih so that all
  raw NDI inputs are rendering.  The gate fails a camera that is not rendering
  (all-black frames hash identically), which is the correct fail-safe: a camera
  that would show static during a recording should block the run.

Usage:
  python3 scripts/frozen-camera-gate.py --host 10.77.9.202 [options]

Exit codes (same as frozen-camera-gate Rust binary):
  0   PASS  — all cameras are live
  1   FAIL  — one or more cameras are FROZEN (names printed to stdout)
  2   ERROR — bad args / OBS WS connection failure

Env:
  OBS_PASSWORD  — OBS WebSocket password (override --password)
  FROZEN_GATE_BIN  — path to frozen-camera-gate Rust binary
                     (default: probed from PROBE_BIN_DIR or next to this script)
  OBS_OP_TIMEOUT_S — per-request OBS WS timeout in seconds (default: 60)
"""
import argparse
import base64
import hashlib
import json
import os
import subprocess
import sys
import time

PORT = 4455

try:
    from websocket import WebSocketTimeoutException, create_connection
except ImportError:
    sys.exit("missing dep: pip install websocket-client")


# ─── pure helpers (unit-testable without OBS) ────────────────────────────────

def _hash_png(data: bytes) -> str:
    """SHA-1 of raw PNG bytes → hex string.  Used for frame-to-frame freshness
    comparison; SHA-1 is intentionally fast (not crypto) and produces compact
    hex strings for the JSON timeline passed to the Rust binary."""
    return hashlib.sha1(data).hexdigest()


def _extract_png(b64_data: str) -> "bytes | None":
    """Extract raw PNG bytes from an OBS GetSourceScreenshot imageData string.

    OBS returns either a bare base64 string or a data-URI starting with
    ``data:image/png;base64,``.  Returns ``None`` on any decode failure."""
    if not b64_data:
        return None
    try:
        b64 = b64_data.split(",", 1)[1] if b64_data.startswith("data:") else b64_data
        return base64.b64decode(b64)
    except Exception:
        return None


def _build_timeline_json(timelines: "dict[str, list[str | None]]") -> str:
    """Serialise the per-camera hash timeline dict to the JSON format that the
    frozen-camera-gate Rust binary reads from stdin."""
    return json.dumps(timelines)


def _find_verdict_bin(explicit: "str | None") -> str:
    """Locate the frozen-camera-gate Rust binary.

    Search order (first found wins):
      1. ``--verdict-bin`` CLI arg / ``FROZEN_GATE_BIN`` env var
      2. ``$PROBE_BIN_DIR/frozen-camera-gate`` (same dir the E2E harness uses)
      3. ``<dir of this script>/frozen-camera-gate`` (local dev build)
    Exits with code 2 if none found."""
    if explicit:
        return explicit
    env_bin = os.environ.get("FROZEN_GATE_BIN", "")
    if env_bin and os.path.isfile(env_bin):
        return env_bin
    probe_dir = os.environ.get("PROBE_BIN_DIR", "")
    if probe_dir:
        candidate = os.path.join(probe_dir, "frozen-camera-gate")
        if os.path.isfile(candidate):
            return candidate
    here = os.path.dirname(os.path.abspath(__file__))
    local = os.path.join(here, "..", "target", "release", "frozen-camera-gate")
    local = os.path.normpath(local)
    if os.path.isfile(local):
        return local
    sys.exit(
        "ERROR: frozen-camera-gate binary not found. "
        "Pass --verdict-bin, set FROZEN_GATE_BIN, or set PROBE_BIN_DIR. "
        "To build: cargo build --release --bin frozen-camera-gate  # airuleset:build-ok"
    )


# ─── OBS WebSocket helpers (reuse _conn / _rpc pattern from obs_phase2.py) ───

OBS_OP_TIMEOUT_S = float(os.environ.get("OBS_OP_TIMEOUT_S", "60"))


def _conn(host, password=""):
    ws = create_connection(f"ws://{host}:{PORT}", timeout=10)
    hello = json.loads(ws.recv())
    # Subscribe to ZERO events — pure request/response, immune to event floods (#331).
    ident = {"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}
    auth = hello["d"].get("authentication")
    if auth:
        secret = base64.b64encode(
            hashlib.sha256((password + auth["salt"]).encode()).digest()
        ).decode()
        resp = base64.b64encode(
            hashlib.sha256((secret + auth["challenge"]).encode()).digest()
        ).decode()
        ident["d"]["authentication"] = resp
    ws.send(json.dumps(ident))
    json.loads(ws.recv())
    return ws


def _rpc(ws, rtype, rdata=None, ignore_err=False):
    """Send an obs-websocket request and return its responseData."""
    ws.send(json.dumps({"op": 6, "d": {
        "requestType": rtype, "requestId": rtype, "requestData": rdata or {}}}))
    t0 = time.monotonic()
    while True:
        if OBS_OP_TIMEOUT_S > 0 and (time.monotonic() - t0) >= OBS_OP_TIMEOUT_S:
            raise TimeoutError(
                f"obs-websocket request {rtype!r} got no response within "
                f"{OBS_OP_TIMEOUT_S:.0f}s (#328)"
            )
        try:
            m = json.loads(ws.recv())
        except WebSocketTimeoutException:
            continue
        if m["op"] == 7 and m["d"]["requestId"] == rtype:
            st = m["d"]["requestStatus"]
            if not st["result"] and not ignore_err:
                raise RuntimeError(f"{rtype} failed: {st}")
            return m["d"].get("responseData") or {}


def _screenshot_hash(ws, source_name: str) -> "str | None":
    """Capture a GetSourceScreenshot of *source_name*, return SHA-1 hex of PNG
    bytes, or None on any failure (capture or decode error)."""
    res = _rpc(ws, "GetSourceScreenshot", {
        "sourceName": source_name,
        "imageFormat": "png",
        "imageWidth": 320,
    }, ignore_err=True)
    data = res.get("imageData") if res else None
    if not data:
        return None
    png = _extract_png(data)
    if not png:
        return None
    return _hash_png(png)


# ─── main capture loop ────────────────────────────────────────────────────────

def _capture_timelines(
    ws,
    sources: "list[str]",
    n_samples: int,
    cadence_s: float,
) -> "dict[str, list[str | None]]":
    """Capture *n_samples* screenshots of each source at *cadence_s* intervals.

    Returns {source_name: [hash_or_None, ...]}.
    Prints per-sample progress to stderr so the operator can see what is happening."""
    timelines: dict[str, list[str | None]] = {s: [] for s in sources}
    for i in range(n_samples):
        sys.stderr.write(f"  sample {i + 1}/{n_samples}")
        for src in sources:
            h = _screenshot_hash(ws, src)
            timelines[src].append(h)
            sys.stderr.write(f"  {src}={'ok' if h else 'FAIL'}")
        sys.stderr.write("\n")
        if i < n_samples - 1:
            time.sleep(cadence_s)
    return timelines


def _run_verdict(
    timeline_json: str,
    verdict_bin: str,
    threshold: int,
) -> "tuple[int, str]":
    """Run the frozen-camera-gate Rust binary, return (exit_code, stdout)."""
    result = subprocess.run(
        [verdict_bin, "--threshold", str(threshold)],
        input=timeline_json.encode(),
        capture_output=True,
    )
    return result.returncode, result.stdout.decode().strip()


def main():
    default_sources = "NDI cam1,NDI cam2,NDI cam3,NDI cam5"

    ap = argparse.ArgumentParser(
        description="#365 frozen-camera gate — verifies all raw NDI inputs are updating"
    )
    ap.add_argument("--host", required=True,
                    help="strih OBS WebSocket host (e.g. 10.77.9.202)")
    ap.add_argument("--password", default="",
                    help="OBS WS password (or set OBS_PASSWORD env var)")
    ap.add_argument("--sources", default=default_sources,
                    help=f"comma-separated source names to check (default: {default_sources})")
    ap.add_argument("--samples", type=int, default=8,
                    help="number of screenshot samples per source (default: 8 → ~8 s window)")
    ap.add_argument("--cadence", type=float, default=1.0,
                    help="seconds between samples (default: 1.0)")
    ap.add_argument("--threshold", type=int, default=3,
                    help="consecutive static samples before FROZEN (default: 3)")
    ap.add_argument("--verdict-bin", default="",
                    help="path to frozen-camera-gate binary (default: auto-discover)")
    args = ap.parse_args()

    password = os.environ.get("OBS_PASSWORD", args.password)
    sources = [s.strip() for s in args.sources.split(",") if s.strip()]
    verdict_bin = _find_verdict_bin(args.verdict_bin or os.environ.get("FROZEN_GATE_BIN", ""))

    sys.stderr.write(
        f"[frozen-camera-gate] host={args.host} sources={sources} "
        f"samples={args.samples} cadence={args.cadence}s threshold={args.threshold}\n"
    )
    sys.stderr.write(
        f"[frozen-camera-gate] precondition: Multiview projector must be OPEN (#276)\n"
    )

    try:
        ws = _conn(args.host, password)
    except Exception as e:
        sys.stderr.write(f"ERROR: cannot connect to OBS at {args.host}:{PORT}: {e}\n")
        sys.exit(2)

    try:
        timelines = _capture_timelines(ws, sources, args.samples, args.cadence)
    finally:
        try:
            ws.close()
        except Exception:
            pass

    timeline_json = _build_timeline_json(timelines)
    sys.stderr.write(f"[frozen-camera-gate] timeline JSON: {timeline_json}\n")

    exit_code, verdict_out = _run_verdict(timeline_json, verdict_bin, args.threshold)
    print(verdict_out)

    if exit_code == 0:
        sys.stderr.write("[frozen-camera-gate] PASS — all cameras are updating\n")
    else:
        sys.stderr.write(
            f"[frozen-camera-gate] FAIL — {verdict_out} "
            f"(check that the Multiview projector is open and that the camera box "
            f"and its NDI source are running)\n"
        )

    sys.exit(exit_code)


if __name__ == "__main__":
    main()
