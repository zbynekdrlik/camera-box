#!/usr/bin/env python3
"""Issue 1404 -- the stream OBS side of the full-path E2E's YouTube stage.

The stream OBS (stream-snv) publishes its program to restreamer's inpoint
(`rtmp://127.0.0.1:1234/live`, its configured stream service); restreamer's av-gate session carries
that to YouTube. This tool only starts / stops that output and reads when the recording began:

  youtube_leg_obs.py --host H [--password P] stream-start   -> {"publish_utc": t}
  youtube_leg_obs.py --host H [--password P] stream-stop    -> {"unpublish_utc": t, "was_active": b}
  youtube_leg_obs.py --host H [--password P] record-start   -> {"record_start_utc": t}

exit 0 OK, 2 refused / failed / timed out (one JSON object on stdout either way, `reason` on a
failure). It goes through obs_phase2's client (`_conn` / `_rpc`, so its bounded requests and its
production-scene guard); it never selects a scene. stream-start REFUSES when the box is already
streaming: that stream is not ours (a production broadcast, restreamer's CI), so we never take it
over or stop it. record-start = now - GetRecordStatus.outputDuration (obs-websocket's frame count x
frame time, so it can only read late by the lagged frames; the E2E windows keep seconds of margin).
"""
import argparse
import json
import os
import sys
import time

_HERE = os.path.dirname(os.path.abspath(__file__))
if _HERE not in sys.path:
    sys.path.insert(0, _HERE)

EXIT_OK, EXIT_FAIL = 0, 2
START_DEADLINE_S, STOP_DEADLINE_S, POLL_S = 20.0, 30.0, 0.5


class Refused(Exception):
    def __init__(self, reason, **extra):
        super().__init__(reason)
        self.extra = extra


def _active(rpc):
    return bool((rpc("GetStreamStatus") or {}).get("outputActive", False))


def stream_start(rpc, now=time.time, clock=time.monotonic, sleep=time.sleep):
    if _active(rpc):
        raise Refused("the stream OBS is already streaming before the YouTube stage started: not ours, "
                      "never taken over", foreign=True)
    rpc("StartStream")
    end = clock() + START_DEADLINE_S
    while clock() < end:
        if _active(rpc):
            return {"publish_utc": now()}
        sleep(POLL_S)
    raise Refused(f"StartStream sent but the stream output was not active within {START_DEADLINE_S:.0f} s")


def stream_stop(rpc, now=time.time, clock=time.monotonic, sleep=time.sleep):
    if not _active(rpc):
        return {"unpublish_utc": now(), "was_active": False}
    rpc("StopStream", ignore_err=True)
    end = clock() + STOP_DEADLINE_S
    while clock() < end:
        if not _active(rpc):
            return {"unpublish_utc": now(), "was_active": True}
        sleep(POLL_S)
    raise Refused(f"StopStream sent but the stream output is still active after {STOP_DEADLINE_S:.0f} s")


def record_start(rpc, now=time.time):
    st = rpc("GetRecordStatus") or {}
    if not st.get("outputActive", False):
        raise Refused("the stream OBS is not recording")
    dur = st.get("outputDuration")
    if not isinstance(dur, (int, float)) or dur < 0:
        raise Refused(f"GetRecordStatus has no usable outputDuration ({dur!r})")
    return {"record_start_utc": now() - dur / 1000.0}


ACTIONS = {"stream-start": stream_start, "stream-stop": stream_stop, "record-start": record_start}


def main(argv=None):
    ap = argparse.ArgumentParser(description="issue 1404: stream OBS output for the YouTube stage")
    ap.add_argument("--host", required=True)
    ap.add_argument("--password", default=os.environ.get("OBS_PASSWORD", ""))
    ap.add_argument("action", choices=sorted(ACTIONS))
    a = ap.parse_args(argv)
    import obs_phase2  # lazy: obs_phase2 needs the websocket client, the tests inject `rpc`

    try:
        ws = obs_phase2._conn(a.host, a.password)
    except Exception as e:  # noqa: BLE001 -- an unreachable OBS is a failed step, never a crash
        print(json.dumps({"ok": False, "reason": f"stream OBS {a.host} unreachable: {type(e).__name__}: {e}"}))
        return EXIT_FAIL
    try:
        out = ACTIONS[a.action](lambda rtype, rdata=None, ignore_err=False:
                                obs_phase2._rpc(ws, rtype, rdata, ignore_err=ignore_err))
    except Refused as e:
        print(json.dumps(dict(e.extra, ok=False, reason=str(e))))
        return EXIT_FAIL
    except Exception as e:  # noqa: BLE001 -- a failed request is a failed step
        print(json.dumps({"ok": False, "reason": f"{a.action} on {a.host}: {type(e).__name__}: {e}"}))
        return EXIT_FAIL
    finally:
        ws.close()
    print(json.dumps(dict(out, ok=True)))
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
