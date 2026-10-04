#!/usr/bin/env python3
"""strih-browser-keeper (issue 1399): keep every OBS browser source on strih-lx loaded.

WHY (owner, 4.10.2026: "tie browser sceny maju byt vzdy nacitane"): obs-browser loads a browser
source's page ONCE, when the source is created at OBS start, and never retries a failed first load.
On 4.10.2026 strih-lx booted before presenter.lan / fohabl.lan answered, so `Browser camera crew`,
`Odpocet`, `CG-presenter` and `Browser Ableset` stayed empty until a manual `refreshnocache`.

WHAT (design comment 5977447339, Approach 1): a supervised --user unit (systemd/strih-browser-keeper
.service, Restart=always, started by the kiosk openbox autostart) that
  * connects to the local obs-websocket (127.0.0.1:4455, no auth on strih-lx) and reconnects forever;
  * every PASS_INTERVAL_S lists every `browser_source` input with its URL over WS -- no source list
    is hard-coded, a source added later is picked up;
  * TCP-probes each URL's host:port (bounded: one daemon thread per target, one deadline per pass);
  * presses the `refreshnocache` button (PressInputPropertiesButton) of a source:
      - ONCE after every WS (re)connect = an OBS (re)start, as soon as its host is first reachable;
      - after that, whenever its host goes unreachable -> reachable;
    never while the host is down, never periodically on a working page (a refresh blanks the source
    for a moment). The decision is the pure `decide`, pytest-tested as a table.
  * writes a small state file every pass (the verify-strih "last pass recent" read, `--check-state`).

A keeper restart while OBS keeps running is a new WS connection too, so it refreshes each source
once (a short blank) -- the design's rule "once per connect" cannot tell it from an OBS restart.

One log line per refresh and per reachability transition (journal: SyslogIdentifier
strih-browser-keeper). Std-only except python3-websocket (the client every strih seeder uses).
"""
import argparse
import json
import os
import socket
import sys
import threading
import time
from dataclasses import dataclass
from typing import Optional
from urllib.parse import urlsplit

OBS_HOST = "127.0.0.1"
OBS_PORT = 4455
PASS_INTERVAL_S = 5.0
PROBE_TIMEOUT_S = 5.0
WS_TIMEOUT_S = 10.0
# The verify-strih read: a keeper that has not finished a pass for this long is dead or wedged
# (a pass is <= PASS_INTERVAL_S + PROBE_TIMEOUT_S + a few WS round trips).
STATE_MAX_AGE_S = 60.0

BROWSER_KIND = "browser_source"
# obs-browser's own default (vendor/obs-studio/plugins/obs-browser/obs-browser-plugin.cpp): a source
# that never changed its url carries no "url" key in GetInputSettings (non-defaults only).
BROWSER_DEFAULT_URL = "https://obsproject.com/browser-source"
REFRESH_BUTTON = "refreshnocache"
DEFAULT_PORTS = {"http": 80, "https": 443}

ACTION_CONNECT = "connect"
ACTION_RECOVERED = "recovered"
ACTION_TEXT = {
    ACTION_CONNECT: "first reachable after connect",
    ACTION_RECOVERED: "page server back after being unreachable",
}


# --- pure decisions ---------------------------------------------------------------------------

@dataclass(frozen=True)
class SourceState:
    """What the keeper remembers about one browser source: the connect epoch it was refreshed in
    (None = never) and its host's last probe result (None = never probed)."""
    refreshed_epoch: Optional[int] = None
    reachable: Optional[bool] = None


def decide(prev, reachable, epoch):
    """(per-source state, probe result, connect epoch) -> (new state, action or None).

    * host unreachable -> never refresh, remember it is down;
    * host reachable and not yet refreshed in THIS connect epoch -> ACTION_CONNECT (OBS loaded the
      page at its start, maybe before the server answered);
    * host reachable, already refreshed this epoch, and it was down at the last probe ->
      ACTION_RECOVERED (the page server restarted, the page may have lost its content);
    * otherwise nothing -- a working page is never refreshed.
    The caller commits the new state only after the refresh request succeeded."""
    prev = prev if prev is not None else SourceState()
    if not reachable:
        return SourceState(prev.refreshed_epoch, False), None
    if prev.refreshed_epoch != epoch:
        return SourceState(epoch, True), ACTION_CONNECT
    if prev.reachable is False:
        return SourceState(epoch, True), ACTION_RECOVERED
    return SourceState(epoch, True), None


def transition(prev_reachable, reachable):
    """The reachability log word for one probe: 'first-up' / 'first-down' (first probe),
    'up' / 'down' (a change), or None (no change -- nothing to log)."""
    if prev_reachable is None:
        return "first-up" if reachable else "first-down"
    if prev_reachable == reachable:
        return None
    return "up" if reachable else "down"


TRANSITION_TEXT = {
    "first-up": "reachable",
    "first-down": "unreachable",
    "up": "unreachable -> reachable",
    "down": "reachable -> unreachable",
}


def browser_source_url(settings):
    """The URL a browser source loads, from its GetInputSettings `inputSettings` (non-defaults
    only): None for a local-file source or a non-string / empty url."""
    if not isinstance(settings, dict):
        return None
    if settings.get("is_local_file") is True:
        return None
    url = settings.get("url", BROWSER_DEFAULT_URL)
    if not isinstance(url, str) or not url.strip():
        return None
    return url.strip()


def probe_target(url):
    """(host, port) a URL's page server answers on, or None when it has none. A URL without a
    scheme (`fohabl.lan`, how the Ableset source is set) is http, so port 80; https is 443; an
    explicit port wins; any other scheme (file:, about:, data:) has no network target."""
    if not isinstance(url, str) or not url.strip():
        return None
    url = url.strip()
    if "://" not in url:
        url = "http://" + url
    try:
        parts = urlsplit(url)
        scheme = parts.scheme.lower()
        host = parts.hostname
        port = parts.port
    except ValueError:
        return None
    if scheme not in DEFAULT_PORTS or not host:
        return None
    return host, port if port is not None else DEFAULT_PORTS[scheme]


def target_text(target):
    host, port = target
    return "[%s]:%d" % (host, port) if ":" in host else "%s:%d" % (host, port)


def state_verdict(state, now, max_age=STATE_MAX_AGE_S):
    """(state file JSON, wall clock now) -> (ok, one-line text) for verify-strih: the keeper's last
    pass must be recent AND it must hold an obs-websocket connection."""
    if not isinstance(state, dict) or not isinstance(state.get("updated_epoch_s"), (int, float)):
        return False, "state unreadable (no updated_epoch_s)"
    age = now - state["updated_epoch_s"]
    if age > max_age or age < -max_age:
        return False, "stale: last pass %.0f s ago (limit %.0f s) -- the keeper is not running its loop" % (
            age, max_age)
    if state.get("connected") is not True:
        return False, "last pass %.0f s ago but NOT connected to obs-websocket (%s)" % (
            max(age, 0.0), state.get("last_error") or "no error recorded")
    sources = state.get("sources") if isinstance(state.get("sources"), list) else []
    reachable = sum(1 for s in sources if isinstance(s, dict) and s.get("reachable") is True)
    return True, ("last pass %.0f s ago, connected (epoch %s), %d browser source(s), %d with a reachable "
                  "page server, %s refresh(es) since start") % (
        max(age, 0.0), state.get("connect_epoch"), len(sources), reachable, state.get("refreshes", 0))


# --- obs-websocket client -----------------------------------------------------------------------

class ObsError(Exception):
    """The connection is unusable (protocol violation, auth demanded, request timeout)."""


class ObsRequestError(Exception):
    """OBS answered a request with a failure status (the connection itself is fine)."""


def _ws_exception_types():
    try:
        from websocket import WebSocketException
    except ImportError:
        return ()
    return (WebSocketException,)


class ObsClient:
    """A minimal obs-websocket 5 client (the obs_phase2 `_conn`/`_rpc` shape): no event
    subscriptions (the event-flood lesson -- this client only does request/response) and a hard
    deadline per request."""

    def __init__(self, host=OBS_HOST, port=OBS_PORT, timeout=WS_TIMEOUT_S, password=""):
        from websocket import create_connection
        self._timeout = timeout
        self._rid = 0
        self.ws = create_connection("ws://%s:%d" % (host, port), timeout=timeout)
        try:
            hello = self._recv()
            if hello.get("op") != 0:
                raise ObsError("expected Hello (op 0), got op %r" % hello.get("op"))
            ident = {"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": 0}}
            auth = (hello.get("d") or {}).get("authentication")
            if auth:
                if not password:
                    raise ObsError("obs-websocket demands auth but the keeper has no password")
                import base64
                import hashlib
                secret = base64.b64encode(hashlib.sha256((password + auth["salt"]).encode()).digest()).decode()
                ident["d"]["authentication"] = base64.b64encode(
                    hashlib.sha256((secret + auth["challenge"]).encode()).digest()).decode()
            self.ws.send(json.dumps(ident))
            identified = self._recv()
            if identified.get("op") != 2:
                raise ObsError("expected Identified (op 2), got op %r" % identified.get("op"))
        except BaseException:
            self.close()
            raise

    def _recv(self):
        raw = self.ws.recv()
        try:
            msg = json.loads(raw)
        except (TypeError, ValueError) as e:
            raise ObsError("not a JSON message: %s" % e)
        if not isinstance(msg, dict):
            raise ObsError("not a JSON object: %r" % (msg,))
        return msg

    def request(self, rtype, data=None):
        self._rid += 1
        rid = "keeper-%d" % self._rid
        self.ws.send(json.dumps({"op": 6, "d": {"requestType": rtype, "requestId": rid,
                                                "requestData": data or {}}}))
        deadline = time.monotonic() + self._timeout
        while True:
            if time.monotonic() >= deadline:
                raise ObsError("%s: no response within %.0f s" % (rtype, self._timeout))
            msg = self._recv()
            d = msg.get("d") or {}
            if msg.get("op") == 7 and d.get("requestId") == rid:
                status = d.get("requestStatus") or {}
                if not status.get("result"):
                    raise ObsRequestError("%s failed: code %s %s" % (
                        rtype, status.get("code"), status.get("comment") or ""))
                return d.get("responseData") or {}

    def close(self):
        """Close the socket. Returns the close error text, or None: a dead socket often fails to
        close, and the caller logs it -- it must never mask the error that made it close."""
        try:
            self.ws.close()
        except Exception as e:
            return "close failed: %s" % e
        return None


def list_browser_sources(obs, log):
    """[(inputName, url or None)] for every browser_source input OBS has right now. An input that
    vanishes between the list and its settings read is skipped (OBS answers ResourceNotFound)."""
    out = []
    for item in obs.request("GetInputList").get("inputs") or []:
        if not isinstance(item, dict):
            continue
        kind = item.get("unversionedInputKind") or item.get("inputKind")
        name = item.get("inputName")
        if kind != BROWSER_KIND or not isinstance(name, str):
            continue
        try:
            settings = obs.request("GetInputSettings", {"inputName": name}).get("inputSettings")
        except ObsRequestError as e:
            log("browser source '%s' settings unreadable (%s) -- skipped this pass" % (name, e))
            continue
        out.append((name, browser_source_url(settings)))
    return out


# --- bounded TCP probes -------------------------------------------------------------------------

def tcp_probe(host, port, timeout=PROBE_TIMEOUT_S):
    """True iff a TCP connection to host:port opens within the timeout."""
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except (OSError, ValueError):
        return False


class Prober:
    """Probes each distinct target in its own daemon thread, joined against ONE deadline per pass,
    so a hung name lookup (getaddrinfo ignores the socket timeout) can never stall the loop. A
    probe still running at the deadline reads unreachable, and a target whose previous probe is
    still running is not probed again -- at most one thread per target."""

    def __init__(self, probe_fn=tcp_probe, timeout=PROBE_TIMEOUT_S):
        self._probe_fn = probe_fn
        self._timeout = timeout
        self._inflight = {}

    def probe_all(self, targets):
        results = {}
        started = []
        for target in sorted(set(targets)):
            running = self._inflight.get(target)
            if running is not None and running.is_alive():
                results[target] = False
                continue
            box = {}

            def _run(t=target, out=box):
                try:
                    out["ok"] = bool(self._probe_fn(t[0], t[1], self._timeout))
                except Exception as e:  # a probe that raises is an unreachable target, never a crash
                    out["ok"] = False
                    out["error"] = str(e)

            thread = threading.Thread(target=_run, name="probe-%s" % target_text(target), daemon=True)
            self._inflight[target] = thread
            thread.start()
            started.append((target, thread, box))
        deadline = time.monotonic() + self._timeout + 1.0
        for target, thread, box in started:
            thread.join(max(0.0, deadline - time.monotonic()))
            results[target] = box.get("ok", False) if not thread.is_alive() else False
        return results


# --- the keeper loop ----------------------------------------------------------------------------

class Keeper:
    """The per-source memory + one pass. Holds the states across reconnects: the connect epoch is
    what makes every source refresh once again after OBS restarts."""

    def __init__(self, prober, log):
        self.prober = prober
        self.log = log
        self.states = {}
        self.refreshes = 0
        self._unwatched = set()
        self._press_errors = {}
        self.last_sources = []

    def run_pass(self, obs, epoch):
        sources = list_browser_sources(obs, self.log)
        watched = []
        for name, url in sources:
            target = probe_target(url)
            if target is None:
                if (name, url) not in self._unwatched:
                    self._unwatched.add((name, url))
                    self.log("browser source '%s' has no network page server (url %r) -- not watched" % (name, url))
                continue
            watched.append((name, url, target))
        results = self.prober.probe_all([t for _, _, t in watched])
        listed = {(name, url) for name, url, _ in watched}
        self.states = {k: v for k, v in self.states.items() if k in listed}
        summary = []
        for name, url, target in watched:
            key = (name, url)
            prev = self.states.get(key)
            reachable = results.get(target, False)
            word = transition(prev.reachable if prev else None, reachable)
            if word is not None:
                self.log("browser source '%s' page server %s %s" % (name, target_text(target), TRANSITION_TEXT[word]))
            new, action = decide(prev, reachable, epoch)
            if action is not None:
                try:
                    obs.request("PressInputPropertiesButton", {"inputName": name, "propertyName": REFRESH_BUTTON})
                except ObsRequestError as e:
                    if self._press_errors.get(key) != str(e):
                        self._press_errors[key] = str(e)
                        self.log("refresh of browser source '%s' failed (%s) -- retried next pass" % (name, e))
                    # not committed: the next pass decides the same refresh again
                    new = SourceState(prev.refreshed_epoch if prev else None, reachable)
                else:
                    self._press_errors.pop(key, None)
                    self.refreshes += 1
                    self.log("refreshed browser source '%s' (%s): %s, connect epoch %d" % (
                        name, url, ACTION_TEXT[action], epoch))
            self.states[key] = new
            summary.append({"name": name, "url": url, "target": target_text(target), "reachable": reachable,
                            "refreshed_epoch": new.refreshed_epoch})
        self.last_sources = summary
        return summary


def write_state(path, state):
    """Atomic JSON write (temp + rename in the same directory). Returns None, or the error text
    for the caller to log: a failed state write must never stop the keeper."""
    if not path:
        return None
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


def run(connect, prober, *, interval=PASS_INTERVAL_S, state_file=None, log=None, sleep=time.sleep,
        clock=time.time, should_stop=None, endpoint="%s:%d" % (OBS_HOST, OBS_PORT)):
    """The keeper loop: connect -> passes until the connection breaks -> reconnect, forever (or
    until should_stop()). `connect` returns an ObsClient-shaped object; everything time-, network-
    and OBS-shaped is injected so the real loop runs under test."""
    log = log or (lambda msg: print(msg, flush=True))
    should_stop = should_stop or (lambda: False)
    conn_errors = (OSError, ObsError, ObsRequestError) + _ws_exception_types()
    keeper = Keeper(prober, log)
    epoch = 0
    last_connect_error = None
    state_error = None

    def _state(connected, error=None):
        nonlocal state_error
        err = write_state(state_file, {
            "version": 1, "updated_epoch_s": clock(), "connected": connected, "connect_epoch": epoch,
            "endpoint": endpoint, "refreshes": keeper.refreshes, "sources": keeper.last_sources,
            "last_error": error,
        })
        if err != state_error:
            state_error = err
            if err:
                log("state file %s not written: %s" % (state_file, err))

    while not should_stop():
        try:
            obs = connect()
        except conn_errors as e:
            if str(e) != last_connect_error:
                last_connect_error = str(e)
                log("obs-websocket %s not reachable (%s) -- retrying every %.0f s" % (endpoint, e, interval))
            _state(False, "connect: %s" % e)
            sleep(interval)
            continue
        last_connect_error = None
        epoch += 1
        log("connected to obs-websocket %s (connect epoch %d)" % (endpoint, epoch))
        try:
            while not should_stop():
                keeper.run_pass(obs, epoch)
                _state(True)
                sleep(interval)
        except conn_errors as e:
            log("lost obs-websocket %s in connect epoch %d (%s) -- reconnecting" % (endpoint, epoch, e))
            _state(False, "lost: %s" % e)
            sleep(interval)
        finally:
            close_err = obs.close()
            if close_err:
                log("obs-websocket %s: %s" % (endpoint, close_err))


def check_state_file(path, now=None, max_age=STATE_MAX_AGE_S):
    """--check-state: (exit code, line) for verify-strih."""
    try:
        with open(path) as f:
            state = json.load(f)
    except (OSError, ValueError) as e:
        return 1, "state file %s unreadable (%s) -- is strih-browser-keeper.service running?" % (path, e)
    ok, text = state_verdict(state, time.time() if now is None else now, max_age)
    return (0 if ok else 1), text


def default_state_file():
    runtime = os.environ.get("XDG_RUNTIME_DIR") or "/run/user/%d" % os.getuid()
    return os.path.join(runtime, "strih-browser-keeper.json")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--host", default=OBS_HOST)
    ap.add_argument("--port", type=int, default=OBS_PORT)
    ap.add_argument("--interval", type=float, default=PASS_INTERVAL_S)
    ap.add_argument("--probe-timeout", type=float, default=PROBE_TIMEOUT_S)
    ap.add_argument("--state-file", default=None, help="default: $XDG_RUNTIME_DIR/strih-browser-keeper.json")
    ap.add_argument("--check-state", metavar="FILE", help="grade a state file (verify-strih) and exit 0/1")
    ap.add_argument("--max-age", type=float, default=STATE_MAX_AGE_S)
    a = ap.parse_args(argv)
    if a.check_state:
        rc, line = check_state_file(a.check_state, max_age=a.max_age)
        print(line)
        return rc
    password = os.environ.get("OBS_PASSWORD", "")
    endpoint = "%s:%d" % (a.host, a.port)
    run(lambda: ObsClient(a.host, a.port, WS_TIMEOUT_S, password), Prober(timeout=a.probe_timeout),
        interval=a.interval, state_file=a.state_file or default_state_file(), endpoint=endpoint)
    return 0


if __name__ == "__main__":
    sys.exit(main())
