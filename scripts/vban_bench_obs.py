#!/usr/bin/env python3
"""Issue 1460 slice 1 -- the OBS side of the VBAN receiver bench (scripts/vban-receiver-bench.sh).

Every OBS request goes through obs_phase2.py's ONE websocket client (`_conn` / `_rpc`, with its
production-scene guard); this file owns only what the bench does with it:

  preflight  READ-ONLY. Refuses (exit 4) when the bench cannot run safely: the port is one a real
             receiver listens on (vban_emulator_plan.port_refusal, the ONE list); the obs-vban input
             kind is not loaded; the track is the program track 1, the stream track (AdvOut
             TrackIndex) or outside 2..6; an earlier run's input is still there while RecTracks
             already holds the track (its original value is unknown: finish that run with
             --cleanup); the output mode is not Advanced / the recording is not Standard / it
             records FLV (one track only); a recording runs; another vban input already listens on
             the port; the input name is taken by an input that is not an earlier bench run's; the
             program scene or RecTracks cannot be read. Reports (never refuses) every input that
             feeds the test track, so a polluted track is named before the run (the analysis then
             FAILs a track that is not clean).
  setup      creates the TEMPORARY input -- `net.nagater.obs-vban.source` on its own port + stream
             name -- as a scene item of the current PROGRAM scene (libobs mixes only a source in an
             output mix's active tree; anything else is rendered for meters only), routes it to the
             dedicated track ONLY (read back), sets monitoring off (read back), ORs the track into
             AdvOut RecTracks (read back; AdvancedOutput::SetupRecording re-reads it at every
             StartRecord), and makes it ACTIVE: in Studio Mode with scene duplication the program is
             a private copy, so the same scene is re-programmed once (OBS re-duplicates it) and the
             preview scene the swap may change is put back. Every step is written to the state file
             BEFORE it runs, so `teardown` undoes exactly what happened. The input is silent until
             the emulator sends to its port, so nothing reaches a track before the routing is read
             back.
  teardown   removes the input (only one whose kind, port and stream name are the bench's),
             restores RecTracks (only while it still holds the value setup wrote) and, after a setup
             that failed between the re-program and its own preview restore, the operator's preview
             scene -- only while the preview still shows OBS's swap (a scene staged since is never
             touched). Idempotent.

The pilot this input receives must never be heard: track 1 (OBS mix 0: the program, the NDI
output, the HDMI output audio) and the stream track are refused, and monitoring is off.

Pure helpers (`tracks_payload`, `rectracks_with`, `audio_stream_index`, `preflight_reasons`,
`matches_bench_input`, `input_settings`) and the three operations take an injected `rpc`
(rtype, rdata=None, ignore_err=False) -> responseData, so the Tier-0 tests drive them with a fake OBS
(tests/python/test_vban_bench_obs_1460.py).

Usage: vban_bench_obs.py {preflight|setup|teardown} --host H [--password P] --state FILE
          [--input-name N] [--port P] [--stream-name S] [--track T] [--extra-setting KEY=VALUE ...]
Exit: 0 ok, 2 OBS unreachable / unreadable, 3 usage, 4 refused before any change (preflight),
5 failed after a change (setup / teardown: the state file says what is left).
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import vban_emulator_plan as vp  # noqa: E402  (the ONE refused-port list)

VBAN_INPUT_KIND = "net.nagater.obs-vban.source"
MONITOR_OFF = "OBS_MONITORING_TYPE_NONE"
TRACKS = range(1, 7)
PROGRAM_TRACK = 1
ACTIVE_WAIT_S = 10.0
ACTIVE_POLL_S = 0.25


class BenchError(RuntimeError):
    """A bench step failed; the state file holds what was done."""


def tracks_payload(track):
    """SetInputAudioTracks body: the ONE dedicated track on, every other off."""
    return {str(i): i == track for i in TRACKS}


def rectracks_with(rectracks, track):
    return int(rectracks) | (1 << (track - 1))


def audio_stream_index(rectracks, track):
    """The 0-based audio stream of TRACK in a recording made with RECTRACKS (OBS writes the selected
    tracks in ascending order)."""
    if not rectracks & (1 << (track - 1)):
        raise ValueError("track %d is not in RecTracks %d" % (track, rectracks))
    return bin(rectracks & ((1 << (track - 1)) - 1)).count("1")


BENCH_KEYS = ("port", "stream_name", "ip_from")


def input_settings(port, stream_name, extra=None):
    """The test input's settings. An extra setting may add a key (the issue-1439 receive reserve),
    never override the bench's own: the refused ports (`preflight`, which `setup` always runs first)
    are checked on `port` alone."""
    clash = sorted(set(extra or {}) & set(BENCH_KEYS))
    if clash:
        raise ValueError("--extra-setting may not set %s (the bench's own keys)" % ", ".join(clash))
    out = {"port": int(port), "stream_name": stream_name, "ip_from": ""}
    out.update(extra or {})
    return out


def matches_bench_input(entry_kind, settings, port, stream_name):
    """True when an existing input is an earlier bench run's (kind + port + stream name)."""
    return (entry_kind == VBAN_INPUT_KIND and _int_or_none((settings or {}).get("port")) == int(port)
            and (settings or {}).get("stream_name") == stream_name)


def preflight_reasons(f):
    """The refusals of one preflight read (`f` = the facts dict `preflight` builds). Pure."""
    r = []
    if f.get("port_refusal"):
        r.append(f["port_refusal"])
    if not f.get("kind_available"):
        r.append("the obs-vban input kind %s is not loaded in this OBS" % VBAN_INPUT_KIND)
    t = f.get("track")
    if not isinstance(t, int) or t not in TRACKS or t == PROGRAM_TRACK:
        r.append("track %r: the test track must be 2..6 (track 1 is the program mix)" % (t,))
    if f.get("stream_track") is not None and f.get("stream_track") == t:
        r.append("track %d is the stream track (AdvOut TrackIndex)" % t)
    if f.get("output_mode") != "Advanced":
        r.append("the output mode is %r, not Advanced (RecTracks applies only there)" % f.get("output_mode"))
    if f.get("rec_type") not in (None, "Standard"):
        r.append("the recording type is %r, not Standard" % f.get("rec_type"))
    if f.get("rec_format") == "flv":
        r.append("the recording format is flv (one audio track only)")
    if f.get("rectracks") is None:
        r.append("AdvOut RecTracks is unreadable")
    if f.get("recording"):
        r.append("a recording is running")
    if not f.get("program_scene"):
        r.append("the program scene is unreadable")
    elif f.get("program_scene") in (f.get("never_program") or ()):
        # issue 1380: the setup re-programs the CURRENT program scene; a production scene our tooling
        # never programs is refused here, before the input is created in it.
        r.append("the program scene %r is a production scene our tooling never programs" % f["program_scene"])
    for name in f.get("port_users") or []:
        r.append("the vban input %r already listens on port %s" % (name, f.get("port")))
    if f.get("name_taken"):
        r.append("an input named %r exists and is not an earlier bench run's" % f.get("input_name"))
    rt = f.get("rectracks")
    if f.get("leftover_input") and isinstance(rt, int) and isinstance(t, int) and t in TRACKS \
            and rt & (1 << (t - 1)):
        r.append("an earlier bench run's input is still there and RecTracks (%d) already holds track %d: "
                 "its original value is unknown -- finish that run with `vban-receiver-bench.sh --cleanup "
                 "<its run dir>` (or remove the input and restore RecTracks by hand)" % (rt, t))
    return r


def _param(rpc, cat, name):
    d = rpc("GetProfileParameter", {"parameterCategory": cat, "parameterName": name}, ignore_err=True) or {}
    v = d.get("parameterValue")
    return v if v not in (None, "") else d.get("defaultParameterValue")


def _int_or_none(v):
    try:
        return int(v)
    except (TypeError, ValueError):
        return None


def preflight(rpc, input_name, port, stream_name, track):
    """Read-only facts + refusals (module doc)."""
    kinds = (rpc("GetInputKindList", {"unversioned": False}, ignore_err=True) or {}).get("inputKinds") or []
    inputs = (rpc("GetInputList", ignore_err=True) or {}).get("inputs") or []
    port_users, feeders, name_taken, leftover = [], [], False, False
    for i in inputs:
        name, kind = i.get("inputName"), i.get("inputKind")
        settings = None
        if kind == VBAN_INPUT_KIND:
            settings = (rpc("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings") or {}
        if name == input_name:
            if matches_bench_input(kind, settings, port, stream_name):
                leftover = True
            else:
                name_taken = True
            continue
        if kind == VBAN_INPUT_KIND and _int_or_none((settings or {}).get("port")) == int(port):
            port_users.append(name)
        tr = (rpc("GetInputAudioTracks", {"inputName": name}, ignore_err=True) or {}).get("inputAudioTracks") or {}
        if tr.get(str(track)):
            feeders.append(name)
    studio = bool((rpc("GetStudioModeEnabled", ignore_err=True) or {}).get("studioModeEnabled"))
    prog = (rpc("GetCurrentProgramScene", ignore_err=True) or {}).get("currentProgramSceneName")
    prev = (rpc("GetCurrentPreviewScene", ignore_err=True) or {}).get("currentPreviewSceneName") if studio else None
    rec = rpc("GetRecordStatus", ignore_err=True) or {}
    facts = {
        "input_name": input_name, "port": int(port), "stream_name": stream_name, "track": track,
        "port_refusal": vp.port_refusal(int(port)),
        "kind_available": VBAN_INPUT_KIND in kinds,
        "output_mode": _param(rpc, "Output", "Mode"),
        "rec_type": _param(rpc, "AdvOut", "RecType"),
        "rec_format": _param(rpc, "AdvOut", "RecFormat2"),
        "rectracks": _int_or_none(_param(rpc, "AdvOut", "RecTracks")),
        "stream_track": _int_or_none(_param(rpc, "AdvOut", "TrackIndex")),
        "recording": bool(rec.get("outputActive", True)),
        "studio_mode": studio, "program_scene": prog, "preview_scene": prev,
        "never_program": _never_program_scenes(),
        "port_users": port_users, "track_feeders": feeders,
        "name_taken": name_taken, "leftover_input": leftover,
    }
    facts["reasons"] = preflight_reasons(facts)
    return facts


def _save(state_path, state):
    tmp = state_path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(state, fh, indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, state_path)


def _active(rpc, name):
    return bool((rpc("GetSourceActive", {"sourceName": name}, ignore_err=True) or {}).get("videoActive"))


def setup(rpc, state_path, input_name, port, stream_name, track, extra=None,
          sleep=time.sleep, monotonic=time.monotonic):
    """Create + route + activate the test input (module doc). Returns the state; raises BenchError
    after a change (the state file is current), ValueError when the preflight refuses (no change)."""
    facts = preflight(rpc, input_name, port, stream_name, track)
    if facts["reasons"]:
        raise ValueError("; ".join(facts["reasons"]))
    settings = input_settings(port, stream_name, extra)  # a bad value refuses here, before any change
    state = {"input_name": input_name, "port": int(port), "stream_name": stream_name, "track": track,
             "program_scene": facts["program_scene"], "preview_scene": facts["preview_scene"],
             "studio_mode": facts["studio_mode"], "steps": []}
    try:
        return _setup_changes(rpc, state_path, state, facts, settings, track, sleep, monotonic)
    except BenchError:
        raise
    except Exception as e:  # noqa: BLE001 -- after the first change every failure is "failed after a change"
        raise BenchError("%s: %s" % (type(e).__name__, e)) from e


def _setup_changes(rpc, state_path, state, facts, settings, track, sleep, monotonic):
    """The mutating half of `setup` (every step saved to the state file before it runs)."""
    input_name = state["input_name"]
    if facts["leftover_input"]:
        state["steps"].append("remove-leftover")
        _save(state_path, state)
        rpc("RemoveInput", {"inputName": input_name})
    state["created"] = True
    state["steps"].append("create")
    _save(state_path, state)
    rpc("CreateInput", {"sceneName": facts["program_scene"], "inputName": input_name,
                        "inputKind": VBAN_INPUT_KIND, "inputSettings": settings,
                        "sceneItemEnabled": True})
    want = tracks_payload(track)
    rpc("SetInputAudioTracks", {"inputName": input_name, "inputAudioTracks": want})
    got = (rpc("GetInputAudioTracks", {"inputName": input_name}) or {}).get("inputAudioTracks") or {}
    if {k: bool(got.get(k)) for k in want} != want:
        raise BenchError("the test input's tracks read back %r, want only track %d" % (got, track))
    rpc("SetInputAudioMonitorType", {"inputName": input_name, "monitorType": MONITOR_OFF})
    mon = (rpc("GetInputAudioMonitorType", {"inputName": input_name}) or {}).get("monitorType")
    if mon != MONITOR_OFF:
        raise BenchError("the test input's monitoring reads back %r, want %s" % (mon, MONITOR_OFF))
    orig = facts["rectracks"]
    new = rectracks_with(orig, track)
    state.update({"rectracks_orig": orig, "rectracks_set": new, "audio_stream_index": audio_stream_index(new, track)})
    state["steps"].append("rectracks")
    _save(state_path, state)
    if new != orig:
        rpc("SetProfileParameter", {"parameterCategory": "AdvOut", "parameterName": "RecTracks",
                                    "parameterValue": str(new)})
        if _int_or_none(_param(rpc, "AdvOut", "RecTracks")) != new:
            raise BenchError("AdvOut RecTracks did not read back %d" % new)
    if not _active(rpc, input_name):
        state["steps"].append("reprogram")
        _save(state_path, state)
        rpc("SetCurrentProgramScene", {"sceneName": facts["program_scene"]})
        deadline = monotonic() + ACTIVE_WAIT_S
        while not _active(rpc, input_name):
            if monotonic() >= deadline:
                raise BenchError("the test input is not active %.0f s after re-programming %r"
                                 % (ACTIVE_WAIT_S, facts["program_scene"]))
            sleep(ACTIVE_POLL_S)
        if facts["studio_mode"] and facts["preview_scene"]:
            cur = (rpc("GetCurrentPreviewScene", ignore_err=True) or {}).get("currentPreviewSceneName")
            if cur != facts["preview_scene"]:
                rpc("SetCurrentPreviewScene", {"sceneName": facts["preview_scene"]})
                state["preview_restored"] = True
    state["active"] = True
    state["steps"].append("active")
    _save(state_path, state)
    return state


def teardown(rpc, state):
    """Undo what `state` says setup did. Returns {ok, actions, left}."""
    actions, left = [], []
    name = state.get("input_name")
    if state.get("created") or "remove-leftover" in (state.get("steps") or []):
        inputs = (rpc("GetInputList", ignore_err=True) or {}).get("inputs")
        if inputs is None:
            left.append("the input list is unreadable: %r may still exist" % name)
        else:
            hit = [i for i in inputs if i.get("inputName") == name]
            if hit:
                settings = (rpc("GetInputSettings", {"inputName": name}, ignore_err=True) or {}).get("inputSettings")
                if matches_bench_input(hit[0].get("inputKind"), settings, state.get("port"), state.get("stream_name")):
                    rpc("RemoveInput", {"inputName": name}, ignore_err=True)
                    still = [i for i in (rpc("GetInputList", ignore_err=True) or {}).get("inputs") or []
                             if i.get("inputName") == name]
                    (left if still else actions).append(
                        ("the input %r is still there after RemoveInput" if still else "removed the input %r") % name)
                else:
                    left.append("an input named %r exists but is not the bench's -- left alone" % name)
    if "rectracks_set" in state and state.get("rectracks_set") != state.get("rectracks_orig"):
        cur = _int_or_none(_param(rpc, "AdvOut", "RecTracks"))
        orig, setv = state["rectracks_orig"], state["rectracks_set"]
        if cur == setv:
            rpc("SetProfileParameter", {"parameterCategory": "AdvOut", "parameterName": "RecTracks",
                                        "parameterValue": str(orig)}, ignore_err=True)
            back = _int_or_none(_param(rpc, "AdvOut", "RecTracks"))
            (actions if back == orig else left).append(
                "RecTracks restored to %d" % orig if back == orig else "RecTracks reads %r after restoring %d" % (back, orig))
        elif cur != orig:
            left.append("RecTracks is %r, neither the bench's %d nor the original %d -- left alone" % (cur, setv, orig))
    steps = state.get("steps") or []
    prev = state.get("preview_scene")
    if "reprogram" in steps and "active" not in steps and state.get("studio_mode") and prev:
        # setup died between its re-program and its own preview restore. Undo only OBS's swap (the
        # re-programmed scene moved into the preview); a preview the operator staged since -- maybe
        # hours later, during a show, when --cleanup runs -- is theirs and never touched.
        cur = (rpc("GetCurrentPreviewScene", ignore_err=True) or {}).get("currentPreviewSceneName")
        if cur != prev and cur == state.get("program_scene"):
            rpc("SetCurrentPreviewScene", {"sceneName": prev}, ignore_err=True)
            back = (rpc("GetCurrentPreviewScene", ignore_err=True) or {}).get("currentPreviewSceneName")
            (actions if back == prev else left).append(
                "preview restored to %r" % prev if back == prev else "the preview reads %r, not %r" % (back, prev))
        elif cur != prev:
            actions.append("the preview reads %r, staged since the setup: the operator's, not touched" % cur)
    return {"ok": not left, "actions": actions, "left": left}


def _never_program_scenes():
    """obs_phase2's ONE list of the scenes our tooling never programs (issue 1380), lazily imported."""
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import obs_phase2  # noqa: E402
    return sorted(obs_phase2.NEVER_PROGRAM_SCENES)


def _rpc_for(host, password):
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import obs_phase2  # noqa: E402  (lazy: the shared client + its production-scene guard)
    ws = obs_phase2._conn(host, password)

    def rpc(rtype, rdata=None, ignore_err=False):
        return obs_phase2._rpc(ws, rtype, rdata, ignore_err=ignore_err)
    return ws, rpc


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("op", choices=("preflight", "setup", "teardown"))
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("--state", required=True)
    ap.add_argument("--input-name", default="VBAN bench (issue 1460)")
    ap.add_argument("--port", type=int, default=6995)
    ap.add_argument("--stream-name", default="bench1460")
    ap.add_argument("--track", type=int, default=6)
    ap.add_argument("--extra-setting", action="append", default=[])
    a = ap.parse_args(argv)
    extra = {}
    for kv in a.extra_setting:
        k, sep, v = kv.partition("=")
        if not sep or not k:
            print("vban-bench-obs: --extra-setting %r is not KEY=VALUE" % kv, file=sys.stderr)
            return 3
        extra[k] = int(v) if v.lstrip("-").isdigit() else v
    try:
        ws, rpc = _rpc_for(a.host, a.password)
    except Exception as e:  # noqa: BLE001 -- any connect failure is "unreachable", named
        print("vban-bench-obs: OBS %s unreachable: %s" % (a.host, e), file=sys.stderr)
        return 2
    try:
        if a.op == "preflight":
            facts = preflight(rpc, a.input_name, a.port, a.stream_name, a.track)
            print(json.dumps(facts, sort_keys=True))
            return 4 if facts["reasons"] else 0
        if a.op == "setup":
            try:
                state = setup(rpc, a.state, a.input_name, a.port, a.stream_name, a.track, extra)
            except ValueError as e:
                print("vban-bench-obs: refused: %s" % e, file=sys.stderr)
                return 4
            except BenchError as e:
                print("vban-bench-obs: setup FAILED after a change: %s" % e, file=sys.stderr)
                return 5
            print(json.dumps(state, sort_keys=True))
            return 0
        try:
            with open(a.state) as fh:
                state = json.load(fh)
        except (OSError, ValueError):
            print("vban-bench-obs: no setup state in %s -- nothing to tear down" % a.state, file=sys.stderr)
            return 0
        res = teardown(rpc, state)
        print(json.dumps(res, sort_keys=True))
        return 0 if res["ok"] else 5
    finally:
        ws.close()


if __name__ == "__main__":
    sys.exit(main())
