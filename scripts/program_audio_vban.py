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
