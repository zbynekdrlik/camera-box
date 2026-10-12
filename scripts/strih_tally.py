#!/usr/bin/env python3
"""strih-tally: the camera tally lights driven by the strih OBS (issue 1417).

Spec: docs/superpowers/specs/2026-10-12-strih-tally-design.md. One process per Linux strih box
(strih-snv, strih-pp), installed by setup-strih step 16f as strih-tally.service, graded by
verify-strih item 42 (scripts/lib/strih-tally.sh).

  * A WebSocket RELAY on :9240 (the WsTally protocol the ESP8266 firmware in tally/esp/ speaks; the
    firmware hard-codes `atemrpi.local:9240`, path `/`). Plain-text frames:
      - `JOIN <hostname>`        tally -> server, logged with the peer IP;
      - `TALLY-PVW <n>`          preview camera n (0 = none);
      - `TALLY-PGM <n>`          program camera n (0 = none); the firmware keeps the previous one;
      - `TALLY-TRS`              a transition is running;
      - `TALLY-LOCK 1|0`, `SET-LUMINANCE <50..255>` from another client (Companion).
    A protocol text a client sends goes to every OTHER client, never back to the sender; anything else
    is dropped (logged once per client), and a frame over MAX_FRAME_BYTES closes its sender (the
    firmware copies a whole frame into its small heap). A new client gets the current TALLY-PVW +
    TALLY-PGM at once; every client gets them again every 10 s. The frames to one client never
    interleave (one lock per connection).
  * An obs-websocket 5 CLIENT to ws://127.0.0.1:4455 (the strih OBS runs without WS auth). It reads
    studio mode, the program + preview scene and the scene tree; a program / preview / studio-mode
    event re-reads those three, a scene-item / scene-list / input event the whole tree (events queued
    meanwhile are handled in ONE re-read). The scene -> camera rule is the pure
    scripts/strih_tally_decision.py.
  * TRANSITIONS: SceneTransitionStarted sends TALLY-TRS, and TALLY-TRS follows every state change
    while the transition runs (the firmware recolours on TALLY-PGM, so the TRS must come after it).
    SceneTransitionEnded / SceneTransitionVideoEnded (or TRANSITION_MAX_S without either) sends the
    full state again, which puts the lights back to red / green / gray.
  * OBS GONE: retry every 2 s; once OBS has been unreachable (a hung OBS: unanswered WS keepalive
    pings, ~10 s) for 10 s -> TALLY-PVW 0 + TALLY-PGM 0 ONCE (no stale on-air red).
  * `GET /state` on the same port (the websockets process_request hook: a plain HTTP request is
    answered, a WS upgrade proceeds) -> JSON {pgm, pvw, program_scene, preview_scene, studio_mode,
    obs_connected, camera_inputs, tallies}.

One log line per state change, tally JOIN/leave, OBS connect/disconnect, transition and gray fallback.
"""
import argparse
import asyncio
import collections
import json
import logging
import signal
import time

from websockets.asyncio.client import connect
from websockets.asyncio.server import serve

import strih_tally_decision as decision

LOG = logging.getLogger("strih-tally")

# obs-websocket 5 event subscription: Scenes (4) + Inputs (8) + Transitions (16) + SceneItems (128)
# + Ui (1024, StudioModeStateChanged).
EVENT_SUBSCRIPTIONS = 4 + 8 + 16 + 128 + 1024
# Events after which the program / preview / studio mode are read again (a cheap read).
LIGHT_EVENTS = frozenset({
    "CurrentProgramSceneChanged", "CurrentPreviewSceneChanged", "StudioModeStateChanged",
})
# Events that can change which camera a scene holds: the whole scene tree is read again.
TREE_EVENTS = frozenset({
    "SceneItemCreated", "SceneItemRemoved", "SceneItemEnableStateChanged", "SceneListChanged",
    "SceneNameChanged", "InputSettingsChanged", "InputNameChanged",
})
TRANSITION_START = "SceneTransitionStarted"
TRANSITION_ENDS = frozenset({"SceneTransitionEnded", "SceneTransitionVideoEnded"})
# A transition whose end event never came (an interrupted one) is ended after this long.
TRANSITION_MAX_S = 15.0
# The texts the relay passes on: the firmware's commands + JOIN.
RELAY_COMMANDS = frozenset({"JOIN", "TALLY-PGM", "TALLY-PVW", "TALLY-TRS", "TALLY-LOCK", "SET-LUMINANCE"})
# A WsTally frame is a short command line; the ESP8266 copies a whole frame into an Arduino String.
MAX_FRAME_BYTES = 256
JOIN_NAME_MAX = 64
REQUEST_TIMEOUT_S = 10.0
SEND_TIMEOUT_S = 2.0
# WS keepalive to OBS: a hung OBS (TCP open, nothing answering) is dropped within ~10 s.
OBS_PING_S = 5.0
NDI_INPUT_KIND = "ndi_source"


class ObsRequestError(RuntimeError):
    """An obs-websocket request answered with result false."""


class ObsSession:
    """One obs-websocket 5 connection: requests one at a time, events read meanwhile are queued."""

    def __init__(self, ws):
        self.ws = ws
        self.events = collections.deque()
        self._next_id = 0

    async def _recv(self):
        return json.loads(await asyncio.wait_for(self.ws.recv(), REQUEST_TIMEOUT_S))

    async def identify(self):
        hello = await self._recv()
        if hello.get("op") != 0:
            raise RuntimeError(f"expected obs-websocket Hello (op 0), got op {hello.get('op')}")
        if (hello.get("d") or {}).get("authentication"):
            raise RuntimeError("obs-websocket asks for a password; strih-tally runs without one")
        await self.ws.send(json.dumps({"op": 1, "d": {"rpcVersion": 1, "eventSubscriptions": EVENT_SUBSCRIPTIONS}}))
        ident = await self._recv()
        if ident.get("op") != 2:
            raise RuntimeError(f"expected obs-websocket Identified (op 2), got op {ident.get('op')}")

    async def request(self, request_type, data=None):
        self._next_id += 1
        rid = f"tally-{self._next_id}"
        msg = {"op": 6, "d": {"requestType": request_type, "requestId": rid}}
        if data is not None:
            msg["d"]["requestData"] = data
        await self.ws.send(json.dumps(msg))
        while True:
            m = await self._recv()
            d = m.get("d") or {}
            if m.get("op") == 5:
                self.events.append(d)
            elif m.get("op") == 7 and d.get("requestId") == rid:
                status = d.get("requestStatus") or {}
                if not status.get("result"):
                    raise ObsRequestError(
                        f"{request_type} failed: code {status.get('code')} {status.get('comment', '')}".strip())
                return d.get("responseData") or {}

    async def next_batch(self):
        """The queued events, or wait for the next one; then every event already queued behind it."""
        if not self.events:
            while True:
                # no timeout: an idle OBS sends nothing; the WS keepalive pings detect a dead OBS
                m = json.loads(await self.ws.recv())
                if m.get("op") == 5:
                    self.events.append(m.get("d") or {})
                    break
        batch = list(self.events)
        self.events.clear()
        return batch


def _scene_name(resp, key):
    return resp.get(key) or resp.get("sceneName")


def _item_kind(it):
    if it.get("isGroup"):
        return "group"
    return "scene" if it.get("sourceType") == "OBS_SOURCE_TYPE_SCENE" else "input"


def _items(resp):
    return [{"name": it.get("sourceName"), "kind": _item_kind(it),
             "enabled": bool(it.get("sceneItemEnabled", True))} for it in resp.get("sceneItems", [])]


def clean_name(raw):
    """A JOIN hostname as logged and shown: printable characters only, at most JOIN_NAME_MAX."""
    return "".join(ch for ch in raw if ch.isprintable()).strip()[:JOIN_NAME_MAX]


class Tally:
    def __init__(self, obs_url, resend_s=10.0, obs_loss_s=10.0, retry_s=2.0):
        self.obs_url = obs_url
        self.resend_s = resend_s
        self.obs_loss_s = obs_loss_s
        self.retry_s = retry_s
        self.clients = set()
        self.joined = {}
        self._locks = {}
        self._ignored = set()
        self.program = None
        self.preview = None
        self.studio = False
        self.tree = {}
        self.cameras = {}
        self.state = None
        self.obs_connected = False
        self.last_obs_ok = time.monotonic()
        self.gray_sent = False
        self.in_transition = False
        self.transition_since = 0.0
        self._obs_ws = None
        self._tasks = []
        self._server = None

    # --- the relay -------------------------------------------------------------------------------

    @staticmethod
    def _peer(conn):
        addr = getattr(conn, "remote_address", None)
        return addr[0] if addr else "?"

    def _forget(self, conn):
        self.clients.discard(conn)
        self._locks.pop(conn, None)
        self._ignored.discard(conn)
        return self.joined.pop(conn, None)

    async def _send_one(self, conn, texts):
        lock = self._locks.setdefault(conn, asyncio.Lock())
        async with lock:   # one sender at a time per tally: a PVW + PGM pair never interleaves
            try:
                for text in texts:
                    await asyncio.wait_for(conn.send(text), SEND_TIMEOUT_S)
            except Exception as exc:  # noqa: BLE001 -- one bad client never stops the others
                if conn in self.clients:
                    name = self._forget(conn)
                    LOG.warning("send to %s %s failed (%s): dropped", self._peer(conn), name or "(no JOIN)", exc)
                try:
                    await conn.close()
                except Exception as close_exc:  # noqa: BLE001 -- the connection is already broken
                    LOG.debug("closing %s after a failed send: %s", self._peer(conn), close_exc)

    async def send_all(self, texts, exclude=None):
        if not texts:
            return
        targets = [c for c in list(self.clients) if c is not exclude]
        if targets:
            await asyncio.gather(*(self._send_one(c, texts) for c in targets))

    def _state_texts(self):
        texts = decision.full_messages(self.state or (0, 0))
        return texts + ["TALLY-TRS"] if self.in_transition else texts

    async def handler(self, conn):
        peer = self._peer(conn)
        self.clients.add(conn)
        await self._send_one(conn, self._state_texts())
        try:
            async for raw in conn:
                if not isinstance(raw, str):
                    continue
                text = raw.strip()
                command = text.split(" ", 1)[0]
                if command not in RELAY_COMMANDS:
                    if text and conn not in self._ignored:
                        self._ignored.add(conn)
                        LOG.info("client %s sent a non-tally text, not relayed (logged once): %.40r", peer, text)
                    continue
                if command == "JOIN":
                    name = clean_name(text[5:])
                    self.joined[conn] = name
                    text = f"JOIN {name}"
                    LOG.info("JOIN %s %s", name, peer)
                await self.send_all([text], exclude=conn)
        except Exception as exc:  # noqa: BLE001 -- a broken (or oversized-frame) client is just gone
            LOG.info("client %s read error: %s", peer, exc)
        finally:
            name = self._forget(conn)
            LOG.info("LEAVE %s %s", name or "(no JOIN)", peer)

    def state_json(self):
        pgm, pvw = self.state or (0, 0)
        return {
            "pgm": pgm,
            "pvw": pvw,
            "program_scene": self.program,
            "preview_scene": self.preview,
            "studio_mode": self.studio,
            "obs_connected": self.obs_connected,
            "camera_inputs": len(self.cameras),
            "tallies": sorted(f"{self._peer(c)} {n}" for c, n in self.joined.items()),
        }

    def process_request(self, conn, request):
        if request.path.split("?", 1)[0] != "/state":
            return None
        resp = conn.respond(200, json.dumps(self.state_json()) + "\n")
        del resp.headers["Content-Type"]
        resp.headers["Content-Type"] = "application/json"
        return resp

    # --- the OBS client --------------------------------------------------------------------------

    async def _read_tree(self, obs):
        tree = {}
        scenes = [s.get("sceneName") for s in (await obs.request("GetSceneList")).get("scenes", [])]
        groups = []
        for scene in scenes:
            resp = await obs.request("GetSceneItemList", {"sceneName": scene})
            tree[scene] = _items(resp)
            groups += [it["name"] for it in tree[scene] if it["kind"] == "group"]
        for group in groups:
            if group not in tree:
                tree[group] = _items(await obs.request("GetGroupSceneItemList", {"sceneName": group}))
        cameras = {}
        for inp in (await obs.request("GetInputList")).get("inputs", []):
            kind = inp.get("unversionedInputKind") or inp.get("inputKind")
            if kind != NDI_INPUT_KIND:
                continue
            name = inp.get("inputName")
            settings = (await obs.request("GetInputSettings", {"inputName": name})).get("inputSettings", {})
            cam = decision.camera_of_ndi_name(settings.get("ndi_source_name"))
            if cam:
                cameras[name] = cam
        if len(cameras) != len(self.cameras):
            LOG.info("%d camera input(s) mapped by their NDI source name", len(cameras))
        self.tree, self.cameras = tree, cameras

    async def refresh(self, obs, tree, full=False):
        """Re-read OBS and send the change (TALLY-TRS after it during a transition); `full` sends the
        whole state. Returns whether anything was sent."""
        studio = bool((await obs.request("GetStudioModeEnabled")).get("studioModeEnabled"))
        program = _scene_name(await obs.request("GetCurrentProgramScene"), "currentProgramSceneName")
        preview = None
        if studio:
            try:
                preview = _scene_name(await obs.request("GetCurrentPreviewScene"), "currentPreviewSceneName")
            except ObsRequestError as exc:
                LOG.info("no preview scene (%s)", exc)
        if tree:
            await self._read_tree(obs)
        self.studio, self.program, self.preview = studio, program, preview
        new = decision.state_of(program, preview, studio, self.tree, self.cameras)
        old = self.state
        changed = decision.messages(old, new)
        self.state = new
        self.obs_connected = True
        self.last_obs_ok = time.monotonic()
        self.gray_sent = False
        if changed:
            a = old or (0, 0)
            LOG.info("pgm %d->%d pvw %d->%d (program=%s preview=%s)", a[0], new[0], a[1], new[1], program, preview)
        out = self._state_texts() if full else changed
        if out and self.in_transition and not full:
            out = out + ["TALLY-TRS"]
        await self.send_all(out)
        return bool(out)

    async def _handle_batch(self, obs, batch):
        pending = None   # None | "light" | "tree": one re-read for a run of state events

        async def flush():
            nonlocal pending
            if pending:
                await self.refresh(obs, tree=(pending == "tree"))
                pending = None

        for ev in batch:
            kind = ev.get("eventType")
            if kind == TRANSITION_START:
                await flush()
                self.in_transition = True
                self.transition_since = time.monotonic()
                LOG.info("transition started (%s)", (ev.get("eventData") or {}).get("transitionName"))
                if not await self.refresh(obs, tree=False):
                    await self.send_all(["TALLY-TRS"])
            elif kind in TRANSITION_ENDS:
                await flush()
                if self.in_transition:
                    self.in_transition = False
                    LOG.info("transition ended (%s): full state resent", kind)
                    await self.refresh(obs, tree=False, full=True)
            elif kind in TREE_EVENTS:
                pending = "tree"
            elif kind in LIGHT_EVENTS and pending is None:
                pending = "light"
        await flush()

    async def obs_loop(self):
        failing = False
        while True:
            try:
                async with connect(self.obs_url, max_size=None, open_timeout=REQUEST_TIMEOUT_S,
                                   ping_interval=OBS_PING_S, ping_timeout=OBS_PING_S) as ws:
                    self._obs_ws = ws
                    obs = ObsSession(ws)
                    await obs.identify()
                    self.in_transition = False
                    await self.refresh(obs, tree=True)
                    LOG.info("OBS connected: %s (studio=%s program=%s preview=%s, %d camera input(s))",
                             self.obs_url, self.studio, self.program, self.preview, len(self.cameras))
                    failing = False
                    while True:
                        await self._handle_batch(obs, await obs.next_batch())
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 -- OBS down / restarting: retry
                was = self.obs_connected
                self.obs_connected = False
                self.in_transition = False
                if was or not failing:
                    LOG.warning("OBS disconnected: %s: %s (retrying every %ss)", self.obs_url,
                                exc or type(exc).__name__, self.retry_s)
                failing = True
            finally:
                self._obs_ws = None
            await asyncio.sleep(self.retry_s)

    # --- the timers ------------------------------------------------------------------------------

    async def resend_loop(self):
        while True:
            await asyncio.sleep(self.resend_s)
            if self.state is not None:
                await self.send_all(self._state_texts())

    async def fallback_loop(self):
        while True:
            await asyncio.sleep(min(1.0, self.obs_loss_s / 3))
            now = time.monotonic()
            if self.in_transition and now - self.transition_since > TRANSITION_MAX_S:
                self.in_transition = False
                LOG.warning("no transition end event after %ss: full state resent", TRANSITION_MAX_S)
                await self.send_all(self._state_texts())
            if self.obs_connected:
                self.last_obs_ok = now
                continue
            if not self.gray_sent and now - self.last_obs_ok > self.obs_loss_s:
                self.state = (0, 0)
                self.gray_sent = True
                LOG.warning("OBS lost > %ss: tally gray (TALLY-PVW 0 + TALLY-PGM 0)", self.obs_loss_s)
                await self.send_all(decision.full_messages(self.state))

    # --- lifecycle -------------------------------------------------------------------------------

    async def start_server(self, host, port):
        self._server = await serve(self.handler, host, port, process_request=self.process_request,
                                   max_size=MAX_FRAME_BYTES)
        self._tasks = [asyncio.create_task(c) for c in (self.obs_loop(), self.resend_loop(), self.fallback_loop())]
        return self._server

    async def stop(self):
        for task in self._tasks:
            task.cancel()
        for result in await asyncio.gather(*self._tasks, return_exceptions=True):
            if isinstance(result, Exception):
                LOG.warning("a strih-tally loop ended with an error: %s", result)
        self._tasks = []
        if self._obs_ws is not None:
            await self._obs_ws.close()


async def _run(args):
    tally = Tally(args.obs, resend_s=args.resend_s, obs_loss_s=args.obs_loss_s, retry_s=args.retry_s)
    server = await tally.start_server(args.host, args.port)
    LOG.info("strih-tally listening on %s:%d (OBS %s)", args.host, args.port, args.obs)
    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)
    await stop.wait()
    LOG.info("strih-tally stopping")
    await tally.stop()
    server.close()
    await server.wait_closed()


def main(argv=None):
    p = argparse.ArgumentParser(description="Camera tally relay + strih OBS client (issue 1417)")
    p.add_argument("--port", type=int, default=9240)
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--obs", default="ws://127.0.0.1:4455")
    p.add_argument("--resend-s", type=float, default=10.0)
    p.add_argument("--obs-loss-s", type=float, default=10.0)
    p.add_argument("--retry-s", type=float, default=2.0)
    args = p.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(message)s")
    asyncio.run(_run(args))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
