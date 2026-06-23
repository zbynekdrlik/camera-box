#!/usr/bin/env python3
"""#111 — add/remove/check the DistroAV QR render-time burn EFFECT filter on an OBS input.

The #111 burn is an OBS source EFFECT filter (`distroav_qr_burn_filter`, registered by the
vendored DistroAV when `OBS_BURN_QR` is set in the OBS process environment). Attaching it to
the input that feeds a box's PROGRAM output makes every rendered frame carry a per-render QR
(this node's frame_id + RAW render-instant wall-clock gen_ts_ns), so a downstream RECORDING
self-contains a frame-exact per-node timestamp. The recording-verdict (#108) decodes the burns
and computes per-hop ABSOLUTE latency (cam->strih, strih->stream) with no network/idx-30.

Usage:
  obs_burn_filter.py add    --host <ip> --input "<input name>" [--password P]
  obs_burn_filter.py remove --host <ip> --input "<input name>" [--password P]
  obs_burn_filter.py check  --host <ip> --input "<input name>" [--password P]

The filter is registered by DistroAV ONLY when OBS was launched with OBS_BURN_QR set; if the
filter kind is missing, `add` fails LOUDLY (the operator must relaunch OBS with the env). The
burn's run_id / qr_px come from the OBS process env (OBS_BURN_RUN_ID / OBS_BURN_QR_PX), NOT
from filter settings — this script only attaches/detaches the filter, never sets the stamp.

Reuses the obs_phase2 websocket connection helpers so the auth handshake stays in one place.
"""
import argparse
import sys

# Reuse the proven obs-websocket v5 connection + RPC helpers.
from obs_phase2 import _conn, _rpc  # noqa: E402

BURN_FILTER_KIND = "distroav_qr_burn_filter"  # ndi-burn-filter.cpp OBS_NDI_BURN_FILTER_ID
BURN_FILTER_NAME = "DistroAV QR Burn (latency probe)"  # burn_filter_getname()


def _filter_kinds(ws):
    """Set of source-filter kinds OBS currently knows (empty unless OBS_BURN_QR launched it)."""
    data = _rpc(ws, "GetSourceFilterKindList", ignore_err=True)
    return set(data.get("sourceFilterKinds", []))


def _has_filter(ws, input_name):
    data = _rpc(ws, "GetSourceFilterList", {"sourceName": input_name}, ignore_err=True)
    return any(f.get("filterName") == BURN_FILTER_NAME for f in data.get("filters", []))


def cmd_add(ws, input_name):
    kinds = _filter_kinds(ws)
    if BURN_FILTER_KIND not in kinds:
        sys.exit(
            f"[burn] FAIL: filter kind '{BURN_FILTER_KIND}' is NOT registered on this OBS.\n"
            f"        The #111 burn registers ONLY when OBS was launched with OBS_BURN_QR set.\n"
            f"        Relaunch OBS with OBS_BURN_QR=1 (+ OBS_BURN_RUN_ID per node) and retry.\n"
            f"        Known kinds: {sorted(kinds)}"
        )
    if _has_filter(ws, input_name):
        print(f"[burn] already present on '{input_name}' — no-op")
        return
    _rpc(ws, "CreateSourceFilter", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
        "filterKind": BURN_FILTER_KIND,
    })
    # Make sure it is enabled.
    _rpc(ws, "SetSourceFilterEnabled", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
        "filterEnabled": True,
    }, ignore_err=True)
    if not _has_filter(ws, input_name):
        sys.exit(f"[burn] FAIL: filter did not attach to '{input_name}'")
    print(f"[burn] ADDED '{BURN_FILTER_NAME}' to '{input_name}'")


def cmd_remove(ws, input_name):
    if not _has_filter(ws, input_name):
        print(f"[burn] not present on '{input_name}' — no-op")
        return
    _rpc(ws, "RemoveSourceFilter", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
    })
    print(f"[burn] REMOVED '{BURN_FILTER_NAME}' from '{input_name}'")


def cmd_check(ws, input_name):
    kinds = _filter_kinds(ws)
    registered = BURN_FILTER_KIND in kinds
    present = _has_filter(ws, input_name)
    print(f"[burn] kind_registered={registered} filter_on_input={present} input='{input_name}'")
    if not registered:
        print("[burn]   NOTE: OBS was NOT launched with OBS_BURN_QR (filter kind absent)")


def main():
    ap = argparse.ArgumentParser(description="#111 DistroAV QR burn filter add/remove/check")
    ap.add_argument("action", choices=["add", "remove", "check"])
    ap.add_argument("--host", required=True)
    ap.add_argument("--input", required=True, help="OBS input/source name to attach the burn to")
    ap.add_argument("--password", default="")
    a = ap.parse_args()
    ws = _conn(a.host, a.password)
    try:
        {"add": cmd_add, "remove": cmd_remove, "check": cmd_check}[a.action](ws, a.input)
    finally:
        ws.close()


if __name__ == "__main__":
    main()
