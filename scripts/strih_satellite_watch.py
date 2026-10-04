#!/usr/bin/env python3
"""strih-satellite-watch (issue 1399): restart Companion Satellite when its own REST API shows a fault.

WHY (owner, 4.10.2026: "streamdeck tam nejako crashol teraz na strih"): the strih Stream Deck XL is driven
by Companion Satellite (/opt/companion-satellite/companion-satellite, companion-satellite.service). The
unit's Restart=always brings back a Satellite that EXITS; a Satellite that keeps running while it no
longer works leaves the deck dead until someone notices.

WHAT (design comment 5979157737, Approach 1): ONE pass per run; strih-satellite-watch.timer runs it every
30 s (the oneshot strih-satellite-watch.service). A pass
  * reads the Satellite's local REST (127.0.0.1:9999): `/api/status` (`connected`), `/api/surfaces` (the
    open surfaces) and `/api/config` (the Companion host:port it connects to, tcp);
  * TCP-probes that Companion host:port, and reads whether the Stream Deck is on USB (sysfs
    idVendor:idProduct, what lsusb reads; default 0fd9:008f, the strih XL -- never the whole Elgato
    vendor, which also makes capture devices);
  * restarts companion-satellite.service (`systemctl --user --no-block try-restart`) when, SUSTAINED for
    60 s, either
      - not-connected: `connected` is false while the Companion port answers; or
      - no-surfaces: the surfaces list is empty while the Stream Deck is on USB.
    It never restarts while Companion itself is down (or its target is unknown), and a REST that does
    not answer is no information. The decision is the pure `decide()`, pytest-tested as tables.
  * keeps the fault start times (boot clock, so no wall-clock step fakes a window) in a /run state file
    between runs; `--check-state` is verify-strih's "the watch runs" read.

THE HONEST LIMIT: the 4.10.2026 fault had NO REST signature (connected=true, the XL listed, while the
deck did not respond). This watch catches only the classes the REST shows; a hang it cannot see still
needs a report.

Logs: one line when the condition changes (a fault starts, Companion stops answering, the REST goes
silent, all healthy again) and one per restart; a quiet pass logs nothing. The unit keeps only these
lines (LogLevelMax=notice drops the manager's every-30-s Starting/Finished). Std-only.
"""
import argparse
import json
import os
import re
import socket
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass
from http.client import HTTPException
from typing import Optional

REST_URL = "http://127.0.0.1:9999"
SATELLITE_UNIT = "companion-satellite.service"
# The strih Stream Deck XL (lsusb 0fd9:008f, read live by the main 4.10.2026). Only Stream Deck ids
# count: 0fd9 is Elgato's vendor id, which also makes capture devices that a deck check must not see.
STREAM_DECK_USB_IDS = ("0fd9:008f",)
USB_ROOT = "/sys/bus/usb/devices"
SUSTAIN_S = 60.0
HTTP_TIMEOUT_S = 3.0
TCP_TIMEOUT_S = 3.0
SYSTEMCTL_TIMEOUT_S = 15.0
# verify-strih: a watch whose last pass is older than three timer periods is not running.
STATE_MAX_AGE_S = 90.0
STATE_VERSION = 1
MAX_BODY = 1 << 20

FAULT_NOT_CONNECTED = "not-connected"
FAULT_NO_SURFACES = "no-surfaces"
_USB_ID = re.compile(r"^[0-9a-f]{4}:[0-9a-f]{4}$")


@dataclass(frozen=True)
class Observation:
    rest_ok: bool                   # /api/status answered with a JSON object
    rest_error: Optional[str]
    connected: Optional[bool]       # its `connected` (None: the REST is silent or carries no bool)
    surfaces: Optional[tuple]       # the open surfaces' product names (None: unreadable)
    companion: Optional[str]        # "host:port" from /api/config (None: unknown)
    companion_up: Optional[bool]    # that port answers a TCP connect (None: no target)
    companion_error: Optional[str]
    deck_on_usb: Optional[bool]     # a Stream Deck id on USB (None: sysfs unreadable)


@dataclass(frozen=True)
class Decision:
    since: dict      # fault id -> boot-clock second it was first seen in this unbroken streak
    restart: tuple   # the faults that held `sustain`: restart now (empty = no restart)


def faults(obs):
    """The fault ids this pass shows, in a fixed order -- none unless the Companion port answers."""
    if obs.companion_up is not True:
        return ()
    found = []
    if obs.connected is False:
        found.append(FAULT_NOT_CONNECTED)
    if obs.surfaces is not None and len(obs.surfaces) == 0 and obs.deck_on_usb is True:
        found.append(FAULT_NO_SURFACES)
    return tuple(found)


def _is_time(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def decide(since, obs, now, sustain=SUSTAIN_S):
    """(the stored fault start times, this pass's observation, the boot clock now) -> Decision. A fault
    keeps its start time while it holds; a fault not seen this pass is dropped (its window starts over
    when it comes back); a fault held `sustain` seconds restarts the Satellite, and every window starts
    over after a restart. A start time that is not a number or lies in the future counts from now."""
    since = since if isinstance(since, dict) else {}
    kept = {}
    for f in faults(obs):
        t = since.get(f)
        kept[f] = t if (_is_time(t) and t <= now) else now
    due = tuple(f for f in kept if now - kept[f] >= sustain)
    if due:
        return Decision({}, due)
    return Decision(kept, ())


# --- reading the Satellite, Companion and USB -----------------------------------------------------

_OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def fetch_json(url, timeout=HTTP_TIMEOUT_S):
    """(JSON value, None) or (None, error text). Never raises for a network or parse failure."""
    try:
        with _OPENER.open(url, timeout=timeout) as r:
            return json.loads(r.read(MAX_BODY).decode("utf-8", "replace")), None
    except (OSError, ValueError, HTTPException) as e:
        return None, "%s: %s" % (type(e).__name__, e)


def surface_names(raw):
    """/api/surfaces (a JSON array of open surfaces) -> a tuple of product names, None if not an array."""
    if not isinstance(raw, list):
        return None
    names = []
    for s in raw:
        name = (s.get("productName") or s.get("surfaceId")) if isinstance(s, dict) else None
        names.append(name if isinstance(name, str) and name else "?")
    return tuple(names)


def companion_target(config):
    """/api/config -> ((host, port), None) or (None, why). Only a tcp connection has a port to probe."""
    if not isinstance(config, dict):
        return None, "/api/config unreadable"
    proto = config.get("protocol", "tcp")
    if proto != "tcp":
        return None, "the Satellite connects over %r, not tcp -- no port to probe" % proto
    host, port = config.get("host"), config.get("port")
    if isinstance(port, str) and port.isdigit():
        port = int(port)
    if not isinstance(host, str) or not host or not isinstance(port, int) or isinstance(port, bool) \
            or not 0 < port < 65536:
        return None, "no Companion host:port in /api/config (%r:%r)" % (host, port)
    return (host, port), None


def tcp_probe(host, port, timeout=TCP_TIMEOUT_S):
    """(True, None) when host:port accepts a TCP connect, else (False, error text)."""
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True, None
    except OSError as e:
        return False, "%s: %s" % (type(e).__name__, e)


def usb_ids_present(usb_root=USB_ROOT):
    """The set of "vid:pid" (lowercase) under a sysfs USB device tree, None when it is unreadable."""
    try:
        entries = os.listdir(usb_root)
    except OSError:
        return None
    found = set()
    for e in entries:
        d = os.path.join(usb_root, e)
        try:
            with open(os.path.join(d, "idVendor")) as f:
                vid = f.read().strip().lower()
            with open(os.path.join(d, "idProduct")) as f:
                pid = f.read().strip().lower()
        except OSError:
            continue  # an interface or a hub port: no device ids here
        found.add("%s:%s" % (vid, pid))
    return found


def deck_on_usb(usb_root, usb_ids):
    present = usb_ids_present(usb_root)
    if present is None:
        return None
    return bool(present & {i.lower() for i in usb_ids})


def observe(rest_url=REST_URL, usb_root=USB_ROOT, usb_ids=STREAM_DECK_USB_IDS, http_timeout=HTTP_TIMEOUT_S,
            tcp_timeout=TCP_TIMEOUT_S):
    """One read of the Satellite REST, the Companion port and USB. A silent REST stops there: nothing
    else it would report can be trusted, and the pass makes no decision."""
    deck = deck_on_usb(usb_root, usb_ids)
    status, err = fetch_json(rest_url + "/api/status", http_timeout)
    if not isinstance(status, dict):
        return Observation(False, err or "/api/status is not a JSON object", None, None, None, None, None, deck)
    connected = status.get("connected") if isinstance(status.get("connected"), bool) else None
    raw, _err = fetch_json(rest_url + "/api/surfaces", http_timeout)
    config, cerr = fetch_json(rest_url + "/api/config", http_timeout)
    target, why = companion_target(config) if config is not None else (None, "/api/config unreadable: %s" % cerr)
    up, perr = (None, why) if target is None else tcp_probe(target[0], target[1], tcp_timeout)
    return Observation(True, None, connected, surface_names(raw),
                       None if target is None else "%s:%d" % target, up, perr, deck)


def observation_dict(obs):
    return {"rest_ok": obs.rest_ok, "rest_error": obs.rest_error, "connected": obs.connected,
            "surfaces": None if obs.surfaces is None else list(obs.surfaces), "companion": obs.companion,
            "companion_up": obs.companion_up, "companion_error": obs.companion_error,
            "deck_on_usb": obs.deck_on_usb}


# --- one pass: decide, act, log, remember -----------------------------------------------------------

def fault_text(fault, companion):
    if fault == FAULT_NOT_CONNECTED:
        return "not connected to Companion %s while its port answers" % companion
    return "no surface open while the Stream Deck is on USB"


def condition_of(obs, decision):
    """The one-word state a pass logs on change: rest-silent | companion-down | fault:<ids> | ok."""
    if not obs.rest_ok:
        return "rest-silent"
    if obs.companion_up is not True:
        return "companion-down"
    if decision.since:
        return "fault:" + "+".join(decision.since)
    return "ok"


def describe(obs):
    """The healthy-state summary of an observation dict (state file) or an Observation."""
    o = obs if isinstance(obs, dict) else observation_dict(obs)
    surf = o.get("surfaces") if isinstance(o.get("surfaces"), list) else None
    parts = ["Satellite %s to Companion %s" % ("connected" if o.get("connected") is True else "NOT connected",
                                              o.get("companion") or "?")]
    parts.append("%s surface(s)%s" % (len(surf), " (%s)" % ", ".join(surf) if surf else "")
                 if surf is not None else "surfaces unreadable")
    parts.append({True: "the Stream Deck on USB", False: "no Stream Deck on USB"}.get(
        o.get("deck_on_usb"), "USB unreadable"))
    return ", ".join(parts)


def condition_line(cond, obs, sustain, unit, rest_url):
    if cond == "rest-silent":
        return ("Satellite REST %s not answering (%s) -- no decision while it is silent (the unit itself "
                "restarts a Satellite that exits)" % (rest_url, obs.rest_error))
    if cond == "companion-down":
        if obs.companion is None:
            return "Companion target unknown (%s) -- the watch never restarts the Satellite without it" % (
                obs.companion_error)
        return ("Companion %s not answering (%s) -- the watch never restarts the Satellite while Companion is "
                "down" % (obs.companion, obs.companion_error))
    if cond.startswith("fault:"):
        texts = "; ".join(fault_text(f, obs.companion) for f in cond[len("fault:"):].split("+"))
        return "FAULT: %s -- %s is restarted if this holds for %.0f s" % (texts, unit, sustain)
    return None


def load_state(path):
    """(state dict or None, error text or None); a missing file is (None, None): the first run after a boot."""
    try:
        with open(path) as f:
            state = json.load(f)
    except FileNotFoundError:
        return None, None
    except (OSError, ValueError) as e:
        return None, str(e)
    if not isinstance(state, dict):
        return None, "not a JSON object"
    return state, None


def write_state(path, state):
    """Atomic JSON write (temp + rename in the same directory). Returns None or the error text."""
    tmp = "%s.tmp.%d" % (path, os.getpid())
    try:
        os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
        with open(tmp, "w") as f:
            json.dump(state, f, indent=1, sort_keys=True)
            f.write("\n")
        os.replace(tmp, path)
    except OSError as e:
        msg = str(e)
        if os.path.lexists(tmp):
            try:
                os.unlink(tmp)
            except OSError as e2:
                msg += "; temp file %s left behind (%s)" % (tmp, e2)
        return msg
    return None


def boot_clock():
    return time.clock_gettime(time.CLOCK_BOOTTIME)


def run_pass(state_file, observe_fn, restart, *, log, wall=time.time, boot=boot_clock, sustain=SUSTAIN_S,
             unit=SATELLITE_UNIT, rest_url=REST_URL):
    """One timer firing: read the previous state, observe, decide, restart if due, log what changed, write
    the state. `observe_fn()` -> Observation, `restart(unit)` -> (rc, text). Returns the state written."""
    prev, err = load_state(state_file)
    if err:
        log("state file %s unreadable (%s) -- starting with no fault history" % (state_file, err))
    prev = prev or {}
    obs = observe_fn()
    now = boot()
    since_prev = prev.get("since") if isinstance(prev.get("since"), dict) else {}
    d = decide(since_prev, obs, now, sustain)
    restarts = prev.get("restarts") if isinstance(prev.get("restarts"), int) and prev["restarts"] >= 0 else 0
    last_restart = prev.get("last_restart") if isinstance(prev.get("last_restart"), dict) else None
    last_error = None
    if d.restart:
        held = "; ".join("%s, for %.0f s" % (fault_text(f, obs.companion), now - (
            since_prev[f] if _is_time(since_prev.get(f)) and since_prev[f] <= now else now)) for f in d.restart)
        rc, out = restart(unit)
        if rc == 0:
            restarts += 1
            log("restarting %s: %s (restart #%d since boot)" % (unit, held, restarts))
        else:
            last_error = "restart: %s" % (out or "rc %d" % rc)
            log("could NOT restart %s (%s): %s" % (unit, held, out or "rc %d" % rc))
        last_restart = {"epoch_s": wall(), "faults": list(d.restart), "ok": rc == 0,
                        "error": None if rc == 0 else (out or "rc %d" % rc)}
        cond = "restarted"
    else:
        cond = condition_of(obs, d)
        if cond != prev.get("condition"):
            line = condition_line(cond, obs, sustain, unit, rest_url)
            if line is None:  # ok
                prefix = "healthy again" if prev.get("condition") else "watching %s" % unit
                line = "%s: %s" % (prefix, describe(obs))
            log(line)
    state = {"version": STATE_VERSION, "updated_epoch_s": wall(), "boot_s": now, "since": d.since,
             "condition": cond, "observation": observation_dict(obs), "restarts": restarts,
             "last_restart": last_restart, "unit": unit, "sustain_s": sustain, "last_error": last_error}
    werr = write_state(state_file, state)
    if werr:
        log("state file %s not written: %s" % (state_file, werr))
    return state


def systemctl_restart(systemctl="systemctl"):
    """The real restart: queue a try-restart (only a RUNNING unit is restarted, never a stopped one
    started) without waiting for it, so a slow stop never runs into the oneshot's timeout."""
    def restart(unit):
        try:
            r = subprocess.run([systemctl, "--user", "--no-block", "try-restart", unit], capture_output=True,
                               text=True, timeout=SYSTEMCTL_TIMEOUT_S)
        except (OSError, subprocess.TimeoutExpired) as e:
            return 1, "%s: %s" % (type(e).__name__, e)
        return r.returncode, (r.stderr or r.stdout).strip()
    return restart


# --- verify-strih: --check-state ----------------------------------------------------------------------

def state_verdict(state, now, max_age=STATE_MAX_AGE_S):
    """(state file JSON, wall clock now) -> (ok, one-line text): the watch's last pass must be recent."""
    if not isinstance(state, dict) or not _is_time(state.get("updated_epoch_s")):
        return False, "state unreadable (no updated_epoch_s)"
    age = now - state["updated_epoch_s"]
    if age > max_age or age < -max_age:
        return False, ("stale: last pass %.0f s ago (limit %.0f s) -- strih-satellite-watch.timer is not running "
                       "the watch" % (age, max_age))
    o = state.get("observation") if isinstance(state.get("observation"), dict) else {}
    cond = state.get("condition") if isinstance(state.get("condition"), str) else "?"
    if cond == "rest-silent":
        now_text = "Satellite REST not answering (%s)" % o.get("rest_error")
    elif cond == "companion-down":
        now_text = "Companion %s not answering -- no restarts while it is down" % (o.get("companion") or "?")
    else:
        now_text = describe(o)
    since = state.get("since") if isinstance(state.get("since"), dict) else {}
    boot_s = state.get("boot_s")
    if since and _is_time(boot_s):
        held = "; ".join("%s for %.0f s" % (fault_text(f, o.get("companion")), boot_s - t)
                         for f, t in since.items() if _is_time(t))
        now_text += "; FAULT in progress: %s (restart at %.0f s)" % (held, state.get("sustain_s") or SUSTAIN_S)
    restarts = state.get("restarts") if isinstance(state.get("restarts"), int) else 0
    return True, "last pass %.0f s ago: %s; %d restart(s) of %s since boot" % (
        max(age, 0.0), now_text, restarts, state.get("unit") or SATELLITE_UNIT)


def check_state_file(path, now=None, max_age=STATE_MAX_AGE_S):
    """--check-state: (exit code, line) for verify-strih."""
    try:
        with open(path) as f:
            state = json.load(f)
    except (OSError, ValueError) as e:
        return 1, "state file %s unreadable (%s) -- is strih-satellite-watch.timer running?" % (path, e)
    ok, text = state_verdict(state, time.time() if now is None else now, max_age)
    return (0 if ok else 1), text


def default_state_file():
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/run/user/%d" % os.getuid()
    return os.path.join(runtime, "strih-satellite-watch.json")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--rest-url", default=REST_URL, help="the Satellite's local REST base URL")
    ap.add_argument("--unit", default=SATELLITE_UNIT, help="the --user unit to restart")
    ap.add_argument("--usb-id", action="append", metavar="VID:PID",
                    help="a Stream Deck USB id (repeatable; default %s)" % ", ".join(STREAM_DECK_USB_IDS))
    ap.add_argument("--usb-root", default=USB_ROOT)
    ap.add_argument("--sustain", type=float, default=SUSTAIN_S, help="seconds a fault must hold before a restart")
    ap.add_argument("--http-timeout", type=float, default=HTTP_TIMEOUT_S)
    ap.add_argument("--tcp-timeout", type=float, default=TCP_TIMEOUT_S)
    ap.add_argument("--systemctl", default="systemctl", help=argparse.SUPPRESS)
    ap.add_argument("--state-file", default=None, help="default: $XDG_RUNTIME_DIR/strih-satellite-watch.json")
    ap.add_argument("--check-state", metavar="FILE", help="grade a state file (verify-strih) and exit 0/1")
    ap.add_argument("--max-age", type=float, default=STATE_MAX_AGE_S)
    a = ap.parse_args(argv)
    if a.check_state:
        rc, line = check_state_file(a.check_state, max_age=a.max_age)
        print(line)
        return rc
    usb_ids = tuple(i.lower() for i in (a.usb_id or STREAM_DECK_USB_IDS))
    bad = [i for i in usb_ids if not _USB_ID.match(i)]
    if bad:
        ap.error("--usb-id must be VID:PID in hex (e.g. 0fd9:008f), got %s" % ", ".join(bad))
    run_pass(a.state_file or default_state_file(),
             lambda: observe(a.rest_url, a.usb_root, usb_ids, a.http_timeout, a.tcp_timeout),
             systemctl_restart(a.systemctl), log=lambda msg: print(msg, flush=True), sustain=a.sustain,
             unit=a.unit, rest_url=a.rest_url)
    return 0


if __name__ == "__main__":
    sys.exit(main())
