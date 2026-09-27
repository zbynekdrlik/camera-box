#!/usr/bin/env python3
"""e2e_bandwidth_hold.py -- the E2E HOLD of the strih bandwidth roles (issue 1242).

strih-lx pulls full-bandwidth NDI only for the cameras that are shown: a program-path main `NDI camN`
(`genlock_connect_on_show`) PARKS while hidden, and an always-connected monitor twin `MV NDI camN`
(`genlock_monitor`, NDI "lowest") feeds the built-in multiview. An E2E run needs the opposite:

  * every program-path MAIN connected at full bandwidth for the whole measurement (connect-on-show
    off), and
  * every monitor TWIN OFF THE WIRE, so the run measures the program path at the pre-roles strih-lx
    uplink load (a twin from a camera-box sender costs ~58 Mbps even at "lowest"; 7 mains + 7 twins
    tail-dropped the 2.5 GbE uplink during the E2E). A twin gets strih_bandwidth_roles.E2E_TWIN_HOLD
    (genlock off + audio-only): with genlock on, DistroAV's #150 lockdown forces a monitor twin back to
    LOWEST on every settings update, so an audio-only write alone never sticks.

`hold` records the held mains and each twin's ORIGINAL settings in a STABLE state file BEFORE any
write (UNION with a leftover file from a killed run; a leftover's recorded original wins), settles the
mains BEFORE any twin goes off the wire, and never takes a twin off the wire whose main is unconfirmed.
`restore` puts the twins back FIRST and gives connect-on-show back only to a main whose twin landed.
Every read-back is a SETTLE poll: OBS applies an input's settings update on the next video tick after
the WebSocket overlay, so an immediate read would return the overlay even when the update is about to
revert it.

The WebSocket request function, the sleep and the clock are INJECTED (a `Settle` + `rpc(ws, request,
data, ignore_err=...)`): obs_phase2.py passes its own `_rpc` and settle seams at call time (the
issue-1380 stream_dev_scene pattern), so this module has no WebSocket dependency and every obs_phase2
test that monkeypatches `obs_phase2._rpc` drives it unchanged. Used by `obs_phase2.py connect-on-show
--hold/--restore` (scripts/lib/connect-on-show-hold.sh, recording-e2e.sh); tests:
tests/python/test_e2e_twin_hold_1242.py + tests/python/test_strih_bandwidth_roles_1242.py.
"""
import json
import os
from collections import namedtuple

import strih_bandwidth_roles as roles

# The settle poll's timing + seams: min_s before the first read (several render ticks), poll_s between
# sweeps, budget_s after which an input that also had two complete sweeps fails, and sleep/clock.
Settle = namedtuple("Settle", "min_s poll_s budget_s sleep clock")


def connect_on_show_targets(settings_by_input):
    """PURE: the input names whose settings carry genlock_connect_on_show=True (sorted)."""
    return sorted(n for n, s in (settings_by_input or {}).items()
                  if (s or {}).get(roles.CONNECT_ON_SHOW_KEY))


def read_state(path):
    """(held_mains, twin_originals) from the hold state file:
    {"connect_on_show": [...], "twins": {name: {"genlock_fifo": bool, "ndi_bw_mode": int}}}. The legacy
    list of held mains (a leftover from before the twin hold) reads as ([...], {}); no file -> ([], {}).
    scripts/lib/connect-on-show-hold.sh `connect_on_show_held_mains` reads the SAME shape (pinned by
    tests/python/test_e2e_twin_hold_1242.py)."""
    try:
        with open(path) as fh:
            data = json.load(fh)
    except FileNotFoundError:
        return [], {}
    except (OSError, ValueError) as e:
        raise RuntimeError(f"connect-on-show hold state {path!r} unreadable: {e}") from e
    if isinstance(data, list):
        mains, twins = data, {}
    elif isinstance(data, dict):
        mains, twins = data.get("connect_on_show") or [], data.get("twins") or {}
    else:
        mains, twins = [], {}
    mains = sorted({str(n) for n in mains if n}) if isinstance(mains, list) else []
    twins = ({str(n): dict(v) for n, v in twins.items() if n and isinstance(v, dict)}
             if isinstance(twins, dict) else {})
    return mains, twins


def write_state(path, mains, twins):
    """Write the state file atomically (tmp + rename), creating its directory on demand."""
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)
    tmp = f"{path}.tmp"
    with open(tmp, "w") as fh:
        json.dump({"connect_on_show": sorted(mains),
                   "twins": {n: twins[n] for n in sorted(twins)}}, fh)
    os.replace(tmp, path)


def _defaults(rpc, ws):
    return (rpc(ws, "GetInputDefaultSettings", {"inputKind": "ndi_source"}, ignore_err=True)
            or {}).get("defaultInputSettings") or {}


def await_settled(rpc, ws, wants, match, settle):
    """Poll each input's EFFECTIVE settings (type defaults, read once, under the explicit settings) until
    match(effective, wants[name]) holds on two consecutive reads, the first no earlier than
    settle.min_s after the caller's writes. A GetInputSettings request error is never a match (a hold
    target can equal the type default). An input fails only once settle.budget_s has passed AND it had
    two complete sweeps (a slow WebSocket never fails a write it never re-read). Returns the names that
    never settled (sorted; [] = every write landed)."""
    if not wants:
        return []
    start = settle.clock()
    settle.sleep(settle.min_s)
    defaults = _defaults(rpc, ws)
    streak = {n: 0 for n in wants}
    sweeps = 0
    while True:
        for n in sorted(streak):
            if streak[n] >= 2:
                continue
            resp = rpc(ws, "GetInputSettings", {"inputName": n}, ignore_err=True) or {}
            ok = "inputSettings" in resp and match({**defaults, **(resp.get("inputSettings") or {})},
                                                   wants[n])
            streak[n] = streak[n] + 1 if ok else 0
        sweeps += 1
        pending = sorted(n for n, c in streak.items() if c < 2)
        if not pending or (sweeps >= 2 and settle.clock() - start >= settle.budget_s):
            return pending
        settle.sleep(settle.poll_s)


def write_and_settle(rpc, ws, wants, match, settle):
    """Write every wants[name] (overlay), then wait for all of them to settle. Returns the failed
    names (sorted)."""
    for n in sorted(wants):
        rpc(ws, "SetInputSettings", {"inputName": n, "inputSettings": dict(wants[n]), "overlay": True},
            ignore_err=True)
    return await_settled(rpc, ws, wants, match, settle)


def hold(rpc, ws, state_path, settle):
    """HOLD for an E2E run: every connect-on-show MAIN stays connected (flag off) and every role-marked
    monitor TWIN goes off the wire (roles.E2E_TWIN_HOLD).
      1. Records the held mains and each twin's ORIGINAL (roles.twin_hold_original of its EFFECTIVE
         settings) -- UNION with any leftover state file, a leftover's recorded original winning --
         BEFORE any write. An input whose settings cannot be read is a failure (it may be a twin still
         on the wire, or a main about to be measured parked).
      2. Writes the mains present and WAITS for them to settle.
      3. Only then writes the twins present, except a twin whose main did not settle or could not be
         read (that main may still be parked, so the camera keeps its twin as its one configured
         receiver), and waits for them to settle.
    Returns (held_mains, held_twins, failed_names): the inputs written this run and the ones that
    failed (unreadable, or a write that never read back)."""
    inputs = (rpc(ws, "GetInputList", {"inputKind": "ndi_source"}) or {}).get("inputs") or []
    settings, unreadable = {}, []
    for i in inputs:
        name = i.get("inputName")
        if not name or i.get("inputKind", "ndi_source") != "ndi_source":
            continue
        resp = rpc(ws, "GetInputSettings", {"inputName": name}, ignore_err=True) or {}
        if "inputSettings" not in resp:
            unreadable.append(name)
            continue
        settings[name] = resp.get("inputSettings") or {}
    left_mains, left_twins = read_state(state_path)
    twin_targets = roles.twin_hold_targets(settings)
    defaults = _defaults(rpc, ws) if twin_targets else {}
    twins = {tw: roles.twin_hold_original({**defaults, **settings[tw]}) for tw in twin_targets}
    twins.update(left_twins)  # a leftover twin reads HELD now; its recorded original is the truth
    mains = sorted(set(left_mains) | set(connect_on_show_targets(settings)))
    write_state(state_path, mains, twins)
    held_mains = [n for n in mains if n in settings]
    failed_mains = write_and_settle(rpc, ws, {n: {roles.CONNECT_ON_SHOW_KEY: False} for n in held_mains},
                                    roles.settings_match, settle)
    unconfirmed = set(failed_mains) | set(unreadable)
    held_twins = sorted(n for n in twins if n in settings and roles.twin_main(n) not in unconfirmed)
    failed_twins = write_and_settle(rpc, ws, {n: dict(roles.E2E_TWIN_HOLD) for n in held_twins},
                                    roles.settings_match, settle)
    return held_mains, held_twins, sorted(set(unreadable) | set(failed_mains) | set(failed_twins))


def restore(rpc, ws, state_path, settle):
    """RESTORE the hold, twins FIRST: every recorded twin present gets roles.twin_restore_values(original)
    (genlock on + the monitor role -- DistroAV's lockdown then puts LOWEST back itself) and is verified
    after it settles; only then does every held main present get connect-on-show back -- EXCEPT a main
    whose twin did not settle, which stays held (full bandwidth, and in the state file), so that camera
    keeps its main configured to connect. An input deleted / renamed since the hold has
    nothing to restore (done). The state file is removed only when every restore landed. No state
    file -> ([], [], []). Returns (restored_names, failed_names, held_back_mains)."""
    if not os.path.exists(state_path):
        return [], [], []
    mains, twins = read_state(state_path)
    present = {i.get("inputName") for i in
               (rpc(ws, "GetInputList", {"inputKind": "ndi_source"}) or {}).get("inputs") or []}
    twin_wants = {n: roles.twin_restore_values(twins[n]) for n in twins if n in present}
    failed_twins = write_and_settle(rpc, ws, twin_wants, roles.settings_match, settle)
    main_names = [n for n in mains if n in present and roles.twin_name(n) not in failed_twins]
    held_back = [n for n in mains if n in present and roles.twin_name(n) in failed_twins]
    failed_mains = write_and_settle(rpc, ws, {n: {roles.CONNECT_ON_SHOW_KEY: True} for n in main_names},
                                    roles.settings_match, settle)
    failed = sorted(set(failed_twins) | set(failed_mains))
    if not failed:
        os.remove(state_path)
    restored = [n for n in sorted(twin_wants) + main_names if n not in failed]
    return restored, failed, held_back
