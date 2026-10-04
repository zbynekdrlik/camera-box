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
  * TCP-probes each URL's host:port (bounded: one daemon thread per target, one deadline per pass); a
    page server counts as down only after DOWN_AFTER failed probes in a row, and a probe that did not
    finish (a hung name lookup) is no information at all -- one slow probe never reloads a page;
  * presses the `refreshnocache` button (PressInputPropertiesButton) of a source:
      - ONCE per OBS RUN, as soon as its host is first reachable (OBS may have loaded the page before
        its server answered);
      - after that, whenever its host goes unreachable -> reachable;
    never while the host is down, never periodically on a working page (a refresh blanks the source
    for a moment). A refresh OBS refuses keeps its state, so the same refresh is pressed again next
    pass. The decision is the pure `decide` (+ `debounce`), pytest-tested as tables.
  * writes a small state file every pass (the verify-strih "last pass recent" read, `--check-state`).

The OBS RUN (main ROZHODNUTE 5978724027): the epoch goes up only when OBS itself (re)started -- never
on a keeper (re)start or a WS reconnect alone, because a refresh blanks a graphic on air. On every
connect the keeper reads the OBS identity (the local `obs` process start time from /proc, and the
obs-websocket GetStats `renderTotalFrames`, which a restart sends backwards) and compares it with the
one it stored in the state file (`obs_restarted`). A keeper that restarts finds the stored identity,
epoch and per-source states in the state file (/run: gone after a boot = OBS just started).

One log line per refresh and per reachability transition (journal: SyslogIdentifier
strih-browser-keeper). Std-only except python3-websocket (the client every strih seeder uses).
"""
import argparse
import json
import os
import re
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
# A page server is down only after this many failed probes in a row (~10 s at the pass interval):
# a single lost TCP connect must never read as an outage, whose end would reload a working page.
DOWN_AFTER = 2
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
# A URL that STARTS with a scheme (`fohabl.lan/?next=http://x` has none: it is http).
_HAS_SCHEME = re.compile(r"^[A-Za-z][A-Za-z0-9+.-]*://")

ACTION_OBS_RUN = "obs-run"
ACTION_RECOVERED = "recovered"
ACTION_TEXT = {
    ACTION_OBS_RUN: "first reachable in this OBS run",
    ACTION_RECOVERED: "page server back after being unreachable",
}
# The local OBS process name (/proc/<pid>/comm of /usr/bin/obs) whose start time identifies an OBS run.
OBS_PROCESS_NAME = "obs"
LOOPBACK_HOSTS = ("127.0.0.1", "localhost", "::1")
STATE_VERSION = 2


# --- pure decisions ---------------------------------------------------------------------------

@dataclass(frozen=True)
class SourceState:
    """What the keeper remembers about one browser source: the OBS run epoch it was refreshed in
    (None = never) and its host's last reachability verdict (None = none yet)."""
    refreshed_epoch: Optional[int] = None
    reachable: Optional[bool] = None


def debounce(fails, up, result, down_after=DOWN_AFTER):
    """One page server's probe history -> its reachability verdict.

    (failed probes in a row, last verdict True/False/None, this pass's probe True/False/None) ->
    (failed probes in a row, verdict). A good probe is reachable at once. A failed probe counts, and the
    server is down only from the `down_after`-th failure in a row; before that the last verdict holds.
    A probe that did not finish (None) changes nothing."""
    if result is True:
        return 0, True
    if result is None:
        return fails, up
    fails += 1
    if fails >= down_after:
        return fails, False
    return fails, up


def decide(prev, reachable, epoch):
    """(per-source state, reachability verdict, OBS run epoch) -> (new state, action or None).

    * verdict unknown (None: no probe has finished or failed often enough yet) -> nothing changes;
    * host unreachable -> never refresh, remember it is down;
    * host reachable and not yet refreshed in THIS OBS run -> ACTION_OBS_RUN (OBS loaded the page at
      its start, maybe before the server answered);
    * host reachable, already refreshed this epoch, and it was down at the last probe ->
      ACTION_RECOVERED (the page server restarted, the page may have lost its content);
    * otherwise nothing -- a working page is never refreshed.
    The caller commits the new state only after the refresh request succeeded."""
    prev = prev if prev is not None else SourceState()
    if reachable is None:
        return prev, None
    if not reachable:
        return SourceState(prev.refreshed_epoch, False), None
    if prev.refreshed_epoch != epoch:
        return SourceState(epoch, True), ACTION_OBS_RUN
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
    if not _HAS_SCHEME.match(url):
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
    return True, ("last pass %.0f s ago, connected (OBS run epoch %s), %d browser source(s), %d with a "
                  "reachable page server, %s refresh(es) since the keeper started") % (
        max(age, 0.0), state.get("obs_epoch"), len(sources), reachable, state.get("refreshes", 0))


# --- the OBS run: identity + the keeper's own memory across its restarts -------------------------

def _is_count(v):
    return isinstance(v, int) and not isinstance(v, bool) and v >= 0


def proc_start_ticks(stat_text):
    """/proc/<pid>/stat text -> the process start time (field 22, clock ticks since boot), or None.
    The comm field is in parentheses and may itself hold spaces and ')', so split after the LAST ')'."""
    if not isinstance(stat_text, str):
        return None
    rp = stat_text.rfind(")")
    if rp < 0:
        return None
    fields = stat_text[rp + 1:].split()
    if len(fields) < 20 or not fields[19].isdigit():
        return None
    return int(fields[19])


def local_obs_process_identity(proc_root="/proc", name=OBS_PROCESS_NAME, uid=None):
    """'<boot id>:<start ticks>' of the newest local process called NAME owned by UID (default: ours),
    or None when there is none. Two runs of OBS never share it: a restart is a new process."""
    uid = os.getuid() if uid is None else uid
    try:
        with open(os.path.join(proc_root, "sys", "kernel", "random", "boot_id")) as f:
            boot = f.read().strip()
    except OSError:
        boot = ""
    best = None
    try:
        pids = [p for p in os.listdir(proc_root) if p.isdigit()]
    except OSError:
        return None
    for pid in pids:
        base = os.path.join(proc_root, pid)
        try:
            if os.stat(base).st_uid != uid:
                continue
            with open(os.path.join(base, "comm")) as f:
                if f.read().strip() != name:
                    continue
            with open(os.path.join(base, "stat")) as f:
                ticks = proc_start_ticks(f.read())
        except OSError:
            continue  # the process exited while we looked
        if ticks is not None and (best is None or ticks > best):
            best = ticks
    return None if best is None else "%s:%d" % (boot, best)


def obs_restarted(stored, current):
    """(stored OBS identity, current one) -> True when this is a NEW OBS run (refresh round).

    Each identity is {"process": '<boot>:<start ticks>' or None, "frames": renderTotalFrames or None}.
    * nothing stored (the first keeper run after a boot: OBS just started) -> new run;
    * both process identities known -> new run iff they differ (exact; a frame count is not needed);
    * else both frame counts known -> new run iff the count went backwards (a restart starts at 0);
    * else nothing comparable -> new run (a refresh is safer for content than a page left empty)."""
    if not isinstance(stored, dict) or not isinstance(current, dict):
        return True
    sp, cp = stored.get("process"), current.get("process")
    if isinstance(sp, str) and isinstance(cp, str):
        return sp != cp
    sf, cf = stored.get("frames"), current.get("frames")
    if _is_count(sf) and _is_count(cf):
        return cf < sf
    return True


def source_entry(name, url, target, reachable, state):
    """One `sources` row of the state file. `reachable` is THIS keeper's current verdict (None = none
    yet); `refreshed_epoch` + `remembered_reachable` are the per-source memory (state, a SourceState or
    None) a restarted keeper takes over -- kept apart, so a restart while a verdict is still unknown
    never forgets that the page server was down."""
    return {"name": name, "url": url, "target": target_text(target) if target is not None else None,
            "reachable": reachable,
            "refreshed_epoch": state.refreshed_epoch if state is not None else None,
            "remembered_reachable": state.reachable if state is not None else None}


def restored_sources(state):
    """The per-source states a previous keeper process left in its state file: {(name, url):
    SourceState}. Malformed entries are skipped; a file without a valid obs_epoch restores nothing."""
    if not isinstance(state, dict) or not _is_count(state.get("obs_epoch")):
        return {}
    out = {}
    for s in state.get("sources") if isinstance(state.get("sources"), list) else []:
        if not isinstance(s, dict) or not isinstance(s.get("name"), str) or not isinstance(s.get("url"), str):
            continue
        ep, up = s.get("refreshed_epoch"), s.get("remembered_reachable")
        if (ep is not None and not _is_count(ep)) or (up is not None and not isinstance(up, bool)):
            continue
        out[(s["name"], s["url"])] = SourceState(ep, up)
    return out


def load_state(path):
    """(state dict or None, error text or None) of a previous keeper's state file. A missing file is
    (None, None): the first run after a boot."""
    if not path:
        return None, None
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


def obs_render_frames(obs):
    """OBS's GetStats `renderTotalFrames` (frames rendered since OBS started), or None when OBS
    refuses the request or answers something that is not a count. A broken connection still raises."""
    try:
        frames = obs.request("GetStats").get("renderTotalFrames")
    except ObsRequestError:
        return None
    if isinstance(frames, float) and frames.is_integer():
        frames = int(frames)
    return frames if _is_count(frames) else None


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
    probe still running at the deadline reads None (no information, never "down") and is kept:
    while it runs, the target is not probed again (at most one thread per target), and once it has
    finished its late result is the target's result on the next pass -- slow DNS plus a black-holed
    connect must still read down eventually."""

    def __init__(self, probe_fn=tcp_probe, timeout=PROBE_TIMEOUT_S):
        self._probe_fn = probe_fn
        self._timeout = timeout
        self._pending = {}  # target -> (thread, result box) of a probe that missed its deadline

    def probe_all(self, targets):
        results = {}
        started = []
        wanted = sorted(set(targets))
        for target in wanted:
            pending = self._pending.get(target)
            if pending is not None:
                thread, box = pending
                if thread.is_alive():
                    results[target] = None
                    continue
                del self._pending[target]
                results[target] = box.get("ok", False)  # the late result, used once
                continue
            box = {}

            def _run(t=target, out=box):
                try:
                    out["ok"] = bool(self._probe_fn(t[0], t[1], self._timeout))
                except Exception as e:  # a probe that raises is an unreachable target, never a crash
                    out["ok"] = False
                    out["error"] = str(e)

            thread = threading.Thread(target=_run, name="probe-%s" % target_text(target), daemon=True)
            thread.start()
            started.append((target, thread, box))
        deadline = time.monotonic() + self._timeout + 1.0
        for target, thread, box in started:
            thread.join(max(0.0, deadline - time.monotonic()))
            if thread.is_alive():
                self._pending[target] = (thread, box)
                results[target] = None
            else:
                results[target] = box.get("ok", False)
        self._pending = {t: p for t, p in self._pending.items() if t in results}
        return results


# --- the keeper loop ----------------------------------------------------------------------------

class Keeper:
    """The per-source memory + one pass. Holds the states across reconnects (and, through the state
    file, across keeper restarts): a new OBS run epoch is what makes every source refresh once again."""

    def __init__(self, prober, log):
        self.prober = prober
        self.log = log
        self.states = {}        # (name, url) -> SourceState, committed only after a refresh succeeded
        self.targets = {}       # (host, port) -> (failed probes in a row, verdict), see debounce()
        self._logged = {}       # (name, url) -> the reachability last logged for it
        self.refreshes = 0
        self._unwatched = set()
        self._press_errors = {}
        self.last_sources = []

    def restore(self, states):
        """Take over the per-source states a previous keeper process stored (restored_sources()), and
        keep them in the state file until this keeper's first pass (a restart before it must not lose them)."""
        self.states = dict(states)
        self._logged = {k: v.reachable for k, v in states.items() if v.reachable is not None}
        self.last_sources = [source_entry(name, url, probe_target(url), None, st)
                             for (name, url), st in self.states.items()]

    def _verdicts(self, targets):
        """Probe each target once and debounce it into a reachability verdict (True/False/None)."""
        results = self.prober.probe_all(targets)
        verdicts = {}
        for target in set(targets):
            fails, up = debounce(*self.targets.get(target, (0, None)), results.get(target))
            self.targets[target] = (fails, up)
            verdicts[target] = up
        self.targets = {t: v for t, v in self.targets.items() if t in verdicts}
        return verdicts

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
        verdicts = self._verdicts([t for _, _, t in watched])
        listed = {(name, url) for name, url, _ in watched}
        self.states = {k: v for k, v in self.states.items() if k in listed}
        self._logged = {k: v for k, v in self._logged.items() if k in listed}
        summary = []
        for name, url, target in watched:
            key = (name, url)
            reachable = verdicts.get(target)
            if reachable is not None:
                word = transition(self._logged.get(key), reachable)
                if word is not None:
                    self.log("browser source '%s' page server %s %s" % (name, target_text(target), TRANSITION_TEXT[word]))
                self._logged[key] = reachable
            prev = self.states.get(key)
            new, action = decide(prev, reachable, epoch)
            if action is not None:
                try:
                    obs.request("PressInputPropertiesButton", {"inputName": name, "propertyName": REFRESH_BUTTON})
                except ObsRequestError as e:
                    if self._press_errors.get(key) != str(e):
                        self._press_errors[key] = str(e)
                        self.log("refresh of browser source '%s' failed (%s) -- pressed again next pass" % (name, e))
                    new = prev  # not committed: the next pass decides the same refresh again
                else:
                    self._press_errors.pop(key, None)
                    self.refreshes += 1
                    self.log("refreshed browser source '%s' (%s): %s, OBS run epoch %d" % (
                        name, url, ACTION_TEXT[action], epoch))
            if new is not None:
                self.states[key] = new
            summary.append(source_entry(name, url, target, reachable, new))
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
        clock=time.time, should_stop=None, endpoint="%s:%d" % (OBS_HOST, OBS_PORT), obs_process=None):
    """The keeper loop: connect -> passes until the connection breaks -> reconnect, forever (or
    until should_stop()). `connect` returns an ObsClient-shaped object and `obs_process` returns the
    local OBS process identity (or None); everything time-, network- and OBS-shaped is injected so the
    real loop runs under test. The OBS run epoch, its identity and the per-source states start from
    the previous keeper's state file, so a keeper restart under the same OBS refreshes nothing."""
    log = log or (lambda msg: print(msg, flush=True))
    should_stop = should_stop or (lambda: False)
    obs_process = obs_process or (lambda: None)
    conn_errors = (OSError, ObsError, ObsRequestError) + _ws_exception_types()
    keeper = Keeper(prober, log)
    prev_state, load_error = load_state(state_file)
    if load_error:
        log("state file %s unreadable (%s) -- starting as after a boot" % (state_file, load_error))
    epoch, identity = 0, None
    if prev_state is not None and _is_count(prev_state.get("obs_epoch")):
        epoch = prev_state["obs_epoch"]
        if isinstance(prev_state.get("obs_identity"), dict):
            identity = dict(prev_state["obs_identity"])
        restored = restored_sources(prev_state)
        keeper.restore(restored)
        log("resumed from %s: OBS run epoch %d, %d browser source(s) remembered, OBS identity %s" % (
            state_file, epoch, len(restored), identity))
    last_connect_error = None
    state_error = None

    def _state(connected, error=None):
        nonlocal state_error
        err = write_state(state_file, {
            "version": STATE_VERSION, "updated_epoch_s": clock(), "connected": connected, "obs_epoch": epoch,
            "obs_identity": identity, "endpoint": endpoint, "refreshes": keeper.refreshes,
            "sources": keeper.last_sources, "last_error": error,
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
        try:
            current = {"process": obs_process(), "frames": obs_render_frames(obs)}
            if obs_restarted(identity, current):
                # past every epoch a restored source carries, so each one is refreshed once
                epoch = max([epoch] + [s.refreshed_epoch or 0 for s in keeper.states.values()]) + 1
                log("connected to obs-websocket %s: a new OBS run (process %s, %s frames) -> OBS run epoch %d, "
                    "each browser source is refreshed once as soon as its page server answers" % (
                        endpoint, current["process"], current["frames"], epoch))
            else:
                log("connected to obs-websocket %s: the same OBS run as before (OBS run epoch %d) -- no "
                    "refresh round" % (endpoint, epoch))
            identity = current
            while not should_stop():
                keeper.run_pass(obs, epoch)
                frames = obs_render_frames(obs)
                if frames is not None:
                    identity["frames"] = frames
                _state(True)
                sleep(interval)
        except conn_errors as e:
            log("lost obs-websocket %s in OBS run epoch %d (%s) -- reconnecting" % (endpoint, epoch, e))
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
    ap.add_argument("--obs-process-name", default=OBS_PROCESS_NAME,
                    help="the local OBS process name whose start time identifies an OBS run")
    a = ap.parse_args(argv)
    if a.check_state:
        rc, line = check_state_file(a.check_state, max_age=a.max_age)
        print(line)
        return rc
    password = os.environ.get("OBS_PASSWORD", "")
    endpoint = "%s:%d" % (a.host, a.port)
    # The process identity is read only for a LOCAL OBS; a remote one falls back to the frame count.
    if a.host in LOOPBACK_HOSTS:
        obs_process = lambda: local_obs_process_identity(name=a.obs_process_name)  # noqa: E731
    else:
        obs_process = None
    run(lambda: ObsClient(a.host, a.port, WS_TIMEOUT_S, password), Prober(timeout=a.probe_timeout),
        interval=a.interval, state_file=a.state_file or default_state_file(), endpoint=endpoint,
        obs_process=obs_process)
    return 0


if __name__ == "__main__":
    sys.exit(main())
