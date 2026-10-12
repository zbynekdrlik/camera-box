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
    Every text a client sends goes to every OTHER client, never back to the sender. A new client
    gets the current TALLY-PVW + TALLY-PGM at once; every client gets them again every 10 s.
  * An obs-websocket 5 CLIENT to ws://127.0.0.1:4455 (the strih OBS runs without WS auth). It reads
    studio mode, the program + preview scene and the scene tree, and re-reads on every program /
    preview / studio-mode / transition / scene-item / input event. The scene -> camera rule is the
    pure scripts/strih_tally_decision.py. OBS unreachable for 10 s -> TALLY-PVW 0 + TALLY-PGM 0 once
    (no stale on-air red).
  * `GET /state` on the same port (the websockets process_request hook: a plain HTTP request is
    answered, a WS upgrade proceeds) -> JSON {pgm, pvw, program_scene, preview_scene, studio_mode,
    obs_connected, tallies}.

One log line per state change, tally JOIN/leave, OBS connect/disconnect and the gray fallback.
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
    "SceneTransitionEnded",
})
# Events that can change which camera a scene holds: the whole scene tree is read again.
TREE_EVENTS = frozenset({
    "SceneItemCreated", "SceneItemRemoved", "SceneItemEnableStateChanged", "SceneListChanged",
    "InputSettingsChanged", "InputNameChanged",
})
REQUEST_TIMEOUT_S = 10.0
SEND_TIMEOUT_S = 2.0
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

    async def next_event(self):
        if self.events:
            return self.events.popleft()
        while True:
            # no timeout: an idle OBS sends nothing; the WS keepalive pings detect a dead OBS
            m = json.loads(await self.ws.recv())
            if m.get("op") == 5:
                return m.get("d") or {}


def _scene_name(resp, key):
    return resp.get(key) or resp.get("sceneName")


def _item_kind(it):
    if it.get("isGroup"):
        return "group"
    return "scene" if it.get("sourceType") == "OBS_SOURCE_TYPE_SCENE" else "input"


def _items(resp):
    return [{"name": it.get("sourceName"), "kind": _item_kind(it),
             "enabled": bool(it.get("sceneItemEnabled", True))} for it in resp.get("sceneItems", [])]


class Tally:
    def __init__(self, obs_url, resend_s=10.0, obs_loss_s=10.0, retry_s=2.0):
        self.obs_url = obs_url
        self.resend_s = resend_s
        self.obs_loss_s = obs_loss_s
        self.retry_s = retry_s
        self.clients = set()
        self.joined = {}
        self.program = None
        self.preview = None
        self.studio = False
        self.tree = {}
        self.cameras = {}
        self.state = None
        self.obs_connected = False
        self.last_obs_ok = time.monotonic()
        self.gray_sent = False
        self._obs_ws = None
        self._tasks = []
        self._server = None

    # --- the relay -------------------------------------------------------------------------------

    @staticmethod
    def _peer(conn):
        addr = getattr(conn, "remote_address", None)
        return addr[0] if addr else "?"

    async def _send_one(self, conn, texts):
        try:
            for text in texts:
                await asyncio.wait_for(conn.send(text), SEND_TIMEOUT_S)
        except Exception as exc:  # noqa: BLE001 -- one bad client never stops the others
            if conn in self.clients:
                self.clients.discard(conn)
                LOG.warning("send to %s %s failed (%s): dropped", self._peer(conn),
                            self.joined.get(conn, "(no JOIN)"), exc)
                self.joined.pop(conn, None)
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

    async def handler(self, conn):
        peer = self._peer(conn)
        self.clients.add(conn)
        await self._send_one(conn, decision.full_messages(self.state or (0, 0)))
        try:
            async for raw in conn:
                if not isinstance(raw, str):
                    continue
                text = raw.strip()
                if not text:
                    continue
                if text.startswith("JOIN "):
                    self.joined[conn] = text[5:].strip()
                    LOG.info("JOIN %s %s", self.joined[conn], peer)
                await self.send_all([text], exclude=conn)
        except Exception as exc:  # noqa: BLE001 -- a broken client is just gone
            LOG.info("client %s read error: %s", peer, exc)
        finally:
            name = self.joined.pop(conn, None)
            self.clients.discard(conn)
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
        self.tree, self.cameras = tree, cameras

    async def refresh(self, obs, tree):
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
        out = decision.messages(old, new)
        self.state = new
        self.obs_connected = True
        self.last_obs_ok = time.monotonic()
        self.gray_sent = False
        if out:
            a = old or (0, 0)
            LOG.info("pgm %d->%d pvw %d->%d (program=%s preview=%s)", a[0], new[0], a[1], new[1], program, preview)
            await self.send_all(out)

    async def obs_loop(self):
        failing = False
        while True:
            try:
                async with connect(self.obs_url, max_size=None, open_timeout=REQUEST_TIMEOUT_S) as ws:
                    self._obs_ws = ws
                    obs = ObsSession(ws)
                    await obs.identify()
                    await self.refresh(obs, tree=True)
                    LOG.info("OBS connected: %s (studio=%s program=%s preview=%s, %d camera input(s))",
                             self.obs_url, self.studio, self.program, self.preview, len(self.cameras))
                    failing = False
                    while True:
                        ev = await obs.next_event()
                        kind = ev.get("eventType")
                        if kind == "SceneTransitionStarted":
                            LOG.info("transition started (%s)", (ev.get("eventData") or {}).get("transitionName"))
                            await self.send_all(["TALLY-TRS"])
                        elif kind in TREE_EVENTS:
                            await self.refresh(obs, tree=True)
                        elif kind in LIGHT_EVENTS:
                            await self.refresh(obs, tree=False)
            except asyncio.CancelledError:
                raise
            except Exception as exc:  # noqa: BLE001 -- OBS down / restarting: retry
                was = self.obs_connected
                self.obs_connected = False
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
                await self.send_all(decision.full_messages(self.state))

    async def fallback_loop(self):
        while True:
            await asyncio.sleep(min(1.0, self.obs_loss_s / 3))
            now = time.monotonic()
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
        self._server = await serve(self.handler, host, port, process_request=self.process_request)
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
