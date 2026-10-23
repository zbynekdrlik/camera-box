#!/usr/bin/env python3
"""program_audio_vban.py -- issue 1439: the OBS program-audio input as the obs-vban receive source.

On the stream role (design revision 6338127636) the console's VBAN program stream goes straight into
OBS: no intercom hub, no PipeWire. The program-audio input (`Zvuk z mixu`, named by the seed manifest)
becomes the vendored obs-vban SOURCE, which stamps every packet on its receive timeline (the sender's
fitted clock + the fixed "Receive Buffer" R) so the genlock ASRC resamples it to the mix clock.

This module holds the one definition of that source for the two tools that write it:
  * scripts/strih_scenes.py (the OBS launch seed, over obs-websocket) and
  * scripts/obs_collection_port.py (the offline collection port, OBS stopped),
and for the one that only grades it, `obs_phase2.py mbc-input-check` (rig-mode TEST on a Linux stream
box, issue 1462: mbc_vban_transport_problem).
It is pure Python with no websocket dependency; the obs-websocket helpers take any object with a
`req(request_type, data=None, ignore_err=False)` method (strih_scenes.Obs, or a test fake).

Replacing an input of another kind (the pulse capture, a ported Windows device) keeps everything the
operator set on it: its place (and visibility) in every scene, volume, mute, monitoring, mixer tracks,
balance, sync offset and its filters. obs-websocket has no "change kind" request, so the input is
removed and re-created with the same name, and that state is put back.
"""

import json
import string
import time

# The obs-vban source id (vendor/obs-vban: ID_PREFIX "net.nagater.obs-vban." + "source"), pinned by
# tests/python/test_stream_role_vban_1439.py against the vendored source and CMakeLists.
VBAN_SOURCE_KIND = "net.nagater.obs-vban.source"
# The Receive Buffer clamp of the source (VBAN_RECV_RESERVE_MS_MIN / _MAX in
# vendor/obs-vban/src/vban-receive-timeline.h, test-pinned) and the VBAN stream name size.
RESERVE_MS_MIN = 10
RESERVE_MS_MAX = 500
RESERVE_MS_DEFAULT = 40   # VBAN_RECV_RESERVE_MS_DEFAULT, the source's own default
STREAM_NAME_MAX = 16
_STREAM_NAME_CHARS = frozenset(string.ascii_letters + string.digits + "._-")
# A removed input's name can stay taken for a moment (OBS frees it when the last reference to the old
# source goes) and obs-websocket's CreateInput then refuses it. Every create first waits, bounded, for
# the name to be free: CREATE_POLLS reads CREATE_POLL_S apart (3 s), at most twice (one retry).
CREATE_POLL_S = 0.1
CREATE_POLLS = 30


def vban_source_settings(port, stream_name, receive_buffer_ms):
    """Pure: the obs-vban source settings receiving STREAM_NAME on UDP PORT from any host, with the fixed
    Receive Buffer RECEIVE_BUFFER_MS. Refuses (ValueError) anything the box facts could not hold."""
    if isinstance(port, bool) or not isinstance(port, int) or not 1024 <= port <= 65535:
        raise ValueError("port %r is not a UDP port in 1024..65535" % (port,))
    if (not isinstance(stream_name, str) or not 1 <= len(stream_name) <= STREAM_NAME_MAX
            or not set(stream_name) <= _STREAM_NAME_CHARS):
        raise ValueError("stream name %r is not a VBAN stream name (1..16 letters, digits, . _ -)" % (stream_name,))
    if (isinstance(receive_buffer_ms, bool) or not isinstance(receive_buffer_ms, int)
            or not RESERVE_MS_MIN <= receive_buffer_ms <= RESERVE_MS_MAX):
        raise ValueError("receive buffer %r ms is outside %d..%d" % (receive_buffer_ms, RESERVE_MS_MIN, RESERVE_MS_MAX))
    # ip_from "" = accept every host (vban_udp_set_host); the console's address is never pinned here.
    return {"port": port, "stream_name": stream_name, "ip_from": "", "receive_buffer_ms": receive_buffer_ms}


def parse_program_audio_vban(text):
    """The manifest's `program_audio_vban` block as obs-vban source settings, or None when the manifest
    declares none (the strih role: the program stays the PipeWire capture). A block that is present but
    malformed raises ValueError: the caller must leave the input alone rather than fall back to the
    pulse capture."""
    try:
        d = json.loads(text)
    except ValueError:
        return None
    if not isinstance(d, dict) or "program_audio_vban" not in d:
        return None
    v = d["program_audio_vban"]
    if not isinstance(v, dict):
        raise ValueError("program_audio_vban is not an object")
    return vban_source_settings(v.get("port"), v.get("stream_name"), v.get("receive_buffer_ms"))


def settings_differences(current, want):
    """Pure: `key current != want` for every key of WANT the input's CURRENT settings do not carry
    (an absent key is its OBS default: port 6980, an empty name / host, R 40 ms)."""
    defaults = {"port": 6980, "stream_name": "", "ip_from": "", "receive_buffer_ms": RESERVE_MS_DEFAULT}
    cur = dict(defaults)
    cur.update(current or {})
    return ["%s %r != %r" % (k, cur.get(k), v) for k, v in want.items() if cur.get(k) != v]


def mbc_vban_transport_problem(kind, settings, muted, want_stream, want_port):
    """Pure (issue 1462): rig-mode TEST's #901 item-2 decision for a LINUX stream box, whose program-audio
    input (`mbc` on stream-snv) is this obs-vban RECEIVE source fed by mbc.lan's VBAN stream (no Dante
    card there). A short problem string, or None when the input is this source, on the box facts'
    stream name WANT_STREAM and UDP port WANT_PORT, unmuted. Mute is named FIRST and alone, like the
    Dante check (obs_phase2._mbc_transport_problem). The settings go through settings_differences, so
    a key obs-websocket leaves out reads as the source's own default (port 6980, an empty stream name).
    The Receive Buffer is the operator's setting (verify-strih item 9 grades it), not this check's."""
    if muted:
        return "input is MUTED (mute must be OFF for the measurement chain to be usable)"
    if kind != VBAN_SOURCE_KIND:
        return ("input kind is %r, expected the obs-vban receive source %r (the stream role's program-audio "
                "input, issue 1439)" % (kind, VBAN_SOURCE_KIND))
    diffs = settings_differences(settings or {}, {"stream_name": want_stream, "port": want_port})
    if diffs:
        return "the obs-vban source does not carry the box facts' stream: " + "; ".join(diffs)
    return None


def kind_available(obs, kind):
    """True when OBS has the input KIND loaded (GetInputKindList, unversioned ids). An OBS that does not
    answer reads False: never remove an input for a kind that might not be creatable."""
    kinds = (obs.req("GetInputKindList", {"unversioned": True}, ignore_err=True) or {}).get("inputKinds")
    return isinstance(kinds, list) and kind in kinds


def _value(obs, req, data, key):
    return (obs.req(req, data, ignore_err=True) or {}).get(key)


def input_state_read(obs, name):
    """The operator state of input NAME that a re-create must put back: every scene holding it (in the
    scene list's order) with the item's visibility, volume, mute, monitoring, mixer tracks, balance,
    sync offset and filters. A value OBS does not answer is None (never restored)."""
    who = {"inputName": name}
    scenes = []
    for sc in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", []) or []:
        sn = sc.get("sceneName")
        items = (obs.req("GetSceneItemList", {"sceneName": sn}, ignore_err=True) or {}).get("sceneItems", [])
        for it in items or []:
            if it.get("sourceName") == name:
                scenes.append((sn, it.get("sceneItemEnabled") is not False))
                break
    filters = (obs.req("GetSourceFilterList", {"sourceName": name}, ignore_err=True) or {}).get("filters")
    return {
        "scenes": scenes,
        "volume_mul": _value(obs, "GetInputVolume", who, "inputVolumeMul"),
        "muted": _value(obs, "GetInputMute", who, "inputMuted"),
        "monitor": _value(obs, "GetInputAudioMonitorType", who, "monitorType"),
        "tracks": _value(obs, "GetInputAudioTracks", who, "inputAudioTracks"),
        "balance": _value(obs, "GetInputAudioBalance", who, "inputAudioBalance"),
        "sync_offset": _value(obs, "GetInputAudioSyncOffset", who, "inputAudioSyncOffset"),
        "filters": sorted((f for f in (filters or []) if isinstance(f, dict) and f.get("filterName")),
                          key=lambda f: f.get("filterIndex", 0)),
    }


def input_kind_listed(obs, name):
    """The inputKind GetInputList lists for input NAME: '' when OBS lists no such input, None when OBS
    does not answer, '?' for an input listed without a kind."""
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs")
    if not isinstance(inputs, list):
        return None
    for i in inputs:
        if isinstance(i, dict) and i.get("inputName") == name:
            return i.get("inputKind") or "?"
    return ""


def listed_text(name, listed):
    """Pure: what INPUT_KIND_LISTED's answer means, for a status line."""
    if listed is None:
        return "OBS does not answer GetInputList"
    if listed == "":
        return "OBS has no '%s' input" % name
    return "OBS still lists '%s' as %s" % (name, listed)


def _await_free(obs, name, sleep):
    for _ in range(CREATE_POLLS):
        if input_kind_listed(obs, name) == "":
            return
        sleep(CREATE_POLL_S)


def input_create_verified(obs, scene, name, kind, settings, remove_on_retry=False, sleep=None):
    """Create input NAME as KIND with SETTINGS in SCENE once the name is free (a bounded wait) and read
    it back from GetInputList. When OBS does not list it as KIND, try once more: wait for the name again
    (REMOVE_ON_RETRY first removes an input still listed under it, the old one of a re-create) and
    create again. Returns (the last CreateInput answer, the kind GetInputList lists afterwards: KIND on
    success, otherwise input_kind_listed's answer)."""
    sleep = sleep or time.sleep
    resp, listed = None, None
    for attempt in range(2):
        if attempt and remove_on_retry and listed not in ("", None):
            obs.req("RemoveInput", {"inputName": name}, ignore_err=True)
        _await_free(obs, name, sleep)
        resp = obs.req("CreateInput", {"sceneName": scene, "inputName": name, "inputKind": kind,
                                       "inputSettings": settings}, ignore_err=True)
        listed = input_kind_listed(obs, name)
        if listed == kind:
            break
    return resp, listed


def _hide_item(obs, scene, resp):
    item_id = (resp or {}).get("sceneItemId")
    if isinstance(item_id, int):
        obs.req("SetSceneItemEnabled", {"sceneName": scene, "sceneItemId": item_id, "sceneItemEnabled": False},
                ignore_err=True)


def input_recreate(obs, name, kind, settings, state, fallback_scene, sleep=None):
    """Remove input NAME and create it again as KIND with SETTINGS (input_create_verified: a bounded
    wait for the name, read back, one retry), then put STATE (input_state_read, read BEFORE the call)
    back. The first scene that held it gets the new input (else FALLBACK_SCENE); every other one gets a
    scene item. Returns (True, the number of scenes it was put back in), or (False, the kind
    GetInputList lists for NAME) when OBS never listed it as KIND: nothing is put back then."""
    scenes = state.get("scenes") or [(fallback_scene, True)]
    obs.req("RemoveInput", {"inputName": name}, ignore_err=True)
    first, enabled = scenes[0]
    resp, listed = input_create_verified(obs, first, name, kind, settings, remove_on_retry=True, sleep=sleep)
    if listed != kind:
        return False, listed
    if not enabled:
        _hide_item(obs, first, resp)
    for scene, enabled in scenes[1:]:
        resp = obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": name}, ignore_err=True)
        if not enabled:
            _hide_item(obs, scene, resp)
    who = {"inputName": name}
    if isinstance(state.get("volume_mul"), (int, float)) and not isinstance(state.get("volume_mul"), bool):
        obs.req("SetInputVolume", dict(who, inputVolumeMul=state["volume_mul"]), ignore_err=True)
    if isinstance(state.get("muted"), bool):
        obs.req("SetInputMute", dict(who, inputMuted=state["muted"]), ignore_err=True)
    if isinstance(state.get("monitor"), str) and state["monitor"]:
        obs.req("SetInputAudioMonitorType", dict(who, monitorType=state["monitor"]), ignore_err=True)
    if isinstance(state.get("tracks"), dict) and state["tracks"]:
        obs.req("SetInputAudioTracks", dict(who, inputAudioTracks=state["tracks"]), ignore_err=True)
    if isinstance(state.get("balance"), (int, float)) and not isinstance(state.get("balance"), bool):
        obs.req("SetInputAudioBalance", dict(who, inputAudioBalance=state["balance"]), ignore_err=True)
    offset = state.get("sync_offset")
    if isinstance(offset, int) and not isinstance(offset, bool) and offset:
        obs.req("SetInputAudioSyncOffset", dict(who, inputAudioSyncOffset=offset), ignore_err=True)
    for f in state.get("filters") or []:
        obs.req("CreateSourceFilter", {"sourceName": name, "filterName": f["filterName"],
                                       "filterKind": f.get("filterKind"),
                                       "filterSettings": f.get("filterSettings") or {}}, ignore_err=True)
        if f.get("filterEnabled") is False:
            obs.req("SetSourceFilterEnabled", {"sourceName": name, "filterName": f["filterName"],
                                               "filterEnabled": False}, ignore_err=True)
    return True, len(scenes)
