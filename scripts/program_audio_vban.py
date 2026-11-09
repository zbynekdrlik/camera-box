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
re-created with the same name and that state is put back.

issue 1495 (live stream-snv 9.11.2026): the old input is never removed FIRST. A removed source stays
listed and keeps its name until its last reference goes (obs_source_remove only marks it removed), and
the av-sync dock looks `mbc` up BY NAME every 5 s and keeps what it finds -- a removed source too -- so
the old `mbc` lingered until an OBS restart, CreateInput refused the name, and the program audio had
already left every scene. Now the old input is RENAMED aside (`<name> (seed replace N)`), the new one is
created beside it with hidden scene items and the operator state, then the old one is removed and the
new items shown. A create OBS refuses renames the old one back; an interrupted replace leaves the aside
input, which the next launch seed resumes from.
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
    if parse_program_audio_dante(text):   # issue 1485: the Dante program (a malformed block raises)
        return dict(DANTE_SEED)
    if not isinstance(d, dict) or "program_audio_vban" not in d:
        return None
    v = d["program_audio_vban"]
    if not isinstance(v, dict):
        raise ValueError("program_audio_vban is not an object")
    return vban_source_settings(v.get("port"), v.get("stream_name"), v.get("receive_buffer_ms"))


# issue 1485: the obs release's Dante source (plugins/dante-input in zbynekdrlik/obs). It has NO
# settings: the device comes from ~/.camera-box/dante-input.toml at module load, and the kind is
# registered only when that file exists.
DANTE_SOURCE_KIND = "dante_input"


DANTE_SEED = {"__kind__": DANTE_SOURCE_KIND}


def parse_program_audio_dante(text):
    """issue 1485: True when the manifest declares `program_audio_dante` (the strih box whose program is
    the Dante device strih-obs), else False. A block that is present but not an object raises ValueError:
    the caller must leave the input alone rather than turn it into another kind."""
    try:
        d = json.loads(text)
    except ValueError:
        return False
    if not isinstance(d, dict) or "program_audio_dante" not in d:
        return False
    if not isinstance(d["program_audio_dante"], dict):
        raise ValueError("program_audio_dante is not an object")
    return True


def program_audio_want(source, pulse_kind, pulse_settings):
    """issue 1485: (kind, settings) the seed makes the program-audio input from what
    parse_program_audio_vban returned: None = the pulse capture (PULSE_KIND, PULSE_SETTINGS), the
    DANTE_SEED marker = the obs release's dante_input (no settings), else the obs-vban source's."""
    if not source:
        return pulse_kind, pulse_settings
    if source.get("__kind__") == DANTE_SOURCE_KIND:
        return DANTE_SOURCE_KIND, {}
    return VBAN_SOURCE_KIND, dict(source)


def kind_missing_text(kind, want_kind):
    """The seed's status when OBS has no WANT_KIND source loaded: the input is kept as it is."""
    if want_kind == DANTE_SOURCE_KIND:
        return ("kept %s: OBS has no %s source registered (the obs release's dante-input.so loads it only "
                "with ~/.camera-box/dante-input.toml, setup-strih step 13; issue 1485)" % (kind or "absent", want_kind))
    return "kept %s: OBS has no %s source loaded (deploy the strih bundle with obs-vban, issue 1439)" % (
        kind or "absent", want_kind)


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


def mbc_dante_transport_problem(kind, muted):
    """Pure (issue 1495): rig-mode TEST's #901 item-2 decision for a Linux stream box whose program-audio
    input (`mbc` on stream-snv) is the obs release's dante_input source (the box's Dante device stream-obs,
    which subscribes mbc-snv's stream mix). A short problem string, or None when the input is that source,
    unmuted. Mute is named FIRST and alone, like the other two checks. The source has no settings: which
    transmitter it hears is the device's dante-input.toml (verify-strih item 9 grades it)."""
    if muted:
        return "input is MUTED (mute must be OFF for the measurement chain to be usable)"
    if kind != DANTE_SOURCE_KIND:
        return ("input kind is %r, expected the Dante source %r (the box's program Dante device, issue 1495)"
                % (kind, DANTE_SOURCE_KIND))
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
    scene list's order) with the item's visibility and index, volume, mute, monitoring, mixer tracks,
    balance, sync offset and filters. A value OBS does not answer is None (never restored)."""
    who = {"inputName": name}
    scenes = []
    for sc in (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes", []) or []:
        sn = sc.get("sceneName")
        items = (obs.req("GetSceneItemList", {"sceneName": sn}, ignore_err=True) or {}).get("sceneItems", [])
        for pos, it in enumerate(items or []):
            if it.get("sourceName") == name:
                index = it.get("sceneItemIndex")
                scenes.append((sn, it.get("sceneItemEnabled") is not False,
                               index if isinstance(index, int) and not isinstance(index, bool) else pos))
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


def input_create_verified(obs, scene, name, kind, settings, sleep=None, enabled=True):
    """Create input NAME as KIND with SETTINGS in SCENE (its scene item ENABLED or hidden) once the name
    is free (a bounded wait) and read it back from GetInputList. When OBS does not list it as KIND, try
    once more (wait for the name, create again). Never removes anything. Returns (the last CreateInput
    answer, the kind GetInputList lists afterwards: KIND on success, otherwise input_kind_listed's
    answer)."""
    sleep = sleep or time.sleep
    resp, listed = None, None
    for _attempt in range(2):
        _await_free(obs, name, sleep)
        req = {"sceneName": scene, "inputName": name, "inputKind": kind, "inputSettings": settings}
        if not enabled:
            req["sceneItemEnabled"] = False
        resp = obs.req("CreateInput", req, ignore_err=True)
        listed = input_kind_listed(obs, name)
        if listed == kind:
            break
    return resp, listed


# issue 1495: the name a replace parks the old input under while the new one is created beside it.
ASIDE_TAG = " (seed replace "


def aside_inputs(inputs, name):
    """Pure: the names in INPUTS (GetInputList's `inputs`) that a replace of NAME parked aside
    (`<name> (seed replace N)`), newest (highest N) first."""
    found = []
    for i in inputs or []:
        n = i.get("inputName") if isinstance(i, dict) else None
        if isinstance(n, str) and n.startswith(name + ASIDE_TAG) and n.endswith(")"):
            num = n[len(name) + len(ASIDE_TAG):-1]
            if num.isascii() and num.isdigit():
                found.append((int(num), n))
    return [n for _num, n in sorted(found, reverse=True)]


def _free_aside_name(obs, name):
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs") or []
    taken = {i.get("inputName") for i in inputs if isinstance(i, dict)}
    n = 1
    while "%s%s%d)" % (name, ASIDE_TAG, n) in taken:
        n += 1
    return "%s%s%d)" % (name, ASIDE_TAG, n)


def _rename(obs, old, new):
    """SetInputName OLD -> NEW, read back: True only when OBS lists NEW and no longer OLD."""
    obs.req("SetInputName", {"inputName": old, "newInputName": new}, ignore_err=True)
    return input_kind_listed(obs, new) not in ("", None) and input_kind_listed(obs, old) == ""


def _item_id(resp):
    item_id = (resp or {}).get("sceneItemId")
    return item_id if isinstance(item_id, int) and not isinstance(item_id, bool) else None


def _restore_levels(obs, name, state):
    """The mixer state a hidden item does not make audible: volume, mute, tracks, balance, sync offset."""
    who = {"inputName": name}
    if isinstance(state.get("volume_mul"), (int, float)) and not isinstance(state.get("volume_mul"), bool):
        obs.req("SetInputVolume", dict(who, inputVolumeMul=state["volume_mul"]), ignore_err=True)
    if isinstance(state.get("muted"), bool):
        obs.req("SetInputMute", dict(who, inputMuted=state["muted"]), ignore_err=True)
    if isinstance(state.get("tracks"), dict) and state["tracks"]:
        obs.req("SetInputAudioTracks", dict(who, inputAudioTracks=state["tracks"]), ignore_err=True)
    if isinstance(state.get("balance"), (int, float)) and not isinstance(state.get("balance"), bool):
        obs.req("SetInputAudioBalance", dict(who, inputAudioBalance=state["balance"]), ignore_err=True)
    offset = state.get("sync_offset")
    if isinstance(offset, int) and not isinstance(offset, bool) and offset:
        obs.req("SetInputAudioSyncOffset", dict(who, inputAudioSyncOffset=offset), ignore_err=True)


def _is_headphone_filter(f):
    return f.get("filterKind") == AUDIO_MONITOR_FILTER_KIND


def _restore_filters(obs, name, state, headphones):
    """The filters in index order: HEADPHONES False = every filter but the audio monitor ones (they shape
    the program sound, so they are on before the new items are shown), True = only the audio monitor
    ones (they play to the operator's headphones whatever the scene items show, so they come only once
    the old input is gone: never the old and the new input together, issue 1495 review round 1)."""
    for f in state.get("filters") or []:
        if _is_headphone_filter(f) != headphones:
            continue
        obs.req("CreateSourceFilter", {"sourceName": name, "filterName": f["filterName"],
                                       "filterKind": f.get("filterKind"),
                                       "filterSettings": f.get("filterSettings") or {}}, ignore_err=True)
        if f.get("filterEnabled") is False:
            obs.req("SetSourceFilterEnabled", {"sourceName": name, "filterName": f["filterName"],
                                               "filterEnabled": False}, ignore_err=True)


def _restore_filter_order(obs, name, state):
    """The audio monitor filters were created last: move them back to their index (after the show; the
    order is the headphones' tap point only)."""
    for f in state.get("filters") or []:
        index = f.get("filterIndex")
        if _is_headphone_filter(f) and isinstance(index, int) and not isinstance(index, bool):
            obs.req("SetSourceFilterIndex", {"sourceName": name, "filterName": f["filterName"],
                                             "filterIndex": index}, ignore_err=True)


def _swap_in(obs, name, aside, state, items, sleep):
    """The end of a replace, once NAME exists with a HIDDEN item per ITEMS entry (scene, the new item's id,
    the old item's index, the old item's visibility): the levels and the program filters, each new item
    at the old index, the old input removed, the items that were visible shown at once (a one-request
    gap), then monitoring + the audio monitor filters (the operator's headphones play whatever the items
    show, so they never overlap the old input). Returns the old input's name when OBS still
    lists it after a bounded wait (a holder keeps it), else None."""
    _restore_levels(obs, name, state)
    _restore_filters(obs, name, state, headphones=False)
    for scene, item_id, index, _enabled in items:
        if item_id is not None and isinstance(index, int):
            obs.req("SetSceneItemIndex", {"sceneName": scene, "sceneItemId": item_id, "sceneItemIndex": index},
                    ignore_err=True)
    obs.req("RemoveInput", {"inputName": aside}, ignore_err=True)
    for scene, item_id, _index, enabled in items:
        if item_id is not None and enabled:
            obs.req("SetSceneItemEnabled", {"sceneName": scene, "sceneItemId": item_id,
                                            "sceneItemEnabled": True}, ignore_err=True)
    if isinstance(state.get("monitor"), str) and state["monitor"]:
        obs.req("SetInputAudioMonitorType", {"inputName": name, "monitorType": state["monitor"]}, ignore_err=True)
    _restore_filters(obs, name, state, headphones=True)
    _restore_filter_order(obs, name, state)
    _await_free(obs, aside, sleep)
    return aside if input_kind_listed(obs, aside) not in ("", None) else None


def input_recreate(obs, name, kind, settings, state, fallback_scene, sleep=None, source=None):
    """Re-create input NAME as KIND with SETTINGS keeping STATE (input_state_read of the old input, read
    BEFORE the call), never removing the old input before the new one exists (issue 1495):

      1. the old input (NAME, or SOURCE: the aside input an interrupted replace left) is renamed aside
         and read back -- a by-name holder of the old source (the av-sync dock) then finds the new input
         or nothing, and lets the old one go;
      2. NAME is created as KIND (input_create_verified: read back, one retry) with a HIDDEN item in the
         first scene that held the old input and in every other one (a hidden item is not mixed: no
         double sound);
      3. _swap_in: the levels, the old indexes, the old input removed, monitoring + filters, the items
         that were visible shown.

    An old input in no scene: the new one is created in FALLBACK_SCENE and its item removed again, so it
    never enters a scene the old one was not in. Returns (True, (scenes kept, the old input's name when
    OBS still lists it after a bounded wait, else None)), or (False, the reason): the old input is then
    never removed, and renamed back to NAME whenever OBS lets it."""
    sleep = sleep or time.sleep
    if source is None or source == name:
        aside = _free_aside_name(obs, name)
        if not _rename(obs, name, aside):
            return False, ("OBS did not rename '%s' to '%s' (SetInputName): nothing removed, kept the old "
                           "input" % (name, aside))
    else:
        aside = source
    scenes = state.get("scenes") or []
    first = scenes[0][0] if scenes else fallback_scene
    resp, listed = input_create_verified(obs, first, name, kind, settings, sleep=sleep, enabled=False)
    if listed != kind:
        why = listed_text(name, listed) + " after two CreateInput"
        if listed == "" and _rename(obs, aside, name):
            return False, why + "; kept the old input (renamed back to '%s', in every scene)" % name
        return False, (why + "; kept the old input as '%s' in every scene -- rename it back to '%s' by hand "
                       "(the next launch seed resumes from it)" % (aside, name))
    items = []   # (scene, the new item's id, the old item's index, the old item's visibility)
    if scenes:
        items.append((first, _item_id(resp), scenes[0][2], scenes[0][1]))
        for scene, enabled, index in scenes[1:]:
            r = obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": name, "sceneItemEnabled": False},
                        ignore_err=True)
            items.append((scene, _item_id(r), index, enabled))
    elif _item_id(resp) is not None:
        obs.req("RemoveSceneItem", {"sceneName": first, "sceneItemId": _item_id(resp)}, ignore_err=True)
    return True, (len(scenes), _swap_in(obs, name, aside, state, items, sleep))


def _scene_item_of(obs, scene, name):
    for it in (obs.req("GetSceneItemList", {"sceneName": scene}, ignore_err=True) or {}).get("sceneItems") or []:
        if it.get("sourceName") == name:
            return it
    return None


def resumable_aside(obs, asides):
    """The aside input an interrupted replace left that still holds scene items (its old input's place
    and state), newest first, or None. A removed aside a holder keeps is listed but in no scene: never a
    resume source (issue 1495 review round 1)."""
    for aside in asides:
        if input_state_read(obs, aside).get("scenes"):
            return aside
    return None


def finish_interrupted_replace(obs, name, aside, sleep=None):
    """NAME already has the wanted kind but ASIDE still holds scene items: the replace died after its
    create (the new items hidden, the state maybe missing). Give NAME an item in every scene ASIDE is in
    (an existing one is hidden first where ASIDE's was hidden), then _swap_in. Returns (scenes kept,
    the lingering aside name or None)."""
    state = input_state_read(obs, aside)
    items = []
    for scene, enabled, index in state.get("scenes") or []:
        it = _scene_item_of(obs, scene, name)
        if it is None:
            r = obs.req("CreateSceneItem", {"sceneName": scene, "sourceName": name, "sceneItemEnabled": False},
                        ignore_err=True)
            item_id = _item_id(r)
        else:
            item_id = _item_id(it)
            if not enabled and it.get("sceneItemEnabled") is not False and item_id is not None:
                obs.req("SetSceneItemEnabled", {"sceneName": scene, "sceneItemId": item_id,
                                                "sceneItemEnabled": False}, ignore_err=True)
        items.append((scene, item_id, index, enabled))
    return len(items), _swap_in(obs, name, aside, state, items, sleep or time.sleep)


def replace_keeping_state(obs, name, kind, settings, fallback_scene, source, source_kind, state_from=None):
    """The launch seed's replace of input NAME as KIND (input_recreate from SOURCE: NAME itself, or the
    aside input of an interrupted replace, of kind SOURCE_KIND), with the state read first from
    STATE_FROM (default SOURCE: an interrupted replace's aside when NAME exists only half made, review
    round 2). Returns the status line: `replaced (...)`, or a loud `replace FAILED (...)`."""
    state = input_state_read(obs, state_from or source)
    ok, info = input_recreate(obs, name, kind, settings, state, fallback_scene, source=source)
    was = "was %s" % (source_kind or "absent")
    if source != name:
        was = "resumed the interrupted replace of '%s', %s" % (source, was)
    if not ok:
        return "replace FAILED (%s): %s -- restart OBS (the launch seed runs again)" % (was, info)
    kept, lingering = info
    return "replaced (%s; kept in %d scene(s))%s" % (was, kept, _lingering_text(lingering))


def _lingering_text(lingering):
    if not lingering:
        return ""
    return (" -- the old input still lingers as '%s' (another holder keeps a reference, e.g. the av-sync "
            "dock until its next lookup)" % lingering)


def settle_aside_inputs(obs, name, asides, finish=True):
    """The aside inputs left beside a program-audio input NAME of the wanted kind: the newest one that
    still holds scene items is FINISHED (finish_interrupted_replace; FINISH False: a replace just ran, so
    every aside is stale), every other one removed. Returns the status suffix naming them ('' when none)."""
    out, finished = "", False
    for aside in asides:
        if finish and not finished and input_state_read(obs, aside).get("scenes"):
            kept, lingering = finish_interrupted_replace(obs, name, aside)
            finished = True
            out += " (finished the interrupted replace of '%s': kept in %d scene(s))%s" % (
                aside, kept, _lingering_text(lingering))
            continue
        obs.req("RemoveInput", {"inputName": aside}, ignore_err=True)
        out += " (removed the leftover '%s' of an interrupted replace)" % aside
    return out


# --- issue 1465 item 3: no pulse capture input on the stream role ----------------------------------------
# The stream operator's headphones are the locked PipeWire's one sink (scripts/stream_operator_audio.py):
# VLC, Chrome and YouTube play there. A pulse capture input in the stream OBS could capture that operator
# mix (or the leftover strih-program-source, `Zvuk z mixu` on stream-snv) into the program, so on the
# stream role (the seed manifest's `"pulse_capture": "forbid"`) the launch seed removes every SCENE pulse
# capture input and names each one with the device it captured (the operator can rebuild it from the log).
# The program-audio input is never touched here (its kind is the obs-vban seed's job, graded by verify item
# 9), and a global audio channel (Settings > Audio: Desktop / Mic) is the frontend's: obs-websocket's
# RemoveInput would not release the channel, so it is only NAMED, and verify FAILs it.
PULSE_CAPTURE_KINDS = ("pulse_input_capture", "pulse_output_capture")
SPECIAL_INPUT_KEYS = ("desktop1", "desktop2", "mic1", "mic2", "mic3", "mic4")


def parse_pulse_capture_rule(text):
    """The manifest's `pulse_capture` rule: "forbid" (the stream role), else None. Lenient, never raises."""
    try:
        d = json.loads(text)
    except ValueError:
        return None
    v = d.get("pulse_capture") if isinstance(d, dict) else None
    return "forbid" if v == "forbid" else None


def _special_input_names(obs):
    sp = obs.req("GetSpecialInputs", ignore_err=True) or {}
    return {sp.get(k) for k in SPECIAL_INPUT_KEYS if isinstance(sp.get(k), str) and sp.get(k)}


def pulse_capture_inputs(obs):
    """[(name, kind, "scene" | "global channel")] of every pulse capture input OBS lists, by name."""
    inputs = (obs.req("GetInputList", ignore_err=True) or {}).get("inputs") or []
    special = _special_input_names(obs)
    out = []
    for i in inputs:
        if isinstance(i, dict) and i.get("inputKind") in PULSE_CAPTURE_KINDS and i.get("inputName"):
            name = i["inputName"]
            out.append((name, i["inputKind"], "global channel" if name in special else "scene"))
    return sorted(out)


def pulse_capture_report(obs):
    """verify's read-only line: `pulse capture inputs: none` or each one with its kind and place."""
    found = pulse_capture_inputs(obs)
    if not found:
        return "pulse capture inputs: none"
    return "pulse capture inputs: " + "; ".join("'%s' (%s, %s)" % f for f in found)


def _remove_scene_items_of(obs, name):
    """Remove every scene item that shows input NAME, in every scene (issue 1462). libobs drops a
    removed source's items only while their scene RENDERS, and in Studio Mode the program renders a
    private duplicate: a scene that is never shown on its own keeps the item, the item keeps the input,
    and RemoveInput alone changes nothing (stream-snv's `Development`, 24.10.2026)."""
    scenes = (obs.req("GetSceneList", ignore_err=True) or {}).get("scenes") or []
    for sc in scenes:
        scene = sc.get("sceneName") if isinstance(sc, dict) else None
        if not scene:
            continue
        items = (obs.req("GetSceneItemList", {"sceneName": scene}, ignore_err=True) or {}).get("sceneItems") or []
        for it in items:
            if isinstance(it, dict) and it.get("sourceName") == name and it.get("sceneItemId") is not None:
                obs.req("RemoveSceneItem", {"sceneName": scene, "sceneItemId": it["sceneItemId"]},
                        ignore_err=True)


def drop_pulse_capture_inputs(obs, keep=None, sleep=None):
    """Remove every scene pulse capture input except KEEP (the program-audio input), each read back from
    GetInputList (a bounded wait, the name can stay listed a moment); name every global channel of a pulse
    kind instead of removing it. Returns the status for the seed line."""
    sleep = sleep or time.sleep
    parts = []
    for name, kind, place in pulse_capture_inputs(obs):
        if name == keep:
            continue
        if place != "scene":
            parts.append("global channel '%s' (%s) NOT removed -- set it Disabled in OBS Settings > Audio"
                         % (name, kind))
            continue
        settings = (obs.req("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings")
        dev = settings.get("device_id") if isinstance(settings, dict) else None
        _remove_scene_items_of(obs, name)
        obs.req("RemoveInput", {"inputName": name}, ignore_err=True)
        _await_free(obs, name, sleep)
        listed = input_kind_listed(obs, name)
        if listed == "":
            parts.append("removed '%s' (%s on %s)" % (name, kind, dev or "the default device"))
        else:
            parts.append("remove FAILED: '%s' (%s) -- restart OBS, the launch seed runs again"
                         % (name, listed_text(name, listed)))
    return "; ".join(parts) or "no pulse capture input"


# --- issue 1440: the browser sources' page audio, rerouted into OBS and muted -----------------------------
# On a box without a sound server (the strih role, issue 1440) a browser source with `reroute_audio` off
# plays its page sound to the default device (the cutters' cans on strih-snv, the MG-XU cans on
# strih-pp). With `reroute_audio` on, obs-browser hands that sound to OBS as the source's own audio, and
# the source muted in the mixer keeps it off every bus. The seed (strih_scenes.py --bootstrap, the strih
# manifest's `browser_audio: reroute-mute`) sets both, verify-strih reads them back (--browser-audio).
BROWSER_SOURCE_KIND = "browser_source"
BROWSER_AUDIO_RULE = "reroute-mute"


def parse_browser_audio_rule(text):
    """The manifest's `browser_audio` rule: "reroute-mute" (the strih role), else None. Lenient, never raises."""
    try:
        d = json.loads(text)
    except ValueError:
        return None
    v = d.get("browser_audio") if isinstance(d, dict) else None
    return BROWSER_AUDIO_RULE if v == BROWSER_AUDIO_RULE else None


def browser_sources(obs):
    """[(name, reroute_audio, muted)] of every browser source OBS lists; a value OBS did not answer is None."""
    inputs = (obs.req("GetInputList", {"inputKind": BROWSER_SOURCE_KIND}, ignore_err=True) or {}).get("inputs") or []
    out = []
    for i in inputs:
        if not isinstance(i, dict) or i.get("inputKind") != BROWSER_SOURCE_KIND or not i.get("inputName"):
            continue
        name = i["inputName"]
        st = (obs.req("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings")
        # obs-browser's default is reroute_audio off, and GetInputSettings lists only the keys set.
        reroute = (st.get("reroute_audio") is True) if isinstance(st, dict) else None
        muted = (obs.req("GetInputMute", {"inputName": name}, ignore_err=True) or {}).get("inputMuted")
        out.append((name, reroute, muted if isinstance(muted, bool) else None))
    return out


def browser_audio_problems(entries):
    """Pure: the problems of [(name, reroute_audio, muted)]: `'<name>' reroute_audio off` / `'<name>' not
    muted` (`unreadable` when OBS did not answer)."""
    problems = []
    for name, reroute, muted in entries:
        if reroute is not True:
            problems.append("'%s' reroute_audio %s" % (name, "unreadable" if reroute is None else "off"))
        if muted is not True:
            problems.append("'%s' %s" % (name, "mute unreadable" if muted is None else "not muted"))
    return problems


def _browser_audio_ok_text(n):
    return "%d browser source(s) reroute their audio into OBS, muted" % n


def mute_browser_sources(obs):
    """Set reroute_audio + mute on every browser source that lacks either (nothing else touched), read both
    back. Returns the status for the seed line: the count, the sources changed, or `FAILED: <read-back>`."""
    changed = []
    for name, reroute, muted in browser_sources(obs):
        what = []
        if reroute is not True:
            obs.req("SetInputSettings", {"inputName": name, "inputSettings": {"reroute_audio": True},
                                         "overlay": True}, ignore_err=True)
            what.append("reroute_audio on")
        if muted is not True:
            obs.req("SetInputMute", {"inputName": name, "inputMuted": True}, ignore_err=True)
            what.append("muted")
        if what:
            changed.append("'%s' %s" % (name, " + ".join(what)))
    after = browser_sources(obs)
    problems = browser_audio_problems(after)
    if problems:
        return "FAILED: %s -- restart OBS, the launch seed runs again" % "; ".join(problems)
    text = _browser_audio_ok_text(len(after))
    return text + (" (set: %s)" % ", ".join(changed) if changed else "")


def browser_audio_report(obs):
    """verify's read-only line: `browser audio: OK (...)` or the problems."""
    found = browser_sources(obs)
    if not found:
        return "browser audio: OK (no browser source)"
    problems = browser_audio_problems(found)
    if problems:
        return "browser audio: " + "; ".join(problems)
    return "browser audio: OK (%s)" % _browser_audio_ok_text(len(found))


# issue 1436 (owner 25.10.2026): on the Windows stream box the operator heard the program audio through
# exeldro's Audio Monitor plugin on its own fader. The stream manifest names the pulse sink in
# `program_audio_monitor_filter`; the seed adds ONE such filter to the program input when it has no
# `audio_monitor` filter yet (an operator's own filter and its settings always win, never rewritten).
AUDIO_MONITOR_FILTER_KIND = "audio_monitor"
AUDIO_MONITOR_FILTER_NAME = "Sluchadla operatora"


def parse_program_audio_monitor_filter(text):
    """issue 1436: the pulse sink of the program input's Audio Monitor filter from the manifest, or None
    (the strih role, an older manifest, a non-JSON text). Lenient, never raises."""
    try:
        d = json.loads(text)
    except (TypeError, ValueError):
        return None
    v = d.get("program_audio_monitor_filter") if isinstance(d, dict) else None
    return v if isinstance(v, str) and v else None


def audio_monitor_filter_settings(device):
    """issue 1436: the seeded filter's settings: the pulse sink by id and name, full volume, unmuted."""
    return {"device": device, "deviceName": device, "volume": 100.0}


def seed_audio_monitor_filter(obs, name, device):
    """issue 1436: add the Audio Monitor filter to input NAME playing to DEVICE, unless NAME already has an
    `audio_monitor` filter (kept as it is). Returns a status word; never raises."""
    got = obs.req("GetSourceFilterList", {"sourceName": name}, ignore_err=True) or {}
    filters = got.get("filters")
    if not isinstance(filters, list):
        return "untouched: the filters of '%s' are unreadable" % name
    for f in filters:
        if isinstance(f, dict) and f.get("filterKind") == AUDIO_MONITOR_FILTER_KIND:
            return "kept: '%s' already has the Audio Monitor filter '%s'" % (name, f.get("filterName"))
    obs.req("CreateSourceFilter", {"sourceName": name, "filterName": AUDIO_MONITOR_FILTER_NAME,
                                   "filterKind": AUDIO_MONITOR_FILTER_KIND,
                                   "filterSettings": audio_monitor_filter_settings(device)}, ignore_err=True)
    back = obs.req("GetSourceFilter", {"sourceName": name, "filterName": AUDIO_MONITOR_FILTER_NAME},
                   ignore_err=True) or {}
    if back.get("filterKind") != AUDIO_MONITOR_FILTER_KIND:
        return "create FAILED: OBS lists no '%s' filter on '%s' (the Audio Monitor plugin not loaded? issue 1436)" \
            % (AUDIO_MONITOR_FILTER_NAME, name)
    return "added '%s' -> %s" % (AUDIO_MONITOR_FILTER_NAME, device)
