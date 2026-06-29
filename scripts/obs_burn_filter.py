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


def _filter_enabled(ws, input_name):
    """Read the burn filter's ENABLED state (None if the filter is absent).

    #334: a DISABLED effect filter's video_render is never invoked by OBS, so the burn never
    renders even when genlock_burn=true and the C++ setter fires. The `check` gate and the `add`
    path therefore MUST consider this, not just the filter's presence.
    """
    data = _rpc(ws, "GetSourceFilterList", {"sourceName": input_name}, ignore_err=True)
    for f in data.get("filters", []):
        if f.get("filterName") == BURN_FILTER_NAME:
            return bool(f.get("filterEnabled"))
    return None


def compute_burn_on(genlock_burn, filter_present, filter_enabled):
    """Pure gate for whether a measurement burn will actually RENDER into the program.

    A burn renders only when ALL hold (#257 + #334):
      - the per-source genlock_burn bool is true,
      - the renderer effect filter is attached, AND
      - that filter is ENABLED (a disabled effect filter never renders — #334).
    """
    return (genlock_burn is True) and bool(filter_present) and bool(filter_enabled)


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
    """Make sure the burn RENDERER filter is attached AND ENABLED (the genlock_burn bool only gates
    rendering; no filter = nothing to render, and a DISABLED filter never renders either — #334).
    Idempotent: creates the filter if absent, then unconditionally (re-)enables it."""
    if not _has_filter(ws, input_name):
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
        if not _has_filter(ws, input_name):
            sys.exit(f"[burn] FAIL: burn filter did not attach to '{input_name}'")
    # #334: ALWAYS (re-)enable — a present-but-disabled filter never renders the burn, which is
    # exactly how the strih(911002)+stream(911004) burns went missing from the recording.
    _rpc(ws, "SetSourceFilterEnabled", {
        "sourceName": input_name,
        "filterName": BURN_FILTER_NAME,
        "filterEnabled": True,
    }, ignore_err=True)


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
    enabled = _filter_enabled(ws, input_name)
    registered = BURN_FILTER_KIND in _filter_kinds(ws)
    # `burn_on` is the authoritative tell: a burn RENDERS only when the per-source bool is true
    # AND the renderer filter is attached AND that filter is ENABLED. A present-but-DISABLED
    # filter never renders (its video_render is not invoked), so it must NOT report burn_on=True
    # (the #334 false positive that let the [4b/8] pre-record gate pass with no burn in the
    # recording). genlock_burn/filter_on_input/filter_enabled/kind_registered are kept for diag.
    burn_on = compute_burn_on(burn, present, enabled)
    print(
        f"[burn] burn_on={burn_on} genlock_burn={burn} filter_on_input={present} "
        f"filter_enabled={enabled} kind_registered={registered} input='{input_name}'"
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
