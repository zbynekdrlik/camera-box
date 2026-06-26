#!/usr/bin/env python3
"""#111/#257 — toggle/inspect the per-source MEASUREMENT BURN on an OBS NDI input.

#257 replaced the env-gated burn (OBS_BURN_QR + attach/detach the filter) with a per-source
`genlock_burn` bool on the NDI source itself. The DistroAV QR burn EFFECT filter
(`distroav_qr_burn_filter`) is ALWAYS registered and its render is gated LIVE by the parent
source's `genlock_burn` flag (applied by ndi_source_update -> obs_source_set_genlock_burn, NO OBS
restart). So switching the burn on/off is now a single OBS WebSocket SetInputSettings, no relaunch:

  obs_burn_filter.py add    --host <ip> --input "<input name>" [--password P]   # genlock_burn=true (+ ensure filter)
  obs_burn_filter.py remove --host <ip> --input "<input name>" [--password P]   # genlock_burn=false
  obs_burn_filter.py check  --host <ip> --input "<input name>" [--password P]   # report state

The run_id / corner / qr size come from the box's host role (strih=911002/bottom-left,
stream=911004/bottom-right), NOT env or filter settings — this script only flips the per-source
`genlock_burn` bool (and ensures the renderer filter is present). The recording-verdict (#108)
pairs hops on the reserved run_ids exactly as before.

Reuses the obs_phase2 websocket connection helpers so the auth handshake stays in one place.
"""
import argparse
import sys

# Reuse the proven obs-websocket v5 connection + RPC helpers.
from obs_phase2 import _conn, _rpc  # noqa: E402

BURN_FILTER_KIND = "distroav_qr_burn_filter"  # ndi-burn-filter.cpp OBS_NDI_BURN_FILTER_ID
BURN_FILTER_NAME = "DistroAV QR Burn (latency probe)"  # burn_filter_getname()
BURN_SETTING = "genlock_burn"  # #257: PROP_BURN — the per-source measurement-burn bool


def _filter_kinds(ws):
    """Set of source-filter kinds OBS currently knows (the burn filter is always registered)."""
    data = _rpc(ws, "GetSourceFilterKindList", ignore_err=True)
    return set(data.get("sourceFilterKinds", []))


def _has_filter(ws, input_name):
    data = _rpc(ws, "GetSourceFilterList", {"sourceName": input_name}, ignore_err=True)
    return any(f.get("filterName") == BURN_FILTER_NAME for f in data.get("filters", []))


def _genlock_burn(ws, input_name):
    """Read the per-source genlock_burn bool from the input's settings (None if unknown)."""
    data = _rpc(ws, "GetInputSettings", {"inputName": input_name}, ignore_err=True)
    return data.get("inputSettings", {}).get(BURN_SETTING)


def _set_genlock_burn(ws, input_name, value):
    _rpc(ws, "SetInputSettings", {
        "inputName": input_name,
        "inputSettings": {BURN_SETTING: bool(value)},
        "overlay": True,  # merge — never clobber the source's other (forced) settings
    })


def _ensure_filter(ws, input_name):
    """Make sure the burn RENDERER filter is attached (the genlock_burn bool only gates rendering;
    no filter = nothing to render even with the bool on). Idempotent."""
    if _has_filter(ws, input_name):
        return
    kinds = _filter_kinds(ws)
    if BURN_FILTER_KIND not in kinds:
        sys.exit(
            f"[burn] FAIL: filter kind '{BURN_FILTER_KIND}' is NOT registered on this OBS.\n"
            f"        The #111/#257 burn filter is registered by the camera-box DistroAV build —\n"
            f"        this OBS is not running it (stock OBS?). Deploy the genlock build + relaunch.\n"
            f"        Known kinds: {sorted(kinds)}"
        )
    _rpc(ws, "CreateSourceFilter", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
        "filterKind": BURN_FILTER_KIND,
    })
    _rpc(ws, "SetSourceFilterEnabled", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
        "filterEnabled": True,
    }, ignore_err=True)
    if not _has_filter(ws, input_name):
        sys.exit(f"[burn] FAIL: burn filter did not attach to '{input_name}'")


def cmd_add(ws, input_name):
    """Turn the measurement burn ON: ensure the renderer filter is present + genlock_burn=true."""
    _ensure_filter(ws, input_name)
    _set_genlock_burn(ws, input_name, True)
    if _genlock_burn(ws, input_name) is not True:
        sys.exit(f"[burn] FAIL: genlock_burn did not turn on for '{input_name}'")
    print(f"[burn] ON  genlock_burn=true on '{input_name}' (filter present, runtime — no restart)")


def cmd_remove(ws, input_name):
    """Turn the measurement burn OFF: genlock_burn=false (the renderer stays, pass-through)."""
    _set_genlock_burn(ws, input_name, False)
    if _genlock_burn(ws, input_name) not in (False, None):
        sys.exit(f"[burn] FAIL: genlock_burn did not turn off for '{input_name}'")
    print(f"[burn] OFF genlock_burn=false on '{input_name}' (runtime — no restart)")


def cmd_check(ws, input_name):
    burn = _genlock_burn(ws, input_name)
    present = _has_filter(ws, input_name)
    registered = BURN_FILTER_KIND in _filter_kinds(ws)
    # `burn_on` is the authoritative tell (a burn renders only when the bool is true AND the
    # renderer filter is present). kind_registered/filter_on_input kept for diagnostics.
    print(
        f"[burn] burn_on={burn is True} genlock_burn={burn} filter_on_input={present} "
        f"kind_registered={registered} input='{input_name}'"
    )
    if not registered:
        print("[burn]   NOTE: this OBS does not have the camera-box burn filter (stock OBS?)")


def main():
    ap = argparse.ArgumentParser(description="#111/#257 per-source measurement-burn toggle/check")
    ap.add_argument("action", choices=["add", "remove", "check"])
    ap.add_argument("--host", required=True)
    ap.add_argument("--input", required=True, help="OBS NDI input/source name to toggle the burn on")
    ap.add_argument("--password", default="")
    a = ap.parse_args()
    ws = _conn(a.host, a.password)
    try:
        {"add": cmd_add, "remove": cmd_remove, "check": cmd_check}[a.action](ws, a.input)
    finally:
        ws.close()


if __name__ == "__main__":
    main()
